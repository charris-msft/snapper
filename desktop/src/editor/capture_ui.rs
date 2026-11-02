use super::*;
use crate::{
    capture::{
        CaptureFlow, CaptureOptions, Destination, FlowPhase, FlowToken, PixelRect, Preferences,
        Preset, RememberedRegion, Shortcuts, TargetKind,
    },
    media::{preview::CameraPreview, Request},
    platform::{
        hotkey::{GlobalShortcuts, GlobalStop},
        selection::{self, Foreground, Overlay},
        Area, Monitor,
    },
};
use std::sync::atomic::Ordering;

pub(super) struct CaptureUi {
    pub(super) preferences: Preferences,
    pub(super) flow: CaptureFlow,
    pub(super) launcher: bool,
    pub(super) startup_error: Option<String>,
    settings_error: Option<String>,
    settings: bool,
    hotkeys: Option<GlobalShortcuts>,
    escape: Option<GlobalStop>,
    overlay: Option<Overlay>,
    previous_focus: Option<Foreground>,
    monitors: Vec<Monitor>,
    area: Area,
    flow_monitor: Option<Monitor>,
    cameras: Vec<CameraInfo>,
    audio: Vec<AudioDevice>,
    windows: Vec<WindowGuide>,
    window_index: usize,
    preview: Option<CameraPreview>,
    preview_texture: Option<TextureHandle>,
    preview_size: (u32, u32),
    pending_arm: Option<FlowToken>,
    active_options: Option<CaptureOptions>,
    pending_delivery: Option<(String, CaptureOptions)>,
    shortcut_draft: Shortcuts,
    preset_name: String,
}

impl CaptureUi {
    pub(super) fn new(ctx: &egui::Context, library: &Library, window: isize) -> Result<Self> {
        let (preferences, settings_error) = match Preferences::load(library) {
            Ok(value) => (value, None),
            Err(error) => (Preferences::default(), Some(format!(
                "Capture preferences could not be read: {error:#}. Defaults are shown without overwriting the saved file. Restore preferences.json before saving settings."))),
        };
        let monitors = platform::screen::monitors()?;
        let area = monitors.first().context("No monitor is available")?.area();
        let context = ctx.clone();
        let (hotkeys, shortcut_error) =
            match GlobalShortcuts::new(&preferences.shortcuts, move || {
                if let Err(error) = platform::process::restore_owned_window(window) {
                    eprintln!("Shortcut window restore: {error:#}");
                }
                context.request_repaint();
            }) {
                Ok(keys) => (Some(keys), None),
                Err(error) => (
                    None,
                    Some(format!("Global shortcuts are inactive: {error:#}")),
                ),
            };
        let startup_error = match (&settings_error, shortcut_error) {
            (Some(a), Some(b)) => Some(format!("{a}\n{b}")),
            (Some(a), None) => Some(a.clone()),
            (None, b) => b,
        };
        Ok(Self {
            shortcut_draft: preferences.shortcuts.clone(),
            preferences,
            settings_error,
            startup_error,
            hotkeys,
            flow: CaptureFlow::default(),
            launcher: true,
            settings: false,
            escape: None,
            overlay: None,
            previous_focus: None,
            monitors,
            area,
            flow_monitor: None,
            cameras: vec![],
            audio: vec![],
            windows: vec![],
            window_index: 0,
            preview: None,
            preview_texture: None,
            preview_size: (640, 360),
            pending_arm: None,
            active_options: None,
            pending_delivery: None,
            preset_name: String::new(),
        })
    }

    pub(super) fn receive_devices(&mut self, cameras: Vec<CameraInfo>, audio: Vec<AudioDevice>) {
        for system in [true, false] {
            let selected = if system {
                &mut self.preferences.capture.output_id
            } else {
                &mut self.preferences.capture.microphone_id
            };
            if selected.is_none() {
                *selected = audio
                    .iter()
                    .find(|device| device.system == system && device.default)
                    .map(|device| device.id.clone());
            }
        }
        self.cameras = cameras;
        self.audio = audio;
    }

    pub(super) fn receive_windows(&mut self, windows: Vec<WindowGuide>) {
        self.windows = windows;
        self.window_index = 0;
    }
}

