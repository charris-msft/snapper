use super::ComApartment;
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::fmt;
use windows::core::{Interface, HSTRING};
use windows::Win32::{
    Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
    Media::Audio::*,
    System::Com::{
        CoCreateInstance, CoTaskMemFree, StructuredStorage::PropVariantToString, CLSCTX_ALL,
        STGM_READ,
    },
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub system: bool,
    pub default: bool,
}

fn device_id(device: &IMMDevice) -> Result<String> {
    // SAFETY: Copy the COM-allocated identifier before freeing its allocation exactly once.
    unsafe {
        let value = device.GetId()?;
        let text = value.to_string();
        CoTaskMemFree(Some(value.0.cast()));
        Ok(text?)
    }
}

pub fn list() -> Result<Vec<AudioDevice>> {
    let _apartment = ComApartment::new()?;
    // SAFETY: Read endpoint metadata on its COM thread. No IAudioClient is activated here.
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let mut devices = Vec::new();
        for system in [true, false] {
            let flow = if system { eRender } else { eCapture };
            let default_id = match enumerator.GetDefaultAudioEndpoint(flow, eConsole) {
                Ok(device) => Some(device_id(&device)?),
                Err(error) if error.code().0 as u32 == 0x80070490 => None,
                Err(error) => return Err(error).context("Reading the default audio route"),
            };
            let endpoints = enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)?;
            let count = endpoints.GetCount()?;
            ensure!(
                count <= 64,
                "Audio enumeration exceeds its 64-device-per-direction budget."
            );
            for index in 0..count {
                let device = endpoints.Item(index)?;
                let id = device_id(&device)?;
                let store = device.OpenPropertyStore(STGM_READ)?;
                let value = store.GetValue(&PKEY_Device_FriendlyName)?;
                let mut buffer = [0u16; 512];
                PropVariantToString(&value, &mut buffer)
                    .context("Reading an audio device's display name")?;
                let length = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
                let name = String::from_utf16(&buffer[..length])?;
                devices.push(AudioDevice {
                    default: default_id.as_ref() == Some(&id),
                    id,
                    name,
                    system,
                });
            }
        }
        Ok(devices)
    }
}

pub const PCM_SAMPLE_RATE: u32 = 48_000;

pub fn source_label(system: bool) -> &'static str {
    if system {
        "System audio (loopback)"
    } else {
        "Microphone"
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PacketTiming {
    pub sequence: u64,
    pub timestamp: i64,
    pub device_position: u64,
    pub frames: u32,
    pub flags: u32,
    pub client_rate_hz: u32,
    pub engine_mix_rate_hz: Option<u32>,
}

impl PacketTiming {
    pub fn discontinuity(self) -> bool {
        self.flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0
    }

    pub fn rate_converted(self) -> bool {
        self.engine_mix_rate_hz
            .is_some_and(|rate| rate != self.client_rate_hz)
    }

    pub fn client_position_end(self) -> Result<u64> {
        ensure!(
            !self.rate_converted(),
            "Converted PCM frame counts cannot be added to a device position."
        );
        self.device_position
            .checked_add(u64::from(self.frames))
            .context("Audio device position overflow")
    }

    pub fn validate(self) -> Result<()> {
        ensure!(
            self.frames > 0 && self.frames <= PCM_SAMPLE_RATE,
            "Audio packet exceeds the nonempty one-second allocation budget."
        );
        ensure!(
            self.client_rate_hz == PCM_SAMPLE_RATE,
            "Audio packet does not use the selected 48 kHz PCM stream format."
        );
        ensure!(
            self.engine_mix_rate_hz.is_none_or(|rate| rate > 0),
            "Audio endpoint engine mix rate is invalid."
        );
        ensure!(self.timestamp >= 0, "Audio QPC timestamp is invalid.");
        ensure!(self.flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 == 0,
            "Audio source reported an invalid timestamp. Capture interrupted rather than guessing synchronization.");
        if !self.rate_converted() {
            self.client_position_end()?;
        }
        Ok(())
    }
}

impl fmt::Display for PacketTiming {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(out, "sequence={} position={} frames={} qpc_100ns={} flags={:#x} client_rate_hz={} engine_mix_rate_hz={:?}",
            self.sequence, self.device_position, self.frames, self.timestamp, self.flags,
            self.client_rate_hz, self.engine_mix_rate_hz)
    }
}

pub struct Packet {
    pub timing: PacketTiming,
    pub pcm: Vec<u8>,
}

#[derive(Clone, Copy)]
struct AcquiredTiming {
    timestamp: u64,
    device_position: u64,
    frames: u32,
    flags: u32,
}

