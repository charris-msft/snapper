use super::pause::{PauseTimeline, SAMPLE_RATE};
use crate::platform::audio::Packet;
use anyhow::{ensure, Context, Result};
use std::collections::VecDeque;

pub struct Prepared {
    pub sample: u64,
    pub pcm: Vec<u8>,
    pub silence_frames: u64,
    native_frames: usize,
    revision: u64,
}

struct Pending {
    sample: u64,
    offset: usize,
    packet: Packet,
    device_gap: Option<(u64, u64)>,
}

pub struct AudioTimeline {
    system: bool,
    origin: i64,
    committed: u64,
    drained: u64,
    last_delivery: i64,
    last_timestamp: Option<i64>,
    device_end: Option<u64>,
    previous_wall_end: u64,
    pending: VecDeque<Pending>,
    released: usize,
    revision: u64,
}

impl AudioTimeline {
    pub fn new(system: bool, origin: i64) -> Self {
        Self {
            system,
            origin,
            committed: 0,
            drained: 0,
            last_delivery: origin,
            last_timestamp: None,
            device_end: None,
            previous_wall_end: 0,
            pending: VecDeque::new(),
            released: 0,
            revision: 0,
        }
    }

    pub fn observe(&mut self, packet: Packet, observed: i64) -> Result<bool> {
        ensure!(
            packet.frames > 0
                && packet.frames <= 48_000
                && packet.pcm.len() == packet.frames as usize * 4,
            "Audio sample count disagrees with its bounded PCM payload."
        );
        ensure!(
            self.last_timestamp
                .is_none_or(|stamp| packet.timestamp >= stamp),
            "Audio timestamp moved backwards."
        );
        ensure!(
            packet.timestamp <= observed + 1_000_000,
            "Audio timestamp is more than 100 ms in the future."
        );
        ensure!(
            self.device_end
                .is_none_or(|end| packet.device_position >= end),
            "Audio device sample position moved backwards."
        );
        let device_gap = self
            .device_end
            .map(|end| (self.previous_wall_end, packet.device_position - end));
        self.device_end = Some(
            packet
                .device_position
                .checked_add(u64::from(packet.frames))
                .context("Audio device position overflow")?,
        );
        let discontinuity = packet.discontinuity && self.last_timestamp.is_some();
        self.last_timestamp = Some(packet.timestamp);
        self.last_delivery = observed;
        let start = (i128::from(packet.timestamp) - i128::from(self.origin))
            * i128::from(SAMPLE_RATE)
            / 10_000_000;
        let trim = if start < 0 {
            (-start).min(i128::from(packet.frames)) as usize
        } else {
            0
        };
        let sample = u64::try_from(start.max(0)).context("Audio timestamp overflow")?;
        self.previous_wall_end = sample + u64::from(packet.frames) - trim as u64;
        if trim == packet.frames as usize {
            self.released += packet.pcm.len();
            return Ok(discontinuity);
        }
        ensure!(
            self.pending.len() < 512,
            "The per-source pending audio packet budget is full."
        );
        self.pending.push_back(Pending {
            sample,
            offset: trim,
            packet,
            device_gap,
        });
        Ok(discontinuity)
    }

    pub fn observe_drained(&mut self, observed: i64) -> Result<()> {
        if !self.system {
            ensure!(observed - self.last_delivery <= 10_000_000,
                "The selected microphone stopped delivering packets for 1 second. It was not replaced with generated silence.");
        }
        self.drained = self
            .drained
            .max(((observed - self.origin - 2_500_000).max(0) as u64) * SAMPLE_RATE / 10_000_000);
        Ok(())
    }

    fn consume(&mut self, frames: usize) {
        if let Some(packet) = self.pending.front_mut() {
            packet.offset += frames;
            packet.sample += frames as u64;
        }
        if self
            .pending
            .front()
            .is_some_and(|p| p.offset == p.packet.frames as usize)
        {
            if let Some(packet) = self.pending.pop_front() {
                self.released += packet.packet.pcm.len();
            }
        }
        self.revision += 1;
    }

    fn silence(&self, until: u64) -> Option<Prepared> {
        let frames = until.saturating_sub(self.committed).min(4800);
        (frames > 0).then(|| Prepared {
            sample: self.committed,
            pcm: vec![0; frames as usize * 4],
            silence_frames: frames,
            native_frames: 0,
            revision: self.revision,
        })
    }