impl Editor {
    fn capture_notice(&mut self, result: Result<()>) -> bool {
        match result {
            Ok(()) => true,
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                false
            }
        }
    }

    fn center_capture_window(&mut self, width: u32, height: u32) {
        if let Some(m) = self
            .capture_ui
            .monitors
            .iter()
            .find(|m| m.id == self.capture_ui.area.monitor)
        {
            let width = width * m.dpi / 96;
            let height = height * m.dpi / 96;
            let x = m.left + m.width.saturating_sub(width) as i32 / 2;
            let y = m.top + m.height.saturating_sub(height) as i32 / 2;
            self.capture_notice(selection::position_owned(self.window_handle, x, y));
        }
    }

    pub(super) fn show_launcher(&mut self, ctx: &egui::Context) {
        self.capture_ui.launcher = true;
        self.capture_ui.settings = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
            egui::WindowLevel::Normal,
        ));
        ctx.send_viewport_cmd(egui::ViewportCommand::Resizable(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(Vec2::new(480.0, 440.0)));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(Vec2::new(560.0, 680.0)));
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        self.capture_notice(selection::show_owned(self.window_handle, false));
        self.hide_for_capture = false;
        self.center_capture_window(560, 680);
    }

    pub(super) fn show_editor(&mut self, ctx: &egui::Context) {
        self.capture_ui.launcher = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
            egui::WindowLevel::Normal,
        ));
        ctx.send_viewport_cmd(egui::ViewportCommand::Resizable(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(Vec2::new(980.0, 640.0)));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(Vec2::new(1280.0, 850.0)));
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        self.center_capture_window(1280, 850);
    }

    pub(super) fn save_preferences(&mut self) -> bool {
        if let Some(error) = &self.capture_ui.settings_error {
            self.error = Some(error.clone());
            return false;
        }
        self.capture_ui.preferences.show_recent = self.show_recent;
        self.capture_notice(self.capture_ui.preferences.save(&self.library))
    }

    pub(super) fn begin_capture(&mut self, ctx: &egui::Context, repeat: bool) {
        if self.renderer_fixture || self.recorder.is_some() || self.unsaved() || self.busy() {
            self.error = Some(
                "Finish the current save or recording before selecting another capture.".into(),
            );
            return;
        }
        if self.capture_ui.preview.is_some() {
            self.error =
                Some("Stop the camera preview before adjusting the capture region.".into());
            return;
        }
        let result = (|| -> Result<()> {
            let monitors = platform::screen::monitors()?;
            let monitor = monitors
                .iter()
                .find(|m| m.id == self.capture_ui.area.monitor)
                .context("The chosen monitor disconnected. Choose a connected monitor.")?
                .clone();
            let repeated = if repeat {
                Some(
                    self.capture_ui
                        .preferences
                        .last_region
                        .as_ref()
                        .context("There is no previous capture region yet.")?
                        .revalidate(&monitors)?,
                )
            } else {
                None
            };
            if !self.capture_ui.flow.active() {
                self.capture_ui.previous_focus = Some(Foreground::remember());
                let context = ctx.clone();
                let window = self.window_handle;
                self.capture_ui.escape = Some(GlobalStop::escape(move || {
                    if let Err(error) = platform::process::restore_owned_window(window) {
                        eprintln!("Cancellation window restore: {error:#}");
                    }
                    context.request_repaint();
                })?);
            }
            self.capture_ui.monitors = monitors;
            self.capture_ui.flow_monitor = Some(monitor.clone());
            let token = self.capture_ui.flow.begin()?;
            let target = self.capture_ui.preferences.capture.target;
            if let Some(area) = repeated {
                self.capture_ui.area = area.clone();
                self.capture_ui.flow_monitor = self
                    .capture_ui
                    .monitors
                    .iter()
                    .find(|m| m.id == area.monitor)
                    .cloned();
                self.capture_ui.flow.selected(token, area);
                self.show_launcher(ctx);
                selection::show_owned(self.window_handle, true)?;
            } else if target == TargetKind::Monitor {
                self.capture_ui.area = monitor.area();
                self.capture_ui.flow.selected(token, monitor.area());
                self.show_launcher(ctx);
                selection::show_owned(self.window_handle, true)?;
            } else {
                let initial = if target == TargetKind::WindowGuide {
                    let area = self
                        .capture_ui
                        .windows
                        .get(self.capture_ui.window_index)
                        .context("Find windows and select a window first.")?
                        .area(&monitor)?;
                    Some(PixelRect {
                        x: area.x,
                        y: area.y,
                        width: area.width,
                        height: area.height,
                    })
                } else {
                    None
                };
                selection::hide_owned(self.window_handle)?;
                let context = ctx.clone();
                self.capture_ui.overlay = Some(Overlay::start(
                    token,
                    self.window_handle,
                    monitor,
                    initial,
                    move || context.request_repaint(),
                )?);
            }
            self.status = "Select the pixels to capture. Esc cancels the whole operation.".into();
            Ok(())
        })();
        if let Err(error) = result {
            self.cancel_capture_flow(ctx);
            self.error = Some(format!("{error:#}"));
        }
    }

    pub(super) fn cancel_capture_flow(&mut self, ctx: &egui::Context) {
        // Invalidate the ticket before disposing native resources or processing any queued event.
        self.capture_ui.flow.cancel();
        self.capture_ui.pending_arm = None;
        self.capture_ui.overlay = None;
        self.capture_ui.escape = None;
        if let Some(preview) = &self.capture_ui.preview {
            preview.stop();
        }
        self.capture_ui.preview_texture = None;
        self.show_launcher(ctx);
        self.status =
            "Capture cancelled. No image saved, clipboard changed or recording started.".into();
        if self.capture_ui.preview.is_none() {
            if let Some(focus) = self.capture_ui.previous_focus.take() {
                self.capture_notice(focus.restore());
            }
        } else {
            self.status =
                "Capture cancelled. Releasing the explicitly opened camera preview.".into();
        }
    }

    fn validate_preflight(&self) -> Result<()> {
        let options = &self.capture_ui.preferences.capture;
        options.validate()?;
        let monitors = platform::screen::monitors()?;
        let original = self
            .capture_ui
            .flow_monitor
            .as_ref()
            .context("Choose a capture target first.")?;
        RememberedRegion {
            monitor: original.clone(),
            area: self.capture_ui.area.clone(),
        }
        .revalidate(&monitors)?;
        let sources = options.effective_sources();
        if sources.webcam {
            ensure!(
                self.capture_ui
                    .cameras
                    .iter()
                    .any(|camera| Some(&camera.id) == options.camera_id.as_ref()),
                "Choose an available camera or turn Webcam off."
            );
        }
        for (enabled, system, selected) in [
            (sources.system_audio, true, &options.output_id),
            (sources.microphone, false, &options.microphone_id),
        ] {
            ensure!(
                !enabled
                    || self
                        .capture_ui
                        .audio
                        .iter()
                        .any(|device| device.system == system
                            && Some(&device.id) == selected.as_ref()),
                "Choose an available {} device or turn that source off.",
                if system {
                    "system-output"
                } else {
                    "microphone"
                }
            );
        }
        ensure!(!(options.video && options.destination == Destination::Clipboard), "Video can open in the library or save to a file; image clipboard routing is for screenshots.");
        if options.destination == Destination::File {
            ensure!(
                options
                    .output_directory
                    .as_ref()
                    .is_some_and(|path| path.is_dir()),
                "Choose an existing local output folder before capturing."
            );
        }
        if options.video {
            selection::exclude_from_capture(self.window_handle)?;
        }
        Ok(())
    }

    fn arm_capture(&mut self, ctx: &egui::Context) {
        if self.renderer_fixture {
            return;
        }
        if !self.capture_notice(self.validate_preflight()) {
            return;
        }
        let Some(token) = self.capture_ui.flow.token() else {
            return;
        };
        if let Some(preview) = &self.capture_ui.preview {
            preview.stop();
            self.capture_ui.pending_arm = Some(token);
            self.status = "Releasing camera preview before the countdown.".into();
            return;
        }
        let deadline = Instant::now()
            + Duration::from_secs(u64::from(
                self.capture_ui.preferences.capture.countdown_seconds,
            ));
        self.capture_ui.flow.arm(token, deadline);
        self.status = "Ready. Esc cancels before any capture source starts.".into();
        ctx.request_repaint();
    }

    fn start_selected(&mut self, ctx: &egui::Context, area: Area) {
        let options = self.capture_ui.preferences.capture.clone();
        let request = Request {
            schema: 1,
            session_dir: self.library.root.join("sessions").join(library::id()),
            library_root: self.library.root.clone(),
            area: area.clone(),
            sources: options.effective_sources(),
            camera_id: options.camera_id.clone(),
            microphone_id: options.microphone_id.clone(),
            output_id: options.output_id.clone(),
            pip: Some(options.pip),
            video: options.video,
            max_seconds: u64::from(options.limit_minutes) * 60,
            synthetic: false,
            test_fault: None,
        };
        if !self.capture_notice(request.validate()) {
            return;
        }
        self.capture_ui.escape = None;
        self.capture_ui.overlay = None;
        self.capture_ui.previous_focus = None;
        if let Some(monitor) = self.capture_ui.flow_monitor.clone() {
            self.capture_ui.preferences.last_region = Some(RememberedRegion { monitor, area });
        }
        if !self.save_preferences() {
            self.show_launcher(ctx);
            return;
        }
        if options.video {
            ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Resizable(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(Vec2::new(380.0, 100.0)));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(Vec2::new(400.0, 118.0)));
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
                egui::WindowLevel::AlwaysOnTop,
            ));
        } else {
            if !self.capture_notice(selection::hide_owned(self.window_handle)) {
                return;
            }
            self.hide_for_capture = true;
        }
        self.capture_ui.active_options = Some(options.clone());
        self.recorder = Some(if let Some(keys) = &self.capture_ui.hotkeys {
            keys.stop.flag.store(false, Ordering::Release);
            Recorder::start_with_stop(request, keys.stop.flag.clone())
        } else {
            Recorder::start(request)
        });
        if !options.video {
            let completed = self.recorder.as_ref().unwrap().completion_signal();
            let window = self.window_handle;
            let context = ctx.clone();
            thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(30);
                while !completed.load(Ordering::Acquire) && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(25));
                }
                if let Err(error) = platform::process::restore_owned_window(window) {
                    eprintln!("Screenshot completion window restore: {error:#}");
                }
                context.request_repaint();
            });
        }
        self.last_progress = Progress::default();
    }

    pub(super) fn capture_finished(&mut self, ctx: &egui::Context, progress: &Progress) {
        if let Some(keys) = &self.capture_ui.hotkeys {
            keys.stop.flag.store(false, Ordering::Release);
        }
        if let (Some(id), Some(options)) =
            (&progress.capture_id, self.capture_ui.active_options.take())
        {
            self.capture_ui.pending_delivery = Some((id.clone(), options));
        }
        self.show_launcher(ctx);
    }

    pub(super) fn poll_delivery(&mut self, ctx: &egui::Context) {
        if self.busy() || self.unsaved() {
            return;
        }
        let Some((id, options)) = &self.capture_ui.pending_delivery else {
            return;
        };
        let Some(capture) = self
            .captures
            .iter()
            .find(|capture| &capture.id == id)
            .cloned()
        else {
            return;
        };
        let options = options.clone();
        self.capture_ui.pending_delivery = None;
        if options.destination == Destination::Editor {
            self.open_capture(ctx, capture);
            return;
        }
        let library = self.library.clone();
        self.task(ctx, move || {
            let folder = library.capture_dir(&capture)?;
            if options.destination == Destination::Clipboard {
                let image = library::load_image(&folder.join("original.png"))?;
                arboard::Clipboard::new()?
                    .set_image(arboard::ImageData {
                        width: image.width() as usize,
                        height: image.height() as usize,
                        bytes: Cow::Borrowed(image.as_raw()),
                    })
                    .context("Image is saved locally, but copying it to the clipboard failed")?;
                Ok(TaskResult::Notice(
                    "Screenshot saved locally and copied to the clipboard.".into(),
                ))
            } else {
                let (file, extension) = if capture.kind == CaptureKind::Image {
                    ("original.png", "png")
                } else {
                    ("recording.mp4", "mp4")
                };
                let destination = options
                    .output_directory
                    .context("Saved output folder is missing")?
                    .join(format!("snapper-{}.{}", capture.id, extension));
                copy_new(&folder.join(file), &destination)?;
                Ok(TaskResult::Notice(format!(
                    "Saved to {}. The library original is retained.",
                    destination.display()
                )))
            }
        });
    }

    pub(super) fn poll_capture_flow(&mut self, ctx: &egui::Context) {
        if self.renderer_fixture {
            return;
        }
        let mut capture = false;
        let mut repeat = false;
        let mut cancel = false;
        if let Some(keys) = &self.capture_ui.hotkeys {
            if let Some(error) = keys.take_error() {
                self.error = Some(error);
            }
            capture = keys.capture.flag.swap(false, Ordering::AcqRel);
            repeat = keys.repeat.flag.swap(false, Ordering::AcqRel);
            if self.recorder.is_none() {
                cancel = keys.stop.flag.swap(false, Ordering::AcqRel);
            }
        }
        if let Some(escape) = &self.capture_ui.escape {
            cancel |= escape.flag.swap(false, Ordering::AcqRel);
            if let Some(error) = escape.take_error() {
                self.error = Some(error);
                cancel = true;
            }
        }
        cancel |= self.capture_ui.flow.active()
            && ctx.input(|input| input.key_pressed(egui::Key::Escape));
        if cancel {
            self.cancel_capture_flow(ctx);
        } else if (capture || repeat) && !self.capture_ui.flow.active() && self.recorder.is_none() {
            self.begin_capture(ctx, repeat);
        }
        let event =
            self.capture_ui
                .overlay
                .as_ref()
                .and_then(|overlay| match overlay.events.try_recv() {
                    Ok(event) => Some(event),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        self.capture_ui
                            .flow
                            .token()
                            .map(|token| {
                                selection::OverlayEvent {
                            token,
                            result: Err(
                                "Region selector exited without a result. Nothing was captured."
                                    .into(),
                            ),
                        }
                            })
                    }
                });
        if let Some(event) = event {
            self.capture_ui.overlay = None;
            if self.capture_ui.flow.token() == Some(event.token) {
                match event.result {
                    Ok(Some(area)) => {
                        self.capture_ui.area = area.clone();
                        self.capture_ui.flow.selected(event.token, area);
                        self.show_launcher(ctx);
                        self.capture_notice(selection::show_owned(self.window_handle, true));
                    }
                    Ok(None) => self.cancel_capture_flow(ctx),
                    Err(error) => {
                        self.cancel_capture_flow(ctx);
                        self.error = Some(error);
                    }
                }
            }
        }
        if let Some(preview) = &self.capture_ui.preview {
            if let Some(frame) = preview.take_frame() {
                self.capture_ui.preview_size = (frame.width, frame.height);
                match frame.into_rgba() {
                    Ok(image) => {
                        self.capture_ui.preview_texture =
                            Some(texture(ctx, "live camera preview", &image))
                    }
                    Err(error) => self.error = Some(format!("Camera preview frame: {error:#}")),
                }
            }
            if let Some(error) = preview.take_error() {
                self.error = Some(error);
                self.capture_ui.pending_arm = None;
            }
            if preview.finished() {
                self.capture_ui.preview = None;
                self.capture_ui.preview_texture = None;
                if let Some(token) = self.capture_ui.pending_arm.take() {
                    if self.capture_ui.flow.token() == Some(token) {
                        self.arm_capture(ctx);
                    }
                } else if !self.capture_ui.flow.active() {
                    if let Some(focus) = self.capture_ui.previous_focus.take() {
                        self.capture_notice(focus.restore());
                    }
                    self.status =
                        "Capture cancelled. Camera preview released; no capture was saved.".into();
                }
            }
        }
        if self.capture_ui.flow.active()
            || self.capture_ui.preview.is_some()
            || self.recorder.is_some()
        {
            ctx.request_repaint_after(Duration::from_millis(33));
        }
    }

    pub(super) fn admit_capture_timer(&mut self, ctx: &egui::Context) {
        if self.renderer_fixture {
            return;
        }
        // Keyboard and button cancellation both run before timer admission.
        if let Some(token) = self.capture_ui.flow.token() {
            if matches!(self.capture_ui.flow.phase, FlowPhase::Countdown { deadline, .. } if Instant::now() >= deadline)
            {
                if let Err(error) = self.validate_preflight() {
                    self.cancel_capture_flow(ctx);
                    self.error = Some(format!("{error:#}"));
                } else if let Some(area) = self.capture_ui.flow.take_start(token, Instant::now()) {
                    self.start_selected(ctx, area);
                }
            }
        }
    }

    pub(super) fn capture_surface(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().frame(egui::Frame::new().fill(Color32::from_rgb(247, 249, 249)).inner_margin(24.0)).show(ctx, |ui| {
            ui.horizontal(|ui| {
                crop_mark(ui);
                ui.heading(RichText::new("Snapper").size(24.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled(!self.capture_ui.flow.active(), egui::Button::new("Settings")).clicked() {
                        self.capture_ui.settings = !self.capture_ui.settings;
                    }
                    if ui.add_enabled(!self.capture_ui.flow.active(), egui::Button::new("Library")).clicked() {
                        self.page_library = true; self.show_editor(ctx);
                    }
                });
            });
            ui.add_space(14.0);
            if let Some(error) = self.error.clone() {
                egui::Frame::new().fill(Color32::from_rgb(255, 237, 235)).inner_margin(10.0).show(ui, |ui| {
                    ui.label(RichText::new(error).color(Color32::from_rgb(128, 30, 35)));
                    if ui.small_button("Dismiss").clicked() { self.error = None; }
                });
                ui.add_space(10.0);
            }
            egui::ScrollArea::vertical().show(ui, |ui| {
                if self.capture_ui.settings { self.capture_settings(ui, ctx); return; }
                match self.capture_ui.flow.phase.clone() {
                    FlowPhase::Idle => self.launcher_controls(ui, ctx),
                    FlowPhase::Selecting => {
                        ui.heading("Select your region");
                        ui.label("Drag on the chosen monitor. Move or resize the rectangle, then choose Review sources.");
                        ui.label(RichText::new("Esc to cancel").strong());
                        if ui.button("Cancel selection").clicked() { self.cancel_capture_flow(ctx); }
                    }
                    FlowPhase::Review(_) => self.preflight_controls(ui, ctx),
                    FlowPhase::Countdown { deadline, .. } => {
                        let remaining = deadline.saturating_duration_since(Instant::now()).as_secs_f32().ceil() as u32;
                        ui.heading(if self.capture_ui.preferences.capture.video { "Recording starts in" } else { "Screenshot in" });
                        ui.label(RichText::new(remaining.to_string()).size(64.0).strong().color(TEAL));
                        ui.label("No capture source starts until this countdown ends.");
                        ui.label(RichText::new("Esc to cancel").strong());
                        if ui.add_sized([ui.available_width(), 40.0], egui::Button::new("Cancel capture")).clicked() { self.cancel_capture_flow(ctx); }
                    }
                }
                ui.add_space(16.0);
                ui.separator();
                ui.label(&self.status);
                ui.small(&self.graphics_label);
            });
        });
    }

    fn launcher_controls(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.label("Capture a moment or record an explanation.");
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.selectable_value(
                &mut self.capture_ui.preferences.capture.video,
                false,
                "Screenshot",
            );
            if ui
                .selectable_value(
                    &mut self.capture_ui.preferences.capture.video,
                    true,
                    "Recording",
                )
                .changed()
                && self.capture_ui.preferences.capture.destination == Destination::Clipboard
            {
                self.capture_ui.preferences.capture.destination = Destination::Editor;
            }
        });
        ui.add_space(14.0);
        ui.label(RichText::new("What to capture").strong());
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(
                &mut self.capture_ui.preferences.capture.target,
                TargetKind::Region,
                "Region",
            );
            ui.selectable_value(
                &mut self.capture_ui.preferences.capture.target,
                TargetKind::Monitor,
                "Full monitor",
            );
            ui.selectable_value(
                &mut self.capture_ui.preferences.capture.target,
                TargetKind::WindowGuide,
                "Window guide",
            );
        });
        let old = self.capture_ui.area.monitor.clone();
        egui::ComboBox::from_id_salt("capture monitor")
            .width(ui.available_width())
            .selected_text(
                self.capture_ui
                    .monitors
                    .iter()
                    .find(|m| m.id == old)
                    .map(monitor_label)
                    .unwrap_or_else(|| "Choose a monitor".into()),
            )
            .show_ui(ui, |ui| {
                for monitor in &self.capture_ui.monitors {
                    ui.selectable_value(
                        &mut self.capture_ui.area.monitor,
                        monitor.id.clone(),
                        monitor_label(monitor),
                    );
                }
            });
        if old != self.capture_ui.area.monitor {
            if let Some(monitor) = self
                .capture_ui
                .monitors
                .iter()
                .find(|m| m.id == self.capture_ui.area.monitor)
            {
                self.capture_ui.area = monitor.area();
            }
        }
        if ui.small_button("Refresh displays").clicked() {
            match platform::screen::monitors() {
                Ok(monitors) => self.capture_ui.monitors = monitors,
                Err(error) => self.error = Some(format!("{error:#}")),
            }
        }
        if self.capture_ui.preferences.capture.target == TargetKind::WindowGuide {
            ui.label("A window guides a fixed screen region. Other windows can cover it; it does not follow the window.");
            if ui
                .add_enabled(!self.busy(), egui::Button::new("Find windows"))
                .clicked()
            {
                self.task(ctx, || {
                    Ok(TaskResult::WindowGuides(selection::window_guides()?))
                });
            }
            egui::ComboBox::from_id_salt("window guide")
                .width(ui.available_width())
                .selected_text(
                    self.capture_ui
                        .windows
                        .get(self.capture_ui.window_index)
                        .map(|w| w.title.as_str())
                        .unwrap_or("Choose a window"),
                )
                .show_ui(ui, |ui| {
                    for (index, window) in self.capture_ui.windows.iter().enumerate() {
                        ui.selectable_value(
                            &mut self.capture_ui.window_index,
                            index,
                            format!("{} (PID {})", window.title, window.process),
                        );
                    }
                });
        }
        ui.add_space(18.0);
        let label = if self.capture_ui.preferences.capture.target == TargetKind::Monitor {
            "Review full-monitor capture"
        } else {
            "Select area"
        };
        if primary_button(ui, label, !self.busy() && !self.unsaved()) {
            self.begin_capture(ctx, false);
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.capture_ui.preferences.last_region.is_some()
                        && !self.busy()
                        && !self.unsaved(),
                    egui::Button::new("Repeat last region"),
                )
                .clicked()
            {
                self.begin_capture(ctx, true);
            }
            ui.small(&self.capture_ui.preferences.shortcuts.capture);
        });
        ui.small("Source choices come next. Nothing records when you select a region.");
        let presets = self.capture_ui.preferences.presets.clone();
        if !presets.is_empty() {
            ui.add_space(12.0);
            egui::ComboBox::from_id_salt("capture preset")
                .selected_text("Use a saved preset")
                .show_ui(ui, |ui| {
                    for preset in presets {
                        if ui.selectable_label(false, &preset.name).clicked() {
                            self.capture_ui.preferences.capture = preset.options;
                            self.status = format!(
                                "Preset '{}' selected. No sources were opened.",
                                preset.name
                            );
                        }
                    }
                });
        }
        ui.add_space(18.0);
        ui.horizontal(|ui| {
            ui.heading("Recent");
            if ui
                .add_enabled(
                    !self.busy() && !self.unsaved(),
                    egui::Button::new("Import image"),
                )
                .clicked()
            {
                self.import(ctx);
            }
        });
        let captures: Vec<_> = self.captures.iter().take(4).cloned().collect();
        if captures.is_empty() {
            ui.label("Your first screenshot or recording will appear here.");
        }
        for capture in captures {
            ui.horizontal(|ui| {
                if let Some(image) = self.thumbnails.get(&capture.id) {
                    if ui
                        .add(
                            egui::Image::new(image)
                                .fit_to_exact_size(Vec2::new(80.0, 48.0))
                                .sense(Sense::click()),
                        )
                        .clicked()
                    {
                        self.open_capture(ctx, capture.clone());
                    }
                }
                if ui
                    .selectable_label(
                        false,
                        format!(
                            "{}  -  {} x {}",
                            capture.title, capture.width, capture.height
                        ),
                    )
                    .clicked()
                {
                    self.open_capture(ctx, capture);
                }
            });
        }
    }

    fn preflight_controls(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.heading(if self.capture_ui.preferences.capture.video {
            "Ready to record"
        } else {
            "Ready to capture"
        });
        let area = self.capture_ui.area.clone();
        ui.label(format!(
            "{} x {} physical pixels at {}, {} on {}",
            area.width, area.height, area.x, area.y, area.monitor
        ));
        ui.horizontal(|ui| {
            ui.label(RichText::new("Esc to cancel").strong());
            if ui
                .add_enabled(
                    self.capture_ui.preview.is_none(),
                    egui::Button::new("Select again"),
                )
                .clicked()
            {
                self.begin_capture(ctx, false);
            }
        });
        ui.collapsing("Precise coordinates", |ui| {
            if let Some(monitor) = self.capture_ui.flow_monitor.clone() {
                egui::Grid::new("physical region precision")
                    .num_columns(4)
                    .show(ui, |ui| {
                        ui.label("Left");
                        ui.add(
                            egui::DragValue::new(&mut self.capture_ui.area.x)
                                .range(0..=monitor.width.saturating_sub(1)),
                        );
                        ui.label("Top");
                        ui.add(
                            egui::DragValue::new(&mut self.capture_ui.area.y)
                                .range(0..=monitor.height.saturating_sub(1)),
                        );
                        ui.end_row();
                        ui.label("Width");
                        ui.add(
                            egui::DragValue::new(&mut self.capture_ui.area.width)
                                .range(1..=monitor.width - self.capture_ui.area.x),
                        );
                        ui.label("Height");
                        ui.add(
                            egui::DragValue::new(&mut self.capture_ui.area.height)
                                .range(1..=monitor.height - self.capture_ui.area.y),
                        );
                        ui.end_row();
                    });
                if let FlowPhase::Review(area) = &mut self.capture_ui.flow.phase {
                    *area = self.capture_ui.area.clone();
                }
            }
        });
        ui.add_space(12.0);
        self.source_controls(ui, ctx);
        ui.add_space(12.0);
        self.destination_controls(ui, ctx);
        ui.horizontal(|ui| {
            ui.label("Countdown");
            ui.add(
                egui::DragValue::new(&mut self.capture_ui.preferences.capture.countdown_seconds)
                    .range(0..=10)
                    .suffix(" seconds"),
            );
        });
        ui.add_space(14.0);
        let label = if self.capture_ui.preferences.capture.video {
            "Record"
        } else {
            "Capture"
        };
        if primary_button(
            ui,
            label,
            self.capture_ui.pending_arm.is_none() && !self.busy(),
        ) {
            self.arm_capture(ctx);
        }
        if ui.button("Cancel capture").clicked() {
            self.cancel_capture_flow(ctx);
        }
    }

    fn source_controls(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Sources").strong());
            if ui
                .add_enabled(
                    !self.busy() && self.capture_ui.preview.is_none(),
                    egui::Button::new("Refresh devices"),
                )
                .clicked()
            {
                self.task(ctx, || {
                    Ok(TaskResult::Devices(
                        platform::camera::list()?,
                        platform::audio::list()?,
                    ))
                });
            }
        });
        ui.checkbox(
            &mut self.capture_ui.preferences.capture.sources.cursor,
            "Include cursor",
        );
        let webcam_before = self.capture_ui.preferences.capture.sources.webcam;
        ui.checkbox(
            &mut self.capture_ui.preferences.capture.sources.webcam,
            "Webcam",
        );
        if webcam_before && !self.capture_ui.preferences.capture.sources.webcam {
            if let Some(preview) = &self.capture_ui.preview {
                preview.stop();
            }
            self.capture_ui.preview_texture = None;
        }
        if self.capture_ui.preferences.capture.sources.webcam {
            let choices = self
                .capture_ui
                .cameras
                .iter()
                .map(|camera| (camera.id.clone(), camera.name.clone()))
                .collect::<Vec<_>>();
            ui.add_enabled_ui(self.capture_ui.preview.is_none(), |ui| {
                device_picker(
                    ui,
                    "Camera",
                    &mut self.capture_ui.preferences.capture.camera_id,
                    &choices,
                );
            });
            if let Some(preview) = &self.capture_ui.preview {
                ui.label(
                    RichText::new(if preview.stopping() {
                        "Releasing camera preview..."
                    } else {
                        "Camera preview is ON"
                    })
                    .strong()
                    .color(TEAL),
                );
                if ui.button("Stop preview").clicked() {
                    preview.stop();
                }
            } else if ui
                .add_enabled(
                    self.capture_ui.preferences.capture.camera_id.is_some(),
                    egui::Button::new("Preview camera (turns camera on)"),
                )
                .clicked()
                && !self.renderer_fixture
            {
                let context = ctx.clone();
                if let Some(id) = self.capture_ui.preferences.capture.camera_id.clone() {
                    self.capture_ui.preview = Some(CameraPreview::start(id, false, move || {
                        context.request_repaint()
                    }));
                }
            }
            self.pip_controls(ui);
        }
        ui.add_enabled_ui(self.capture_ui.preferences.capture.video, |ui| {
            ui.checkbox(
                &mut self.capture_ui.preferences.capture.sources.system_audio,
                "System audio",
            );
            if self.capture_ui.preferences.capture.sources.system_audio {
                let choices = self
                    .capture_ui
                    .audio
                    .iter()
                    .filter(|device| device.system)
                    .map(|device| (device.id.clone(), device.name.clone()))
                    .collect::<Vec<_>>();
                device_picker(
                    ui,
                    "Output route",
                    &mut self.capture_ui.preferences.capture.output_id,
                    &choices,
                );
                ui.small("Records this output's entire mix, including other apps.");
            }
            ui.checkbox(
                &mut self.capture_ui.preferences.capture.sources.microphone,
                "Microphone",
            );
            if self.capture_ui.preferences.capture.sources.microphone {
                let choices = self
                    .capture_ui
                    .audio
                    .iter()
                    .filter(|device| !device.system)
                    .map(|device| (device.id.clone(), device.name.clone()))
                    .collect::<Vec<_>>();
                device_picker(
                    ui,
                    "Microphone",
                    &mut self.capture_ui.preferences.capture.microphone_id,
                    &choices,
                );
            }
        });
        ui.small(if self.capture_ui.preferences.capture.video {
            "Choices do not turn devices on. Only explicit Preview or Record activates a selected source."
        } else { "Screenshots never open audio sources. Their recording choices are retained." });
        if self.capture_ui.preferences.capture.video {
            ui.horizontal(|ui| {
                ui.label("Recording limit");
                ui.add(
                    egui::DragValue::new(&mut self.capture_ui.preferences.capture.limit_minutes)
                        .range(1..=120)
                        .suffix(" minutes"),
                );
            });
            ui.small("Video fits within 1920 x 1080 at 30 fps; screenshots retain the selected physical resolution.");
        }
    }

    fn pip_controls(&mut self, ui: &mut egui::Ui) {
        ui.label("Camera placement - drag the tile");
        let area = &self.capture_ui.area;
        let scale = (ui.available_width().min(440.0) / area.width.max(1) as f32)
            .min(200.0 / area.height.max(1) as f32);
        let (canvas, _) = ui.allocate_exact_size(
            Vec2::new(area.width as f32 * scale, area.height as f32 * scale),
            Sense::hover(),
        );
        ui.painter()
            .rect_filled(canvas, 4.0, Color32::from_rgb(223, 231, 230));
        ui.painter().rect_stroke(
            canvas,
            4.0,
            Stroke::new(1.0_f32, TEAL),
            egui::StrokeKind::Inside,
        );
        let (cw, ch) = self.capture_ui.preview_size;
        let r = self
            .capture_ui
            .preferences
            .capture
            .pip
            .rect(area.width, area.height, cw, ch);
        let tile = Rect::from_min_size(
            canvas.min + Vec2::new(r.x as f32, r.y as f32) * scale,
            Vec2::new(r.width as f32, r.height as f32) * scale,
        );
        if let Some(texture) = &self.capture_ui.preview_texture {
            ui.painter().image(
                texture.id(),
                tile,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        } else {
            ui.painter().rect_filled(tile, 3.0, TEAL);
            ui.painter().text(
                tile.center(),
                egui::Align2::CENTER_CENTER,
                "Camera",
                egui::FontId::proportional(12.0),
                Color32::WHITE,
            );
        }
        let drag = ui.interact(tile, ui.id().with("pip drag"), Sense::drag());
        if drag.dragged() {
            let delta = drag.drag_delta();
            let pip = &mut self.capture_ui.preferences.capture.pip;
            pip.x = (pip.x as f32
                + delta.x / ((area.width - r.width).max(1) as f32 * scale) * 1000.0)
                .clamp(0.0, 1000.0) as u16;
            pip.y = (pip.y as f32
                + delta.y / ((area.height - r.height).max(1) as f32 * scale) * 1000.0)
                .clamp(0.0, 1000.0) as u16;
        }
        let mut percent = self.capture_ui.preferences.capture.pip.width / 10;
        if ui
            .add(egui::Slider::new(&mut percent, 10..=50).text("Camera width (%)"))
            .changed()
        {
            self.capture_ui.preferences.capture.pip.width = percent * 10;
        }
    }

    fn destination_controls(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.label(RichText::new("After capture").strong());
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(
                &mut self.capture_ui.preferences.capture.destination,
                Destination::Editor,
                "Open editor",
            );
            ui.add_enabled_ui(!self.capture_ui.preferences.capture.video, |ui| {
                ui.selectable_value(
                    &mut self.capture_ui.preferences.capture.destination,
                    Destination::Clipboard,
                    "Copy image",
                );
            });
            ui.selectable_value(
                &mut self.capture_ui.preferences.capture.destination,
                Destination::File,
                "Save file",
            );
        });
        if self.capture_ui.preferences.capture.destination == Destination::File {
            ui.label(
                self.capture_ui
                    .preferences
                    .capture
                    .output_directory
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "Choose a local folder".into()),
            );
            if ui
                .add_enabled(!self.busy(), egui::Button::new("Choose folder"))
                .clicked()
            {
                self.task(ctx, || {
                    Ok(match rfd::FileDialog::new().pick_folder() {
                        Some(path) => TaskResult::OutputDirectory(path),
                        None => TaskResult::Notice("Output folder selection cancelled.".into()),
                    })
                });
            }
        }
        ui.small("An immutable original is retained in your local library.");
    }

    fn capture_settings(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.heading("Capture settings");
        ui.label("Saved choices never activate hardware when Snapper opens.");
        ui.add_space(12.0);
        ui.label(RichText::new("Global shortcuts").strong());
        egui::Grid::new("shortcut settings")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("Select capture");
                ui.text_edit_singleline(&mut self.capture_ui.shortcut_draft.capture);
                ui.end_row();
                ui.label("Repeat region");
                ui.text_edit_singleline(&mut self.capture_ui.shortcut_draft.repeat);
                ui.end_row();
                ui.label("Stop");
                ui.text_edit_singleline(&mut self.capture_ui.shortcut_draft.stop);
                ui.end_row();
            });
        ui.small("Use Ctrl, Shift or Alt plus a letter, digit or function key. Collisions are reported, never taken over.");
        if ui.button("Apply shortcuts").clicked() {
            let proposed = self.capture_ui.shortcut_draft.clone();
            if self.capture_notice(proposed.parsed().map(|_| ())) {
                self.capture_ui.hotkeys = None;
                let context = ctx.clone();
                let window = self.window_handle;
                match GlobalShortcuts::new(&proposed, move || {
                    if let Err(error) = platform::process::restore_owned_window(window) {
                        eprintln!("Shortcut restore: {error:#}");
                    }
                    context.request_repaint();
                }) {
                    Ok(keys) => {
                        self.capture_ui.hotkeys = Some(keys);
                        self.capture_ui.preferences.shortcuts = proposed;
                        if self.save_preferences() {
                            self.status = "Shortcuts applied and saved.".into();
                        }
                    }
                    Err(error) => {
                        let context = ctx.clone();
                        let restore = GlobalShortcuts::new(
                            &self.capture_ui.preferences.shortcuts,
                            move || context.request_repaint(),
                        );
                        self.error = Some(match restore {
                            Ok(keys) => { self.capture_ui.hotkeys = Some(keys); format!("New shortcuts were not applied: {error:#}. Previous shortcuts restored.") }
                            Err(restore_error) => format!("New shortcuts failed: {error:#}. Previous shortcuts could not be restored: {restore_error:#}. Use the visible controls."),
                        });
                    }
                }
            }
        }
        ui.add_space(14.0);
        ui.label(RichText::new("Presets").strong());
        ui.add(
            egui::TextEdit::singleline(&mut self.capture_ui.preset_name)
                .hint_text("Name the current capture setup")
                .char_limit(80),
        );
        if ui.button("Save current setup as preset").clicked() {
            let preset = Preset {
                name: self.capture_ui.preset_name.trim().into(),
                options: self.capture_ui.preferences.capture.clone(),
            };
            let mut candidate = self.capture_ui.preferences.clone();
            candidate.presets.push(preset);
            if self.capture_notice(candidate.validate()) {
                self.capture_ui.preferences = candidate;
                if self.save_preferences() {
                    self.status = "Capture preset saved. No device was opened.".into();
                    self.capture_ui.preset_name.clear();
                }
            }
        }
        for preset in &self.capture_ui.preferences.presets {
            ui.label(&preset.name);
        }
        ui.add_space(12.0);
        ui.checkbox(&mut self.show_recent, "Show recent captures in the editor");
        if ui.button("Save preferences").clicked() && self.save_preferences() {
            self.status = "Preferences saved locally.".into();
        }
        if ui.button("Back to capture").clicked() {
            self.capture_ui.settings = false;
        }
    }

    pub(super) fn recording_dock(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(247, 249, 249))
                    .inner_margin(12.0),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let drag = ui
                        .add(
                            egui::Label::new(RichText::new("Snapper").strong())
                                .sense(Sense::drag()),
                        )
                        .on_hover_text("Drag to move recording controls");
                    if drag.drag_started() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    ui.label(format!(
                        "{:02}:{:02}",
                        self.last_progress.elapsed_ms / 60_000,
                        self.last_progress.elapsed_ms / 1000 % 60
                    ));
                    ui.label(format!("{:?}", self.last_progress.phase));
                    if let Some(recorder) = &self.recorder {
                        if matches!(
                            self.last_progress.phase,
                            Phase::DevicesStopped | Phase::Finalizing
                        ) {
                            if ui.button("Cancel saving").clicked() {
                                recorder.cancel_finalization();
                            }
                        } else if ui.button("Stop").clicked() {
                            recorder.stop();
                        }
                    }
                });
                ui.small(&self.last_progress.detail);
                ui.small(format!(
                    "Stop anywhere: {}  |  Controls excluded from capture",
                    self.capture_ui.preferences.shortcuts.stop
                ));
            });
    }
}