struct PacketReader {
    system: bool,
    engine_mix_rate_hz: u32,
    sequence: u64,
    previous: Option<PacketTiming>,
}

impl PacketReader {
    fn finish(
        &mut self,
        index: u8,
        acquired: AcquiredTiming,
        copy_pcm: impl FnOnce(PacketTiming) -> Result<Vec<u8>>,
        release: impl FnOnce() -> Result<()>,
    ) -> Result<Packet> {
        let sequence = self.sequence.checked_add(1);
        let previous = self.previous;
        let result = (|| -> Result<Packet> {
            let sequence = sequence.context("Audio packet sequence overflow")?;
            self.sequence = sequence;
            let timing = PacketTiming {
                sequence,
                timestamp: i64::try_from(acquired.timestamp)
                    .context("Audio QPC timestamp overflow")?,
                device_position: acquired.device_position,
                frames: acquired.frames,
                flags: acquired.flags,
                client_rate_hz: PCM_SAMPLE_RATE,
                engine_mix_rate_hz: Some(self.engine_mix_rate_hz),
            };
            timing.validate()?;
            let pcm = copy_pcm(timing)?;
            ensure!(
                pcm.len() == timing.frames as usize * 4,
                "Audio sample count disagrees with its bounded PCM payload."
            );
            Ok(Packet { timing, pcm })
        })();
        let released = release().context("IAudioCaptureClient::ReleaseBuffer");
        let result = match (result, released) {
            (result, Ok(())) => result,
            (Ok(_), Err(error)) => Err(error),
            (Err(error), Err(release_error)) => {
                Err(error.context(format!("Packet release also failed: {release_error:#}")))
            }
        };
        let packet = result.with_context(|| format!(
            "{} acquisition stream index={index}: current=[sequence={} position={} frames={} qpc_100ns={} flags={:#x} client_rate_hz={} engine_mix_rate_hz={}] previous=[{}]",
            source_label(self.system), sequence.map(|value| value.to_string()).unwrap_or_else(|| "overflow".into()),
            acquired.device_position, acquired.frames, acquired.timestamp, acquired.flags,
            PCM_SAMPLE_RATE, self.engine_mix_rate_hz,
            previous.map(|packet| packet.to_string()).unwrap_or_else(|| "none".into()),
        ))?;
        self.previous = Some(packet.timing);
        Ok(packet)
    }
}