    pub fn prepare_next(
        &mut self,
        clock: &PauseTimeline,
        sealed_wall: u64,
        media_limit: u64,
        final_drain: bool,
    ) -> Result<Option<Prepared>> {
        let sealed_wall = clock
            .frozen()
            .map_or(sealed_wall, |end| sealed_wall.min(end));
        loop {
            let Some(front) = self.pending.front() else {
                if !self.system {
                    return Ok(None);
                }
                let horizon = if final_drain {
                    sealed_wall
                } else {
                    self.drained.min(sealed_wall)
                };
                let until = clock.media_at(horizon).min(media_limit);
                return Ok(self.silence(until));
            };
            if front.sample >= sealed_wall {
                if final_drain {
                    self.consume(front.packet.frames as usize - front.offset);
                    continue;
                }
                // Future packets stay charged and uncommitted until this wall interval is sealed.
                return Ok(None);
            }
            let available = (front.packet.frames as usize - front.offset)
                .min((sealed_wall - front.sample) as usize);
            let start = front.sample;
            let end = start + available as u64;
            let Some(active) = clock.next_active(start, end) else {
                self.consume(available);
                continue;
            };
            if active.wall > start {
                self.consume((active.wall - start) as usize);
                continue;
            }
            if let Some((previous, missing)) = front.device_gap {
                let paused = clock.paused_between(previous.min(start), start);
                ensure!(
                    missing.saturating_sub(paused) <= 24_000,
                    "Audio device lost more than 500 ms of active samples."
                );
                self.pending.front_mut().unwrap().device_gap = None;
            }
            if active.media >= media_limit {
                self.consume(available);
                continue;
            }
            let overlap = self.committed.saturating_sub(active.media);
            ensure!(overlap <= 960,
                "Active audio arrived more than 20 ms behind committed media. Capture interrupted rather than silently dropping sound.");
            if overlap > 0 {
                self.consume(overlap.min(active.frames) as usize);
                continue;
            }
            let gap = active.media.saturating_sub(self.committed);
            ensure!(gap <= 24_000, "Active audio timestamp gap exceeded 500 ms.");
            if gap > 480 {
                return Ok(self.silence(active.media));
            }
            let frames = active.frames.min(4800).min(media_limit - active.media) as usize;
            let packet = self.pending.front().unwrap();
            let offset = packet.offset * 4;
            return Ok(Some(Prepared {
                sample: active.media,
                pcm: packet.packet.pcm[offset..offset + frames * 4].to_vec(),
                silence_frames: 0,
                native_frames: frames,
                revision: self.revision,
            }));
        }
    }

    pub fn commit(&mut self, prepared: &Prepared) -> Result<()> {
        ensure!(
            prepared.revision == self.revision,
            "Prepared audio was superseded before admission."
        );
        self.committed = self
            .committed
            .max(prepared.sample + prepared.pcm.len() as u64 / 4);
        if prepared.native_frames > 0 {
            self.consume(prepared.native_frames);
        } else {
            self.revision += 1;
        }
        Ok(())
    }

