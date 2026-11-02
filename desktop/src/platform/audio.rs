use super::ComApartment;
use anyhow::{ensure, Context, Result};
use windows::Win32::{
    Media::Audio::*,
    System::Com::{CoCreateInstance, CLSCTX_ALL},
};

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
        let apartment = ComApartment::new()?;
        // SAFETY: COM is initialized on this thread; all output pointers and format storage are live.
        // Only selected sources reach activation; enumeration never opens an endpoint.
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let device = enumerator
                .GetDefaultAudioEndpoint(if system { eRender } else { eCapture }, eConsole)
                .context(if system {
                    "No default system-output audio device"
                } else {
                    "No default microphone or permission denied"
                })?;
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
