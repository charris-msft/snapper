//! THESIS: choose pixels, review sources, then commit one intentional capture.
//! OWN-WORLD: compact Windows controls, neutral surfaces, a single green action.
//! STORY: select -> review -> countdown -> capture; Escape ends the entire flow.
//! FIRST VIEWPORT: mode and target above one Select area action, recent work below.
//! FORM: the user-approved compact launcher, native overlay and movable recording dock.

use crate::{
    library::{self, Library},
    media::Sources,
    platform::{Area, Monitor},
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc,
    },
    time::Instant,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetKind {
    #[default]
    Region,
    Monitor,
    WindowGuide,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Destination {
    #[default]
    Editor,
    Clipboard,
    File,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipLayout {
    pub x: u16,
    pub y: u16,
    pub width: u16,
}

impl Default for PipLayout {
    fn default() -> Self {
        Self {
            x: 970,
            y: 970,
            width: 250,
        }
    }
}

impl PipLayout {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.x <= 1000 && self.y <= 1000 && (100..=500).contains(&self.width),
            "Camera placement must be inside the capture; size must be 10-50 percent."
        );
        Ok(())
    }

    pub fn rect(
        &self,
        width: u32,
        height: u32,
        camera_width: u32,
        camera_height: u32,
    ) -> PixelRect {
        let mut w = (u64::from(width) * u64::from(self.width) / 1000).max(1) as u32;
        let mut h = (u64::from(w) * u64::from(camera_height) / u64::from(camera_width.max(1)))
            .max(1) as u32;
        if h > height {
            h = height.max(1);
            w = (u64::from(h) * u64::from(camera_width) / u64::from(camera_height.max(1))).max(1)
                as u32;
        }
        w = w.min(width).max(1);
        h = h.min(height).max(1);
        PixelRect {
            x: (u64::from(width.saturating_sub(w)) * u64::from(self.x) / 1000) as u32,
            y: (u64::from(height.saturating_sub(h)) * u64::from(self.y) / 1000) as u32,
            width: w,
            height: h,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureOptions {
    pub video: bool,
    pub target: TargetKind,
    pub sources: Sources,
    pub camera_id: Option<String>,
    pub microphone_id: Option<String>,
    pub output_id: Option<String>,
    pub pip: PipLayout,
    pub countdown_seconds: u8,
    pub limit_minutes: u32,
    pub destination: Destination,
    pub output_directory: Option<PathBuf>,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            video: false,
            target: TargetKind::Region,
            sources: Sources::default(),
            camera_id: None,
            microphone_id: None,
            output_id: None,
            pip: PipLayout::default(),
            countdown_seconds: 3,
            limit_minutes: 60,
            destination: Destination::Editor,
            output_directory: None,
        }
    }
}

impl CaptureOptions {
    pub fn effective_sources(&self) -> Sources {
        let mut sources = self.sources.clone();
        if !self.video {
            sources.microphone = false;
            sources.system_audio = false;
        }
        sources
    }

    pub fn validate(&self) -> Result<()> {
        self.pip.validate()?;
        ensure!(
            self.countdown_seconds <= 10,
            "Countdown must be between 0 and 10 seconds."
        );
        ensure!(
            (1..=120).contains(&self.limit_minutes),
            "Recording limit must be 1-120 minutes."
        );
        for id in [&self.camera_id, &self.microphone_id, &self.output_id]
            .into_iter()
            .flatten()
        {
            ensure!(
                !id.is_empty() && id.len() <= 4096,
                "Saved device identifier is invalid."
            );
        }
        if let Some(path) = &self.output_directory {
            ensure!(
                path.is_absolute(),
                "The output directory must be an absolute local path."
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shortcut {
    pub control: bool,
    pub shift: bool,
    pub alt: bool,
    pub key: u32,
}

impl Shortcut {
    pub fn parse(text: &str) -> Result<Self> {
        let mut result = Self {
            control: false,
            shift: false,
            alt: false,
            key: 0,
        };
        for part in text.split('+').map(|s| s.trim().to_ascii_uppercase()) {
            match part.as_str() {
                "CTRL" if !result.control => result.control = true,
                "SHIFT" if !result.shift => result.shift = true,
                "ALT" if !result.alt => result.alt = true,
                key if result.key == 0 => {
                    result.key = if key == "PRINTSCREEN" {
                        0x2c
                    } else if let Some(f) =
                        key.strip_prefix('F').and_then(|s| s.parse::<u32>().ok())
                    {
                        ensure!((1..=24).contains(&f), "Function keys are F1-F24.");
                        0x70 + f - 1
                    } else if key.len() == 1 && key.as_bytes()[0].is_ascii_alphanumeric() {
                        u32::from(key.as_bytes()[0])
                    } else {
                        anyhow::bail!(
                            "Use Ctrl/Shift/Alt plus a letter, digit, F1-F24 or PrintScreen."
                        );
                    };
                }
                _ => anyhow::bail!("A shortcut needs exactly one key and no duplicate modifiers."),
            }
        }
        ensure!(result.key != 0 && (result.control || result.alt || result.shift),
            "Include at least one Ctrl, Alt or Shift modifier; unmodified keys stay with the active app.");
        Ok(result)
    }

    pub fn label(&self) -> String {
        let mut parts = Vec::new();
        if self.control {
            parts.push("Ctrl".into());
        }
        if self.shift {
            parts.push("Shift".into());
        }
        if self.alt {
            parts.push("Alt".into());
        }
        parts.push(match self.key {
            0x70..=0x87 => format!("F{}", self.key - 0x70 + 1),
            0x2c => "PrintScreen".into(),
            key => char::from_u32(key).unwrap_or('?').to_string(),
        });
        parts.join("+")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Shortcuts {
    pub capture: String,
    pub repeat: String,
    pub stop: String,
}

impl Default for Shortcuts {
    fn default() -> Self {
        Self {
            capture: "Ctrl+Shift+F8".into(),
            repeat: "Ctrl+Shift+F7".into(),
            stop: "Ctrl+Shift+F9".into(),
        }
    }
}

impl Shortcuts {
    pub fn parsed(&self) -> Result<[Shortcut; 3]> {
        let keys = [
            Shortcut::parse(&self.capture)?,
            Shortcut::parse(&self.repeat)?,
            Shortcut::parse(&self.stop)?,
        ];
        ensure!(
            keys[0] != keys[1] && keys[0] != keys[2] && keys[1] != keys[2],
            "Capture, Repeat and Stop must use different shortcuts."
        );
        Ok(keys)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RememberedRegion {
    pub monitor: Monitor,
    pub area: Area,
}

impl RememberedRegion {
    pub fn revalidate(&self, monitors: &[Monitor]) -> Result<Area> {
        let current = monitors
            .iter()
            .find(|m| m.id == self.monitor.id)
            .context("The previous monitor is disconnected. Select a region again.")?;
        ensure!(
            current == &self.monitor,
            "Display position, physical size or DPI changed. Select the region again."
        );
        validate_area(&self.area, monitors)?;
        Ok(self.area.clone())
    }
}

pub fn validate_area(area: &Area, monitors: &[Monitor]) -> Result<()> {
    crate::model::validate_size(area.width, area.height)?;
    let monitor = monitors
        .iter()
        .find(|m| m.id == area.monitor)
        .context("The selected monitor is no longer connected.")?;
    ensure!(
        area.x
            .checked_add(area.width)
            .is_some_and(|x| x <= monitor.width)
            && area
                .y
                .checked_add(area.height)
                .is_some_and(|y| y <= monitor.height),
        "The region must fit inside one monitor in physical pixels."
    );
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub options: CaptureOptions,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub schema: u32,
    pub capture: CaptureOptions,
    pub shortcuts: Shortcuts,
    pub last_region: Option<RememberedRegion>,
    pub presets: Vec<Preset>,
    pub show_recent: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            schema: 1,
            capture: CaptureOptions::default(),
            shortcuts: Shortcuts::default(),
            last_region: None,
            presets: vec![],
            show_recent: true,
        }
    }
}

impl Preferences {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.schema == 1, "Unsupported capture preferences version.");
        self.capture.validate()?;
        self.shortcuts.parsed()?;
        ensure!(
            self.presets.len() <= 20,
            "At most 20 capture presets can be saved."
        );
        let mut names = std::collections::HashSet::new();
        for preset in &self.presets {
            ensure!(
                !preset.name.trim().is_empty() && preset.name.len() <= 80,
                "Preset names must contain 1-80 characters."
            );
            ensure!(
                names.insert(preset.name.to_lowercase()),
                "Preset names must be unique."
            );
            preset.options.validate()?;
        }
        Ok(())
    }

    pub fn load(library: &Library) -> Result<Self> {
        let path = library.root.join("preferences.json");
        if !path.exists() {
            return Ok(Self::default());
        }
        let prefs: Self = library::read_json(&path)?;
        prefs.validate()?;
        Ok(prefs)
    }

    pub fn save(&self, library: &Library) -> Result<()> {
        self.validate()?;
        library::write_json(&library.root.join("preferences.json"), self)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl PixelRect {
    pub fn area(self, monitor: &str) -> Area {
        Area {
            monitor: monitor.into(),
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
        }
    }

    pub fn right(self) -> i32 {
        (self.x + self.width) as i32
    }
    pub fn bottom(self) -> i32 {
        (self.y + self.height) as i32
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragKind {
    New,
    Move,
    Resize {
        left: bool,
        top: bool,
        right: bool,
        bottom: bool,
    },
}

#[derive(Clone, Copy)]
struct Drag {
    kind: DragKind,
    x: i32,
    y: i32,
    original: PixelRect,
}

pub struct Selection {
    pub rect: Option<PixelRect>,
    width: u32,
    height: u32,
    radius: i32,
    drag: Option<Drag>,
    cancelled: bool,
}

impl Selection {
    pub fn new(monitor: &Monitor, initial: Option<PixelRect>) -> Self {
        Self {
            rect: initial.filter(|r| {
                r.width > 0
                    && r.height > 0
                    && r.x.checked_add(r.width).is_some_and(|v| v <= monitor.width)
                    && r.y
                        .checked_add(r.height)
                        .is_some_and(|v| v <= monitor.height)
            }),
            width: monitor.width,
            height: monitor.height,
            radius: (8 * monitor.dpi / 96).max(6) as i32,
            drag: None,
            cancelled: false,
        }
    }

    pub fn hit(&self, x: i32, y: i32) -> DragKind {
        let Some(r) = self.rect else {
            return DragKind::New;
        };
        let in_x = x >= r.x as i32 - self.radius && x <= r.right() + self.radius;
        let in_y = y >= r.y as i32 - self.radius && y <= r.bottom() + self.radius;
        if !in_x || !in_y {
            return DragKind::New;
        }
        let left = (x - r.x as i32).abs() <= self.radius;
        let right = (x - r.right()).abs() <= self.radius;
        let top = (y - r.y as i32).abs() <= self.radius;
        let bottom = (y - r.bottom()).abs() <= self.radius;
        if left || right || top || bottom {
            DragKind::Resize {
                left,
                top,
                right,
                bottom,
            }
        } else {
            DragKind::Move
        }
    }

    pub fn down(&mut self, x: i32, y: i32) {
        if self.cancelled {
            return;
        }
        let x = x.clamp(0, self.width as i32);
        let y = y.clamp(0, self.height as i32);
        self.drag = Some(Drag {
            kind: self.hit(x, y),
            x,
            y,
            original: self.rect.unwrap_or_default(),
        });
    }

    pub fn motion(&mut self, x: i32, y: i32) {
        if self.cancelled {
            return;
        }
        let Some(drag) = self.drag else { return };
        let x = x.clamp(0, self.width as i32);
        let y = y.clamp(0, self.height as i32);
        let r = drag.original;
        let (mut left, mut top, mut right, mut bottom) =
            (r.x as i32, r.y as i32, r.right(), r.bottom());
        match drag.kind {
            DragKind::New => {
                left = drag.x.min(x);
                right = drag.x.max(x);
                top = drag.y.min(y);
                bottom = drag.y.max(y);
            }
            DragKind::Move => {
                left = (left + x - drag.x).clamp(0, (self.width - r.width) as i32);
                top = (top + y - drag.y).clamp(0, (self.height - r.height) as i32);
                right = left + r.width as i32;
                bottom = top + r.height as i32;
            }
            DragKind::Resize {
                left: l,
                top: t,
                right: r,
                bottom: b,
            } => {
                if l {
                    left = x.min(right - 1).max(0);
                }
                if t {
                    top = y.min(bottom - 1).max(0);
                }
                if r {
                    right = x.max(left + 1).min(self.width as i32);
                }
                if b {
                    bottom = y.max(top + 1).min(self.height as i32);
                }
            }
        }
        if right > left && bottom > top {
            self.rect = Some(PixelRect {
                x: left as u32,
                y: top as u32,
                width: (right - left) as u32,
                height: (bottom - top) as u32,
            });
        }
    }

    pub fn up(&mut self, x: i32, y: i32) {
        self.motion(x, y);
        self.drag = None;
    }

    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub fn capture_lost(&mut self) -> bool {
        if !self.dragging() {
            return false;
        }
        self.cancel();
        true
    }

    pub fn captured_motion(
        &mut self,
        x: i32,
        y: i32,
        button_down: bool,
        owns_capture: bool,
    ) -> bool {
        if self.dragging() && (!button_down || !owns_capture) {
            self.cancel();
            return false;
        }
        self.motion(x, y);
        !self.cancelled
    }

    pub fn nudge(&mut self, dx: i32, dy: i32, resize: bool) {
        if self.cancelled || self.drag.is_some() {
            return;
        }
        if let Some(mut r) = self.rect {
            if resize {
                r.width = (r.width as i32 + dx).clamp(1, (self.width - r.x) as i32) as u32;
                r.height = (r.height as i32 + dy).clamp(1, (self.height - r.y) as i32) as u32;
            } else {
                r.x = (r.x as i32 + dx).clamp(0, (self.width - r.width) as i32) as u32;
                r.y = (r.y as i32 + dy).clamp(0, (self.height - r.height) as i32) as u32;
            }
            self.rect = Some(r);
        }
    }

    pub fn accepted(&self) -> Option<PixelRect> {
        if self.cancelled || self.drag.is_some() {
            None
        } else {
            self.rect
        }
    }

    pub fn cancel(&mut self) {
        self.cancelled = true;
        self.drag = None;
        self.rect = None;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlowToken(u64);

pub struct CaptureAdmission {
    token: FlowToken,
    state: AtomicU8,
    stop: Arc<AtomicBool>,
}

impl CaptureAdmission {
    pub fn new(token: FlowToken) -> Self {
        Self {
            token,
            state: AtomicU8::new(0),
            stop: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn cancelled(&self) -> bool {
        self.state.load(Ordering::Acquire) == 1
    }
    pub fn cancel(&self) {
        // This CAS and admit's CAS are the shared linearization boundary. If admission
        // already won, this signal becomes Stop for that capture's unique worker gate.
        let _ = self
            .state
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
        self.stop.store(true, Ordering::Release);
    }
    pub fn admit<T>(
        &self,
        token: FlowToken,
        start: impl FnOnce(Arc<AtomicBool>) -> T,
    ) -> Option<T> {
        if self.token != token
            || self
                .state
                .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return None;
        }
        Some(start(self.stop.clone()))
    }
}

#[derive(Clone, Debug)]
pub enum FlowPhase {
    Idle,
    Selecting,
    Review(Area),
    Countdown { area: Area, deadline: Instant },
}

pub struct CaptureFlow {
    generation: u64,
    pub phase: FlowPhase,
}

impl Default for CaptureFlow {
    fn default() -> Self {
        Self {
            generation: 0,
            phase: FlowPhase::Idle,
        }
    }
}

impl CaptureFlow {
    pub fn begin(&mut self) -> Result<FlowToken> {
        self.generation = self
            .generation
            .checked_add(1)
            .context("Capture token limit reached. Restart Snapper.")?;
        self.phase = FlowPhase::Selecting;
        Ok(FlowToken(self.generation))
    }
    pub fn token(&self) -> Option<FlowToken> {
        self.active().then_some(FlowToken(self.generation))
    }
    pub fn active(&self) -> bool {
        !matches!(self.phase, FlowPhase::Idle)
    }
    pub fn selected(&mut self, token: FlowToken, area: Area) -> bool {
        if self.token() != Some(token) || !matches!(self.phase, FlowPhase::Selecting) {
            return false;
        }
        self.phase = FlowPhase::Review(area);
        true
    }
    pub fn arm(&mut self, token: FlowToken, deadline: Instant) -> bool {
        if self.token() != Some(token) {
            return false;
        }
        let FlowPhase::Review(area) = &self.phase else {
            return false;
        };
        self.phase = FlowPhase::Countdown {
            area: area.clone(),
            deadline,
        };
        true
    }
    pub fn take_start(&mut self, token: FlowToken, now: Instant) -> Option<Area> {
        if self.token() != Some(token) {
            return None;
        }
        let FlowPhase::Countdown { area, deadline } = &self.phase else {
            return None;
        };
        if now < *deadline {
            return None;
        }
        let area = area.clone();
        self.phase = FlowPhase::Idle;
        Some(area)
    }
    pub fn cancel(&mut self) -> bool {
        let active = self.active();
        self.phase = FlowPhase::Idle;
        active
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn monitor() -> Monitor {
        Monitor {
            id: "test".into(),
            left: -3840,
            top: -200,
            width: 3840,
            height: 2160,
            primary: true,
            dpi: 144,
        }
    }

    #[test]
    fn selection_uses_physical_pixels_at_150_percent_and_negative_desktop_origin() {
        let m = monitor();
        let mut selection = Selection::new(&m, None);
        selection.down(600, 300);
        selection.motion(1800, 1200);
        assert!(selection.accepted().is_none());
        selection.up(1800, 1200);
        assert_eq!(
            selection.accepted(),
            Some(PixelRect {
                x: 600,
                y: 300,
                width: 1200,
                height: 900
            })
        );
        validate_area(&selection.accepted().unwrap().area(&m.id), &[m]).unwrap();
    }

    #[test]
    fn escape_before_during_and_after_drag_releases_gesture_and_rejects_late_mouseup() {
        for stage in 0..4 {
            let mut selection = Selection::new(&monitor(), None);
            if stage >= 1 {
                selection.down(10, 20);
                selection.motion(500, 600);
            }

            if stage >= 2 {
                selection.up(500, 600);
            }
            if stage >= 3 {
                selection.down(250, 300);
                selection.motion(350, 400);
            }
            selection.cancel();
            selection.up(700, 800);
            selection.down(40, 40);
            selection.motion(900, 900);
            assert!(!selection.dragging());
            assert!(selection.accepted().is_none());
            assert!(selection.rect.is_none());
        }
    }

    #[test]
    fn unexpected_native_capture_loss_quarantines_all_late_pointer_events() {
        let mut selection = Selection::new(&monitor(), None);
        selection.down(100, 100);
        assert!(selection.captured_motion(400, 400, true, true));
        assert!(selection.capture_lost());
        selection.up(900, 900);
        assert!(!selection.captured_motion(1000, 1000, false, false));
        assert!(selection.accepted().is_none());
        assert!(!selection.dragging());
        let mut fresh = Selection::new(&monitor(), None);
        fresh.down(100, 100);
        fresh.up(500, 500);
        assert!(
            !fresh.capture_lost(),
            "Normal ReleaseCapture after mouse-up must keep the selection."
        );
        assert_eq!(fresh.accepted().unwrap().width, 400);
        fresh.down(300, 300);
        assert!(!fresh.captured_motion(400, 400, false, true));
        assert!(fresh.accepted().is_none());
    }

    #[test]
    fn admission_wins_then_escape_is_stop_not_a_new_capture_or_clipboard_action() {
        let mut flow = CaptureFlow::default();
        let token = flow.begin().unwrap();
        let gate = CaptureAdmission::new(token);
        let stop = gate.admit(token, |stop| stop).unwrap();
        gate.cancel();
        assert!(stop.load(Ordering::Acquire));
        assert!(
            !gate.cancelled(),
            "Admission already won; the signal is now Stop."
        );
        assert!(gate.admit(token, |_| ()).is_none());
        let next = CaptureAdmission::new(flow.begin().unwrap());
        gate.cancel();
        assert!(
            !next.cancelled(),
            "An old handler must not cancel a reopened selection."
        );
    }

    #[test]
    fn resize_move_reverse_drag_and_keyboard_stay_in_monitor() {
        let mut selection = Selection::new(&monitor(), None);
        selection.down(1800, 1200);
        selection.up(600, 300);
        selection.down(1800, 1200);
        selection.up(1900, 1300);
        assert_eq!(selection.rect.unwrap().width, 1300);
        selection.down(1000, 600);
        selection.up(-100, -100);
        assert_eq!(selection.rect.unwrap().x, 0);
        selection.nudge(100_000, 100_000, false);
        let r = selection.rect.unwrap();
        assert_eq!((r.right(), r.bottom()), (3840, 2160));
        selection.nudge(-100_000, -100_000, true);
        assert_eq!(
            (
                selection.rect.unwrap().width,
                selection.rect.unwrap().height
            ),
            (1, 1)
        );
    }

    #[test]
    fn cancellation_invalidates_selection_review_and_countdown_callbacks() {
        for stage in 0..3 {
            let mut flow = CaptureFlow::default();
            let token = flow.begin().unwrap();
            if stage >= 1 {
                assert!(flow.selected(token, monitor().area()));
            }
            if stage >= 2 {
                assert!(flow.arm(token, Instant::now()));
            }
            assert!(flow.cancel());
            assert!(!flow.selected(token, monitor().area()));
            assert!(!flow.arm(token, Instant::now()));
            assert!(flow
                .take_start(token, Instant::now() + Duration::from_secs(30))
                .is_none());
        }
    }

    #[test]
    fn rapid_cancel_reopen_never_accepts_old_mouseup_or_timer() {
        let mut flow = CaptureFlow::default();
        let old = flow.begin().unwrap();
        flow.selected(old, monitor().area());
        flow.arm(old, Instant::now());
        flow.cancel();
        let new = flow.begin().unwrap();
        assert_ne!(old, new);
        assert!(!flow.selected(old, monitor().area()));
        assert!(flow.take_start(old, Instant::now()).is_none());
        assert!(flow.selected(new, monitor().area()));
        let deadline = Instant::now() + Duration::from_secs(3);
        flow.arm(new, deadline);
        assert!(flow
            .take_start(new, deadline - Duration::from_millis(1))
            .is_none());
        assert!(flow.take_start(new, deadline).is_some());
        assert!(flow.take_start(new, deadline).is_none());
    }

    #[test]
    fn last_region_rejects_dpi_size_position_or_connection_changes() {
        let m = monitor();
        let saved = RememberedRegion {
            monitor: m.clone(),
            area: m.area(),
        };
        assert!(saved.revalidate(std::slice::from_ref(&m)).is_ok());
        for changed in [
            Monitor {
                dpi: 96,
                ..m.clone()
            },
            Monitor {
                left: 0,
                ..m.clone()
            },
            Monitor {
                width: 2560,
                ..m.clone()
            },
            Monitor {
                id: "other".into(),
                ..m
            },
        ] {
            assert!(saved.revalidate(&[changed]).is_err());
        }
        assert!(saved.revalidate(&[]).is_err());
    }

    #[test]
    fn saved_source_choices_are_data_only_and_images_never_open_audio() {
        let temp = tempfile::tempdir().unwrap();
        let library = Library::open(temp.path().into()).unwrap();
        let mut prefs = Preferences::default();
        prefs.capture.sources = Sources {
            cursor: true,
            webcam: true,
            system_audio: true,
            microphone: true,
        };
        prefs.capture.camera_id = Some("chosen-camera".into());
        prefs.capture.microphone_id = Some("chosen-mic".into());
        prefs.capture.output_id = Some("chosen-output".into());
        prefs.show_recent = false;
        prefs.presets.push(Preset {
            name: "Narration".into(),
            options: prefs.capture.clone(),
        });
        prefs.save(&library).unwrap();
        let loaded = Preferences::load(&library).unwrap();
        assert_eq!(loaded.capture, prefs.capture);
        assert_eq!(loaded.presets.len(), 1);
        assert!(!loaded.show_recent);
        assert!(!loaded.capture.effective_sources().microphone);
        assert!(!loaded.capture.effective_sources().system_audio);
        assert_eq!(
            std::fs::read_dir(library.root.join("captures"))
                .unwrap()
                .count(),
            0
        );
        assert_eq!(
            std::fs::read_dir(library.root.join("sessions"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn malformed_preferences_are_reported_not_overwritten() {
        let temp = tempfile::tempdir().unwrap();
        let library = Library::open(temp.path().into()).unwrap();
        std::fs::write(library.root.join("preferences.json"), b"{broken").unwrap();
        assert!(Preferences::load(&library).is_err());
        assert_eq!(
            std::fs::read(library.root.join("preferences.json")).unwrap(),
            b"{broken"
        );
    }

    #[test]
    fn shortcuts_reject_ambiguity_duplicates_and_bare_escape() {
        assert_eq!(
            Shortcut::parse("shift+ctrl+f8").unwrap().label(),
            "Ctrl+Shift+F8"
        );
        for value in [
            "Escape",
            "F8",
            "Ctrl",
            "Ctrl++A",
            "Ctrl+Ctrl+A",
            "Ctrl+F25",
            "Ctrl+A+B",
        ] {
            assert!(Shortcut::parse(value).is_err(), "{value}");
        }
        let mut keys = Shortcuts::default();
        keys.repeat = keys.capture.clone();
        assert!(keys.parsed().is_err());
    }

    #[test]
    fn pip_placement_and_size_are_bounded_for_wide_and_tall_cameras() {
        for camera in [(1280, 720), (120, 720)] {
            for position in [(0, 0), (1000, 0), (0, 1000), (1000, 1000)] {
                let layout = PipLayout {
                    x: position.0,
                    y: position.1,
                    width: 500,
                };
                layout.validate().unwrap();
                let r = layout.rect(320, 180, camera.0, camera.1);
                assert!(r.width > 0 && r.height > 0 && r.right() <= 320 && r.bottom() <= 180);
            }
        }
    }
}
