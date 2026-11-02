use anyhow::{Context, Result};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use windows::Win32::{
    Foundation::{LPARAM, WPARAM},
    System::Threading::GetCurrentThreadId,
    UI::{
        Input::KeyboardAndMouse::{
            RegisterHotKey, UnregisterHotKey, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, VK_F9,
        },
        WindowsAndMessaging::{GetMessageW, PostThreadMessageW, MSG, WM_HOTKEY, WM_QUIT},
    },
};

pub struct GlobalStop {
    pub flag: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    id: Arc<AtomicU32>,
    error: Arc<Mutex<Option<String>>>,
    thread: Option<JoinHandle<()>>,
}
impl GlobalStop {
    pub fn new(repaint: impl Fn() + Send + 'static) -> Result<Self> {
        Self::with_key(u32::from(VK_F9.0), repaint)
    }
    fn with_key(key: u32, repaint: impl Fn() + Send + 'static) -> Result<Self> {
        let flag = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(AtomicBool::new(false));
        let id = Arc::new(AtomicU32::new(0));
        let error = Arc::new(Mutex::new(None));
        let (ready, rx) = mpsc::sync_channel(1);
        let stop = flag.clone();
        let closing = shutdown.clone();
        let thread_id = id.clone();
        let errors = error.clone();
        let handle = thread::spawn(move || {
            // SAFETY: This thread owns the registration and its private message queue.
            unsafe {
                thread_id.store(GetCurrentThreadId(), Ordering::Release);
                if let Err(e) = RegisterHotKey(None, 1, MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT, key)
                {
                    let _ = ready.send(Err(e));
                    return;
                }
                let _ = ready.send(Ok(()));
                let mut message = MSG::default();
                while !closing.load(Ordering::Acquire) {
                    let status = GetMessageW(&mut message, None, 0, 0).0;
                    if status == 0 {
                        break;
                    }
                    if status < 0 {
                        *errors.lock().unwrap_or_else(|e| e.into_inner()) = Some(format!(
                            "Global Stop message loop failed: {}",
                            windows::core::Error::from_win32()
                        ));
                        repaint();
                        break;
                    }
                    if message.message == WM_HOTKEY && message.wParam.0 == 1 {
                        stop.store(true, Ordering::Release);
                        repaint();
                    }
                }
                if let Err(e) = UnregisterHotKey(None, 1) {
                    *errors.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(format!("Global Stop shortcut release failed: {e}"));
                    repaint();
                }
            }
        });
        let hotkey = Self {
            flag,
            shutdown,
            id,
            error,
            thread: Some(handle),
        };
        rx.recv_timeout(Duration::from_secs(2)).context("Global Stop shortcut registration exceeded two seconds")?
            .context("Ctrl+Shift+F9 is unavailable, possibly because another app owns it. The Stop button and focused Escape key still work")?;
        Ok(hotkey)
    }
    pub fn take_error(&self) -> Option<String> {
        self.error.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}
impl Drop for GlobalStop {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        let id = self.id.load(Ordering::Acquire);
        if id != 0 {
            // SAFETY: This is our registration thread, never a foreground app or global queue.
            if let Err(e) = unsafe { PostThreadMessageW(id, WM_QUIT, WPARAM(0), LPARAM(0)) } {
                if self.thread.as_ref().is_some_and(|t| !t.is_finished()) {
                    eprintln!("Global Stop shutdown notification: {e}");
                }
            }
        }
        if self.thread.as_ref().is_some_and(|t| t.is_finished()) {
            if let Some(t) = self.thread.take() {
                if t.join().is_err() {
                    eprintln!("Global Stop thread panicked.");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stop_message_reaches_control_flag_without_gui_dispatch() {
        let hotkey = GlobalStop::with_key(0x87, || {}).unwrap();
        // SAFETY: This posts only to the test-owned hotkey queue; it does not simulate input.
        unsafe {
            PostThreadMessageW(
                hotkey.id.load(Ordering::Acquire),
                WM_HOTKEY,
                WPARAM(1),
                LPARAM(0),
            )
            .unwrap();
        }
        for _ in 0..100 {
            if hotkey.flag.load(Ordering::Acquire) {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert!(hotkey.flag.load(Ordering::Acquire));
    }
}
