use super::{Area, Frame, Monitor};
use anyhow::{ensure, Context, Result};
use crossbeam_channel::{bounded, Receiver};
use std::{
    mem::size_of,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock,
    },
    time::Duration,
};
use windows::{
    core::{factory, IInspectable, Interface},
    Foundation::TypedEventHandler,
    Graphics::{
        Capture::{
            Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem,
            GraphicsCaptureSession,
        },
        DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
    },
    Win32::{
        Foundation::{BOOL, HMODULE, LPARAM, RECT},
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0},
            Direct3D11::*,
            Dxgi::{Common::*, *},
            Gdi::*,
        },
        System::WinRT::{
            Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess},
            Graphics::Capture::IGraphicsCaptureItemInterop,
            RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED,
        },
        UI::{
            HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
            WindowsAndMessaging::MONITORINFOF_PRIMARY,
        },
    },
};

struct Runtime;
impl Runtime {
    fn new() -> Result<Self> {
        // SAFETY: Initialization and its matching teardown occur on the capture thread.
        unsafe {
            RoInitialize(RO_INIT_MULTITHREADED)?;
        }
        Ok(Self)
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        // SAFETY: This balances successful RoInitialize on the same thread.
        unsafe {
            RoUninitialize();
        }
    }
}

unsafe extern "system" fn monitor_callback(
    handle: HMONITOR,
    _: HDC,
    _: *mut RECT,
    data: LPARAM,
) -> BOOL {
    // SAFETY: EnumDisplayMonitors invokes this synchronously with our exclusive Vec pointer.
    let collection = unsafe { &mut *(data.0 as *mut Vec<(HMONITOR, Monitor)>) };
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: The initialized struct has the EXW size and the callback supplies a live monitor.
    if !unsafe { GetMonitorInfoW(handle, &mut info.monitorInfo) }.as_bool() {
        return BOOL(0);
    }
    let r = info.monitorInfo.rcMonitor;
    let (mut dpi_x, mut dpi_y) = (0, 0);
    // SAFETY: Enumeration supplies a live monitor and both output scalars are initialized.
    if unsafe { GetDpiForMonitor(handle, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }.is_err() {
        return BOOL(0);
    }
    let len = info
        .szDevice
        .iter()
        .position(|v| *v == 0)
        .unwrap_or(info.szDevice.len());
    collection.push((
        handle,
        Monitor {
            id: String::from_utf16_lossy(&info.szDevice[..len]),
            left: r.left,
            top: r.top,
            width: (r.right - r.left) as u32,
            height: (r.bottom - r.top) as u32,
            primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
            dpi: dpi_x,
        },
    ));
    BOOL(1)
}

fn monitor_handles() -> Result<Vec<(HMONITOR, Monitor)>> {
    let mut output: Vec<(HMONITOR, Monitor)> = Vec::new();
    // SAFETY: The callback only uses this Vec during the synchronous enumeration.
    unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(monitor_callback),
            LPARAM(&mut output as *mut _ as isize),
        )
        .ok()?;
    }
    ensure!(
        !output.is_empty(),
        "No connected desktop monitor is available."
    );
    output.sort_by_key(|(_, m)| !m.primary);
    Ok(output)
}
pub fn monitors() -> Result<Vec<Monitor>> {
    Ok(monitor_handles()?.into_iter().map(|(_, m)| m).collect())
}

#[derive(serde::Serialize)]
pub struct CaptureSupport {
    monitor: String,
    graphics_capture_supported: bool,
    sdr_verified: bool,
    capture_path: Option<&'static str>,
    detail: Option<String>,
}
pub fn support_report() -> Result<Vec<CaptureSupport>> {
    let _runtime = Runtime::new()?;
    let available = GraphicsCaptureSession::IsSupported()?;
    Ok(monitor_handles()?
        .into_iter()
        .map(|(handle, monitor)| {
            let color = capture_format(handle);
            CaptureSupport {
                monitor: monitor.id,
                graphics_capture_supported: available,
                sdr_verified: matches!(color, Ok(PixelMode::Sdr)),
                capture_path: color.as_ref().ok().map(|mode| mode.label()),
                detail: color.err().map(|e| format!("{e:#}")),
            }
        })
        .collect())
}

