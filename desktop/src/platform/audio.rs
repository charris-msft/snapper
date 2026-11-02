use super::ComApartment;
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
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

pub struct Packet {
    pub timestamp: i64,
    pub device_position: u64,
    pub frames: u32,
    pub pcm: Vec<u8>,
    pub discontinuity: bool,
}

pub struct Audio {
    capture: IAudioCaptureClient,
    client: IAudioClient,
    started: bool,
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
            let format = WAVEFORMATEX {
                wFormatTag: 1,
                nChannels: 2,
                nSamplesPerSec: 48_000,
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
    pub fn packet(&self) -> Result<Option<Packet>> {
        // SAFETY: Acquire/copy/release stay on the owner thread; lengths are bounded before copying.
        unsafe {
            if self.capture.GetNextPacketSize()? == 0 {
                return Ok(None);
            }
            let mut ptr = std::ptr::null_mut();
            let mut frames = 0;
            let mut flags = 0;
            let mut position = 0;
            let mut timestamp = 0;
            self.capture.GetBuffer(
                &mut ptr,
                &mut frames,
                &mut flags,
                Some(&mut position),
                Some(&mut timestamp),
            )?;
            if frames == 0 {
                return Ok(None);
            }
            let result = (|| -> Result<Packet> {
                ensure!(
                    frames <= 48_000,
                    "Audio packet exceeds the one-second allocation budget."
                );
                ensure!(flags&AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32==0,
                    "Audio source reported an invalid timestamp. Capture interrupted rather than guessing synchronization.");
                let bytes = frames as usize * 4;
                let pcm = if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                    vec![0; bytes]
                } else {
                    ensure!(
                        !ptr.is_null() || bytes == 0,
                        "Audio source returned an invalid buffer."
                    );
                    std::slice::from_raw_parts(ptr, bytes).to_vec()
                };
                Ok(Packet {
                    timestamp: timestamp as i64,
                    device_position: position,
                    frames,
                    pcm,
                    discontinuity: flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0,
                })
            })();
            self.capture.ReleaseBuffer(frames)?;
            Ok(Some(result?))
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
