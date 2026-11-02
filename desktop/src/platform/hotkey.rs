use crate::capture::{CaptureAdmission, Shortcut, Shortcuts};
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
            RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL,
            MOD_NOREPEAT, MOD_SHIFT, VK_ESCAPE, VK_F9,
        },
        WindowsAndMessaging::{
            GetMessageW, PeekMessageW, PostThreadMessageW, MSG, PM_REMOVE, WM_APP, WM_HOTKEY,
        },
    },
};
const SHUTDOWN: u32 = WM_APP + 71;

pub struct GlobalStop {
    pub flag: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    id: Arc<AtomicU32>,
    error: Arc<Mutex<Option<String>>>,
    target: Arc<Mutex<Option<Arc<CaptureAdmission>>>>,
    thread: Option<JoinHandle<()>>,
}
impl GlobalStop {
    pub fn new(repaint: impl Fn() + Send + 'static) -> Result<Self> {
        Self::with_key(u32::from(VK_F9.0), repaint)
    }
    fn with_key(key: u32, repaint: impl Fn() + Send + 'static) -> Result<Self> {
        Self::with_binding(
            key,
            MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT,
            "Ctrl+Shift+function key",
            None,
            repaint,
        )
    }
    pub fn shortcut(shortcut: &Shortcut, repaint: impl Fn() + Send + 'static) -> Result<Self> {
        let mut modifiers = MOD_NOREPEAT;
        if shortcut.control {
            modifiers |= MOD_CONTROL;
        }
        if shortcut.shift {
            modifiers |= MOD_SHIFT;
        }
        if shortcut.alt {
            modifiers |= MOD_ALT;
        }
        Self::with_binding(shortcut.key, modifiers, &shortcut.label(), None, repaint)
    }
    pub fn escape(
        gate: Arc<CaptureAdmission>,
        repaint: impl Fn() + Send + 'static,
    ) -> Result<Self> {
        Self::with_binding(
            u32::from(VK_ESCAPE.0),
            MOD_NOREPEAT,
            "Escape (active selection only)",
            Some(gate),
            repaint,
        )
    }
    fn with_binding(
        key: u32,
        modifiers: HOT_KEY_MODIFIERS,
        label: &str,
        cancellation: Option<Arc<CaptureAdmission>>,
        repaint: impl Fn() + Send + 'static,
    ) -> Result<Self> {
        Self::with_dispatch(key, modifiers, label, cancellation, None, repaint)
    }

    fn with_dispatch(
        key: u32,
        modifiers: HOT_KEY_MODIFIERS,
        label: &str,
        cancellation: Option<Arc<CaptureAdmission>>,
        after_dequeue: Option<Box<dyn Fn() + Send>>,
        repaint: impl Fn() + Send + 'static,
    ) -> Result<Self> {
        let flag = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(AtomicBool::new(false));
        let id = Arc::new(AtomicU32::new(0));
        let error = Arc::new(Mutex::new(None));
        let target: Arc<Mutex<Option<Arc<CaptureAdmission>>>> = Arc::new(Mutex::new(None));
        let active_target = target.clone();
        let (ready, rx) = mpsc::sync_channel(1);
        let stop = flag.clone();
        let closing = shutdown.clone();
        let thread_id = id.clone();
        let errors = error.clone();
        let handle = thread::spawn(move || {
            // SAFETY: This thread owns the registration and its private message queue.
            unsafe {
                thread_id.store(GetCurrentThreadId(), Ordering::Release);
                if let Err(e) = RegisterHotKey(None, 1, modifiers, key) {
                    let _ = ready.send(Err(e));
                    return;
                }
                let _ = ready.send(Ok(()));
                let dispatch = || {
                    if let Some(gate) = &cancellation {
                        gate.cancel();
                    } else {
                        stop.store(true, Ordering::Release);
                        if let Some(gate) = active_target
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .clone()
                        {
                            gate.cancel();
                        }
                    }
                    repaint();
                };
                let mut message = MSG::default();
                if closing.load(Ordering::Acquire) {
                    let _ =
                        PostThreadMessageW(GetCurrentThreadId(), SHUTDOWN, WPARAM(0), LPARAM(0));
                }
                loop {
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
                    if message.message == SHUTDOWN {
                        break;
                    }
                    if message.message == WM_HOTKEY && message.wParam.0 == 1 {
                        if let Some(hook) = &after_dequeue {
                            hook();
                        }
                        dispatch();
                    }
                }
                if let Err(e) = UnregisterHotKey(None, 1) {
                    *errors.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(format!("Global Stop shortcut release failed: {e}"));
                    repaint();
                }
                // Registration is now closed. Dispatch already queued signals before
                // releasing this generation; disposal itself never manufactures Escape.
                while PeekMessageW(&mut message, None, WM_HOTKEY, WM_HOTKEY, PM_REMOVE).as_bool() {
                    if message.wParam.0 == 1 {
                        dispatch();
                    }
                }
            }
        });
        let hotkey = Self {
            flag,
            shutdown,
            id,
            error,
            target,
            thread: Some(handle),
        };
        rx.recv_timeout(Duration::from_secs(2)).context("Shortcut registration exceeded two seconds")?
            .with_context(|| format!("{label} is already in use or unavailable. Snapper did not take it from another app; choose a different shortcut."))?;
        Ok(hotkey)
    }
    pub fn take_error(&self) -> Option<String> {
        self.error.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
    pub fn set_target(&self, gate: Option<Arc<CaptureAdmission>>) {
        let mut target = self.target.lock().unwrap_or_else(|e| e.into_inner());
        if self.flag.load(Ordering::Acquire) {
            if let Some(gate) = &gate {
                gate.cancel();
            }
        }
        *target = gate;
    }
}
impl Drop for GlobalStop {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        let id = self.id.load(Ordering::Acquire);
        if id != 0 {
            // SAFETY: This is our registration thread, never a foreground app or global queue.
            if let Err(e) = unsafe { PostThreadMessageW(id, SHUTDOWN, WPARAM(0), LPARAM(0)) } {
                if self.thread.as_ref().is_some_and(|t| !t.is_finished()) {
                    eprintln!("Global Stop shutdown notification: {e}");
                }
            }
        }
        let deadline = std::time::Instant::now() + Duration::from_millis(500);
        while self.thread.as_ref().is_some_and(|t| !t.is_finished())
            && std::time::Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(1));
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

pub struct GlobalShortcuts {
    pub capture: GlobalStop,
    pub repeat: GlobalStop,
    pub stop: GlobalStop,
}

impl GlobalShortcuts {
    pub fn new(shortcuts: &Shortcuts, repaint: impl Fn() + Clone + Send + 'static) -> Result<Self> {
        let [capture, repeat, stop] = shortcuts.parsed()?;
        Ok(Self {
            capture: GlobalStop::shortcut(&capture, repaint.clone())?,
            repeat: GlobalStop::shortcut(&repeat, repaint.clone())?,
            stop: GlobalStop::shortcut(&stop, repaint)?,
        })
    }

