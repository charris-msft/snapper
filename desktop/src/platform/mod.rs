pub mod audio;
pub mod camera;
pub mod hotkey;
pub mod process;
pub mod screen;
pub mod selection;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use windows::Win32::System::{
    Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED},
    Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
};

pub struct ComApartment;
impl ComApartment {
    pub fn new() -> Result<Self> {
        // SAFETY: Each device-owning thread establishes and releases its own MTA.
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        }
        Ok(Self)
    }
}
impl Drop for ComApartment {
    fn drop(&mut self) {
        // SAFETY: This balances the successful initialization on this owning thread.
        unsafe {
            CoUninitialize();
        }
    }
}

pub fn clock_100ns() -> Result<i64> {
    let mut counter = 0i64;
    let mut frequency = 0i64;
    // SAFETY: Both output pointers refer to initialized, live scalar storage.
    unsafe {
        QueryPerformanceCounter(&mut counter)?;
        QueryPerformanceFrequency(&mut frequency)?;
    }
    let ticks = i128::from(counter) * 10_000_000 / i128::from(frequency);
    i64::try_from(ticks).context("Monotonic media clock overflow")
}

pub fn initialize_process() -> Result<()> {
    use windows::Win32::UI::HiDpi::{
        AreDpiAwarenessContextsEqual, GetThreadDpiAwarenessContext, SetProcessDpiAwarenessContext,
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    // SAFETY: This changes only this process's coordinate interpretation before any window/capture.
    unsafe {
        if let Err(error) =
            SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
        {
            if !AreDpiAwarenessContextsEqual(
                GetThreadDpiAwarenessContext(),
                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            )
            .as_bool()
            {
                return Err(error).context("Per-monitor-v2 pixel coordinates could not be established. Capture was not started.");
            }
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Area {
    pub monitor: String,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Monitor {
    pub id: String,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
    #[serde(default = "default_dpi")]
    pub dpi: u32,
}
fn default_dpi() -> u32 {
    96
}
impl Monitor {
    pub fn area(&self) -> Area {
        Area {
            monitor: self.id.clone(),
            x: 0,
            y: 0,
            width: self.width,
            height: self.height,
        }
    }
}

pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
    pub timestamp: i64,
}
impl Frame {
    pub fn into_rgba(mut self) -> Result<image::RgbaImage> {
        for pixel in self.bgra.chunks_exact_mut(4) {
            pixel.swap(0, 2);
            pixel[3] = 255;
        }
        image::RgbaImage::from_raw(self.width, self.height, self.bgra)
            .context("Invalid captured frame size")
    }
}
