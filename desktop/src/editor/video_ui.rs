use super::*;
use crate::video::{self, EditedPreview, VideoControl, VideoDocument, VideoPlan, FPS};

pub(super) enum VideoEditCommand {
    Trim(u32, u32),
    Remove(u32, u32),
    Split(u32),
    Restore,
}

pub(super) enum VideoPlayback {
    Original(VideoDocument),
    Edited(EditedPreview),
}
impl VideoPlayback {
    fn path(&self) -> &Path {
        match self {
            Self::Original(document) => &document.source,
            Self::Edited(preview) => &preview.path,
        }
    }
}

pub(super) struct VideoUi {
    pub(super) document: VideoDocument,
    pub(super) plan: VideoPlan,
    pub(super) dirty: bool,
    pub(super) pending_save: bool,
    preview: Option<EditedPreview>,
    texture: Option<TextureHandle>,
    playhead: u32,
    selection_start: u32,
    selection_end: u32,
    displayed_frame: Option<u32>,
    pending_frame: bool,
    undo: Vec<VideoPlan>,
    redo: Vec<VideoPlan>,
    dragging: Option<u8>,
}

impl VideoUi {
    pub(super) fn new(document: VideoDocument) -> Self {
        let plan = document.plan.clone();
        let end = plan.frames();
        Self {
            document,
            plan,
            dirty: false,
            pending_save: false,
            preview: None,
            texture: None,
            playhead: 0,
            selection_start: 0,
            selection_end: end,
            displayed_frame: None,
            pending_frame: false,
            undo: vec![],
            redo: vec![],
            dragging: None,
        }
    }

    fn apply(&mut self, plan: VideoPlan, checkpoint: bool) -> Result<()> {
        plan.validate()?;
        if self.plan == plan {
            return Ok(());
        }
        if checkpoint {
            if self.undo.len() == 80 {
                self.undo.remove(0);
            }
            self.undo.push(self.plan.clone());
            self.redo.clear();
        }
        let content_changed = self.plan.fingerprint()? != plan.fingerprint()?;
        self.plan = plan;
        self.playhead = self.playhead.min(self.plan.frames() - 1);
        self.selection_start = self.selection_start.min(self.plan.frames() - 1);
        self.selection_end = self
            .selection_end
            .min(self.plan.frames())
            .max(self.selection_start + 1);
        if content_changed {
            self.preview = None;
            self.texture = None;
            self.displayed_frame = None;
            self.pending_frame = false;
        }
        self.dirty = true;
        self.pending_save = true;
        Ok(())
    }
}

pub(super) struct VideoJobUi {
    pub(super) control: Arc<VideoControl>,
}
impl Drop for VideoJobUi {
    fn drop(&mut self) {
        self.control.cancel();
    }
}

impl Editor {
    fn play_video(&mut self, ctx: &egui::Context, playback: VideoPlayback) {
        self.play_video_with(ctx, playback, platform::process::open_local_video);
    }

    pub(super) fn play_video_with(
        &mut self,
        ctx: &egui::Context,
        playback: VideoPlayback,
        open: impl FnOnce(&Path, isize) -> Result<()> + Send + 'static,
    ) {
        if !self.document_edits_allowed() {
            return;
        }
        if self.busy() {
            self.status = "Finish the current operation before requesting playback.".into();
            return;
        }
        let owner = self.window_handle;
        self.status = "Requesting playback in your configured Windows player...".into();
        self.task(ctx, move || {
            let result = (|| -> Result<String> {
                let filename = playback
                    .path()
                    .file_name()
                    .context("Video filename is unavailable")?
                    .to_string_lossy()
                    .into_owned();
                open(playback.path(), owner).context("Opening local video playback")?;
                Ok(filename)
            })()
            .map_err(|error| format!("{error:#}"));
            Ok(TaskResult::PlaybackCompleted(result))
        });
    }

