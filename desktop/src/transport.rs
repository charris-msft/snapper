//! A timestamp-bearing, bounded Matroska stream for the isolated local encoder.
use anyhow::{ensure, Result};
use std::io::Write;

fn id_bytes(id: u32) -> Vec<u8> {
    let b = id.to_be_bytes();
    b[b.iter().position(|v| *v != 0).unwrap_or(3)..].to_vec()
}
fn size_bytes(value: u64) -> Vec<u8> {
    for len in 1..=8 {
        if value < (1u64 << (7 * len)) - 1 {
            let encoded = value | (1u64 << (7 * len));
            return encoded.to_be_bytes()[8 - len..].to_vec();
        }
    }
    unreachable!("bounded media elements fit an EBML size")
}
fn element(id: u32, bytes: &[u8]) -> Vec<u8> {
    let mut out = id_bytes(id);
    out.extend(size_bytes(bytes.len() as u64));
    out.extend(bytes);
    out
}
fn uint(id: u32, value: u64) -> Vec<u8> {
    let b = value.to_be_bytes();
    element(id, &b[b.iter().position(|v| *v != 0).unwrap_or(7)..])
}
fn group(id: u32, children: Vec<Vec<u8>>) -> Vec<u8> {
    element(id, &children.concat())
}
fn text(id: u32, value: &str) -> Vec<u8> {
    element(id, value.as_bytes())
}

pub struct Matroska<W: Write> {
    output: W,
    width: u32,
    height: u32,
    audio_tracks: u8,
}

impl<W: Write> Matroska<W> {
    pub fn new(mut output: W, width: u32, height: u32, audio_tracks: u8) -> Result<Self> {
        ensure!(
            width > 0 && height > 0 && width <= 3840 && height <= 2160,
            "Recording canvas exceeds 3840x2160."
        );
        ensure!(
            audio_tracks <= 2,
            "Only two independent audio sources are supported."
        );
        output.write_all(&group(
            0x1a45dfa3,
            vec![
                uint(0x4286, 1),
                uint(0x42f7, 1),
                uint(0x42f2, 4),
                uint(0x42f3, 8),
                text(0x4282, "matroska"),
                uint(0x4287, 4),
                uint(0x4285, 2),
            ],
        ))?;
        output.write_all(&[
            0x18, 0x53, 0x80, 0x67, 0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        ])?;
        output.write_all(&group(
            0x1549a966,
            vec![
                uint(0x2ad7b1, 1_000_000),
                text(0x4d80, "Snapper"),
                text(0x5741, "Snapper"),
            ],
        ))?;
        let mut bitmap = Vec::with_capacity(40);
        bitmap.extend(40u32.to_le_bytes());
        bitmap.extend((width as i32).to_le_bytes());
        bitmap.extend((-(height as i32)).to_le_bytes());
        bitmap.extend(1u16.to_le_bytes());
        bitmap.extend(32u16.to_le_bytes());
        bitmap.extend(0u32.to_le_bytes());
        bitmap.extend((width * height * 4).to_le_bytes());
        bitmap.extend([0; 16]);
        let mut tracks = vec![group(
            0xae,
            vec![
                uint(0xd7, 1),
                uint(0x73c5, 1),
                uint(0x83, 1),
                uint(0x9c, 0),
                text(0x86, "V_MS/VFW/FOURCC"),
                element(0x63a2, &bitmap),
                uint(0x23e383, 33_333_333),
                group(
                    0xe0,
                    vec![uint(0xb0, width as u64), uint(0xba, height as u64)],
                ),
            ],
        )];
        for i in 0..audio_tracks {
            tracks.push(group(
                0xae,
                vec![
                    uint(0xd7, u64::from(i) + 2),
                    uint(0x73c5, u64::from(i) + 2),
                    uint(0x83, 2),
                    uint(0x9c, 0),
                    text(0x86, "A_PCM/INT/LIT"),
                    group(
                        0xe1,
                        vec![
                            element(0xb5, &48000f64.to_be_bytes()),
                            uint(0x9f, 2),
                            uint(0x6264, 16),
                        ],
                    ),
                ],
            ));
        }
        output.write_all(&group(0x1654ae6b, tracks))?;
        Ok(Self {
            output,
            width,
            height,
            audio_tracks,
        })
    }
    fn packet(&mut self, track: u8, milliseconds: u64, payload: &[u8]) -> Result<()> {
        let timecode = uint(0xe7, milliseconds);
        let block_len = payload.len() + 4;
        let block_size = size_bytes(block_len as u64);
        self.output.write_all(&id_bytes(0x1f43b675))?;
        self.output.write_all(&size_bytes(
            (timecode.len() + 1 + block_size.len() + block_len) as u64,
        ))?;
        self.output.write_all(&timecode)?;
        self.output.write_all(&[0xa3])?;
        self.output.write_all(&block_size)?;
        self.output.write_all(&[0x80 | track, 0, 0, 0x80])?;
        self.output.write_all(payload)?;
        Ok(())
    }
    pub fn video(&mut self, milliseconds: u64, bgra: &[u8]) -> Result<()> {
        ensure!(
            bgra.len() == self.width as usize * self.height as usize * 4,
            "Video frame length is inconsistent with its canvas."
        );
        self.packet(1, milliseconds, bgra)
    }
    pub fn audio(&mut self, index: u8, milliseconds: u64, pcm: &[u8]) -> Result<()> {
        ensure!(
            index < self.audio_tracks && pcm.len() % 4 == 0 && pcm.len() <= 192_000,
            "Invalid or oversized PCM packet."
        );
        self.packet(index + 2, milliseconds, pcm)
    }
    pub fn finish(mut self) -> Result<W> {
        self.output.flush()?;
        Ok(self.output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vint_boundaries_and_element_lengths() {
        assert_eq!(size_bytes(0), [0x80]);
        assert_eq!(size_bytes(126), [0xfe]);
        assert_eq!(size_bytes(127), [0x40, 0x7f]);
        assert_eq!(element(0xe7, &[1]), [0xe7, 0x81, 1]);
    }
    #[test]
    fn transport_preserves_gap_in_timestamps() {
        let mut m = Matroska::new(Vec::new(), 2, 2, 0).unwrap();
        m.video(0, &[0; 16]).unwrap();
        m.video(1500, &[0; 16]).unwrap();
        let b = m.finish().unwrap();
        assert!(b.windows(4).any(|w| w == [0xe7, 0x82, 0x05, 0xdc]));
    }
    #[test]
    fn off_audio_has_no_audio_track() {
        let mut m = Matroska::new(Vec::new(), 2, 2, 0).unwrap();
        assert!(m.audio(0, 0, &[0; 4]).is_err());
        let bytes = m.finish().unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("A_PCM"));
    }
}