fn primary_button(ui: &mut egui::Ui, label: &str, enabled: bool) -> bool {
    ui.add_enabled(
        enabled,
        egui::Button::new(RichText::new(label).color(Color32::WHITE).strong())
            .fill(TEAL)
            .min_size(Vec2::new(ui.available_width(), 42.0)),
    )
    .clicked()
}

fn monitor_label(monitor: &Monitor) -> String {
    format!(
        "{}{}  -  {} x {} px ({}%)",
        if monitor.primary { "Primary " } else { "" },
        monitor.id,
        monitor.width,
        monitor.height,
        monitor.dpi * 100 / 96
    )
}

fn device_picker(
    ui: &mut egui::Ui,
    label: &str,
    selected: &mut Option<String>,
    choices: &[(String, String)],
) {
    egui::ComboBox::from_id_salt(label)
        .width(ui.available_width())
        .selected_text(
            choices
                .iter()
                .find(|(id, _)| Some(id) == selected.as_ref())
                .map(|(_, name)| format!("{label}: {name}"))
                .unwrap_or_else(|| format!("Choose {label}")),
        )
        .show_ui(ui, |ui| {
            for (id, name) in choices {
                ui.selectable_value(selected, Some(id.clone()), name);
            }
        });
}

fn crop_mark(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::hover());
    let stroke = Stroke::new(2.5_f32, TEAL);
    for (x, y, dx, dy) in [
        (3.0, 3.0, 1.0, 1.0),
        (25.0, 3.0, -1.0, 1.0),
        (3.0, 25.0, 1.0, -1.0),
        (25.0, 25.0, -1.0, -1.0),
    ] {
        let p = rect.min + Vec2::new(x, y);
        ui.painter()
            .line_segment([p, p + Vec2::new(7.0 * dx, 0.0)], stroke);
        ui.painter()
            .line_segment([p, p + Vec2::new(0.0, 7.0 * dy)], stroke);
    }
}

fn copy_new(source: &Path, destination: &Path) -> Result<()> {
    library::copy_file_new(source, destination)
}