    pub fn take_error(&self) -> Option<String> {
        let errors: Vec<_> = [&self.capture, &self.repeat, &self.stop]
            .into_iter()
            .filter_map(GlobalStop::take_error)
            .collect();
        (!errors.is_empty()).then(|| errors.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn escape_returned_by_getmessage_survives_disposal_after_admission() {
        use crate::capture::CaptureFlow;
        use std::sync::Barrier;
        let token = CaptureFlow::default().begin().unwrap();
        let gate = Arc::new(CaptureAdmission::new(token));
        let recorder_stop = gate.admit(token, |stop| stop).unwrap();
        let dequeued = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        let arrived = dequeued.clone();
        let release = resume.clone();
        let hotkey = GlobalStop::with_dispatch(
            0x85,
            MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT,
            "dequeued cancellation test",
            Some(gate),
            Some(Box::new(move || {
                arrived.wait();
                release.wait();
            })),
            || {},
        )
        .unwrap();
        let closing = hotkey.shutdown.clone();
        // SAFETY: Post only to the actual handler's test-owned queue, not physical input.
        unsafe {
            PostThreadMessageW(
                hotkey.id.load(Ordering::Acquire),
                WM_HOTKEY,
                WPARAM(1),
                LPARAM(0),
            )
            .unwrap();
        }
        dequeued.wait();
        let disposing = thread::spawn(move || drop(hotkey));
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !closing.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(closing.load(Ordering::Acquire));
        resume.wait();
        disposing.join().unwrap();
        assert!(recorder_stop.load(Ordering::Acquire));
    }

    #[test]
    fn normal_registration_disposal_does_not_stop_an_admitted_recording() {
        use crate::capture::CaptureFlow;
        let token = CaptureFlow::default().begin().unwrap();
        let gate = Arc::new(CaptureAdmission::new(token));
        let stop = gate.admit(token, |stop| stop).unwrap();
        drop(
            GlobalStop::with_binding(
                0x84,
                MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT,
                "normal disposal test",
                Some(gate),
                || {},
            )
            .unwrap(),
        );
        assert!(!stop.load(Ordering::Acquire));
    }
    #[test]
    fn delivered_cancellation_during_final_preflight_prevents_all_start_side_effects() {
        use crate::capture::CaptureFlow;
        use std::sync::{atomic::AtomicUsize, Barrier};
        let mut flow = CaptureFlow::default();
        let token = flow.begin().unwrap();
        let gate = Arc::new(CaptureAdmission::new(token));
        let entered_preflight = Arc::new(Barrier::new(2));
        let finish_preflight = Arc::new(Barrier::new(2));
        let effects = Arc::new(AtomicUsize::new(0));
        let candidate = gate.clone();
        let entered = entered_preflight.clone();
        let finish = finish_preflight.clone();
        let recorded = effects.clone();
        let admission = thread::spawn(move || {
            assert!(!candidate.cancelled());
            entered.wait();
            finish.wait();
            candidate.admit(token, |_| {
                recorded.fetch_add(1, Ordering::AcqRel);
            })
        });
        let (delivered, received) = mpsc::sync_channel(1);
        let hotkey = GlobalStop::with_binding(
            0x86,
            MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT,
            "test cancellation",
            Some(gate.clone()),
            move || {
                let _ = delivered.send(());
            },
        )
        .unwrap();
        entered_preflight.wait();
        // SAFETY: Deliver to the actual handler in the test-owned queue, never synthesize user input.
        unsafe {
            PostThreadMessageW(
                hotkey.id.load(Ordering::Acquire),
                WM_HOTKEY,
                WPARAM(1),
                LPARAM(0),
            )
            .unwrap();
        }
        received.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(gate.cancelled());
        finish_preflight.wait();
        assert!(admission.join().unwrap().is_none());
        assert_eq!(
            effects.load(Ordering::Acquire),
            0,
            "No worker start, source, output or clipboard action may run."
        );
    }
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