#[derive(Clone, Copy)]
enum PixelMode {
    Sdr,
    FloatToSdr,
}
impl PixelMode {
    fn label(self) -> &'static str {
        match self {
            Self::Sdr => "Windows Graphics Capture - SDR",
            Self::FloatToSdr => {
                "Windows Graphics Capture - Float16 to SDR conversion (bright highlights clipped)"
            }
        }
    }
}
fn capture_format(monitor: HMONITOR) -> Result<PixelMode> {
    // SAFETY: Returned COM references own the queried descriptors; no borrowed buffers escape.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        for ai in 0..32 {
            let adapter = match factory.EnumAdapters1(ai) {
                Ok(a) => a,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(e.into()),
            };
            for oi in 0..32 {
                let output = match adapter.EnumOutputs(oi) {
                    Ok(o) => o,
                    Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                    Err(e) => return Err(e.into()),
                };
                if output.GetDesc()?.Monitor == monitor {
                    let output: IDXGIOutput6 = output
                        .cast()
                        .context("Cannot verify the monitor's SDR color mode")?;
                    let desc = output.GetDesc1()?;
                    ensure!(desc.ColorSpace==DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
                        "This monitor uses HDR/advanced color. Snapper currently supports SDR captures only. Choose an SDR monitor.");
                    return Ok(PixelMode::Sdr);
                }
            }
        }
    }
    // Virtual/RDP displays need not appear among physical DXGI outputs. Capture in
    // linear Float16 and explicitly convert to SDR instead of guessing their color space.
    Ok(PixelMode::FloatToSdr)
}

pub struct Screen {
    item: GraphicsCaptureItem,
    closed_token: windows::Foundation::EventRegistrationToken,
    target_closed: Arc<AtomicBool>,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    context: ID3D11DeviceContext,
    staging: ID3D11Texture2D,
    ready: Receiver<()>,
    area: Area,
    monitor_size: (i32, i32),
    closed: bool,
    pixel_mode: PixelMode,
    _runtime: Runtime,
}

