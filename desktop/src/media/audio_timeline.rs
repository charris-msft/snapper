use crate::platform::audio::Packet;
use anyhow::{ensure, Result};

pub struct Chunk {
    pub sample: u64,
    pub pcm: Vec<u8>,
}
pub struct Plan {
    pub chunks: Vec<Chunk>,
    pub silence_frames: u64,
    pub discontinuity: bool,
}

pub struct AudioTimeline {
    system: bool,
    origin: i64,
    committed_until: u64,
    last_delivery: i64,
    last_timestamp: Option<i64>,
    device_end: Option<u64>,
}

impl AudioTimeline {
    pub fn new(system: bool, origin: i64) -> Self {
        Self {
            system,
            origin,
            committed_until: 0,
            last_delivery: origin,
            last_timestamp: None,
            device_end: None,
        }
    }

    pub fn packet(&mut self, packet: Packet, now: i64) -> Result<Plan> {
        ensure!(
            packet.pcm.len() == packet.frames as usize * 4,
            "Audio sample count disagrees with its PCM payload."
        );
        ensure!(
            self.last_timestamp.is_none_or(|t| packet.timestamp >= t),
            "Audio timestamp moved backwards."
        );
        ensure!(
            packet.timestamp <= now + 1_000_000,
            "Audio timestamp is more than 100 ms in the future."
        );
        if let Some(end) = self.device_end {
            ensure!(
                packet.device_position >= end,
                "Audio device sample position moved backwards."
            );
            ensure!(
                packet.device_position - end <= 24_000,
                "Audio device lost more than 500 ms of samples."
            );
        }
        let discontinuity = packet.discontinuity && self.last_timestamp.is_some();
        self.device_end = Some(
            packet
                .device_position
                .checked_add(u64::from(packet.frames))
                .ok_or_else(|| anyhow::anyhow!("Audio device position overflow"))?,
        );
        self.last_timestamp = Some(packet.timestamp);
        self.last_delivery = now;
        let delta = i128::from(packet.timestamp) - i128::from(self.origin);
        let start = (delta * 48_000 / 10_000_000) as i64;
        let trim =
            start.unsigned_abs().min(u64::from(packet.frames)) as usize * usize::from(start < 0);
        let sample = start.max(0) as u64;
        let frames = packet.frames as usize - trim;
        let mut plan = Plan {
            chunks: vec![],
            silence_frames: 0,
            discontinuity,
        };
        if frames == 0 {
            return Ok(plan);
        }
        ensure!(self.committed_until.saturating_sub(sample)<=960,
            "Audio arrived more than 20 ms behind committed media. Recording interrupted rather than silently dropping sound.");
        let gap = sample.saturating_sub(self.committed_until);
        ensure!(gap <= 24_000, "Audio timestamp gap exceeded 500 ms.");
        if gap > 480 {
            plan.chunks.push(Chunk {
                sample: self.committed_until,
                pcm: vec![0; gap as usize * 4],
            });
            plan.silence_frames = gap;
        }
        plan.chunks.push(Chunk {
            sample,
            pcm: packet.pcm[trim * 4..].to_vec(),
        });
        self.committed_until = self.committed_until.max(sample + frames as u64);
        Ok(plan)
    }

    pub fn idle(&mut self, now: i64) -> Result<Option<Chunk>> {
        if !self.system {
            ensure!(now-self.last_delivery<=10_000_000,
                "The selected microphone stopped delivering packets for 1 second. Recording interrupted; it was not replaced with generated silence.");
            return Ok(None);
        }
        let horizon = ((now - self.origin - 2_500_000).max(0) as u64) * 48_000 / 10_000_000;
        if horizon <= self.committed_until {
            return Ok(None);
        }
        let frames = (horizon - self.committed_until).min(4800);
        let chunk = Chunk {
            sample: self.committed_until,
            pcm: vec![0; frames as usize * 4],
        };
        self.committed_until += frames;
        Ok(Some(chunk))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn packet(position: u64, time: i64, frames: u32) -> Packet {
        Packet {
            device_position: position,
            timestamp: time,
            frames,
            pcm: vec![0; frames as usize * 4],
            discontinuity: false,
        }
    }
    #[test]
    fn missing_microphone_fails_while_legitimate_silent_packets_keep_it_alive() {
        let mut missing = AudioTimeline::new(false, 0);
        assert!(missing.idle(9_000_000).unwrap().is_none());
        assert!(missing.idle(10_000_001).is_err());
        let mut silent = AudioTimeline::new(false, 0);
        for i in 0..300u64 {
            let now = i as i64 * 100_000;
            silent.packet(packet(i * 480, now, 480), now).unwrap();
            assert!(silent.idle(now + 90_000).unwrap().is_none());
        }
    }
    #[test]
    fn quiet_loopback_is_distinct_from_missing_microphone() {
        let mut system = AudioTimeline::new(true, 0);
        let chunk = system.idle(10_000_001).unwrap().unwrap();
        assert_eq!(chunk.pcm.len(), 4800 * 4);
        assert_eq!(chunk.sample, 0);
    }
    #[test]
    fn fractional_millisecond_packets_preserve_sample_positions() {
        let mut timeline = AudioTimeline::new(false, 0);
        for i in 0..100u64 {
            let sample = i * 127;
            let time = (i128::from(sample) * 10_000_000 / 48_000) as i64;
            let plan = timeline.packet(packet(sample, time, 127), time).unwrap();
            assert_eq!(plan.silence_frames, 0);
            assert!((plan.chunks[0].sample as i64 - sample as i64).abs() <= 1);
        }
    }
    #[test]
    fn device_regression_and_excessive_gaps_are_not_hidden() {
        let mut timeline = AudioTimeline::new(false, 0);
        timeline.packet(packet(100, 0, 480), 0).unwrap();
        assert!(timeline.packet(packet(10, 100_000, 480), 100_000).is_err());
        let mut gap = AudioTimeline::new(false, 0);
        assert!(gap.packet(packet(0, 6_000_000, 480), 6_000_000).is_err());
    }
}
