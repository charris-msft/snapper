use super::pause::{PauseTimeline, SAMPLE_RATE};
use crate::platform::audio::{source_label, Packet, PacketTiming};
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
    last_packet: Option<PacketTiming>,
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
            last_packet: None,
            previous_wall_end: 0,
            pending: VecDeque::new(),
            released: 0,
            revision: 0,
        }
    }

    pub fn observe(&mut self, packet: Packet, observed: i64) -> Result<bool> {
        let timing = packet.timing;
        let previous = self.last_packet;
        self.observe_packet(packet, observed).with_context(|| {
            format!(
            "{} audio observation: current=[{timing}] previous=[{}] observed_qpc_100ns={observed}",
            source_label(self.system),
            previous.map(|packet| packet.to_string()).unwrap_or_else(|| "none".into()),
        )
        })
    }

    fn observe_packet(&mut self, packet: Packet, observed: i64) -> Result<bool> {
        let timing = packet.timing;
        timing.validate()?;
        ensure!(
            packet.pcm.len() == timing.frames as usize * 4,
            "Audio sample count disagrees with its bounded PCM payload."
        );
        ensure!(
            self.last_packet
                .is_none_or(|previous| timing.timestamp >= previous.timestamp),
            "Audio timestamp moved backwards."
        );
        ensure!(
            i128::from(timing.timestamp) <= i128::from(observed) + 1_000_000,
            "Audio timestamp is more than 100 ms in the future."
        );
        // DATA_DISCONTINUITY explicitly ends correlation with the previous device position.
        // The QPC timeline and active gap/overlap checks remain independent of this boundary.
        let device_gap = if timing.discontinuity() {
            None
        } else if let Some(previous) = self.last_packet {
            let end = previous.end_position()?;
            ensure!(timing.device_position >= end,
                "Unflagged audio device position moved backwards or overlapped: position={} previous_end={end}.",
                timing.device_position);
            Some((self.previous_wall_end, timing.device_position - end))
        } else {
            None
        };
        let discontinuity = timing.discontinuity() && self.last_packet.is_some();
        let start = (i128::from(timing.timestamp) - i128::from(self.origin))
            * i128::from(SAMPLE_RATE)
            / 10_000_000;
        let trim = if start < 0 {
            (-start).min(i128::from(timing.frames)) as usize
        } else {
            0
        };
        let sample = u64::try_from(start.max(0)).context("Audio timestamp overflow")?;
        ensure!(
            trim == timing.frames as usize || self.pending.len() < 512,
            "The per-source pending audio packet budget is full."
        );
        self.last_packet = Some(timing);
        self.last_delivery = observed;
        self.previous_wall_end = sample + u64::from(timing.frames) - trim as u64;
        if trim == timing.frames as usize {
            self.released += packet.pcm.len();
            return Ok(discontinuity);
        }
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
                "The selected microphone stopped delivering packets for 1 second. It was not replaced with generated silence. Last packet={:?}, observed_qpc_100ns={observed}.",
                self.last_packet);
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
            .is_some_and(|p| p.offset == p.packet.timing.frames as usize)
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
                    self.consume(front.packet.timing.frames as usize - front.offset);
                    continue;
                }
                // Future packets stay charged and uncommitted until this wall interval is sealed.
                return Ok(None);
            }
            let timing = front.packet.timing;
            let source = source_label(self.system);
            let available =
                (timing.frames as usize - front.offset).min((sealed_wall - front.sample) as usize);
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
                    "{source}: audio device lost more than 500 ms of active samples. Packet=[{timing}] missing_frames={missing} paused_frames={paused}."
                );
                self.pending.front_mut().unwrap().device_gap = None;
            }
            if active.media >= media_limit {
                self.consume(available);
                continue;
            }
            let overlap = self.committed.saturating_sub(active.media);
            ensure!(overlap <= 960,
                "{source}: active audio arrived more than 20 ms behind committed media. Capture interrupted rather than silently dropping sound. Packet=[{timing}] active_sample={} committed_sample={} overlap_frames={overlap}.",
                active.media, self.committed);
            if overlap > 0 {
                self.consume(overlap.min(active.frames) as usize);
                continue;
            }
            let gap = active.media.saturating_sub(self.committed);
            ensure!(gap <= 24_000, "{source}: active audio timestamp gap exceeded 500 ms. Packet=[{timing}] active_sample={} committed_sample={} gap_frames={gap}.",
                active.media, self.committed);
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
    use windows::Win32::Media::Audio::{
        AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR,
    };
    fn packet(position: u64, sample: u64, frames: u32, marker: u8) -> Packet {
        Packet {
            timing: PacketTiming {
                sequence: sample / 480 + 1,
                device_position: position,
                timestamp: (sample as i128 * 10_000_000 / i128::from(SAMPLE_RATE)) as i64,
                frames,
                flags: 0,
                client_rate_hz: SAMPLE_RATE as u32,
                engine_mix_rate_hz: None,
            },
            pcm: vec![marker; frames as usize * 4],
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
    fn declared_discontinuity_starts_new_position_correlation_without_replacing_pcm() {
        for system in [false, true] {
            let mut timeline = AudioTimeline::new(system, 0);
            let clock = PauseTimeline::default();
            let mut output = Vec::new();
            for (position, sample, marker, discontinuity) in
                [(9600, 0, 1, false), (0, 480, 2, true), (480, 960, 3, false)]
            {
                let mut next = packet(position, sample, 480, marker);
                next.timing.flags = if discontinuity {
                    AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32
                } else {
                    0
                };
                let observed = next.timing.timestamp + 100_000;
                assert_eq!(
                    timeline.observe(next, observed).unwrap(),
                    discontinuity,
                    "A declared discontinuity ends correlation with the previous device position, not the QPC timeline."
                );
                output.extend(drain(&mut timeline, &clock, sample + 480));
            }
            assert_eq!(
                output,
                vec![
                    (0, vec![1; 1920]),
                    (480, vec![2; 1920]),
                    (960, vec![3; 1920])
                ]
            );
            assert_eq!(timeline.take_released_bytes(), 5760);
        }
    }

    #[test]
    fn unflagged_position_overlap_fails_before_and_after_a_declared_reset() {
        for system in [false, true] {
            for reset in [false, true] {
                let mut timeline = AudioTimeline::new(system, 0);
                timeline.observe(packet(9600, 0, 480, 1), 100_000).unwrap();
                if reset {
                    let mut next = packet(0, 480, 480, 2);
                    next.timing.flags = AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32;
                    timeline.observe(next, 200_000).unwrap();
                }
                let expected_end = if reset { 480 } else { 10_080 };
                let mut overlap = packet(expected_end - 1, 960, 480, 3);
                overlap.timing.engine_mix_rate_hz = Some(16_000);
                let error = timeline.observe(overlap, 300_000).unwrap_err();
                let description = format!("{error:#}");
                for field in [
                    source_label(system),
                    "Unflagged",
                    "previous_end=",
                    "current=[",
                    "previous=[",
                    "sequence=",
                    "frames=480",
                    "qpc_100ns=200000",
                    "flags=0x0",
                    "client_rate_hz=48000",
                    "engine_mix_rate_hz=Some(16000)",
                    "observed_qpc_100ns=300000",
                ] {
                    assert!(description.contains(field), "{description}");
                }
                assert!(
                    description.len() < 1024,
                    "The bounded worker diagnostic must retain the numeric context."
                );
                assert_eq!(
                    timeline.last_packet.unwrap().device_position,
                    if reset { 0 } else { 9600 }
                );
            }
        }
    }

    #[test]
    fn declared_reset_still_rejects_bad_qpc_and_reports_raw_flags() {
        for system in [false, true] {
            for bad_time in ["regression", "future", "uncertain"] {
                let mut timeline = AudioTimeline::new(system, 0);
                timeline
                    .observe(packet(9600, 480, 480, 1), 200_000)
                    .unwrap();
                let mut next = packet(0, 960, 480, 2);
                next.timing.flags = AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32;
                let expected_error = match bad_time {
                    "regression" => {
                        next.timing.timestamp = 99_999;
                        "timestamp moved backwards"
                    }
                    "future" => {
                        next.timing.timestamp = 1_300_001;
                        "more than 100 ms in the future"
                    }
                    _ => {
                        next.timing.flags |= AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32;
                        "invalid timestamp"
                    }
                };
                let error = format!("{:#}", timeline.observe(next, 300_000).unwrap_err());
                assert!(error.contains(expected_error), "{error}");
                assert!(error.contains(if bad_time == "uncertain" {
                    "flags=0x5"
                } else {
                    "flags=0x1"
                }));
                assert_eq!(timeline.last_packet.unwrap().device_position, 9600);
            }
        }
    }

    #[test]
    fn declared_reset_cannot_hide_active_timestamp_gap_or_overlap() {
        for system in [false, true] {
            for overlap in [false, true] {
                let mut timeline = AudioTimeline::new(system, 0);
                let clock = PauseTimeline::default();
                timeline.observe(packet(9600, 0, 1920, 1), 400_000).unwrap();
                assert_eq!(drain(&mut timeline, &clock, 1920), vec![(0, vec![1; 7680])]);
                let sample = if overlap { 480 } else { 26_401 };
                let mut next = packet(0, sample, 480, 2);
                next.timing.flags = AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32;
                let observed = next.timing.timestamp + 100_000;
                timeline.observe(next, observed).unwrap();
                let error = match timeline.prepare_next(&clock, sample + 480, u64::MAX, false) {
                    Err(error) => format!("{error:#}"),
                    Ok(_) => panic!("A discontinuity hid an invalid active-time interval."),
                };
                assert!(error.contains(source_label(system)));
                assert!(
                    error.contains(if overlap {
                        "more than 20 ms behind"
                    } else {
                        "gap exceeded 500 ms"
                    }),
                    "{error}"
                );
                assert!(error.contains("flags=0x1"));
                assert_eq!(timeline.committed, 1920);
            }
        }
    }

    #[test]
    fn first_declared_discontinuity_does_not_invent_a_previous_epoch() {
        let mut timeline = AudioTimeline::new(false, 0);
        let mut first = packet(0, 0, 480, 7);
        first.timing.flags = AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32;
        assert!(!timeline.observe(first, 100_000).unwrap());
        assert_eq!(
            drain(&mut timeline, &PauseTimeline::default(), 480),
            vec![(0, vec![7; 1920])]
        );
        assert!(timeline.observe(packet(479, 480, 480, 8), 200_000).is_err());
    }

    #[test]
    fn declared_reset_inside_pause_retains_only_active_pcm_and_the_frozen_stop_tail() {
        for system in [false, true] {
            let mut clock = PauseTimeline::default();
            clock.set_paused(480, true).unwrap();
            clock.set_paused(1440, false).unwrap();
            clock.freeze(1679);
            let mut timeline = AudioTimeline::new(system, 0);
            for (position, sample, marker, discontinuity) in [
                (9600, 0, 1, false),
                (0, 480, 2, true),
                (480, 960, 3, false),
                (960, 1440, 4, false),
            ] {
                let mut next = packet(position, sample, 480, marker);
                if discontinuity {
                    next.timing.flags = AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32;
                }
                timeline.observe(next, 400_000).unwrap();
            }
            let mut output = Vec::new();
            while let Some(prepared) = timeline.prepare_next(&clock, 1679, u64::MAX, true).unwrap()
            {
                assert_eq!(prepared.silence_frames, 0);
                output.push((prepared.sample, prepared.pcm.clone()));
                timeline.commit(&prepared).unwrap();
            }
            assert_eq!(output, vec![(0, vec![1; 1920]), (480, vec![4; 239 * 4])]);
            assert_eq!(timeline.committed, 719);
            assert_eq!(timeline.take_released_bytes(), 4 * 1920);
        }
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