impl Screen {
    pub fn open(area: &Area, cursor: bool) -> Result<Self> {
        let runtime = Runtime::new()?;
        ensure!(
            GraphicsCaptureSession::IsSupported()?,
            "Windows Graphics Capture is unavailable. Windows 10 2004 or newer is required."
        );
        crate::model::validate_size(area.width, area.height)?;
        let (handle, monitor) = monitor_handles()?
            .into_iter()
            .find(|(_, m)| m.id == area.monitor)
            .context("Selected monitor was disconnected")?;
        ensure!(
            area.x
                .checked_add(area.width)
                .is_some_and(|v| v <= monitor.width)
                && area
                    .y
                    .checked_add(area.height)
                    .is_some_and(|v| v <= monitor.height),
            "The physical-pixel capture region is outside the selected monitor."
        );
        let pixel_mode = capture_format(handle)?;
        // SAFETY: Device/context stay on this owner; all descriptors and out-pointers are live.
        unsafe {
            let interop: IGraphicsCaptureItemInterop =
                factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
            let item: GraphicsCaptureItem = interop.CreateForMonitor(handle)?;
            let target_closed = Arc::new(AtomicBool::new(false));
            let closed = target_closed.clone();
            let closed_token = item.Closed(&TypedEventHandler::<
                GraphicsCaptureItem,
                IInspectable,
            >::new(move |_, _| {
                closed.store(true, Ordering::Release);
                Ok(())
            }))?;
            let size = item.Size()?;
            ensure!(
                size.Width > 0 && size.Height > 0,
                "Capture target has no drawable area."
            );
            crate::model::validate_size(size.Width as u32,size.Height as u32)
                .context("The complete monitor exceeds the bounded WGC frame-pool budget, even for a cropped capture")?;
            ensure!(
                area.x + area.width <= size.Width as u32
                    && area.y + area.height <= size.Height as u32,
                "The target size changed before capture. Select the monitor/region again."
            );
            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
            let device: ID3D11Device = device.context("D3D did not return a capture device")?;
            let context = context.context("D3D did not return a capture context")?;
            let dxgi: IDXGIDevice = device.cast()?;
            let winrt: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi)?.cast()?;
            let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
                &winrt,
                match pixel_mode {
                    PixelMode::Sdr => DirectXPixelFormat::B8G8R8A8UIntNormalized,
                    PixelMode::FloatToSdr => DirectXPixelFormat::R16G16B16A16Float,
                },
                2,
                size,
            )?;
            let session = pool.CreateCaptureSession(&item)?;
            session
                .SetIsCursorCaptureEnabled(cursor)
                .context("Independent cursor control requires Windows 10 2004 or newer")?;
            let (tx, rx) = bounded(1);
            pool.FrameArrived(
                &TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(move |_, _| {
                    // This notification carries no texture; coalescing cannot grow a frame queue.
                    let _ = tx.try_send(());
                    Ok(())
                }),
            )?;
            let desc = D3D11_TEXTURE2D_DESC {
                Width: area.width,
                Height: area.height,
                MipLevels: 1,
                ArraySize: 1,
                Format: match pixel_mode {
                    PixelMode::Sdr => DXGI_FORMAT_B8G8R8A8_UNORM,
                    PixelMode::FloatToSdr => DXGI_FORMAT_R16G16B16A16_FLOAT,
                },
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                ..Default::default()
            };
            let mut staging = None;
            device.CreateTexture2D(&desc, None, Some(&mut staging))?;
            session.StartCapture()?;
            Ok(Self {
                item,
                closed_token,
                target_closed,
                pool,
                session,
                context,
                staging: staging.context("Missing readback texture")?,
                ready: rx,
                area: area.clone(),
                monitor_size: (size.Width, size.Height),
                closed: false,
                pixel_mode,
                _runtime: runtime,
            })
        }
    }

    pub fn next(&self, timeout: Duration) -> Result<Option<Frame>> {
        ensure!(!self.target_closed.load(Ordering::Acquire),
            "The selected capture display closed or disconnected. Capture stopped; committed media is retained.");
        match self.ready.recv_timeout(timeout) {
            Ok(()) => {}
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => return Ok(None),
            Err(e) => return Err(e.into()),
        }
        let mut latest: Option<Direct3D11CaptureFrame> = None;
        for _ in 0..2 {
            let Some(frame) = optional_frame(self.pool.TryGetNextFrame())? else {
                break;
            };
            if let Some(previous) = latest.replace(frame) {
                previous.Close()?;
            }
        }
        let Some(frame) = latest else {
            return Ok(None);
        };
        let size = frame.ContentSize()?;
        ensure!((size.Width,size.Height)==self.monitor_size,"Monitor resolution changed during capture. Recording stopped to preserve its fixed canvas.");
        let timestamp = frame.SystemRelativeTime()?.Duration;
        // SAFETY: Copy while the frame is checked out; mapped rows are read before Unmap/Close.
        unsafe {
            let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
            let texture: ID3D11Texture2D = access.GetInterface()?;
            let region = D3D11_BOX {
                left: self.area.x,
                top: self.area.y,
                front: 0,
                right: self.area.x + self.area.width,
                bottom: self.area.y + self.area.height,
                back: 1,
            };
            self.context.CopySubresourceRegion(
                &self.staging,
                0,
                0,
                0,
                0,
                &texture,
                0,
                Some(&region),
            );
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let result = (|| -> Result<Frame> {
                let stride = self.area.width as usize * 4;
                let source_stride = self.area.width as usize
                    * match self.pixel_mode {
                        PixelMode::Sdr => 4,
                        PixelMode::FloatToSdr => 8,
                    };
                ensure!(
                    !mapped.pData.is_null() && mapped.RowPitch as usize >= source_stride,
                    "Capture returned an invalid mapped surface."
                );
                let mut bgra = vec![0; stride * self.area.height as usize];
                for (row, dst) in bgra.chunks_exact_mut(stride).enumerate() {
                    let src = std::slice::from_raw_parts(
                        (mapped.pData as *const u8).add(row * mapped.RowPitch as usize),
                        source_stride,
                    );
                    match self.pixel_mode {
                        PixelMode::Sdr => dst.copy_from_slice(src),
                        PixelMode::FloatToSdr => convert_float_row(src, dst)?,
                    }
                }
                Ok(Frame {
                    width: self.area.width,
                    height: self.area.height,
                    bgra,
                    timestamp,
                })
            })();
            self.context.Unmap(&self.staging, 0);
            frame.Close()?;
            result.map(Some)
        }
    }
    pub fn path_label(&self) -> &'static str {
        self.pixel_mode.label()
    }
    pub fn close(&mut self) -> Result<()> {
        if !self.closed {
            self.item.RemoveClosed(self.closed_token)?;
            self.session.Close()?;
            self.pool.Close()?;
            self.closed = true;
        }
        Ok(())
    }
}
impl Drop for Screen {
    fn drop(&mut self) {
        if let Err(e) = self.close() {
            eprintln!("Screen shutdown: {e:#}");
        }
    }
}

