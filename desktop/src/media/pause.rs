use anyhow::{ensure, Result};

pub const SAMPLE_RATE: u64 = 48_000;
const MAX_TRANSITIONS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActiveSpan {
    pub wall: u64,
    pub media: u64,
    pub frames: u64,
}

#[derive(Default)]
pub struct PauseTimeline {
    pauses: Vec<(u64, Option<u64>)>,
    last_transition: u64,
    end: Option<u64>,
}

impl PauseTimeline {
    pub fn paused(&self) -> bool {
        self.pauses.last().is_some_and(|(_, end)| end.is_none())
    }
    pub fn frozen(&self) -> Option<u64> {
        self.end
    }

    pub fn set_paused(&mut self, wall: u64, paused: bool) -> Result<bool> {
        ensure!(
            self.end.is_none(),
            "The stopped media timeline cannot resume."
        );
        ensure!(
            wall >= self.last_transition,
            "Pause control clock moved backwards."
        );
        if self.paused() == paused {
            return Ok(false);
        }
        ensure!(
            self.pauses.len() < MAX_TRANSITIONS || !paused,
            "The 4,096-pause budget is full. Stop and start a new recording."
        );
        if paused {
            self.pauses.push((wall, None));
        } else if let Some((_, end)) = self.pauses.last_mut() {
            *end = Some(wall);
        }
        self.last_transition = wall;
        Ok(true)
    }

    pub fn freeze(&mut self, wall: u64) {
        self.end.get_or_insert(wall);
    }

    pub fn media_at(&self, wall: u64) -> u64 {
        let wall = self.end.map_or(wall, |end| wall.min(end));
        let removed: u64 = self
            .pauses
            .iter()
            .map(|(start, end)| end.unwrap_or(wall).min(wall).saturating_sub(*start))
            .sum();
        wall.saturating_sub(removed)
    }

    pub fn wall_at(&self, media: u64) -> u64 {
        let mut removed = 0;
        for &(start, end) in &self.pauses {
            if media <= start.saturating_sub(removed) {
                return media + removed;
            }
            match end {
                Some(end) => removed += end - start,
                None => return start,
            }
        }
        media + removed
    }

    pub fn paused_between(&self, start: u64, end: u64) -> u64 {
        (end - start).saturating_sub(self.media_at(end).saturating_sub(self.media_at(start)))
    }

    pub fn next_active(&self, start: u64, end: u64) -> Option<ActiveSpan> {
        let end = self.end.map_or(end, |frozen| end.min(frozen));
        let mut wall = start;
        for &(pause, resume) in &self.pauses {
            if wall >= end {
                return None;
            }
            if wall < pause {
                let until = end.min(pause);
                return Some(ActiveSpan {
                    wall,
                    media: self.media_at(wall),
                    frames: until - wall,
                });
            }
            if wall < resume.unwrap_or(u64::MAX) {
                wall = resume.unwrap_or(end).min(end);
            }
        }
        (wall < end).then(|| ActiveSpan {
            wall,
            media: self.media_at(wall),
            frames: end - wall,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn off_grid_pauses_share_an_exact_sample_clock() {
        let mut clock = PauseTimeline::default();
        clock.set_paused(4801, true).unwrap();
        assert_eq!(clock.media_at(480_000), 4801);
        clock.set_paused(14_407, false).unwrap();
        let first = clock.next_active(4320, 14_880).unwrap();
        let second = clock
            .next_active(first.wall + first.frames, 14_880)
            .unwrap();
        assert_eq!(
            first,
            ActiveSpan {
                wall: 4320,
                media: 4320,
                frames: 481
            }
        );
        assert_eq!(
            second,
            ActiveSpan {
                wall: 14_407,
                media: 4801,
                frames: 473
            }
        );
        assert_eq!(clock.media_at(14_880), 5274);
    }

    #[test]
    fn stop_during_pause_freezes_elapsed_through_release_and_finalization() {
        let mut clock = PauseTimeline::default();
        clock.set_paused(48_000, true).unwrap();
        clock.freeze(96_000);
        clock.freeze(500_000);
        assert_eq!(clock.frozen(), Some(96_000));
        assert_eq!(clock.media_at(500_000), 48_000);
        assert!(clock.set_paused(100_000, false).is_err());
    }

    #[test]
    fn limits_measure_active_time_even_after_a_long_pause() {
        let mut clock = PauseTimeline::default();
        clock.set_paused(SAMPLE_RATE, true).unwrap();
        clock.set_paused(100 * SAMPLE_RATE, false).unwrap();
        assert_eq!(clock.media_at(102 * SAMPLE_RATE), 3 * SAMPLE_RATE);
        assert_eq!(clock.wall_at(7200 * SAMPLE_RATE), 7299 * SAMPLE_RATE);
        clock.freeze(clock.wall_at(7200 * SAMPLE_RATE));
        assert_eq!(clock.media_at(u64::MAX), 7200 * SAMPLE_RATE);
    }

    #[test]
    fn redundant_commands_do_not_add_intervals_and_history_is_bounded() {
        let mut clock = PauseTimeline::default();
        for i in 0..MAX_TRANSITIONS {
            assert!(clock.set_paused(i as u64 * 2, true).unwrap());
            assert!(!clock.set_paused(i as u64 * 2, true).unwrap());
            assert!(clock.set_paused(i as u64 * 2 + 1, false).unwrap());
        }
        assert!(clock.set_paused(MAX_TRANSITIONS as u64 * 2, true).is_err());
    }
}