    pub(super) fn video_loading_status(&mut self, ctx: &egui::Context) {
        if !self.switching_capture {
            return;
        }
        if let Some(progress) = self.video_job.as_ref().map(|job| job.control.progress()) {
            egui::Window::new("Opening recording")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(&progress.stage);
                    ui.add(egui::ProgressBar::new(progress.fraction));
                    if ui.button("Cancel").clicked() {
                        self.cancel_video_work();
                    }
                });
        }
    }

    pub(super) fn cancel_video_work(&mut self) {
        if let Some(job) = &self.video_job {
            self.status = if job.control.cancel() {
                "Cancellation requested. Waiting for owned codec processes to exit.".into()
            } else {
                "Final publication already began. Waiting for saved-file confirmation.".into()
            };
        }
    }

    pub(super) fn undo_video(&mut self, redo: bool) {
        if !self.document_edits_allowed() {
            return;
        }
        let Some(video) = &mut self.video else { return };
        let plan = if redo {
            video.redo.pop()
        } else {
            video.undo.pop()
        };
        if let Some(plan) = plan {
            if redo {
                video.undo.push(video.plan.clone());
            } else {
                video.redo.push(video.plan.clone());
            }
            if let Err(error) = video.apply(plan, false) {
                self.error = Some(format!("{error:#}"));
            }
        }
    }

    pub(super) fn execute_video_edit(&mut self, command: VideoEditCommand) -> Result<bool> {
        if !self.document_edits_allowed() {
            return Ok(false);
        }
        let video = self
            .video
            .as_mut()
            .context("No recording is open for editing")?;
        let mut plan = video.plan.clone();
        match command {
            VideoEditCommand::Trim(start, end) => plan = plan.selection(start, end)?,
            VideoEditCommand::Remove(start, end) => plan.remove(start, end)?,
            VideoEditCommand::Split(frame) => {
                plan.split(frame)?;
            }
            VideoEditCommand::Restore => {
                plan = VideoPlan::new(
                    video.document.info.source_sha256.clone(),
                    video.document.info.frames,
                )
            }
        }
        let changed = plan != video.plan;
        video.apply(plan, true)?;
        Ok(changed)
    }

    pub(super) fn install_video_result(
        &mut self,
        ctx: &egui::Context,
        preview: EditedPreview,
        destination: Option<PathBuf>,
    ) {
        if let Some(video) = &mut self.video {
            match video.plan.fingerprint() {
                Ok(hash) if hash == preview.plan_sha256 => {
                    video.preview = Some(preview);
                    video.pending_frame = true;
                }
                Ok(_) => {}
                Err(error) => self.error = Some(format!("{error:#}")),
            }
        }
        self.status = if let Some(path) = destination {
            format!(
                "Edited MP4 saved to {}. Original and recovery segments unchanged.",
                path.display()
            )
        } else {
            "Edited preview ready. Playback and Save MP4 use this verified rendering.".into()
        };
        ctx.request_repaint();
    }

    pub(super) fn install_video_frame(
        &mut self,
        ctx: &egui::Context,
        image: RgbaImage,
        frame: u32,
        fingerprint: String,
    ) {
        if let Some(video) = &mut self.video {
            if video.playhead == frame
                && video
                    .preview
                    .as_ref()
                    .is_some_and(|preview| preview.plan_sha256 == fingerprint)
            {
                video.texture = Some(texture(ctx, "edited video frame", &image));
                video.displayed_frame = Some(frame);
            }
        }
    }

    pub(super) fn poll_video(&mut self, ctx: &egui::Context) {
        if self.busy() || self.switching_capture {
            return;
        }
        let Some(video) = &mut self.video else { return };
        if video.pending_save {
            video.pending_save = false;
            let document = video.document.clone();
            let plan = video.plan.clone();
            self.task(ctx, move || {
                document.save_plan(&plan)?;
                Ok(TaskResult::VideoPlanSaved(plan))
            });
        } else if video.pending_frame {
            let Some(preview) = video.preview.clone() else {
                return;
            };
            video.pending_frame = false;
            let frame = video.playhead;
            let control = Arc::new(VideoControl::default());
            self.video_job = Some(VideoJobUi {
                control: control.clone(),
            });
            control.update("Loading the edited frame preview", 0.0, 0);
            self.task(ctx, move || {
                match video::preview_frame(&preview, frame, &control) {
                    Ok(image) => Ok(TaskResult::VideoFrame(image, frame, preview.plan_sha256)),
                    Err(error) if error.downcast_ref::<media::encoder::Cancelled>().is_some() => {
                        Ok(TaskResult::Notice(
                            "Frame preview cancelled. Video edits are unchanged.".into(),
                        ))
                    }
                    Err(error) => Err(error),
                }
            });
        }
    }

    pub(super) fn render_video(&mut self, ctx: &egui::Context, export: bool, selected: bool) {
        if !self.document_edits_allowed() || self.busy() {
            return;
        }
        let Some(video) = &self.video else { return };
        let plan = if selected {
            match video
                .plan
                .selection(video.selection_start, video.selection_end)
            {
                Ok(plan) => plan,
                Err(error) => {
                    self.error = Some(format!("{error:#}"));
                    return;
                }
            }
        } else {
            video.plan.clone()
        };
        let document = video.document.clone();
        let current_preview = if export && !selected {
            video.preview.clone().filter(|preview| {
                plan.fingerprint()
                    .is_ok_and(|hash| hash == preview.plan_sha256)
            })
        } else {
            None
        };
        let control = Arc::new(VideoControl::default());
        self.video_job = Some(VideoJobUi {
            control: control.clone(),
        });
        control.update(
            if export {
                "Choosing a new MP4 destination"
            } else {
                "Preparing the current video edits"
            },
            0.0,
            0,
        );
        self.task(ctx, move || {
            let destination = if export {
                let Some(path) = rfd::FileDialog::new().add_filter("MP4", &["mp4"])
                    .set_file_name(if selected { "selected-clip.mp4" } else { "edited-recording.mp4" }).save_file()
                else { return Ok(TaskResult::Notice("MP4 export cancelled. Original recording unchanged.".into())); };
                Some(path)
            } else { None };
            if let (Some(preview), Some(destination)) = (current_preview, destination.as_ref()) {
                video::save_preview(&preview, destination, &control)?;
                return Ok(TaskResult::VideoRendered(preview, Some(destination.clone())));
            }
            match video::render_edit(&document, &plan, destination.as_deref(), &control) {
                Ok(preview) => Ok(TaskResult::VideoRendered(preview, destination)),
                Err(error) if error.downcast_ref::<media::encoder::Cancelled>().is_some() =>
                    Ok(TaskResult::Notice("Video work cancelled. No export was published; original recording and recovery segments unchanged.".into())),
                Err(error) => Err(error),
            }
        });
    }

    pub(super) fn focused_video_view(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if !self.document_edits_allowed() {
            ui.label("Loading the next recording. Current editing and exports are paused.");
            return;
        }
        let busy = self.busy();
        let Some(video) = &mut self.video else {
            ui.label("Inspecting the video before enabling editing.");
            return;
        };
        ui.label(format!(
            "{} x {} original | {:.3} s source | {:.3} s edited",
            video.document.info.width,
            video.document.info.height,
            video.document.info.source_seconds,
            video.plan.frames() as f64 / FPS as f64
        ));
        ui.small("Frame-accurate on a 30 fps edit timeline (33.333 ms/frame). Accurate re-encoding, not keyframe-rounded copying.");
        let mut action = 0;
        let mut cancel_requested = false;
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(!busy, egui::Button::new("Update edited preview"))
                .clicked()
            {
                action = 1;
            }
            if ui
                .add_enabled(
                    !busy && video.preview.is_some(),
                    egui::Button::new("Play edited preview"),
                )
                .clicked()
            {
                action = 2;
            }
            if ui
                .add_enabled(!busy, egui::Button::new("Save edited MP4"))
                .clicked()
            {
                action = 3;
            }
            if ui
                .add_enabled(!busy, egui::Button::new("Save selected clip"))
                .clicked()
            {
                action = 4;
            }
            if ui
                .add_enabled(!busy, egui::Button::new("Play original"))
                .clicked()
            {
                action = 5;
            }
        });
        if let Some(progress) = self.video_job.as_ref().map(|job| job.control.progress()) {
            ui.add(egui::ProgressBar::new(progress.fraction).text(format!(
                "{} ({:.1}s)",
                progress.stage,
                progress.elapsed_ms as f64 / 1000.0
            )));
            if ui.button("Cancel video work").clicked() {
                cancel_requested = true;
            }
        }
        if let Some(preview) = &video.texture {
            let scale = (ui.available_width() / preview.size_vec2().x)
                .min(270.0 / preview.size_vec2().y)
                .min(1.0);
            ui.add(egui::Image::new(preview).fit_to_exact_size(preview.size_vec2() * scale));
            if let Some(frame) = video.displayed_frame {
                ui.small(format!("Rendered edit: frame {frame}, {:.3}s. Playback opens this same rendering in your Windows video player.", frame as f64/FPS as f64));
            }
        } else {
            egui::Frame::new().fill(Color32::from_rgb(223,231,230)).inner_margin(18.0).show(ui, |ui| {
                ui.label(if video.preview.is_some() { "Loading the current edited frame..." }
                    else { "Edited preview is not current. Choose Update edited preview to see or play these cuts." });
                ui.small("Existing player windows keep their previous snapshot. Open the updated preview after editing.");
            });
        }
        let frames = video.plan.frames();
        let before_playhead = video.playhead;
        draw_timeline(ui, video);
        ui.horizontal(|ui| {
            if ui.button("< frame").clicked() {
                video.playhead = video.playhead.saturating_sub(1);
            }
            if ui.button("frame >").clicked() {
                video.playhead = (video.playhead + 1).min(frames - 1);
            }
            ui.add(
                egui::DragValue::new(&mut video.playhead)
                    .range(0..=frames - 1)
                    .prefix("Frame "),
            );
            ui.label(format!(
                "{:.3}s / {:.3}s",
                video.playhead as f64 / FPS as f64,
                frames as f64 / FPS as f64
            ));
            if ui.button("Start here").clicked() {
                video.selection_start = video.playhead;
                video.selection_end = video.selection_end.max(video.selection_start + 1);
            }
            if ui.button("End here").clicked() {
                video.selection_end = video.playhead + 1;
                video.selection_start = video.selection_start.min(video.selection_end - 1);
            }
        });
        if video.playhead != before_playhead {
            video.pending_frame = video.preview.is_some();
        }
        ui.horizontal(|ui| {
            ui.label("Selection");
            ui.add(
                egui::DragValue::new(&mut video.selection_start)
                    .range(0..=video.selection_end - 1)
                    .prefix("From frame "),
            );
            ui.add(
                egui::DragValue::new(&mut video.selection_end)
                    .range(video.selection_start + 1..=frames)
                    .prefix("To frame "),
            );
            ui.label(format!(
                "({:.3}s to {:.3}s, end excluded)",
                video.selection_start as f64 / FPS as f64,
                video.selection_end as f64 / FPS as f64
            ));
        });
        let mut proposed = None;
        ui.add_enabled_ui(!busy, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button("Keep selection / trim ends").clicked() {
                    proposed = Some(VideoEditCommand::Trim(
                        video.selection_start,
                        video.selection_end,
                    ));
                }
                if ui.button("Remove selection").clicked() {
                    proposed = Some(VideoEditCommand::Remove(
                        video.selection_start,
                        video.selection_end,
                    ));
                }
                if ui.button("Split at playhead").clicked() {
                    proposed = Some(VideoEditCommand::Split(video.playhead));
                }
                if ui
                    .add_enabled(!video.undo.is_empty(), egui::Button::new("Undo video edit"))
                    .clicked()
                {
                    action = 6;
                }
                if ui
                    .add_enabled(!video.redo.is_empty(), egui::Button::new("Redo"))
                    .clicked()
                {
                    action = 7;
                }
                if ui.button("Restore full original timeline").clicked() {
                    proposed = Some(VideoEditCommand::Restore);
                }
            });
            ui.collapsing(
                format!("{} kept sections", video.plan.sections.len()),
                |ui| {
                    let mut offset = 0;
                    for (index, section) in video.plan.sections.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(format!(
                                "{}: source {:.3}s - {:.3}s",
                                index + 1,
                                section.start as f64 / FPS as f64,
                                section.end as f64 / FPS as f64
                            ));
                            if ui.button("Select section").clicked() {
                                video.selection_start = offset;
                                video.selection_end = offset + section.frames();
                            }
                        });
                        offset += section.frames();
                    }
                },
            );
        });
        let playback = match action {
            2 if proposed.is_none() => video.preview.clone().map(VideoPlayback::Edited),
            5 => Some(VideoPlayback::Original(video.document.clone())),
            _ => None,
        };
        if cancel_requested {
            self.cancel_video_work();
        }
        if let Some(command) = proposed {
            if let Err(error) = self.execute_video_edit(command) {
                self.error = Some(format!("{error:#}"));
                return;
            }
        }
        if let Some(playback) = playback {
            self.play_video(ctx, playback);
        }
        match action {
            1 => self.render_video(ctx, false, false),
            3 => self.render_video(ctx, true, false),
            4 => self.render_video(ctx, true, true),
            6 => self.undo_video(false),
            7 => self.undo_video(true),
            _ => {}
        }
    }
}