pub struct Audio {
    capture: IAudioCaptureClient,
    client: IAudioClient,
    started: bool,
    packets: PacketReader,
    _apartment: ComApartment,
}
impl Audio {
    pub fn open(system: bool) -> Result<Self> {
        Self::open_selected(system, None)
    }
    pub fn open_selected(system: bool, selected: Option<&str>) -> Result<Self> {
        let apartment = ComApartment::new()?;
        // SAFETY: COM is initialized on this thread; all output pointers and format storage are live.
        // Only selected sources reach activation; enumeration never opens an endpoint.
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let flow = if system { eRender } else { eCapture };
            let device = match selected {
                Some(id) => enumerator.GetDevice(&HSTRING::from(id))
                    .context("The selected audio device is unavailable. Choose a connected device; no substitute was activated.")?,
                None => enumerator.GetDefaultAudioEndpoint(flow, eConsole)
                    .context("No default audio device is available")?,
            };
            let endpoint: IMMEndpoint = device.cast()?;
            ensure!(
                endpoint.GetDataFlow()? == flow && device.GetState()? == DEVICE_STATE_ACTIVE,
                "The selected device has the wrong audio direction or was disconnected."
            );
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
            let mix = client
                .GetMixFormat()
                .context("Reading the selected audio endpoint's engine mix rate")?;
            ensure!(
                !mix.is_null(),
                "The selected endpoint returned no engine mix format."
            );
            let engine_mix_rate_hz = (*mix).nSamplesPerSec;
            CoTaskMemFree(Some(mix.cast()));
            ensure!(
                engine_mix_rate_hz > 0,
                "The selected endpoint returned a zero engine mix rate."
            );
            let format = WAVEFORMATEX {
                wFormatTag: 1,
                nChannels: 2,
                nSamplesPerSec: PCM_SAMPLE_RATE,
                nAvgBytesPerSec: 192_000,
                nBlockAlign: 4,
                wBitsPerSample: 16,
                cbSize: 0,
            };
            let flags = AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY
                | if system {
                    AUDCLNT_STREAMFLAGS_LOOPBACK
                } else {
                    0
                };
            client.Initialize(AUDCLNT_SHAREMODE_SHARED,flags,5_000_000,0,&format,None)
                .context("Opening selected audio source at 48 kHz stereo PCM; Windows privacy settings may deny microphone access")?;
            let capture: IAudioCaptureClient = client.GetService()?;
            Ok(Self {
                capture,
                client,
                started: false,
                packets: PacketReader {
                    system,
                    engine_mix_rate_hz,
                    sequence: 0,
                    previous: None,
                },
                _apartment: apartment,
            })
        }
    }
    pub fn start(&mut self) -> Result<()> {
        // SAFETY: The initialized client is used on its owning COM thread.
        unsafe {
            self.client.Start()?;
        }
        self.started = true;
        Ok(())
    }
    pub fn packet(&mut self, index: u8) -> Result<Option<Packet>> {
        self.read_packet(index).with_context(|| {
            format!(
                "{} acquisition stream index={index}: client_rate_hz={} engine_mix_rate_hz={}",
                source_label(self.packets.system),
                PCM_SAMPLE_RATE,
                self.packets.engine_mix_rate_hz
            )
        })
    }

    fn read_packet(&mut self, index: u8) -> Result<Option<Packet>> {
        // SAFETY: Acquire/copy/release stay on the owner thread; lengths are bounded before copying.
        unsafe {
            if self
                .capture
                .GetNextPacketSize()
                .context("IAudioCaptureClient::GetNextPacketSize")?
                == 0
            {
                return Ok(None);
            }
            let mut ptr = std::ptr::null_mut();
            let mut frames = 0;
            let mut flags = 0;
            let mut position = 0;
            let mut timestamp = 0;
            self.capture
                .GetBuffer(
                    &mut ptr,
                    &mut frames,
                    &mut flags,
                    Some(&mut position),
                    Some(&mut timestamp),
                )
                .context("IAudioCaptureClient::GetBuffer")?;
            if frames == 0 {
                return Ok(None);
            }
            let capture = &self.capture;
            self.packets
                .finish(
                    index,
                    AcquiredTiming {
                        timestamp,
                        device_position: position,
                        frames,
                        flags,
                    },
                    |timing| {
                        let bytes = timing.frames as usize * 4;
                        if timing.flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                            Ok(vec![0; bytes])
                        } else {
                            ensure!(!ptr.is_null(), "Audio source returned an invalid buffer.");
                            Ok(std::slice::from_raw_parts(ptr, bytes).to_vec())
                        }
                    },
                    || Ok(capture.ReleaseBuffer(frames)?),
                )
                .map(Some)
        }
    }
    pub fn stop(&mut self) -> Result<()> {
        if self.started {
            // SAFETY: This is the live client started by this same owner.
            unsafe {
                self.client.Stop()?;
            }
            self.started = false;
        }
        Ok(())
    }
}
impl Drop for Audio {
    fn drop(&mut self) {
        if let Err(e) = self.stop() {
            eprintln!("Audio shutdown: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timing() -> PacketTiming {
        PacketTiming {
            sequence: 1,
            timestamp: 100_000,
            device_position: 0,
            frames: 480,
            flags: 0,
            client_rate_hz: PCM_SAMPLE_RATE,
            engine_mix_rate_hz: Some(16_000),
        }
    }

    #[test]
    fn native_packet_validation_keeps_the_acquired_pair_and_releases_rejected_buffers() {
        use std::cell::Cell;
        for (system, index) in [(true, 0), (false, 0), (false, 1)] {
            for preceding in [0, 2] {
                let mut reader = PacketReader {
                    system,
                    engine_mix_rate_hz: 16_000,
                    sequence: 0,
                    previous: None,
                };
                let releases = Cell::new(0);
                for step in 0..preceding {
                    let packet = reader
                        .finish(
                            index,
                            AcquiredTiming {
                                timestamp: step * 100_000,
                                device_position: step * 480,
                                frames: 480,
                                flags: 0,
                            },
                            |_| Ok(vec![step as u8; 1920]),
                            || {
                                releases.set(releases.get() + 1);
                                Ok(())
                            },
                        )
                        .unwrap();
                    assert_eq!(packet.timing.sequence, step + 1);
                }
                let acquired = AcquiredTiming {
                    timestamp: preceding * 100_000,
                    device_position: preceding * 480,
                    frames: 480,
                    flags: (AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0
                        | AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0) as u32,
                };
                let result = reader.finish(
                    index,
                    acquired,
                    |_| panic!("Invalid timestamp must fail before copying PCM."),
                    || {
                        releases.set(releases.get() + 1);
                        Ok(())
                    },
                );
                let error = match result {
                    Err(error) => format!("{error:#}"),
                    Ok(_) => panic!("An uncertain timestamp was accepted."),
                };
                for field in [
                    source_label(system).to_owned(),
                    format!("stream index={index}"),
                    format!(
                        "current=[sequence={} position={}",
                        preceding + 1,
                        preceding * 480
                    ),
                    "flags=0x5".into(),
                    "client_rate_hz=48000".into(),
                    "engine_mix_rate_hz=16000".into(),
                    "invalid timestamp".into(),
                ] {
                    assert!(error.contains(&field), "{error}");
                }
                assert!(
                    error.contains(if preceding == 0 {
                        "previous=[none]"
                    } else {
                        "previous=[sequence=2 position=480 frames=480 qpc_100ns=100000 flags=0x0"
                    }),
                    "{error}"
                );
                assert_eq!(
                    reader.previous.map(|header| header.sequence),
                    (preceding != 0).then_some(preceding)
                );
                assert_eq!(releases.get(), preceding + 1);
                assert!(error.len() < 1024);
            }
        }
    }

    #[test]
    fn native_packet_release_failure_keeps_the_previous_successful_header() {
        let mut reader = PacketReader {
            system: false,
            engine_mix_rate_hz: 48_000,
            sequence: 0,
            previous: None,
        };
        let acquired = AcquiredTiming {
            timestamp: 0,
            device_position: 9600,
            frames: 480,
            flags: 0,
        };
        reader
            .finish(1, acquired, |_| Ok(vec![1; 1920]), || Ok(()))
            .unwrap();
        let result = reader.finish(
            1,
            AcquiredTiming {
                timestamp: 100_000,
                device_position: 10_080,
                ..acquired
            },
            |_| Ok(vec![2; 1920]),
            || anyhow::bail!("Injected ReleaseBuffer failure"),
        );
        let error = match result {
            Err(error) => format!("{error:#}"),
            Ok(_) => panic!("Buffer release failure was hidden."),
        };
        assert!(error.contains("stream index=1"));
        assert!(error.contains("current=[sequence=2 position=10080"));
        assert!(error.contains("previous=[sequence=1 position=9600"));
        assert!(error.contains("Injected ReleaseBuffer failure"));
        assert_eq!(reader.previous.unwrap().sequence, 1);
    }

    #[test]
    fn audio_packet_flags_do_not_turn_invalid_timestamps_into_valid_discontinuities() {
        for flags in [
            0,
            AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32,
            AUDCLNT_BUFFERFLAGS_SILENT.0 as u32,
            (AUDCLNT_BUFFERFLAGS_SILENT.0 | AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0) as u32,
        ] {
            let valid = PacketTiming { flags, ..timing() };
            valid.validate().unwrap();
            assert_eq!(
                valid.discontinuity(),
                flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0
            );
            let invalid = PacketTiming {
                flags: flags | AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32,
                ..valid
            };
            assert!(invalid
                .validate()
                .unwrap_err()
                .to_string()
                .contains("invalid timestamp"));
        }
    }

    #[test]
    fn audio_packet_header_bounds_and_rate_context_are_explicit() {
        let valid = timing();
        for invalid in [
            PacketTiming { frames: 0, ..valid },
            PacketTiming {
                frames: PCM_SAMPLE_RATE + 1,
                ..valid
            },
            PacketTiming {
                timestamp: -1,
                ..valid
            },
            PacketTiming {
                device_position: u64::MAX,
                engine_mix_rate_hz: Some(PCM_SAMPLE_RATE),
                ..valid
            },
            PacketTiming {
                client_rate_hz: 16_000,
                ..valid
            },
            PacketTiming {
                engine_mix_rate_hz: Some(0),
                ..valid
            },
        ] {
            assert!(invalid.validate().is_err());
        }
        let description = valid.to_string();
        for field in [
            "sequence=1",
            "position=0",
            "frames=480",
            "qpc_100ns=100000",
            "flags=0x0",
            "client_rate_hz=48000",
            "engine_mix_rate_hz=Some(16000)",
        ] {
            assert!(description.contains(field), "{description}");
        }
        assert!(description.len() < 256);
    }

    #[test]
    fn converted_headers_cannot_mix_client_frames_with_device_positions() {
        let converted = timing();
        converted.validate().unwrap();
        assert!(converted
            .client_position_end()
            .unwrap_err()
            .to_string()
            .contains("cannot be added to a device position"));
        PacketTiming {
            device_position: u64::MAX,
            ..converted
        }
        .validate()
        .unwrap();
        let unconverted = PacketTiming {
            engine_mix_rate_hz: Some(PCM_SAMPLE_RATE),
            ..converted
        };
        assert_eq!(unconverted.client_position_end().unwrap(), 480);
    }
}
