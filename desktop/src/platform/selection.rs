use super::{Area, Monitor};
use crate::capture::{FlowToken, PixelRect, Selection};
use anyhow::{ensure, Context, Result};
use std::{
    cell::{Cell, RefCell},
    sync::{
        atomic::{AtomicBool, AtomicIsize, Ordering},
        mpsc, Arc, OnceLock,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use windows::{
    core::{w, HSTRING},
    Win32::{
        Foundation::{BOOL, COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::*,
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::{
                EnableWindow, GetCapture, GetKeyState, ReleaseCapture, SetCapture, VK_CONTROL,
                VK_SHIFT,
            },
            WindowsAndMessaging::*,
        },
    },
};

#[derive(Clone, Copy)]
pub struct Foreground {
    handle: isize,
    process: u32,
}

impl Foreground {
    pub fn remember() -> Self {
        let mut process = 0;
        // SAFETY: The foreground handle is borrowed; only its identity is retained.
        let hwnd = unsafe {
            let hwnd = GetForegroundWindow();
            if !hwnd.is_invalid() {
                GetWindowThreadProcessId(hwnd, Some(&mut process));
            }
            hwnd
        };
        Self {
            handle: hwnd.0 as isize,
            process,
        }
    }

    pub fn restore(self) -> Result<()> {
        if self.handle == 0 {
            return Ok(());
        }
        let hwnd = HWND(self.handle as *mut _);
        let mut process = 0;
        // SAFETY: Check for destruction or handle reuse before requesting the prior focus.
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut process));
            ensure!(
                process == self.process && process != 0 && IsWindow(hwnd).as_bool(),
                "The previously focused window closed. Focus was not moved to another app."
            );
            ensure!(SetForegroundWindow(hwnd).as_bool(),
                "Windows did not allow focus restoration. The selection is cancelled; click the previous app to return.");
        }
        Ok(())
    }
}

fn owned(raw: isize) -> Result<HWND> {
    let hwnd = HWND(raw as *mut _);
    let mut process = 0;
    // SAFETY: A borrowed HWND is validated before any operation on it.
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut process));
    }
    ensure!(
        process == std::process::id(),
        "Snapper's window is no longer owned by this process."
    );
    Ok(hwnd)
}

pub fn hide_owned(raw: isize) -> Result<()> {
    let hwnd = owned(raw)?;
    // SAFETY: This only hides the validated application window, not another app.
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    Ok(())
}

pub fn show_owned(raw: isize, activate: bool) -> Result<()> {
    let hwnd = owned(raw)?;
    // SAFETY: This only restores this application's existing window.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        if activate {
            ensure!(
                SetForegroundWindow(hwnd).as_bool(),
                "Windows did not allow Snapper to take focus."
            );
        }
    }
    Ok(())
}

pub fn exclude_from_capture(raw: isize) -> Result<()> {
    let hwnd = owned(raw)?;
    // SAFETY: Display affinity applies only to the current process's top-level window.
    unsafe {
        SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)
            .context("Windows could not exclude the capture controls from screen recording")?;
        let mut affinity = 0;
        GetWindowDisplayAffinity(hwnd, &mut affinity)?;
        ensure!(
            affinity == WDA_EXCLUDEFROMCAPTURE.0,
            "This Windows session does not support excluding the recording controls."
        );
    }
    Ok(())
}

pub fn position_owned(raw: isize, x: i32, y: i32) -> Result<()> {
    let hwnd = owned(raw)?;
    // SAFETY: Position only the validated application window; do not activate it.
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER,
        )?;
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct WindowGuide {
    pub title: String,
    pub process: u32,
    pub bounds: (i32, i32, i32, i32),
}

impl WindowGuide {
    pub fn area(&self, monitor: &Monitor) -> Result<Area> {
        let (left, top, right, bottom) = self.bounds;
        ensure!(left >= monitor.left && top >= monitor.top
            && i64::from(right) <= i64::from(monitor.left) + i64::from(monitor.width)
            && i64::from(bottom) <= i64::from(monitor.top) + i64::from(monitor.height),
            "This window crosses the selected monitor edge. Move it onto one monitor or choose a region.");
        let area = Area {
            monitor: monitor.id.clone(),
            x: (left - monitor.left) as u32,
            y: (top - monitor.top) as u32,
            width: (right - left) as u32,
            height: (bottom - top) as u32,
        };
        crate::capture::validate_area(&area, std::slice::from_ref(monitor))?;
        Ok(area)
    }
}