fn optional_frame(
    result: windows::core::Result<Direct3D11CaptureFrame>,
) -> Result<Option<Direct3D11CaptureFrame>> {
    match result {
        Ok(frame) => Ok(Some(frame)),
        // windows-core 0.58 represents a successful null WinRT result with Error::empty.
        Err(error) if error.code() == windows::core::HRESULT(0) => Ok(None),
        Err(error) => Err(error).context("Getting a Windows capture frame"),
    }
}
fn half_linear(bits: u16) -> f32 {
    let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exponent = i32::from((bits >> 10) & 31);
    let fraction = f32::from(bits & 1023);
    if exponent == 0 {
        sign * fraction * 2.0_f32.powi(-24)
    } else {
        sign * (1024.0 + fraction) * 2.0_f32.powi(exponent - 25)
    }
}
fn srgb_byte(linear: f32) -> u8 {
    let linear = linear.clamp(0.0, 1.0);
    let encoded = if linear <= 0.0031308 {
        12.92 * linear
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round() as u8
}
fn convert_float_row(source: &[u8], destination: &mut [u8]) -> Result<()> {
    static TABLE: OnceLock<Vec<u8>> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        (0..=u16::MAX)
            .map(|bits| srgb_byte(half_linear(bits)))
            .collect()
    });
    ensure!(
        source.len() == destination.len() * 2,
        "Float capture row dimensions disagree."
    );
    for (input, output) in source.chunks_exact(8).zip(destination.chunks_exact_mut(4)) {
        let red = u16::from_le_bytes([input[0], input[1]]);
        let green = u16::from_le_bytes([input[2], input[3]]);
        let blue = u16::from_le_bytes([input[4], input[5]]);
        ensure!(
            [red, green, blue].iter().all(|v| v & 0x7c00 != 0x7c00),
            "Capture returned non-finite color data. The recording was interrupted."
        );
        output.copy_from_slice(&[
            table[blue as usize],
            table[green as usize],
            table[red as usize],
            255,
        ]);
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_frame_is_not_confused_with_a_device_failure() {
        assert!(optional_frame(Err(windows::core::Error::empty()))
            .unwrap()
            .is_none());
        assert!(optional_frame(Err(windows::core::Error::from_hresult(
            windows::Win32::Foundation::E_ACCESSDENIED
        )))
        .is_err());
    }
    #[test]
    fn float_capture_maps_linear_rgb_to_srgb_and_clips_hdr_white() {
        assert_eq!(srgb_byte(half_linear(0x0000)), 0);
        assert_eq!(srgb_byte(half_linear(0x3800)), 188);
        assert_eq!(srgb_byte(half_linear(0x3c00)), 255);
        assert_eq!(srgb_byte(half_linear(0x4000)), 255);
        let mut output = [0; 4];
        convert_float_row(&[0x00, 0x3c, 0x00, 0x38, 0, 0, 0, 0x3c], &mut output).unwrap();
        assert_eq!(output, [0, 188, 255, 255]);
    }
    #[test]
    fn invalid_float_surface_is_reported() {
        assert!(convert_float_row(&[0, 0x7c, 0, 0, 0, 0, 0, 0x3c], &mut [0; 4]).is_err());
    }
}
