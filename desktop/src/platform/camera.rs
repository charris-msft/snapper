use super::{ComApartment, Frame};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use windows::{
    core::{GUID, PWSTR},
    Win32::{Media::MediaFoundation::*, System::Com::CoTaskMemFree},
};

struct Foundation {
    _com: ComApartment,
}
impl Foundation {
    fn new() -> Result<Self> {
        let com = ComApartment::new()?;
        // SAFETY: The owning thread has entered an MTA; startup is paired with shutdown.
        unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_LITE)?;
        }
        Ok(Self { _com: com })
    }
}
impl Drop for Foundation {
    fn drop(&mut self) {
        // SAFETY: This balances successful MFStartup after the owned interfaces have dropped.
        if let Err(e) = unsafe { MFShutdown() } {
            eprintln!("Media Foundation shutdown: {e}");
        }
    }
}
fn attributes(capacity: u32) -> Result<IMFAttributes> {
    let mut a = None;
    // SAFETY: The initialized output slot remains valid for the duration of this call.
    unsafe {
        MFCreateAttributes(&mut a, capacity)?;
    }
    a.context("Media Foundation did not return attributes")
}
fn string(a: &IMFAttributes, key: &GUID) -> Result<String> {
    let mut ptr = PWSTR::null();
    let mut len = 0;
    // SAFETY: MF allocates len UTF-16 units; the returned allocation is copied then freed once.
    unsafe {
        a.GetAllocatedString(key, &mut ptr, &mut len)?;
        let result = String::from_utf16(std::slice::from_raw_parts(ptr.0, len as usize));
        CoTaskMemFree(Some(ptr.0.cast()));
        Ok(result?)
    }
}
fn activators() -> Result<Vec<IMFActivate>> {
    let a = attributes(1)?;
    // SAFETY: The returned array owns count COM references; take each before freeing the array.
    unsafe {
        a.SetGUID(
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
        )?;
        let mut ptr = std::ptr::null_mut();
        let mut count = 0;
        MFEnumDeviceSources(&a, &mut ptr, &mut count)?;
        if count == 0 {
            CoTaskMemFree(Some(ptr.cast()));
            return Ok(vec![]);
        }
        ensure!(
            count <= 64,
            "Camera enumeration exceeds its 64-device budget."
        );
        let values = std::slice::from_raw_parts_mut(ptr, count as usize);
        let mut result = Vec::new();
        for value in values {
            if let Some(value) = value.take() {
                result.push(value);
            }
        }
        CoTaskMemFree(Some(ptr.cast()));
        Ok(result)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CameraInfo {
    pub id: String,
    pub name: String,
}
pub fn list() -> Result<Vec<CameraInfo>> {
    let _mf = Foundation::new()?;
    let mut result = Vec::new();
    for a in activators()? {
        result.push(CameraInfo {
            id: string(&a, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK)?,
            name: string(&a, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME)?,
        });
    }
    Ok(result)
}

pub struct Camera {
    reader: IMFSourceReader,
    source: IMFMediaSource,
    width: u32,
    height: u32,
    stride: i32,
    closed: bool,
    _mf: Foundation,
}
impl Camera {
    pub fn open(id: &str) -> Result<Self> {
        let mf = Foundation::new()?;
        let mut chosen = None;
        for a in activators()? {
            if string(&a, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK)? == id {
                chosen = Some(a);
                break;
            }
        }
        let chosen =
            chosen.context("Selected camera is unavailable. Select a connected camera.")?;
        // SAFETY: MF is initialized; all interfaces and format buffers stay on this owner thread.
        // Activation happens only for an explicitly selected source in the supervised worker.
        unsafe {
            let source: IMFMediaSource = chosen
                .ActivateObject()
                .context("Camera access denied or device busy")?;
            let config = attributes(2)?;
            config.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
            let reader = match MFCreateSourceReaderFromMediaSource(&source, &config) {
                Ok(r) => r,
                Err(e) => {
                    source.Shutdown()?;
                    return Err(e.into());
                }
            };
            let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
            let setup = (|| -> Result<(u32, u32, i32)> {
                let mut selected = None;
                let mut best = 0;
                for index in 0..128 {
                    let native = match reader.GetNativeMediaType(stream, index) {
                        Ok(t) => t,
                        Err(e) if e.code() == MF_E_NO_MORE_TYPES => break,
                        Err(e) => return Err(e.into()),
                    };
                    let size = native.GetUINT64(&MF_MT_FRAME_SIZE)?;
                    let width = (size >> 32) as u32;
                    let height = size as u32;
                    let rate = native.GetUINT64(&MF_MT_FRAME_RATE)?;
                    let denominator = rate as u32;
                    let fps = if denominator == 0 {
                        0.0
                    } else {
                        (rate >> 32) as f64 / f64::from(denominator)
                    };
                    if width <= 1280
                        && height <= 720
                        && width >= 160
                        && height >= 120
                        && fps > 0.0
                        && fps <= 30.1
                    {
                        let score = width * height + (fps as u32) * 10_000;
                        if score > best {
                            best = score;
                            selected = Some(native);
                        }
                    }
                }
                let native = selected
                    .context("Camera has no supported mode at or below 1280x720 and 30 fps")?;
                reader.SetCurrentMediaType(stream, None, &native)?;
                let rgb = MFCreateMediaType()?;
                rgb.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                rgb.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
                reader
                    .SetCurrentMediaType(stream, None, &rgb)
                    .context("Camera RGB32 conversion unavailable")?;
                let actual = reader.GetCurrentMediaType(stream)?;
                let size = actual.GetUINT64(&MF_MT_FRAME_SIZE)?;
                let width = (size >> 32) as u32;
                let height = size as u32;
                ensure!(
                    width <= 1280 && height <= 720 && width > 0 && height > 0,
                    "Camera negotiated an oversized format."
                );
                let stride = match actual.GetUINT32(&MF_MT_DEFAULT_STRIDE) {
                    Ok(v) => v as i32,
                    Err(e) if e.code() == MF_E_ATTRIBUTENOTFOUND => {
                        MFGetStrideForBitmapInfoHeader(MFVideoFormat_RGB32.data1, width)?
                    }
                    Err(e) => return Err(e.into()),
                };
                Ok((width, height, stride))
            })();
            match setup {
                Ok((width, height, stride)) => Ok(Self {
                    reader,
                    source,
                    width,
                    height,
                    stride,
                    closed: false,
                    _mf: mf,
                }),
                Err(e) => {
                    source.Shutdown()?;
                    Err(e)
                }
            }
        }
    }
    pub fn read(&self) -> Result<Option<Frame>> {
        // SAFETY: ReadSample owns the returned sample. Copy only validated strides while locked.
        unsafe {
            let mut flags = 0;
            let mut timestamp = 0;
            let mut sample = None;
            self.reader.ReadSample(
                MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                0,
                None,
                Some(&mut flags),
                Some(&mut timestamp),
                Some(&mut sample),
            )?;
            ensure!(flags&(MF_SOURCE_READERF_ERROR.0|MF_SOURCE_READERF_ENDOFSTREAM.0
                |MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0) as u32==0,
                "Camera ended, changed format, or failed. Capture interrupted instead of freezing its last image.");
            let Some(sample) = sample else {
                return Ok(None);
            };
            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut ptr = std::ptr::null_mut();
            let mut len = 0;
            buffer.Lock(&mut ptr, None, Some(&mut len))?;
            let result = (|| -> Result<Frame> {
                let stride = self.stride.unsigned_abs() as usize;
                ensure!(
                    stride >= self.width as usize * 4
                        && stride * self.height as usize <= len as usize,
                    "Camera returned an inconsistent RGB buffer."
                );
                let mut bgra = vec![0; self.width as usize * self.height as usize * 4];
                for (y, dst) in bgra.chunks_exact_mut(self.width as usize * 4).enumerate() {
                    let row = if self.stride < 0 {
                        self.height as usize - 1 - y
                    } else {
                        y
                    };
                    dst.copy_from_slice(std::slice::from_raw_parts(
                        ptr.add(row * stride),
                        dst.len(),
                    ));
                }
                for pixel in bgra.chunks_exact_mut(4) {
                    pixel[3] = 255;
                }
                Ok(Frame {
                    width: self.width,
                    height: self.height,
                    bgra,
                    timestamp,
                })
            })();
            buffer.Unlock()?;
            Ok(Some(result?))
        }
    }
    pub fn close(&mut self) -> Result<()> {
        if !self.closed {
            // SAFETY: This live source is shut down once on the thread that activated it.
            unsafe {
                self.source.Shutdown()?;
            }
            self.closed = true;
        }
        Ok(())
    }
}
impl Drop for Camera {
    fn drop(&mut self) {
        if let Err(e) = self.close() {
            eprintln!("Camera shutdown: {e:#}");
        }
    }
}