pub fn window_guides() -> Result<Vec<WindowGuide>> {
    struct Enumeration {
        windows: Vec<WindowGuide>,
        error: Option<String>,
    }
    unsafe extern "system" fn callback(hwnd: HWND, data: LPARAM) -> BOOL {
        // SAFETY: EnumWindows keeps the exclusive stack context alive during callbacks.
        let context = unsafe { &mut *(data.0 as *mut Enumeration) };
        // SAFETY: These read-only operations borrow the enumerated window and bounded buffers.
        unsafe {
            if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
                return BOOL(1);
            }
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == std::process::id() {
                return BOOL(1);
            }
            let mut title = [0u16; 512];
            let length = GetWindowTextW(hwnd, &mut title);
            if length <= 0 {
                return BOOL(1);
            }
            let mut rect = RECT::default();
            if let Err(error) = GetWindowRect(hwnd, &mut rect) {
                if !IsWindow(hwnd).as_bool() {
                    return BOOL(1);
                }
                context.error = Some(format!("Reading window bounds failed: {error}"));
                return BOOL(0);
            }
            if rect.right > rect.left && rect.bottom > rect.top {
                context.windows.push(WindowGuide {
                    title: String::from_utf16_lossy(&title[..length as usize]),
                    process: pid,
                    bounds: (rect.left, rect.top, rect.right, rect.bottom),
                });
            }
            if context.windows.len() >= 256 {
                context.error = Some(
                    "More than 256 windows are open. Close some windows or use region selection."
                        .into(),
                );
                return BOOL(0);
            }
        }
        BOOL(1)
    }
    let mut context = Enumeration {
        windows: vec![],
        error: None,
    };
    // SAFETY: The synchronous callback receives the context's exclusive live pointer.
    let result = unsafe { EnumWindows(Some(callback), LPARAM(&mut context as *mut _ as isize)) };
    if let Some(error) = context.error {
        anyhow::bail!("{error}");
    }
    result.context("Listing visible windows for a fixed screen-region guide")?;
    Ok(context.windows)
}

pub struct OverlayEvent {
    pub token: FlowToken,
    pub result: std::result::Result<Option<Area>, String>,
}