fn draw_timeline(ui: &mut egui::Ui, video: &mut VideoUi) {
    let frames = video.plan.frames();
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), 48.0),
        Sense::click_and_drag(),
    );
    let track = rect.shrink2(Vec2::new(8.0, 10.0));
    let x = |frame: u32| track.left() + frame as f32 / frames as f32 * track.width();
    ui.painter()
        .rect_filled(track, 3.0, Color32::from_rgb(201, 211, 214));
    ui.painter().rect_filled(
        Rect::from_min_max(
            Pos2::new(x(video.selection_start), track.top()),
            Pos2::new(x(video.selection_end), track.bottom()),
        ),
        2.0,
        Color32::from_rgb(158, 211, 202),
    );
    let mut offset = 0;
    for section in &video.plan.sections {
        offset += section.frames();
        ui.painter().line_segment(
            [
                Pos2::new(x(offset), track.top()),
                Pos2::new(x(offset), track.bottom()),
            ],
            Stroke::new(1.0_f32, Color32::WHITE),
        );
    }
    for handle in [video.selection_start, video.selection_end] {
        ui.painter().line_segment(
            [
                Pos2::new(x(handle), rect.top()),
                Pos2::new(x(handle), rect.bottom()),
            ],
            Stroke::new(3.0_f32, TEAL),
        );
    }
    ui.painter().line_segment(
        [
            Pos2::new(x(video.playhead), track.top()),
            Pos2::new(x(video.playhead), track.bottom()),
        ],
        Stroke::new(2.0_f32, INK),
    );
    if let Some(pointer) = response.interact_pointer_pos() {
        if response.drag_started() {
            video.dragging = Some(if (pointer.x - x(video.selection_start)).abs() < 10.0 {
                0
            } else if (pointer.x - x(video.selection_end)).abs() < 10.0 {
                1
            } else {
                2
            });
        }
        if response.dragged() || response.clicked() {
            let frame = (((pointer.x - track.left()) / track.width()).clamp(0.0, 1.0)
                * frames as f32)
                .round() as u32;
            match video.dragging.unwrap_or(2) {
                0 => video.selection_start = frame.min(video.selection_end - 1),
                1 => video.selection_end = frame.max(video.selection_start + 1).min(frames),
                _ => video.playhead = frame.min(frames - 1),
            }
        }
    }
    if response.drag_stopped() {
        video.dragging = None;
    }
}