    pub fn take_released_bytes(&mut self) -> usize {
        std::mem::take(&mut self.released)
    }
    pub fn discard_pending(&mut self) -> usize {
        self.released += self
            .pending
            .drain(..)
            .map(|p| p.packet.pcm.len())
            .sum::<usize>();
        self.take_released_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn packet(position: u64, sample: u64, frames: u32, marker: u8) -> Packet {
        Packet {
            device_position: position,
            timestamp: (sample as i128 * 10_000_000 / i128::from(SAMPLE_RATE)) as i64,
            frames,
            pcm: vec![marker; frames as usize * 4],
            discontinuity: false,
        }
    }
    fn drain(
        timeline: &mut AudioTimeline,
        clock: &PauseTimeline,
        sealed: u64,
    ) -> Vec<(u64, Vec<u8>)> {
        let mut output = Vec::new();
        while let Some(prepared) = timeline
            .prepare_next(clock, sealed, u64::MAX, false)
            .unwrap()
        {
            output.push((prepared.sample, prepared.pcm.clone()));
            timeline.commit(&prepared).unwrap();
        }
        output
    }

    #[test]
    fn missing_microphone_fails_while_legitimate_silent_packets_keep_it_alive() {
        let mut missing = AudioTimeline::new(false, 0);
        missing.observe_drained(9_000_000).unwrap();
        assert!(missing.observe_drained(10_000_001).is_err());
        let mut silent = AudioTimeline::new(false, 0);
        for i in 0..300u64 {
            let now = i as i64 * 100_000;
            silent
                .observe(packet(i * 480, i * 480, 480, 0), now)
                .unwrap();
            silent.observe_drained(now + 90_000).unwrap();
            drain(&mut silent, &PauseTimeline::default(), (i + 1) * 480);
        }
    }

    #[test]
    fn quiet_loopback_silence_is_bounded_and_not_a_microphone_fallback() {
        let mut system = AudioTimeline::new(true, 0);
        system.observe_drained(10_000_001).unwrap();
        let prepared = system
            .prepare_next(&PauseTimeline::default(), 48_000, u64::MAX, false)
            .unwrap()
            .unwrap();
        assert_eq!(prepared.pcm.len(), 4800 * 4);
        assert_eq!(prepared.sample, 0);
        let mut mic = AudioTimeline::new(false, 0);
        assert!(mic
            .prepare_next(&PauseTimeline::default(), 48_000, u64::MAX, false)
            .unwrap()
            .is_none());
    }

    #[test]
    fn fractional_millisecond_packets_preserve_sample_positions() {
        let mut timeline = AudioTimeline::new(false, 0);
        for i in 0..100u64 {
            let sample = i * 127;
            timeline
                .observe(
                    packet(sample, sample, 127, 9),
                    (sample * 10_000_000 / SAMPLE_RATE) as i64,
                )
                .unwrap();
            let output = drain(&mut timeline, &PauseTimeline::default(), sample + 127);
            assert!(!output.is_empty());
            assert!((output[0].0 as i64 - sample as i64).abs() <= 1);
        }
    }

    #[test]
    fn device_regressions_and_excessive_active_gaps_are_not_hidden() {
        let mut timeline = AudioTimeline::new(false, 0);
        timeline.observe(packet(100, 0, 480, 1), 0).unwrap();
        assert!(timeline.observe(packet(10, 480, 480, 1), 100_000).is_err());
        let mut gap = AudioTimeline::new(false, 0);
        gap.observe(packet(0, 28_800, 480, 1), 6_000_000).unwrap();
        assert!(gap
            .prepare_next(&PauseTimeline::default(), 30_000, u64::MAX, false)
            .is_err());
    }

    #[test]
    fn different_delivery_times_keep_only_exact_active_pcm_on_both_sources() {
        let mut clock = PauseTimeline::default();
        clock.set_paused(4801, true).unwrap();
        clock.set_paused(14_407, false).unwrap();
        for (system, observed, marker) in [(true, 3_200_000, 7), (false, 3_300_000, 11)] {
            let mut timeline = AudioTimeline::new(system, 0);
            timeline
                .observe(packet(0, 4320, 10_560, marker), observed)
                .unwrap();
            let chunks = drain(&mut timeline, &clock, 16_000);
            let real: Vec<_> = chunks
                .into_iter()
                .filter(|(_, bytes)| bytes[0] == marker)
                .collect();
            assert_eq!(real.iter().map(|(_, p)| p.len() / 4).sum::<usize>(), 954);
            assert_eq!(real[0].0, 4320);
            assert_eq!(real[1].0, 4801);
            assert_eq!(timeline.take_released_bytes(), 10_560 * 4);
        }
    }

    #[test]
    fn paused_late_packets_do_not_conflict_with_idle_silence_commitment() {
        let mut clock = PauseTimeline::default();
        clock.set_paused(480, true).unwrap();
        let mut timeline = AudioTimeline::new(true, 0);
        timeline.observe(packet(0, 0, 480, 7), 100_000).unwrap();
        drain(&mut timeline, &clock, 480);
        timeline.observe_drained(5_500_000).unwrap();
        assert!(drain(&mut timeline, &clock, 26_400).is_empty());
        timeline
            .observe(packet(480, 480, 480, 8), 5_600_000)
            .unwrap();
        assert!(drain(&mut timeline, &clock, 26_880).is_empty());
        clock.set_paused(48_000, false).unwrap();
        timeline
            .observe(packet(48_000, 48_000, 480, 9), 10_100_000)
            .unwrap();
        let resumed = drain(&mut timeline, &clock, 48_480);
        assert_eq!(resumed, vec![(480, vec![9; 480 * 4])]);
    }

    #[test]
    fn future_packet_is_held_charged_until_a_pause_boundary_can_be_sealed() {
        let mut timeline = AudioTimeline::new(false, 0);
        let mut clock = PauseTimeline::default();
        timeline
            .observe(packet(0, 9120, 480, 12), 1_000_000)
            .unwrap();
        assert!(drain(&mut timeline, &clock, 4800).is_empty());
        assert_eq!(timeline.take_released_bytes(), 0);
        clock.set_paused(5280, true).unwrap();
        clock.set_paused(53_280, false).unwrap();
        assert!(drain(&mut timeline, &clock, 60_000).is_empty());
        assert_eq!(timeline.take_released_bytes(), 480 * 4);
    }

    #[test]
    fn stopping_trims_pending_tails_without_waiting_for_resume_or_future_time() {
        let mut clock = PauseTimeline::default();
        clock.freeze(127);
        let mut timeline = AudioTimeline::new(false, 0);
        timeline.observe(packet(0, 0, 480, 13), 100_000).unwrap();
        let prepared = timeline
            .prepare_next(&clock, 127, 127, true)
            .unwrap()
            .unwrap();
        assert_eq!(prepared.pcm.len(), 127 * 4);
        timeline.commit(&prepared).unwrap();
        assert!(timeline
            .prepare_next(&clock, 127, 127, true)
            .unwrap()
            .is_none());
        assert_eq!(timeline.take_released_bytes(), 480 * 4);
    }
}