pub struct Overlay {
    pub events: mpsc::Receiver<OverlayEvent>,
    handle: Arc<AtomicIsize>,
    closing: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

struct State {
    owner: isize,
    token: FlowToken,
    monitor: Monitor,
    selection: RefCell<Selection>,
    result: mpsc::SyncSender<OverlayEvent>,
    repaint: Box<dyn Fn() + Send>,
    terminal: Cell<bool>,
    label: Cell<HWND>,
    confirm: Cell<HWND>,
    font: Cell<HFONT>,
}

impl Drop for State {
    fn drop(&mut self) {
        if !self.font.get().is_invalid() {
            // SAFETY: All child controls are destroyed before this owning-thread font is released.
            if !unsafe { DeleteObject(self.font.get()) }.as_bool() {
                eprintln!(
                    "Region selector font release failed: {}",
                    windows::core::Error::from_win32()
                );
            }
        }
    }
}

const ACCEPT: usize = 101;
const CANCEL: usize = 102;
const CLASS: windows::core::PCWSTR = w!("Snapper.RegionSelection");
static REGISTERED: OnceLock<std::result::Result<(), String>> = OnceLock::new();

impl Overlay {
    pub fn start(
        token: FlowToken,
        owner: isize,
        monitor: Monitor,
        initial: Option<PixelRect>,
        repaint: impl Fn() + Send + 'static,
    ) -> Result<Self> {
        let (tx, events) = mpsc::sync_channel(1);
        let (ready, started) = mpsc::sync_channel(1);
        let handle = Arc::new(AtomicIsize::new(0));
        let closing = Arc::new(AtomicBool::new(false));
        let live = handle.clone();
        let cancelled = closing.clone();
        let thread = thread::spawn(move || {
            let state = State {
                owner,
                token,
                selection: RefCell::new(Selection::new(&monitor, initial)),
                monitor,
                result: tx,
                repaint: Box::new(repaint),
                terminal: Cell::new(false),
                label: Cell::new(HWND::default()),
                confirm: Cell::new(HWND::default()),
                font: Cell::new(HFONT::default()),
            };
            let outcome = run_overlay(&state, &live, &cancelled, &ready);
            if let Err(error) = outcome {
                let _ = ready.try_send(Err(format!("{error:#}")));
                let _ = state.result.try_send(OverlayEvent {
                    token,
                    result: Err(format!("{error:#}")),
                });
                (state.repaint)();
            }
            live.store(0, Ordering::Release);
        });
        let overlay = Self {
            events,
            handle,
            closing,
            thread: Some(thread),
        };
        started
            .recv_timeout(Duration::from_secs(2))
            .context("Region overlay did not open within 2 seconds")?
            .map_err(anyhow::Error::msg)?;
        Ok(overlay)
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        self.closing.store(true, Ordering::Release);
        let handle = self.handle.load(Ordering::Acquire);
        if handle != 0 {
            // SAFETY: Only this overlay's still-owned thread/window receives the close request.
            let closed =
                unsafe { PostMessageW(HWND(handle as *mut _), WM_CLOSE, WPARAM(0), LPARAM(0)) };
            if let Err(error) = closed {
                if self.thread.as_ref().is_some_and(|t| !t.is_finished()) {
                    eprintln!("Region overlay cleanup: {error}");
                }
            }
        }
        if self.thread.as_ref().is_some_and(|t| t.is_finished()) {
            if let Some(thread) = self.thread.take() {
                if thread.join().is_err() {
                    eprintln!("Region overlay thread panicked.");
                }
            }
        }
    }
}

fn run_overlay(
    state: &State,
    handle: &AtomicIsize,
    closing: &AtomicBool,
    ready: &mpsc::SyncSender<std::result::Result<(), String>>,
) -> Result<()> {
    // SAFETY: The window, paint and input resources are created, used and released on this thread.
    unsafe {
        let instance = HINSTANCE(GetModuleHandleW(None)?.0);
        REGISTERED
            .get_or_init(|| {
                let cursor = LoadCursorW(None, IDC_CROSS)
                    .map_err(|e| format!("Loading the selection cursor: {e}"))?;
                let class = WNDCLASSW {
                    lpfnWndProc: Some(window_proc),
                    hInstance: instance,
                    lpszClassName: CLASS,
                    hCursor: cursor,
                    ..Default::default()
                };
                if RegisterClassW(&class) == 0 {
                    Err(format!(
                        "Registering region selector: {}",
                        windows::core::Error::from_win32()
                    ))
                } else {
                    Ok(())
                }
            })
            .as_ref()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
            CLASS,
            w!("Snapper - Select a region - Esc to cancel"),
            WS_POPUP | WS_CLIPCHILDREN,
            state.monitor.left,
            state.monitor.top,
            state.monitor.width as i32,
            state.monitor.height as i32,
            None,
            None,
            instance,
            Some(state as *const State as *const _),
        )?;
        handle.store(hwnd.0 as isize, Ordering::Release);
        let setup = (|| -> Result<()> {
            // A nonzero uniform alpha deliberately retains mouse ownership over the selected
            // interior. A color-keyed hole would forward drags into the underlying application.
            SetLayeredWindowAttributes(hwnd, COLORREF(0), 150, LWA_ALPHA)?;
            SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)?;
            let scale = state.monitor.dpi as i32;
            let unit = |value: i32| value * scale / 96;
            let mut font = LOGFONTW {
                lfHeight: -unit(16),
                lfWeight: FW_NORMAL.0 as i32,
                lfCharSet: DEFAULT_CHARSET,
                lfQuality: CLEARTYPE_QUALITY,
                ..Default::default()
            };
            for (slot, unit) in font.lfFaceName.iter_mut().zip("Segoe UI".encode_utf16()) {
                *slot = unit;
            }
            state.font.set(CreateFontIndirectW(&font));
            ensure!(
                !state.font.get().is_invalid(),
                "The region selector's DPI-scaled control font could not be created."
            );
            let x = (state.monitor.width as i32 - unit(640)).max(0) / 2;
            state.label.set(CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("Drag a region. Resize handles or move inside. Esc to cancel."),
                WS_CHILD | WS_VISIBLE,
                x,
                unit(18),
                unit(640),
                unit(30),
                hwnd,
                None,
                instance,
                None,
            )?);
            state.confirm.set(CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("BUTTON"),
                w!("Review sources"),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32),
                x,
                unit(56),
                unit(160),
                unit(34),
                hwnd,
                HMENU(ACCEPT as *mut _),
                instance,
                None,
            )?);
            let cancel = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("BUTTON"),
                w!("Cancel (Esc)"),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32),
                x + unit(170),
                unit(56),
                unit(140),
                unit(34),
                hwnd,
                HMENU(CANCEL as *mut _),
                instance,
                None,
            )?;
            for control in [state.label.get(), state.confirm.get(), cancel] {
                SendMessageW(
                    control,
                    WM_SETFONT,
                    WPARAM(state.font.get().0 as usize),
                    LPARAM(1),
                );
            }
            update_label(state)?;
            ensure!(
                !closing.load(Ordering::Acquire),
                "Region selection was cancelled before opening."
            );
            let _ = ShowWindow(hwnd, SW_SHOW);
            ensure!(
                SetForegroundWindow(hwnd).as_bool(),
                "Windows did not allow the region selector to take focus."
            );
            Ok(())
        })();
        if let Err(error) = setup {
            DestroyWindow(hwnd)?;
            return Err(error);
        }
        let _ = ready.send(Ok(()));
        let mut message = MSG::default();
        loop {
            let status = GetMessageW(&mut message, None, 0, 0).0;
            if status == 0 {
                break;
            }
            if status < 0 {
                DestroyWindow(hwnd)?;
                anyhow::bail!(
                    "Region input loop failed: {}",
                    windows::core::Error::from_win32()
                );
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

fn update_label(state: &State) -> Result<()> {
    let rectangle = state.selection.borrow().rect;
    let accepted = state.selection.borrow().accepted().is_some();
    let label = match rectangle {
        Some(r) => format!(
            "{} x {} px   |   drag to move; handles to resize   |   Esc to cancel",
            r.width, r.height
        ),
        None => "Drag a region. Enter to review sources. Esc to cancel.".into(),
    };
    // SAFETY: Child controls belong to this live overlay on the current thread.
    unsafe {
        SetWindowTextW(state.label.get(), &HSTRING::from(label))?;
        let _ = EnableWindow(state.confirm.get(), accepted);
    }
    Ok(())
}

fn release_gesture(
    selection: &RefCell<Selection>,
    position: Option<(i32, i32)>,
    release: impl FnOnce(),
) {
    {
        let mut selection = selection.borrow_mut();
        if let Some((x, y)) = position {
            selection.up(x, y);
        } else {
            selection.cancel();
        }
    }
    release();
}

fn finish(hwnd: HWND, state: &State, result: std::result::Result<Option<Area>, String>) {
    if state.terminal.replace(true) {
        return;
    }
    release_gesture(&state.selection, None, || {
        // SAFETY: Release only this thread's capture after all mutable model borrows have ended.
        unsafe {
            let _ = ReleaseCapture();
        }
    });
    // SAFETY: Only this thread's input capture and overlay window are released.
    let closed = unsafe {
        let closed = DestroyWindow(hwnd);
        PostQuitMessage(0);
        closed
    };
    let result = match closed {
        Ok(()) => result,
        Err(error) => Err(format!(
            "Region overlay could not close: {error}. Capture was not started."
        )),
    };
    if let Err(error) = show_owned(state.owner, false) {
        eprintln!("Restoring Snapper after region selection: {error:#}");
    }
    let _ = state.result.try_send(OverlayEvent {
        token: state.token,
        result,
    });
    (state.repaint)();
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_DESTROY {
        // SAFETY: This terminates only the overlay's private message loop.
        unsafe {
            PostQuitMessage(0);
        }
        return LRESULT(0);
    }
    if message == WM_NCCREATE {
        // SAFETY: CreateWindowEx passes our live State pointer through CREATESTRUCTW.
        unsafe {
            let create = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        }
    }
    if !matches!(
        message,
        WM_CLOSE
            | WM_DISPLAYCHANGE
            | WM_DPICHANGED
            | WM_COMMAND
            | WM_KEYDOWN
            | WM_LBUTTONDOWN
            | WM_MOUSEMOVE
            | WM_CAPTURECHANGED
            | WM_CANCELMODE
            | WM_LBUTTONUP
            | WM_PAINT
    ) {
        // SAFETY: Reentrant destruction/control messages do not borrow the mutable input state.
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    // SAFETY: State outlives the message loop; callbacks are serialized on this window's thread.
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const State;
    if pointer.is_null() {
        // SAFETY: Default processing for messages before our State is attached.
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    // SAFETY: Same-thread reentrant callbacks share State. Mutations use short Cell/RefCell
    // borrows that always end before a Win32 call capable of synchronous reentry.
    let state = unsafe { &*pointer };
    let x = (lparam.0 as u16 as i16) as i32;
    let y = ((lparam.0 >> 16) as u16 as i16) as i32;
    match message {
        WM_CAPTURECHANGED => {
            let lost = lparam.0 != hwnd.0 as isize && state.selection.borrow_mut().capture_lost();
            if lost {
                finish(hwnd, state, Ok(None));
            }
            LRESULT(0)
        }
        WM_CANCELMODE => {
            finish(hwnd, state, Ok(None));
            LRESULT(0)
        }
        WM_CLOSE => {
            finish(hwnd, state, Ok(None));
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_DPICHANGED => {
            finish(
                hwnd,
                state,
                Err(
                    "Display layout or DPI changed during selection. Select the region again."
                        .into(),
                ),
            );
            LRESULT(0)
        }
        WM_COMMAND => {
            match wparam.0 & 0xffff {
                ACCEPT => {
                    let accepted = state.selection.borrow().accepted();
                    if let Some(rect) = accepted {
                        finish(hwnd, state, Ok(Some(rect.area(&state.monitor.id))));
                    }
                }
                CANCEL => finish(hwnd, state, Ok(None)),
                _ => {}
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if wparam.0 == 0x1b {
                finish(hwnd, state, Ok(None));
            } else if wparam.0 == 0x0d {
                let accepted = state.selection.borrow().accepted();
                if let Some(rect) = accepted {
                    finish(hwnd, state, Ok(Some(rect.area(&state.monitor.id))));
                }
            } else {
                // SAFETY: Query only modifier state for this focused keyboard event.
                let (step, resize) = unsafe {
                    (
                        if GetKeyState(i32::from(VK_SHIFT.0)) < 0 {
                            10
                        } else {
                            1
                        },
                        GetKeyState(i32::from(VK_CONTROL.0)) < 0,
                    )
                };
                let (dx, dy) = match wparam.0 {
                    0x25 => (-step, 0),
                    0x26 => (0, -step),
                    0x27 => (step, 0),
                    0x28 => (0, step),
                    _ => (0, 0),
                };
                state.selection.borrow_mut().nudge(dx, dy, resize);
                refresh(hwnd, state);
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            state.selection.borrow_mut().down(x, y);
            // SAFETY: The overlay owns input only for the lifetime of this mouse drag.
            unsafe {
                let _ = SetCapture(hwnd);
            }
            refresh(hwnd, state);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            // SAFETY: Query this thread's current mouse-capture owner without changing it.
            let captured = unsafe { GetCapture() } == hwnd;
            let valid =
                state
                    .selection
                    .borrow_mut()
                    .captured_motion(x, y, wparam.0 & 1 != 0, captured);
            if !valid {
                finish(hwnd, state, Ok(None));
                return LRESULT(0);
            }
            refresh(hwnd, state);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if !state.selection.borrow().dragging() {
                return LRESULT(0);
            }
            // SAFETY: A late mouse-up without this overlay's capture cannot finish a gesture.
            if unsafe { GetCapture() } != hwnd {
                finish(hwnd, state, Ok(None));
                return LRESULT(0);
            }
            release_gesture(&state.selection, Some((x, y)), || {
                // SAFETY: No RefMut survives across the synchronous WM_CAPTURECHANGED callback.
                unsafe {
                    let _ = ReleaseCapture();
                }
            });
            refresh(hwnd, state);
            LRESULT(0)
        }
        WM_PAINT => {
            paint(hwnd, state);
            LRESULT(0)
        }
        _ => {
            // SAFETY: Remaining native messages use standard window processing.
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
    }
}

fn refresh(hwnd: HWND, state: &State) {
    if state.terminal.get() {
        return;
    }
    if let Err(error) = update_label(state) {
        finish(
            hwnd,
            state,
            Err(format!("Region controls failed: {error:#}")),
        );
        return;
    }
    // SAFETY: Invalidate only our own client area; painting is handled on this same thread.
    unsafe {
        let _ = InvalidateRect(hwnd, None, false);
    }
}

fn paint(hwnd: HWND, state: &State) {
    let selected_rect = state.selection.borrow().rect;
    // SAFETY: Every GDI object is restored/deleted before EndPaint on this owning thread.
    unsafe {
        let mut paint = PAINTSTRUCT::default();
        let dc = BeginPaint(hwnd, &mut paint);
        let background = CreateSolidBrush(COLORREF(0x00100c08));
        let whole = RECT {
            left: 0,
            top: 0,
            right: state.monitor.width as i32,
            bottom: state.monitor.height as i32,
        };
        FillRect(dc, &whole, background);
        let _ = DeleteObject(background);
        if let Some(r) = selected_rect {
            let selected = CreateSolidBrush(COLORREF(0x006e8278));
            let rect = RECT {
                left: r.x as i32,
                top: r.y as i32,
                right: r.right(),
                bottom: r.bottom(),
            };
            FillRect(dc, &rect, selected);
            let _ = DeleteObject(selected);
            let white = CreateSolidBrush(COLORREF(0x00ffffff));
            FrameRect(dc, &rect, white);
            let radius = (4 * state.monitor.dpi / 96).max(3) as i32;
            for (x, y) in [
                (rect.left, rect.top),
                ((rect.left + rect.right) / 2, rect.top),
                (rect.right, rect.top),
                (rect.left, (rect.top + rect.bottom) / 2),
                (rect.right, (rect.top + rect.bottom) / 2),
                (rect.left, rect.bottom),
                ((rect.left + rect.right) / 2, rect.bottom),
                (rect.right, rect.bottom),
            ] {
                FillRect(
                    dc,
                    &RECT {
                        left: x - radius,
                        top: y - radius,
                        right: x + radius,
                        bottom: y + radius,
                    },
                    white,
                );
            }
            let _ = DeleteObject(white);
        }
        let _ = EndPaint(hwnd, &paint);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn release_capture_seam_allows_synchronous_native_capturechanged_reentry() {
        use crate::capture::CaptureFlow;
        let monitor = Monitor {
            id: "fixture".into(),
            left: 0,
            top: 0,
            width: 640,
            height: 360,
            primary: true,
            dpi: 96,
        };
        let (result, _events) = mpsc::sync_channel(1);
        let state = State {
            owner: 0,
            token: CaptureFlow::default().begin().unwrap(),
            selection: RefCell::new(Selection::new(&monitor, None)),
            monitor,
            result,
            repaint: Box::new(|| {}),
            terminal: Cell::new(false),
            label: Cell::new(HWND::default()),
            confirm: Cell::new(HWND::default()),
            font: Cell::new(HFONT::default()),
        };
        // SAFETY: This creates an invisible message-only test window. It never takes
        // focus or real mouse capture; SendMessage supplies the synchronous callback.
        unsafe {
            let instance = HINSTANCE(GetModuleHandleW(None).unwrap().0);
            let class = w!("Snapper.SelectionReentryTest");
            let registration = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                lpszClassName: class,
                ..Default::default()
            };
            assert_ne!(RegisterClassW(&registration), 0);
            let window = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                w!(""),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                None,
                instance,
                Some(&state as *const State as *const _),
            )
            .unwrap();
            state.selection.borrow_mut().down(20, 20);
            state.selection.borrow_mut().motion(120, 120);
            release_gesture(&state.selection, Some((120, 120)), || {
                SendMessageW(window, WM_CAPTURECHANGED, WPARAM(0), LPARAM(0));
            });
            assert_eq!(state.selection.borrow().accepted().unwrap().width, 100);
            state.selection.borrow_mut().down(60, 60);
            release_gesture(&state.selection, None, || {
                SendMessageW(window, WM_CAPTURECHANGED, WPARAM(0), LPARAM(0));
            });
            assert!(state.selection.borrow().accepted().is_none());
            DestroyWindow(window).unwrap();
            UnregisterClassW(class, instance).unwrap();
            let mut message = MSG::default();
            let _ = PeekMessageW(&mut message, None, WM_QUIT, WM_QUIT, PM_REMOVE);
        }
    }
    #[test]
    fn window_guidance_is_fixed_physical_region_not_cross_monitor_capture() {
        let monitor = Monitor {
            id: "left".into(),
            left: -1920,
            top: 0,
            width: 1920,
            height: 1080,
            primary: false,
            dpi: 144,
        };
        let guide = WindowGuide {
            title: "fixture".into(),
            process: 17,
            bounds: (-1820, 100, -820, 700),
        };
        assert_eq!(
            guide.area(&monitor).unwrap(),
            Area {
                monitor: "left".into(),
                x: 100,
                y: 100,
                width: 1000,
                height: 600
            }
        );
        let crossing = WindowGuide {
            bounds: (-100, 100, 900, 700),
            ..guide
        };
        assert!(crossing.area(&monitor).is_err());
    }
}
