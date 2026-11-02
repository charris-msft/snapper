mod capture_ui;
mod video_ui;

use crate::{
    library::{self, Capture, CaptureKind, Library},
    media::{self, supervisor::Recorder, Phase, Progress, Sources},
    model::*,
    platform::{self, audio::AudioDevice, camera::CameraInfo, selection::WindowGuide},
    render::{RenderCancelled, Renderer},
};
use anyhow::{ensure, Context, Result};
use capture_ui::CaptureUi;
use eframe::egui::{self, Color32, Pos2, Rect, RichText, Sense, Stroke, TextureHandle, Vec2};
use image::RgbaImage;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{
    borrow::Cow,
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use video_ui::{VideoJobUi, VideoUi};

const TEAL: Color32 = Color32::from_rgb(0, 128, 124);
const INK: Color32 = Color32::from_rgb(31, 43, 61);
const RECENT_TEXTURE_LIMIT: usize = 24;

fn annotation_icon(painter: &egui::Painter, rect: Rect, tool: Tool, color: Color32) {
    let p = |x: f32, y: f32| rect.min + Vec2::new(x, y);
    let stroke = Stroke::new(1.5_f32, color);
    let line = |points: &[(f32, f32)]| {
        painter.add(egui::Shape::line(
            points.iter().map(|&(x, y)| p(x, y)).collect(),
            stroke,
        ));
    };
    match tool {
        Tool::Select => line(&[
            (3.0, 2.0),
            (3.0, 14.0),
            (7.0, 10.0),
            (11.0, 15.0),
            (13.0, 13.0),
            (9.0, 8.0),
            (15.0, 8.0),
            (3.0, 2.0),
        ]),
        Tool::Arrow => painter.arrow(p(2.0, 14.0), Vec2::new(12.0, -12.0), stroke),
        Tool::Text => {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "T",
                egui::FontId::proportional(17.0),
                color,
            );
        }
        Tool::Callout => {
            painter.rect_stroke(
                Rect::from_min_max(p(1.0, 2.0), p(15.0, 12.0)),
                3.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            line(&[(4.0, 12.0), (4.0, 16.0), (8.0, 12.0)]);
        }
        Tool::Rectangle => {
            painter.rect_stroke(rect.shrink(2.0), 1.0, stroke, egui::StrokeKind::Inside);
        }
        Tool::Ellipse => {
            painter.circle_stroke(rect.center(), 6.0, stroke);
        }
        Tool::Line => line(&[(2.0, 14.0), (14.0, 2.0)]),
        Tool::Stamp => line(&[(2.0, 8.0), (6.0, 13.0), (15.0, 3.0)]),
        Tool::Fill => {
            for (x, y) in [(2.0, 2.0), (7.0, 2.0), (7.0, 7.0), (12.0, 7.0), (7.0, 12.0)] {
                painter.rect_filled(Rect::from_min_size(p(x, y), Vec2::splat(4.0)), 0.0, color);
            }
        }
        Tool::Replace => {
            painter.circle_filled(p(4.0, 4.0), 3.0, color);
            painter.circle_stroke(p(12.0, 12.0), 3.0, stroke);
            painter.arrow(p(1.0, 12.0), Vec2::new(5.0, 0.0), stroke);
            painter.arrow(p(15.0, 4.0), Vec2::new(-5.0, 0.0), stroke);
        }
        Tool::Blur => {
            for radius in [6.0, 3.5, 1.0] {
                painter.circle_stroke(rect.center(), radius, stroke);
            }
        }
        Tool::Pixelate => {
            for y in 0..3 {
                for x in 0..3 {
                    if (x + y) % 2 == 0 {
                        painter.rect_filled(
                            Rect::from_min_size(
                                p(1.0 + x as f32 * 5.0, 1.0 + y as f32 * 5.0),
                                Vec2::splat(4.0),
                            ),
                            0.0,
                            color,
                        );
                    }
                }
            }
        }
        Tool::Highlight => {
            painter.line_segment([p(2.0, 12.0), p(14.0, 12.0)], Stroke::new(4.0_f32, color));
            line(&[(4.0, 3.0), (12.0, 3.0)]);
        }
        Tool::Eraser => line(&[
            (2.0, 11.0),
            (9.0, 2.0),
            (15.0, 7.0),
            (9.0, 15.0),
            (6.0, 15.0),
            (2.0, 11.0),
            (8.0, 11.0),
        ]),
        Tool::CutHorizontal => {
            line(&[(1.0, 4.0), (15.0, 4.0)]);
            line(&[(1.0, 12.0), (15.0, 12.0)]);
        }
        Tool::CutVertical => {
            line(&[(4.0, 1.0), (4.0, 15.0)]);
            line(&[(12.0, 1.0), (12.0, 15.0)]);
        }
        Tool::CutJagged => {
            line(&[(1.0, 5.0), (4.0, 2.0), (7.0, 6.0), (10.0, 2.0), (15.0, 5.0)]);
            line(&[
                (1.0, 13.0),
                (4.0, 10.0),
                (7.0, 14.0),
                (10.0, 10.0),
                (15.0, 13.0),
            ]);
        }
    }
}

#[derive(Default)]
struct ThumbnailCache {
    textures: HashMap<String, TextureHandle>,
    order: VecDeque<String>,
}

impl ThumbnailCache {
    fn insert(&mut self, ctx: &egui::Context, id: &str, image: &RgbaImage) -> Result<()> {
        ensure!(image.width() > 0 && image.height() > 0
            && image.width() <= library::THUMBNAIL_WIDTH
            && image.height() <= library::THUMBNAIL_HEIGHT,
            "Recent-cache input exceeds the thumbnail texture budget; the editor preview was not cached.");
        self.order.retain(|existing| existing != id);
        self.order.push_back(id.to_owned());
        self.textures.insert(
            id.to_owned(),
            texture(ctx, &format!("thumbnail:{id}"), image),
        );
        while self.order.len() > RECENT_TEXTURE_LIMIT {
            if let Some(oldest) = self.order.pop_front() {
                self.textures.remove(&oldest);
            }
        }
        Ok(())
    }

    fn get(&self, id: &str) -> Option<&TextureHandle> {
        self.textures.get(id)
    }

    fn clear(&mut self) {
        self.textures.clear();
        self.order.clear();
    }
}

struct RenderRequest {
    generation: u64,
    capture: Capture,
    document: Document,
    original: Arc<RgbaImage>,
}
struct RenderResult {
    generation: u64,
    image: Arc<RgbaImage>,
    preview: RgbaImage,
    thumbnail: RgbaImage,
    warning: Option<String>,
}

fn install_render_textures(
    ctx: &egui::Context,
    capture_id: &str,
    result: &RenderResult,
    thumbnails: &mut ThumbnailCache,
) -> Result<TextureHandle> {
    thumbnails.insert(ctx, capture_id, &result.thumbnail)?;
    Ok(texture(ctx, "current image", &result.preview))
}

#[derive(Default)]
struct RenderPublication {
    generation: AtomicU64,
    commit: Mutex<()>,
}

impl RenderPublication {
    fn request(&self, generation: u64) {
        let _commit = self.commit.lock().unwrap_or_else(|e| e.into_inner());
        self.generation.store(generation, Ordering::Release);
    }

    fn is_current(&self, generation: u64) -> bool {
        self.generation.load(Ordering::Acquire) == generation
    }

    fn publish(&self, mut staged: StagedRender) -> Result<RenderResult> {
        let _commit = self.commit.lock().unwrap_or_else(|e| e.into_inner());
        if !self.is_current(staged.result.generation) {
            return Err(RenderCancelled.into());
        }
        ensure!(Instant::now() < staged.deadline,
            "Prepared image exceeded its 30-second cooperative preparation budget; no new revision was published.");
        // All raster work, encoding, writes and flushes happened before this short rename boundary.
        staged
            .document
            .publish()
            .context("Publishing the prepared editable document")?;
        staged
            .capture
            .publish()
            .context("Editable document saved, but capture title/tag publication failed")?;
        if let Some(thumbnail) = staged.thumbnail {
            if let Err(error) = thumbnail.publish() {
                staged.result.warning = Some(format!(
                    "Edits are saved, but the recent thumbnail could not be published: {error:#}"
                ));
            }
        }
        Ok(staged.result)
    }
}

struct StagedRender {
    document: library::StagedFile,
    capture: library::StagedFile,
    thumbnail: Option<library::StagedFile>,
    result: RenderResult,
    deadline: Instant,
}

fn derivative_image(
    image: &RgbaImage,
    max_width: u32,
    max_height: u32,
    check: &dyn Fn() -> Result<()>,
) -> Result<RgbaImage> {
    check()?;
    let scale = (max_width as f64 / image.width() as f64)
        .min(max_height as f64 / image.height() as f64)
        .min(1.0);
    let width = ((image.width() as f64 * scale).floor() as u32).max(1);
    let height = ((image.height() as f64 * scale).floor() as u32).max(1);
    let mut output = RgbaImage::new(width, height);
    for y in 0..height {
        check()?;
        for x in 0..width {
            let pixel = image::imageops::sample_bilinear(
                image,
                (x as f32 + 0.5) / width as f32,
                (y as f32 + 0.5) / height as f32,
            )
            .context("Preview sampling coordinates were outside the source image")?;
            output.put_pixel(x, y, pixel);
        }
    }
    check()?;
    Ok(output)
}

fn stage_render(
    renderer: &Renderer,
    library: &Library,
    request: RenderRequest,
    publication: &RenderPublication,
) -> Result<StagedRender> {
    let generation = request.generation;
    let deadline = Instant::now() + Duration::from_secs(30);
    let check = || -> Result<()> {
        if !publication.is_current(generation) {
            return Err(RenderCancelled.into());
        }
        ensure!(Instant::now() < deadline,
            "Image preparation exceeded 30 seconds; the previous published revision remains available.");
        Ok(())
    };
    check()?;
    let pixels = renderer
        .render_cancellable(&request.original, &request.document, &|| {
            !publication.is_current(generation)
        })
        .context("Rendering did not complete; the immutable original is retained")?;
    let thumbnail_pixels = derivative_image(
        &pixels,
        library::THUMBNAIL_WIDTH,
        library::THUMBNAIL_HEIGHT,
        &check,
    )?;
    let preview = derivative_image(&pixels, 2048, 2048, &check)?;
    let directory = library.capture_dir(&request.capture)?;
    let document = library::stage_json(&directory.join("document.json"), &request.document)?;
    check()?;
    let capture = library::stage_json(&directory.join("capture.json"), &request.capture)?;
    check()?;
    let (thumbnail, warning) =
        match library::stage_image(&directory.join("thumbnail.png"), &thumbnail_pixels) {
            Ok(staged) => (Some(staged), None),
            Err(error) => (
                None,
                Some(format!(
                    "Edits are saved, but the recent thumbnail could not be staged: {error:#}"
                )),
            ),
        };
    check()?;
    Ok(StagedRender {
        document,
        capture,
        thumbnail,
        deadline,
        result: RenderResult {
            generation,
            image: Arc::new(pixels),
            preview,
            thumbnail: thumbnail_pixels,
            warning,
        },
    })
}

type RenderOutcome = std::result::Result<RenderResult, (u64, String)>;
struct RenderService {
    pending: Arc<Mutex<Option<RenderRequest>>>,
    wake: SyncSender<()>,
    result: Arc<Mutex<Option<RenderOutcome>>>,
    publication: Arc<RenderPublication>,
}
impl RenderService {
    fn new(library: Library, ctx: egui::Context) -> Result<Self> {
        let renderer = Renderer::new()?;
        let pending: Arc<Mutex<Option<RenderRequest>>> = Arc::new(Mutex::new(None));
        let result = Arc::new(Mutex::new(None));
        let publication = Arc::new(RenderPublication::default());
        let newest = publication.clone();
        let (wake, rx) = mpsc::sync_channel(1);
        let work = pending.clone();
        let result_slot = result.clone();
        thread::spawn(move || {
            while rx.recv().is_ok() {
                loop {
                    let request = work.lock().unwrap_or_else(|e| e.into_inner()).take();
                    let Some(request) = request else {
                        break;
                    };
                    let generation = request.generation;
                    let output = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                        || -> Result<RenderResult> {
                            newest.publish(stage_render(&renderer, &library, request, &newest)?)
                        },
                    ));
                    let output=match output {
                        Ok(Err(error)) if error.downcast_ref::<RenderCancelled>().is_some()=>continue,
                        Ok(result)=>result.map_err(|error|(generation,format!("{error:#}"))),
                        Err(_)=>Err((generation,"The image worker panicked. The original is retained; undo the last operation or reopen the saved capture.".into())),
                    };
                    *result_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(output);
                    ctx.request_repaint();
                }
            }
        });
        Ok(Self {
            pending,
            wake,
            result,
            publication,
        })
    }
    fn request(&self, request: RenderRequest) {
        let generation = request.generation;
        self.publication.request(generation);
        *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(request);
        match self.wake.try_send(()) {
            Ok(()) | Err(mpsc::TrySendError::Full(())) => {}
            Err(mpsc::TrySendError::Disconnected(())) => {
                self.pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take();
                *self.result.lock().unwrap_or_else(|e|e.into_inner())=Some(Err((generation,
                    "The image worker stopped before this edit was saved. The original and last saved document remain available.".into())));
            }
        }
    }
    fn take(&self) -> Option<RenderOutcome> {
        self.result.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

enum TaskResult {
    Listing(
        Vec<Capture>,
        Vec<(String, RgbaImage)>,
        Vec<PathBuf>,
        Vec<String>,
    ),
    Loaded(Capture, Option<(RgbaImage, Document)>),
    Devices(Vec<CameraInfo>, Vec<AudioDevice>),
    WindowGuides(Vec<WindowGuide>),
    OutputDirectory(PathBuf),
    VideoMetadataSaved(Capture),
    VideoLoaded(Capture, crate::video::VideoDocument),
    VideoPlanSaved(crate::video::VideoPlan),
    VideoRendered(crate::video::EditedPreview, Option<PathBuf>),
    VideoFrame(RgbaImage, u32, String),
    Notice(String),
}
struct Draft {
    annotation: Annotation,
    move_index: Option<usize>,
    start: Point,
}

pub struct Editor {
    _library_lock: std::fs::File,
    capture_ui: CaptureUi,
    window_handle: isize,
    graphics_label: String,
    renderer_fixture: bool,
    smoke_canvas: Option<SmokeCanvas>,
    library: Library,
    render: RenderService,
    task: Option<Receiver<std::result::Result<TaskResult, String>>>,
    captures: Vec<Capture>,
    thumbnails: ThumbnailCache,
    recovery: Vec<PathBuf>,
    current: Option<Capture>,
    original: Option<Arc<RgbaImage>>,
    document: Option<Document>,
    flattened: Option<Arc<RgbaImage>>,
    texture: Option<TextureHandle>,
    generation: u64,
    saved_generation: u64,
    history: History,
    selected: Option<usize>,
    draft: Option<Draft>,
    tool: Tool,
    style: Style,
    text: String,
    source_color: [u8; 4],
    swap_colors: bool,
    picking: bool,
    styles: Vec<QuickStyle>,
    styles_error: Option<String>,
    favorites_only: bool,
    style_name: String,
    tags: String,
    page_library: bool,
    filter: String,
    zoom: f32,
    fit: bool,
    show_effects: bool,
    show_recent: bool,
    status: String,
    error: Option<String>,
    recorder: Option<Recorder>,
    last_progress: Progress,
    pending_open: Option<String>,
    refresh_pending: bool,
    hide_for_capture: bool,
    close_warning: bool,
    property_gesture: bool,
    video_metadata_dirty: bool,
    video: Option<VideoUi>,
    video_job: Option<VideoJobUi>,
}

fn native_icon() -> egui::IconData {
    let mut rgba = Vec::with_capacity(32 * 32 * 4);
    for y in 0i32..32 {
        for x in 0i32..32 {
            let corner_x = x.min(31 - x);
            let corner_y = y.min(31 - y);
            let inside = corner_x >= 7
                || corner_y >= 7
                || (7 - corner_x).pow(2) + (7 - corner_y).pow(2) <= 49;
            let mark = ((6..=8).contains(&corner_x) && (6..=13).contains(&corner_y))
                || ((6..=8).contains(&corner_y) && (6..=13).contains(&corner_x));
            rgba.extend(if !inside {
                [0, 0, 0, 0]
            } else if mark {
                [255, 255, 255, 255]
            } else {
                [0, 128, 124, 255]
            });
        }
    }
    egui::IconData {
        rgba,
        width: 32,
        height: 32,
    }
}

fn native_options() -> eframe::NativeOptions {
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([560.0, 680.0])
            .with_min_inner_size([480.0, 440.0])
            .with_title("Snapper")
            .with_icon(Arc::new(native_icon())),
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: crate::graphics::configuration(),
        ..Default::default()
    }
}

pub fn run(root: PathBuf) -> Result<()> {
    let options = native_options();
    eframe::run_native(
        "Snapper",
        options,
        Box::new(move |cc| Ok(Box::new(Editor::new(cc, root)?))),
    )
    .map_err(|e| anyhow::anyhow!("Native window startup failed: {e}"))
}

impl Editor {
    fn new(cc: &eframe::CreationContext<'_>, root: PathBuf) -> Result<Self> {
        let graphics = cc
            .wgpu_render_state
            .as_ref()
            .context("Direct3D UI render state was not initialized")?;
        let info = graphics.adapter.get_info();
        let graphics_label = format!(
            "D3D12: {}{}",
            info.name,
            if info.device_type == eframe::wgpu::DeviceType::Cpu {
                " (software/WARP)"
            } else {
                ""
            }
        );
        let window_handle = match cc.window_handle()?.as_raw() {
            RawWindowHandle::Win32(handle) => handle.hwnd.get(),
            _ => anyhow::bail!("Snapper requires a native Win32 window."),
        };
        let mut visuals = egui::Visuals::light();
        visuals.panel_fill = Color32::WHITE;
        visuals.window_fill = Color32::from_rgb(248, 250, 252);
        visuals.extreme_bg_color = Color32::from_rgb(238, 242, 246);
        visuals.override_text_color = Some(INK);
        visuals.selection.bg_fill = TEAL;
        visuals.selection.stroke = Stroke::new(1.0_f32, Color32::WHITE);
        cc.egui_ctx.set_visuals(visuals);
        cc.egui_ctx.style_mut(|s| {
            s.spacing.item_spacing = Vec2::new(9.0, 9.0);
            s.spacing.button_padding = Vec2::new(12.0, 7.0);
            s.text_styles
                .insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
            s.text_styles
                .insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
        });
        let library = Library::open(root)?;
        let library_lock = library::exclusive_lock(&library.root.join("application.lock"))?;
        let (styles,styles_error)=match library.styles() {
            Ok(styles)=>(styles,None),
            Err(error)=>(default_styles(),Some(format!(
                "Saved Quick Styles could not be read: {error:#}. Built-in styles are displayed without overwriting styles.json. Restore that file and reopen the app to save style changes."))),
        };
        let capture_ui = CaptureUi::new(&cc.egui_ctx, &library, window_handle)?;
        let render = RenderService::new(library.clone(), cc.egui_ctx.clone())?;
        let startup_error = match (styles_error.clone(), capture_ui.startup_error.clone()) {
            (Some(a), Some(b)) => Some(format!("{a}\n{b}")),
            (a, b) => a.or(b),
        };
        let show_recent = capture_ui.preferences.show_recent;
        let mut app = Self {
            _library_lock: library_lock,
            capture_ui,
            window_handle,
            graphics_label,
            renderer_fixture: false,
            smoke_canvas: None,
            library,
            render,
            task: None,
            captures: vec![],
            thumbnails: ThumbnailCache::default(),
            recovery: vec![],
            current: None,
            original: None,
            document: None,
            flattened: None,
            texture: None,
            generation: 0,
            saved_generation: 0,
            history: History::default(),
            selected: None,
            draft: None,
            tool: Tool::Arrow,
            style: Style::default(),
            text: "Note".into(),
            source_color: [255, 255, 255, 255],
            swap_colors: false,
            picking: false,
            styles,
            styles_error: styles_error.clone(),
            favorites_only: false,
            style_name: String::new(),
            tags: String::new(),
            page_library: false,
            filter: String::new(),
            zoom: 1.0,
            fit: true,
            show_effects: false,
            show_recent,
            status: "Local by design. Nothing is uploaded.".into(),
            error: startup_error,
            recorder: None,
            last_progress: Progress::default(),
            pending_open: None,
            refresh_pending: false,
            hide_for_capture: false,
            close_warning: false,
            property_gesture: false,
            video_metadata_dirty: false,
            video: None,
            video_job: None,
        };
        app.refresh(&cc.egui_ctx);
        Ok(app)
    }
    fn busy(&self) -> bool {
        self.task.is_some()
    }
    fn unsaved(&self) -> bool {
        self.generation != self.saved_generation
            || self.video_metadata_dirty
            || self.video.as_ref().is_some_and(|video| video.dirty)
    }
    fn editable(&self) -> bool {
        self.document.is_some()
    }
    fn task(
        &mut self,
        ctx: &egui::Context,
        work: impl FnOnce() -> Result<TaskResult> + Send + 'static,
    ) {
        if self.busy() {
            self.status = "Another library/export operation is still running.".into();
            return;
        }
        let (tx, rx) = mpsc::sync_channel(1);
        let ctx = ctx.clone();
        thread::spawn(move || {
            let result = work().map_err(|e| format!("{e:#}"));
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        self.task = Some(rx);
    }
    fn refresh(&mut self, ctx: &egui::Context) {
        if self.busy() {
            self.refresh_pending = true;
            return;
        }
        self.refresh_pending = false;
        let library = self.library.clone();
        self.task(ctx, move || {
            let (captures, mut warnings) = library.list()?;
            let mut thumbnails = Vec::new();
            for capture in captures.iter().take(RECENT_TEXTURE_LIMIT) {
                let path = library.capture_dir(capture)?.join("thumbnail.png");
                match library::load_thumbnail(&path) {
                    Ok(image) => thumbnails.push((capture.id.clone(), image)),
                    Err(e) => warnings.push(format!(
                        "Thumbnail unavailable for {}: {e:#}",
                        capture.title
                    )),
                }
            }
            let (recovery, recovery_warnings) = media::encoder::incomplete(&library)?;
            warnings.extend(recovery_warnings);
            Ok(TaskResult::Listing(
                captures, thumbnails, recovery, warnings,
            ))
        });
    }
    fn open_capture(&mut self, ctx: &egui::Context, capture: Capture) {
        if self.unsaved() {
            self.pending_open = Some(capture.id);
            self.status = "Finishing the current image save before switching.".into();
            return;
        }
        if self.busy() {
            self.pending_open = Some(capture.id);
            return;
        }
        let library = self.library.clone();
        let video_control = Arc::new(crate::video::VideoControl::default());
        if capture.kind == CaptureKind::Video {
            self.video_job = Some(VideoJobUi {
                control: video_control.clone(),
            });
        }
        self.task(ctx, move || {
            if capture.kind == CaptureKind::Video {
                let document =
                    crate::video::VideoDocument::open(&library, &capture, &video_control)?;
                return Ok(TaskResult::VideoLoaded(capture, document));
            }
            let data = if capture.kind == CaptureKind::Image {
                Some((
                    library::load_image(&library.capture_dir(&capture)?.join("original.png"))?,
                    library.load_document(&capture)?,
                ))
            } else {
                None
            };
            Ok(TaskResult::Loaded(capture, data))
        });
    }
    fn queue_render(&mut self) {
        if let (Some(capture), Some(document), Some(original)) =
            (&self.current, &self.document, &self.original)
        {
            self.generation += 1;
            self.render.request(RenderRequest {
                generation: self.generation,
                capture: capture.clone(),
                document: document.clone(),
                original: original.clone(),
            });
            self.status = "Rendering and saving edits locally...".into();
        }
    }
    fn checkpoint(&mut self) {
        if let Some(document) = &self.document {
            self.history.checkpoint(document);
        }
    }
    fn change_tool(&mut self, tool: Tool) {
        self.tool = tool;
        self.selected = None;
        self.picking = false;
    }
    fn commit_annotation(&mut self, annotation: Annotation) {
        if self
            .document
            .as_ref()
            .is_some_and(|d| d.layers.len() >= MAX_LAYERS)
        {
            self.error = Some(
                "The 512-layer budget is full. Export a flattened PNG and reopen it to continue."
                    .into(),
            );
            return;
        }
        self.checkpoint();
        if let Some(document) = &mut self.document {
            document.layers.push(annotation);
            self.selected = Some(document.layers.len() - 1);
        }
        self.queue_render();
    }
    fn undo(&mut self, redo: bool) {
        if self.video.is_some() {
            self.undo_video(redo);
            return;
        }
        if let Some(document) = &mut self.document {
            let changed = if redo {
                self.history.redo(document)
            } else {
                self.history.undo(document)
            };
            if changed {
                self.selected = None;
                self.queue_render();
            }
        }
    }
    fn load_poll(&mut self, ctx: &egui::Context) {
        if let Some(result) = self.render.take() {
            match result {
                Ok(result) if result.generation == self.generation => {
                    if let Some(capture) = &self.current {
                        match install_render_textures(
                            ctx,
                            &capture.id,
                            &result,
                            &mut self.thumbnails,
                        ) {
                            Ok(preview) => self.texture = Some(preview),
                            Err(error) => self.error = Some(format!("{error:#}")),
                        }
                    }
                    self.flattened = Some(result.image);
                    self.saved_generation = result.generation;
                    self.status = "Edits saved locally.".into();
                    if let Some(warning) = result.warning {
                        self.error = Some(warning);
                    }
                }
                Ok(_) => {}
                Err((generation, e)) if generation == self.generation => {
                    self.error = Some(e);
                }
                Err(_) => {}
            }
        }
        let result = self.task.as_ref().and_then(|rx| match rx.try_recv() {
            Ok(r) => Some(r),
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err("Background operation ended without a result.".into()))
            }
            Err(mpsc::TryRecvError::Empty) => None,
        });
        if let Some(result) = result {
            self.task = None;
            self.video_job = None;
            match result {
                Ok(TaskResult::Listing(captures, thumbs, recovery, warnings)) => {
                    self.captures = captures;
                    self.recovery = recovery;
                    self.thumbnails.clear();
                    for (id, image) in thumbs {
                        if let Err(error) = self.thumbnails.insert(ctx, &id, &image) {
                            self.error = Some(format!("{error:#}"));
                        }
                    }
                    if !warnings.is_empty() {
                        self.error = Some(warnings.join("\n"));
                    }
                }
                Ok(TaskResult::Loaded(capture, data)) => {
                    self.video = None;
                    self.tags = capture.tags.join(", ");
                    self.current = Some(capture);
                    self.selected = None;
                    self.history = History::default();
                    self.page_library = false;
                    self.texture = None;
                    self.flattened = None;
                    self.fit = true;
                    self.video_metadata_dirty = false;
                    self.show_editor(ctx);
                    if let Some((original, document)) = data {
                        self.original = Some(Arc::new(original));
                        self.document = Some(document);
                        self.queue_render();
                    } else {
                        self.original = None;
                        self.document = None;
                    }
                }
                Ok(TaskResult::Devices(cameras, audio)) => {
                    self.capture_ui.receive_devices(cameras, audio);
                    self.status =
                        "Device names refreshed. No camera or audio source was opened.".into();
                }
                Ok(TaskResult::WindowGuides(windows)) => {
                    self.capture_ui.receive_windows(windows);
                    self.status = "Choose a window to guide a fixed screen region.".into();
                }
                Ok(TaskResult::OutputDirectory(path)) => {
                    self.capture_ui.preferences.capture.output_directory = Some(path);
                }
                Ok(TaskResult::VideoMetadataSaved(saved)) => {
                    self.video_metadata_dirty = self.current.as_ref().is_some_and(|current| {
                        current.id == saved.id
                            && (current.title != saved.title || current.tags != saved.tags)
                    });
                    self.status = "Video title and tags saved locally.".into();
                    self.refresh_pending = true;
                }
                Ok(TaskResult::VideoLoaded(capture, document)) => {
                    self.tags = capture.tags.join(", ");
                    self.current = Some(capture);
                    self.video = Some(VideoUi::new(document));
                    self.original = None;
                    self.document = None;
                    self.flattened = None;
                    self.texture = None;
                    self.selected = None;
                    self.video_metadata_dirty = false;
                    self.page_library = false;
                    self.show_editor(ctx);
                    self.status = "Video ready. Edits are nondestructive; update the edited preview before playback/export.".into();
                }
                Ok(TaskResult::VideoPlanSaved(plan)) => {
                    if let Some(video) = &mut self.video {
                        video.dirty = video.plan != plan;
                        if video.dirty {
                            video.pending_save = true;
                        }
                    }
                    self.status =
                        "Video edit instructions saved. Original recording unchanged.".into();
                }
                Ok(TaskResult::VideoRendered(preview, destination)) => {
                    self.install_video_result(ctx, preview, destination)
                }
                Ok(TaskResult::VideoFrame(image, frame, fingerprint)) => {
                    self.install_video_frame(ctx, image, frame, fingerprint)
                }
                Ok(TaskResult::Notice(message)) => {
                    self.status = message;
                    self.refresh_pending = true;
                }
                Err(error) => self.error = Some(error),
            }
        }
        if let Some(recorder) = &self.recorder {
            self.last_progress = recorder.progress();
            if recorder.finished() {
                let progress = recorder.progress();
                self.recorder = None;
                self.capture_finished(ctx, &progress);
                if progress.phase == Phase::Saved {
                    self.refresh_pending = true;
                    self.status = progress.detail.clone();
                } else {
                    self.error = Some(progress.detail.clone());
                    self.refresh_pending = true;
                }
                self.last_progress = progress;
            }
        }
        if self.refresh_pending && !self.busy() {
            self.refresh(ctx);
        }
        if !self.busy() && !self.unsaved() {
            if let Some(id) = self.pending_open.take() {
                if let Some(capture) = self.captures.iter().find(|c| c.id == id).cloned() {
                    self.open_capture(ctx, capture);
                }
            }
        }
        self.poll_delivery(ctx);
    }
    fn import(&mut self, ctx: &egui::Context) {
        if self.unsaved() {
            self.error =
                Some("Wait for the current image to finish saving before importing.".into());
            return;
        }
        let library = self.library.clone();
        self.task(ctx, move || {
            let Some(path) = rfd::FileDialog::new()
                .add_filter("Images", &["png", "jpg", "jpeg"])
                .pick_file()
            else {
                return Ok(TaskResult::Notice("Import cancelled.".into()));
            };
            let image = library::load_image(&path)?;
            let title = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Imported image");
            let capture = library.import_image(&image, title)?;
            let document = library.load_document(&capture)?;
            Ok(TaskResult::Loaded(capture, Some((image, document))))
        });
    }
    fn export(&mut self, ctx: &egui::Context) {
        let Some(image) = self.flattened.clone() else {
            return;
        };
        self.task(ctx, move || {
            let Some(path) = rfd::FileDialog::new()
                .add_filter("PNG", &["png"])
                .add_filter("JPEG", &["jpg"])
                .set_file_name("capture.png")
                .save_file()
            else {
                return Ok(TaskResult::Notice("Export cancelled.".into()));
            };
            library::export_image(&path, &image)?;
            Ok(TaskResult::Notice(format!(
                "Flattened image exported to {}.",
                path.display()
            )))
        });
    }
    fn copy(&mut self, ctx: &egui::Context) {
        let Some(image) = self.flattened.clone() else {
            return;
        };
        self.task(ctx, move || {
            arboard::Clipboard::new()?
                .set_image(arboard::ImageData {
                    width: image.width() as usize,
                    height: image.height() as usize,
                    bytes: Cow::Borrowed(image.as_raw()),
                })
                .context("Copying the flattened bitmap to the Windows clipboard")?;
            Ok(TaskResult::Notice(
                "Flattened image copied to the clipboard.".into(),
            ))
        });
    }
    fn paste(&mut self, ctx: &egui::Context) {
        if self.unsaved() {
            return;
        }
        let library = self.library.clone();
        self.task(ctx, move || {
            let mut clipboard = arboard::Clipboard::new()?;
            let data = clipboard
                .get_image()
                .context("Clipboard does not contain an image")?;
            validate_size(data.width as u32, data.height as u32)?;
            let image = RgbaImage::from_raw(
                data.width as u32,
                data.height as u32,
                data.bytes.into_owned(),
            )
            .context("Clipboard bitmap size is invalid")?;
            let capture = library.import_image(&image, "Clipboard image")?;
            let document = library.load_document(&capture)?;
            Ok(TaskResult::Loaded(capture, Some((image, document))))
        });
    }
    fn header(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("brand")
            .exact_height(60.0)
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.add_space(12.0);
                    ui.label(RichText::new("Snapper").size(23.0).strong().color(TEAL));
                    ui.add_space(14.0);
                    ui.label(RichText::new("LOCAL ONLY").small().color(TEAL));
                    ui.separator();
                    if ui
                        .add_enabled(
                            !self.unsaved() && !self.busy(),
                            egui::Button::new("New capture"),
                        )
                        .clicked()
                    {
                        self.show_launcher(ctx);
                    }
                    ui.selectable_value(&mut self.page_library, false, "Editor");
                    ui.selectable_value(&mut self.page_library, true, "Library");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let ready = self.flattened.is_some() && !self.unsaved() && !self.busy();
                        if ui
                            .add_enabled(
                                ready,
                                egui::Button::new("Export image")
                                    .fill(TEAL)
                                    .sense(Sense::click()),
                            )
                            .clicked()
                        {
                            self.export(ctx);
                        }
                        if ui
                            .add_enabled(ready, egui::Button::new("Copy image"))
                            .clicked()
                        {
                            self.copy(ctx);
                        }
                        if ui
                            .add_enabled(!self.busy(), egui::Button::new("Import"))
                            .clicked()
                        {
                            self.import(ctx);
                        }
                    });
                });
            });
        if self.video.is_some() && !self.page_library {
            return;
        }
        egui::SidePanel::left("tools")
            .exact_width(146.0)
            .resizable(false)
            .show(ctx, |ui| {
                ui.vertical(|ui| {
                    ui.add_enabled_ui(self.editable() && !self.page_library, |ui| {
                        for tool in Tool::TOOLS {
                            let label = match tool {
                                Tool::CutHorizontal => "Cut H",
                                Tool::CutVertical => "Cut V",
                                Tool::CutJagged => "Jagged cut",
                                Tool::Replace => "Color replace",
                                _ => tool.label(),
                            };
                            let selected = self.tool == tool;
                            let response = ui
                                .selectable_label(selected, format!("     {label}"))
                                .on_hover_text(tool.label());
                            let color = if !ui.is_enabled() {
                                Color32::GRAY
                            } else if selected {
                                Color32::WHITE
                            } else {
                                INK
                            };
                            annotation_icon(
                                ui.painter(),
                                Rect::from_center_size(
                                    Pos2::new(
                                        response.rect.left() + 12.0,
                                        response.rect.center().y,
                                    ),
                                    Vec2::splat(18.0),
                                ),
                                tool,
                                color,
                            );
                            if response.clicked() {
                                self.change_tool(tool);
                            }
                        }
                        ui.separator();
                        if ui
                            .add_enabled(self.history.can_undo(), egui::Button::new("Undo"))
                            .clicked()
                        {
                            self.undo(false);
                        }
                        if ui
                            .add_enabled(self.history.can_redo(), egui::Button::new("Redo"))
                            .clicked()
                        {
                            self.undo(true);
                        }
                    });
                });
            });
    }
    fn properties(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("properties").exact_width(268.0).resizable(false).show(ctx,|ui|{
            egui::ScrollArea::vertical().show(ui,|ui|{
                ui.add_space(10.0);ui.heading("Inspector");
                ui.horizontal(|ui|{
                    ui.selectable_value(&mut self.favorites_only,false,"All");
                    ui.selectable_value(&mut self.favorites_only,true,"Favorites");
                });
                let styles=self.styles.clone();
                let favorites_only=self.favorites_only;
                egui::ComboBox::from_id_salt("saved annotation styles").width(240.0)
                    .selected_text(if favorites_only{"Choose a favorite style"}else{"Choose a saved style"})
                    .show_ui(ui,|ui|{
                        for (i,s) in styles.iter().enumerate().filter(|(_,s)|!favorites_only||s.favorite) {
                            ui.horizontal(|ui|{
                                if ui.selectable_label(false,format!("{} - {}",s.name,s.tool.label())).clicked(){
                                    self.change_tool(s.tool);self.style=s.style.clone();
                                }
                                if ui.add_enabled(!self.busy(),egui::Button::new(if s.favorite{"Unpin"}else{"Pin"})).clicked(){
                                    self.styles[i].favorite = !s.favorite;self.persist_styles(ctx);
                                }
                            });
                        }
                    });
                ui.add(egui::TextEdit::singleline(&mut self.style_name).hint_text("New style name").char_limit(80));
                if ui.add_enabled(!self.busy()&&!self.style_name.trim().is_empty()&&self.tool!=Tool::Select,
                    egui::Button::new("Save current style")).clicked(){
                    let style=QuickStyle{name:self.style_name.trim().to_owned(),tool:self.tool,style:self.style.clone(),favorite:true};
                    if self.styles.len()>=128{self.error=Some("The 128-style budget is full.".into());}
                    else if let Err(e)=style.validate(){self.error=Some(format!("{e:#}"));}
                    else{self.styles.push(style);self.style_name.clear();self.persist_styles(ctx);}
                }
                ui.add_space(10.0);ui.separator();ui.heading("Tool properties");
                let old_style=self.style.clone();let old_text=self.text.clone();
                let old_source=self.source_color;let old_swap=self.swap_colors;
                ui.label(self.selected.and_then(|i|self.document.as_ref()?.layers.get(i)).map(|a|a.tool).unwrap_or(self.tool).label());
                ui.horizontal(|ui|{ui.label("Color");ui.color_edit_button_srgba_unmultiplied(&mut self.style.color);});
                ui.checkbox(&mut self.style.shadow,"Shadow");
                ui.add(egui::Slider::new(&mut self.style.width,0.5..=80.0).text("Width"));
                ui.add(egui::Slider::new(&mut self.style.opacity,0.0..=1.0).text("Opacity"));
                ui.checkbox(&mut self.style.filled,"Filled shape");
                ui.horizontal(|ui|{ui.label("Fill");ui.color_edit_button_srgba_unmultiplied(&mut self.style.fill);});
                egui::ComboBox::from_id_salt("line style").selected_text(format!("{:?}",self.style.line)).show_ui(ui,|ui|{
                    for value in [LineStyle::Solid,LineStyle::Dashed,LineStyle::Dotted]{ui.selectable_value(&mut self.style.line,value,format!("{value:?}"));}
                });
                ui.horizontal(|ui|{
                    ui.label("Start");
                    egui::ComboBox::from_id_salt("start cap").selected_text(format!("{:?}",self.style.start)).show_ui(ui,|ui|{
                        for value in [EndStyle::None,EndStyle::Arrow,EndStyle::Circle,EndStyle::Square]{ui.selectable_value(&mut self.style.start,value,format!("{value:?}"));}
                    });
                });
                ui.horizontal(|ui|{
                    ui.label("End");
                    egui::ComboBox::from_id_salt("end cap").selected_text(format!("{:?}",self.style.end)).show_ui(ui,|ui|{
                        for value in [EndStyle::None,EndStyle::Arrow,EndStyle::Circle,EndStyle::Square]{ui.selectable_value(&mut self.style.end,value,format!("{value:?}"));}
                    });
                });
                ui.add(egui::Slider::new(&mut self.style.start_size,0.5..=12.0).text("Start size"));
                ui.add(egui::Slider::new(&mut self.style.end_size,0.5..=12.0).text("End size"));
                ui.separator();
                ui.label("Text / callout / number stamp");
                ui.add(egui::TextEdit::multiline(&mut self.text).desired_rows(3).char_limit(1024));
                ui.add(egui::Slider::new(&mut self.style.font_size,8.0..=160.0).text("Text size"));
                egui::ComboBox::from_id_salt("stamp").selected_text(format!("{:?}",self.style.stamp)).show_ui(ui,|ui|{
                    for value in [Stamp::Check,Stamp::Cross,Stamp::Star,Stamp::Number]{ui.selectable_value(&mut self.style.stamp,value,format!("{value:?}"));}
                });
                ui.separator();ui.label("Fill / color replacement");
                ui.add(egui::Slider::new(&mut self.style.tolerance,0..=100).text("Tolerance"));
                ui.horizontal(|ui|{ui.label("From");ui.color_edit_button_srgba_unmultiplied(&mut self.source_color);});
                if ui.selectable_label(self.picking,"Pick source color on image").clicked(){self.picking = !self.picking;}
                ui.checkbox(&mut self.swap_colors,"Swap both colors simultaneously");
                ui.small("Fill changes one connected region. Color replacement changes the dragged selection, or the whole image on a click.");
                ui.add(egui::Slider::new(&mut self.style.effect_size,2..=64).text("Blur / pixel size"));
                ui.small("Blur and pixelation are visual effects, not secure redaction.");
                let changed=old_style!=self.style||old_text!=self.text||old_source!=self.source_color||old_swap!=self.swap_colors;
                if changed {
                    if let Some(index)=self.selected {
                        if !self.property_gesture{self.checkpoint();}
                        if let Some(a)=self.document.as_mut().and_then(|d|d.layers.get_mut(index)){
                            a.style=self.style.clone();a.text=self.text.clone();a.source_color=self.source_color;a.swap_colors=self.swap_colors;
                        }
                        self.queue_render();
                    }
                }
                self.property_gesture=ctx.input(|i|i.pointer.any_down())&&(self.property_gesture||changed);
                if ui.add_enabled(self.selected.is_some(),egui::Button::new("Delete selected annotation")).clicked(){self.delete_selected();}
                ui.separator();
                ui.checkbox(&mut self.show_effects,"Image effects / properties");
                if self.show_effects {
                    let before=self.document.clone();
                    if let Some(document)=&mut self.document {
                        ui.checkbox(&mut document.grayscale,"Grayscale");
                        ui.add(egui::Slider::new(&mut document.border,0..=60).text("Border"));
                        ui.color_edit_button_srgba_unmultiplied(&mut document.border_color);
                        ui.label(format!("Original: {} x {}",document.width,document.height));
                        ui.label(format!("{} editable layers",document.layers.len()));
                    }
                    if before!=self.document {
                        if let Some(before)=before{self.history.checkpoint(&before);}
                        self.queue_render();
                    }
                }
            });
        });
    }
    fn persist_styles(&mut self, ctx: &egui::Context) {
        if let Some(error) = &self.styles_error {
            self.error = Some(error.clone());
            return;
        }
        let library = self.library.clone();
        let styles = self.styles.clone();
        self.task(ctx, move || {
            library.save_styles(&styles)?;
            Ok(TaskResult::Notice(
                "Quick styles and Favorites saved.".into(),
            ))
        });
    }
    fn delete_selected(&mut self) {
        if let Some(index) = self.selected {
            self.checkpoint();
            if let Some(d) = &mut self.document {
                if index < d.layers.len() {
                    d.layers.remove(index);
                }
            }
            self.selected = None;
            self.queue_render();
        }
    }
    fn footer(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("footer").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.checkbox(&mut self.show_recent, "Recent").changed() {
                    self.save_preferences();
                }
                if let Some(capture) = &self.current {
                    ui.label(&capture.title);
                }
                ui.separator();
                if ui.button("Fit").clicked() {
                    self.fit = true;
                }
                let mut percent = (self.zoom * 100.0).round() as u32;
                if ui
                    .add(
                        egui::DragValue::new(&mut percent)
                            .range(10..=800)
                            .suffix("%"),
                    )
                    .changed()
                {
                    self.zoom = percent as f32 / 100.0;
                    self.fit = false;
                }
                if let Some(image) = &self.flattened {
                    ui.label(format!("{} x {} px", image.width(), image.height()));
                }
                if self.unsaved() {
                    ui.spinner();
                    ui.label("Saving...");
                } else {
                    ui.label(RichText::new("Local").color(TEAL));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Effects / Properties").clicked() {
                        self.show_effects = !self.show_effects;
                    }
                });
            });
            ui.small(&self.status);
            ui.small(&self.graphics_label);
        });
        if self.show_recent {
            egui::TopBottomPanel::bottom("recent")
                .exact_height(111.0)
                .show(ctx, |ui| {
                    egui::ScrollArea::horizontal().show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let captures =
                                self.captures.iter().take(24).cloned().collect::<Vec<_>>();
                            for capture in captures {
                                ui.vertical(|ui| {
                                    let response =
                                        if let Some(texture) = self.thumbnails.get(&capture.id) {
                                            ui.add(
                                                egui::Image::new(texture)
                                                    .fit_to_exact_size(Vec2::new(120.0, 75.0))
                                                    .sense(Sense::click()),
                                            )
                                        } else {
                                            ui.add_sized(
                                                [120.0, 75.0],
                                                egui::Button::new(
                                                    if capture.kind == CaptureKind::Video {
                                                        "Video"
                                                    } else {
                                                        "Image"
                                                    },
                                                ),
                                            )
                                        };
                                    if response.clicked() {
                                        self.open_capture(ctx, capture.clone());
                                    }
                                    ui.small(capture.title.chars().take(18).collect::<String>());
                                });
                            }
                        });
                    });
                });
        }
    }
    fn central(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().frame(egui::Frame::new().fill(Color32::from_rgb(238,242,246)).inner_margin(16.0)).show(ctx,|ui|{
            if let Some(error)=self.error.clone() {
                egui::Frame::new().fill(Color32::from_rgb(255,235,234)).inner_margin(12.0).show(ui,|ui|{
                    ui.horizontal(|ui|{
                        ui.label(RichText::new("Action needed").strong().color(Color32::from_rgb(150,31,43)));
                        if ui.small_button("Dismiss").clicked(){self.error=None;}
                    });
                    ui.label(error);
                });
                ui.add_space(8.0);
            }
            if self.page_library{self.library_view(ui,ctx);return;}
            if self.current.as_ref().is_some_and(|c|c.kind==CaptureKind::Video) {
                egui::ScrollArea::vertical().show(ui, |ui| self.video_view(ui,ctx));return;
            }
            if self.texture.is_none() {
                ui.vertical_centered(|ui|{
                    ui.add_space((ui.available_height()*0.22).max(10.0));
                    ui.heading(RichText::new("Capture it. Keep control.").size(30.0));
                    ui.add_space(10.0);
                    ui.label("Screenshots, recordings, and focused editing. Everything stays on this PC.");
                    ui.add_space(16.0);
                    if ui.add_enabled(!self.busy(),egui::Button::new("Open a PNG or JPEG")).clicked(){self.import(ctx);}
                    if ui.add_enabled(!self.busy(),egui::Button::new("Paste clipboard image")).clicked(){self.paste(ctx);}
                    ui.add_space(16.0);ui.small("Choose New capture to select a screen region and review sources.");
                    if self.unsaved(){ui.spinner();}
                });
                return;
            }
            if let Some(capture)=&mut self.current {
                let before=capture.clone();
                ui.horizontal(|ui|{
                    ui.add(egui::TextEdit::singleline(&mut capture.title).desired_width(220.0).char_limit(100));
                    ui.label("Tags");
                    ui.add(egui::TextEdit::singleline(&mut self.tags).hint_text("work, idea, bug").desired_width(200.0).char_limit(1024));
                });
                capture.tags=self.tags.split(',').map(str::trim).filter(|s|!s.is_empty()).take(32)
                    .map(|s|s.chars().take(20).collect()).collect();
                if before.title!=capture.title||before.tags!=capture.tags{self.queue_render();}
            }
            ui.add_space(8.0);
            self.canvas(ui,ctx);
        });
    }
    fn library_view(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.heading("Your library");
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Search title or tag"));
            if ui
                .add_enabled(!self.busy(), egui::Button::new("Refresh"))
                .clicked()
            {
                self.refresh(ctx);
            }
        });
        let recovery = self.recovery.clone();
        if !recovery.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new("Interrupted recordings").strong());
            for directory in recovery {
                ui.horizontal(|ui| {
                    ui.label(directory.file_name().unwrap_or_default().to_string_lossy());
                    if ui
                        .add_enabled(
                            !self.busy() && self.recorder.is_none(),
                            egui::Button::new("Recover verified segments"),
                        )
                        .clicked()
                    {
                        self.task(ctx, move || {
                            let capture = media::encoder::recover(&directory)?;
                            Ok(TaskResult::Notice(format!(
                                "Recovered recording {}.",
                                capture.id
                            )))
                        });
                    }
                });
            }
            ui.separator();
        }
        let filter = self.filter.to_ascii_lowercase();
        let captures: Vec<_> = self
            .captures
            .iter()
            .filter(|c| {
                filter.is_empty()
                    || c.title.to_ascii_lowercase().contains(&filter)
                    || c.tags
                        .iter()
                        .any(|t| t.to_ascii_lowercase().contains(&filter))
            })
            .cloned()
            .collect();
        ui.small(format!(
            "{} captures. Originals and editable projects stay local.",
            captures.len()
        ));
        egui::ScrollArea::vertical().show_rows(ui, 86.0, captures.len(), |ui, range| {
            for index in range {
                let capture = &captures[index];
                egui::Frame::new()
                    .fill(Color32::WHITE)
                    .inner_margin(8.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if let Some(texture) = self.thumbnails.get(&capture.id) {
                                if ui
                                    .add(
                                        egui::Image::new(texture)
                                            .fit_to_exact_size(Vec2::new(104.0, 65.0))
                                            .sense(Sense::click()),
                                    )
                                    .clicked()
                                {
                                    self.open_capture(ctx, capture.clone());
                                }
                            }
                            ui.vertical(|ui| {
                                if ui.button(RichText::new(&capture.title).strong()).clicked() {
                                    self.open_capture(ctx, capture.clone());
                                }
                                ui.small(format!(
                                    "{:?} | {} x {} | {}",
                                    capture.kind,
                                    capture.width,
                                    capture.height,
                                    capture.tags.join(", ")
                                ));
                            });
                        });
                    });
            }
        });
    }
    fn video_view(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if let Some(current) = &mut self.current {
            let before = (current.title.clone(), current.tags.clone());
            ui.horizontal(|ui| {
                ui.label("Title");
                ui.add(egui::TextEdit::singleline(&mut current.title).char_limit(100));
                ui.label("Tags");
                ui.add(
                    egui::TextEdit::singleline(&mut self.tags)
                        .hint_text("work, idea, bug")
                        .char_limit(1024),
                );
            });
            current.tags = self
                .tags
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .take(32)
                .map(|s| s.chars().take(20).collect())
                .collect();
            self.video_metadata_dirty |= before != (current.title.clone(), current.tags.clone());
        }
        let Some(capture) = self.current.clone() else {
            return;
        };
        if ui
            .add_enabled(
                self.video_metadata_dirty && !self.busy(),
                egui::Button::new("Save title and tags"),
            )
            .clicked()
        {
            let library = self.library.clone();
            let saved = capture.clone();
            self.task(ctx, move || {
                library.save_capture(&saved)?;
                Ok(TaskResult::VideoMetadataSaved(saved))
            });
        }
        ui.heading("Video edits");
        self.focused_video_view(ui, ctx);
    }
    fn canvas(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(image) = self.flattened.as_ref() else {
            return;
        };
        let (width, height) = (image.width(), image.height());
        if self.fit {
            self.zoom = (ui.available_width() / width as f32)
                .min(ui.available_height() / height as f32)
                .clamp(0.05, 1.0);
        }
        let size = Vec2::new(width as f32 * self.zoom, height as f32 * self.zoom);
        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let (space, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
                let painter = ui.painter_at(space);
                painter.rect_filled(space, 0.0, Color32::from_rgb(209, 214, 220));
                let (columns, rows) = checkerboard_cells(space, ui.clip_rect());
                for y in rows {
                    for x in columns.clone() {
                        if (x + y) % 2 == 0 {
                            painter.rect_filled(
                                Rect::from_min_size(
                                    space.min + Vec2::new(x as f32 * 16.0, y as f32 * 16.0),
                                    Vec2::splat(16.0),
                                ),
                                0.0,
                                Color32::from_rgb(231, 235, 239),
                            );
                        }
                    }
                }
                if let Some(texture) = &self.texture {
                    painter.image(
                        texture.id(),
                        space,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                    if self.renderer_fixture {
                        self.smoke_canvas = self.current.as_ref().map(|capture| SmokeCanvas {
                            capture_id: capture.id.clone(),
                            generation: self.saved_generation,
                            source_size: [width, height],
                            texture_size: texture.size(),
                            draw_rect: space,
                            clip_rect: painter.clip_rect(),
                            viewport: ctx.screen_rect(),
                            pixels_per_point: ctx.pixels_per_point(),
                        });
                    }
                }
                let to_point = |pos: Pos2| Point {
                    x: ((pos.x - space.min.x) / self.zoom).clamp(0.0, width as f32 - 0.01),
                    y: ((pos.y - space.min.y) / self.zoom).clamp(0.0, height as f32 - 0.01),
                };
                let position = response.interact_pointer_pos().map(to_point);
                if response.drag_started() {
                    let start = ctx
                        .input(|i| i.pointer.press_origin())
                        .map(to_point)
                        .or(position);
                    if let Some(start) = start {
                        if self.tool == Tool::Select {
                            let index = self
                                .document
                                .as_ref()
                                .and_then(|d| d.layers.iter().rposition(|a| a.contains(start)));
                            self.select(index);
                            if let Some(annotation) = index
                                .and_then(|i| self.document.as_ref()?.layers.get(i))
                                .cloned()
                            {
                                self.draft = Some(Draft {
                                    annotation,
                                    move_index: index,
                                    start,
                                });
                            }
                        } else {
                            self.draft = Some(Draft {
                                annotation: self.annotation(start, start),
                                move_index: None,
                                start,
                            });
                        }
                    }
                }
                if response.dragged() {
                    if let (Some(draft), Some(point)) = (&mut self.draft, position) {
                        if draft.move_index.is_none() {
                            draft.annotation.b = point;
                            if self.tool == Tool::Eraser && draft.annotation.path.len() < 2048 {
                                let last =
                                    draft.annotation.path.last().copied().unwrap_or(draft.start);
                                if (last.x - point.x).hypot(last.y - point.y) > 1.5 {
                                    draft.annotation.path.push(point);
                                }
                            }
                        }
                    }
                }
                if let Some(draft) = &self.draft {
                    let a = &draft.annotation;
                    let delta = if draft.move_index.is_some() {
                        position
                            .map(|p| Vec2::new(p.x - draft.start.x, p.y - draft.start.y))
                            .unwrap_or_default()
                    } else {
                        Vec2::ZERO
                    };
                    let (x0, y0, x1, y1) = a.bounds();
                    let r = Rect::from_min_max(
                        space.min + Vec2::new(x0 + delta.x, y0 + delta.y) * self.zoom,
                        space.min + Vec2::new(x1 + delta.x, y1 + delta.y) * self.zoom,
                    );
                    painter.rect_stroke(
                        r,
                        0.0,
                        Stroke::new(2.0_f32, TEAL),
                        egui::StrokeKind::Inside,
                    );
                    if matches!(a.tool, Tool::Arrow | Tool::Line) {
                        painter.line_segment(
                            [
                                space.min + Vec2::new(a.a.x, a.a.y) * self.zoom,
                                space.min + Vec2::new(a.b.x, a.b.y) * self.zoom,
                            ],
                            Stroke::new(2.0_f32, TEAL),
                        );
                    }
                    if a.tool == Tool::Eraser {
                        for pair in a.path.windows(2) {
                            painter.line_segment(
                                [
                                    space.min + Vec2::new(pair[0].x, pair[0].y) * self.zoom,
                                    space.min + Vec2::new(pair[1].x, pair[1].y) * self.zoom,
                                ],
                                Stroke::new(
                                    a.style.width * self.zoom,
                                    Color32::from_white_alpha(150),
                                ),
                            );
                        }
                    }
                }
                if response.drag_stopped() {
                    if let Some(mut draft) = self.draft.take() {
                        if let Some(index) = draft.move_index {
                            if let Some(point) = position {
                                let delta = Point {
                                    x: point.x - draft.start.x,
                                    y: point.y - draft.start.y,
                                };
                                draft.annotation.a.x += delta.x;
                                draft.annotation.a.y += delta.y;
                                draft.annotation.b.x += delta.x;
                                draft.annotation.b.y += delta.y;
                                for p in &mut draft.annotation.path {
                                    p.x += delta.x;
                                    p.y += delta.y;
                                }
                                self.checkpoint();
                                if let Some(a) =
                                    self.document.as_mut().and_then(|d| d.layers.get_mut(index))
                                {
                                    *a = draft.annotation;
                                }
                                self.queue_render();
                            }
                        } else {
                            self.commit_annotation(draft.annotation);
                        }
                    }
                } else if response.clicked() {
                    if let Some(point) = position {
                        if self.picking {
                            if let Some(image) = &self.flattened {
                                self.source_color =
                                    image.get_pixel(point.x as u32, point.y as u32).0;
                            }
                            self.picking = false;
                        } else if self.tool == Tool::Select {
                            let index = self
                                .document
                                .as_ref()
                                .and_then(|d| d.layers.iter().rposition(|a| a.contains(point)));
                            self.select(index);
                        } else if matches!(
                            self.tool,
                            Tool::Fill
                                | Tool::Replace
                                | Tool::Eraser
                                | Tool::Text
                                | Tool::Callout
                                | Tool::Stamp
                        ) {
                            let b = Point {
                                x: (point.x
                                    + if self.tool == Tool::Stamp {
                                        80.0
                                    } else {
                                        280.0
                                    })
                                .min(width as f32),
                                y: (point.y + if self.tool == Tool::Stamp { 80.0 } else { 90.0 })
                                    .min(height as f32),
                            };
                            let mut a = self.annotation(point, b);
                            if self.tool == Tool::Replace {
                                a.a = Point::default();
                                a.b = Point {
                                    x: width as f32,
                                    y: height as f32,
                                };
                            }
                            if self.tool == Tool::Eraser {
                                a.b = point;
                            }
                            self.commit_annotation(a);
                        } else {
                            self.status = "Drag on the image to place this tool.".into();
                        }
                    }
                }
                if let Some(annotation) = self
                    .selected
                    .and_then(|i| self.document.as_ref()?.layers.get(i))
                {
                    let (x0, y0, x1, y1) = annotation.bounds();
                    painter.rect_stroke(
                        Rect::from_min_max(
                            space.min + Vec2::new(x0, y0) * self.zoom,
                            space.min + Vec2::new(x1, y1) * self.zoom,
                        ),
                        0.0,
                        Stroke::new(1.0_f32, TEAL),
                        egui::StrokeKind::Outside,
                    );
                }
            });
    }
    fn annotation(&self, a: Point, b: Point) -> Annotation {
        Annotation {
            tool: self.tool,
            a,
            b,
            style: self.style.clone(),
            text: self.text.clone(),
            source_color: self.source_color,
            swap_colors: self.swap_colors,
            path: if self.tool == Tool::Eraser {
                vec![a]
            } else {
                vec![]
            },
        }
    }
    fn select(&mut self, index: Option<usize>) {
        self.selected = index;
        if let Some(a) = index.and_then(|i| self.document.as_ref()?.layers.get(i)) {
            self.style = a.style.clone();
            self.text = a.text.clone();
            self.source_color = a.source_color;
            self.swap_colors = a.swap_colors;
        }
    }
}
impl eframe::App for Editor {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.smoke_canvas = None;
        self.poll_capture_flow(ctx);
        self.load_poll(ctx);
        if !self.capture_ui.launcher && self.recorder.is_none() && !ctx.wants_keyboard_input() {
            if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Z)) {
                self.undo(ctx.input(|i| i.modifiers.shift));
            }
            if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Y)) {
                self.undo(true);
            }
            if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::C))
                && !self.unsaved()
                && !self.busy()
            {
                self.copy(ctx);
            }
            if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::V)) && !self.busy() {
                self.paste(ctx);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Delete)) {
                self.delete_selected();
            }
        }
        if self.recorder.is_some() {
            self.recording_dock(ctx);
        } else if self.capture_ui.launcher || self.capture_ui.flow.active() {
            self.capture_surface(ctx);
        } else {
            self.header(ctx);
            self.footer(ctx);
            if self.video.is_none() || self.page_library {
                self.properties(ctx);
            }
            self.central(ctx);
        }
        self.poll_video(ctx);
        self.video_loading_status(ctx);
        if ctx.input(|i| i.viewport().close_requested()) && self.capture_ui.flow.active() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.cancel_capture_flow(ctx);
        }
        self.admit_capture_timer(ctx);
        if ctx.input(|i| i.viewport().close_requested())
            && (self.recorder.is_some() || self.unsaved() || self.busy())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_warning = true;
        }
        if self.close_warning {
            egui::Window::new("Finish before closing").collapsible(false).resizable(false).show(ctx,|ui|{
                ui.label("A recording, image save, or library operation is still active. Wait for it to finish before closing.");
                if ui.button("Keep working").clicked(){self.close_warning=false;}
                if let Some(recorder)=&self.recorder {
                    if ui.button("Stop recording").clicked(){recorder.stop();self.close_warning=false;}
                }else if self.unsaved()&&self.error.is_some()&&ui.button("Close and discard unsaved changes").clicked(){
                    self.saved_generation=self.generation;
                    if let Some(video)=&mut self.video{video.dirty=false;}
                    self.video_metadata_dirty=false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        }
        if self.busy() || self.unsaved() || self.recorder.is_some() || self.capture_ui.flow.active()
        {
            ctx.request_repaint_after(Duration::from_millis(33));
        }
    }
}
const SMOKE_WIDTH: u32 = 640;
const SMOKE_HEIGHT: u32 = 360;
const SMOKE_TILE_WIDTH: u32 = 80;
const SMOKE_TILE_HEIGHT: u32 = 60;
const SMOKE_COLOR_TOLERANCE: u8 = 8;

#[derive(Clone, Debug, PartialEq)]
struct SmokeCanvas {
    capture_id: String,
    generation: u64,
    source_size: [u32; 2],
    texture_size: [usize; 2],
    draw_rect: Rect,
    clip_rect: Rect,
    viewport: Rect,
    pixels_per_point: f32,
}

fn smoke_tile_color(column: u32, row: u32) -> [u8; 4] {
    match (column, row) {
        (0, 0) => [224, 64, 72, 255],
        (7, 0) => [230, 174, 32, 255],
        (0, 5) => [136, 64, 208, 255],
        (7, 5) => [32, 180, 208, 255],
        _ if (column + row) % 2 == 0 => [30, 180, 120, 255],
        _ => [30, 80, 170, 255],
    }
}

fn smoke_fixture_image() -> RgbaImage {
    RgbaImage::from_fn(SMOKE_WIDTH, SMOKE_HEIGHT, |x, y| {
        image::Rgba(smoke_tile_color(
            x / SMOKE_TILE_WIDTH,
            y / SMOKE_TILE_HEIGHT,
        ))
    })
}

fn verify_smoke_canvas(
    image: &egui::ColorImage,
    canvas: &SmokeCanvas,
    fixture_id: &str,
) -> Result<serde_json::Value> {
    ensure!(
        canvas.capture_id == fixture_id && canvas.generation > 0,
        "The main-canvas draw does not belong to the generated fixture."
    );
    ensure!(
        canvas.source_size == [SMOKE_WIDTH, SMOKE_HEIGHT]
            && canvas.texture_size == [SMOKE_WIDTH as usize, SMOKE_HEIGHT as usize],
        "The main canvas does not contain the expected full-size fixture texture."
    );
    let ppp = canvas.pixels_per_point;
    ensure!(
        ppp.is_finite() && ppp > 0.0,
        "Invalid main-canvas pixels-per-point."
    );
    for rect in [canvas.draw_rect, canvas.clip_rect, canvas.viewport] {
        ensure!(
            rect.is_positive()
                && [rect.min.x, rect.min.y, rect.max.x, rect.max.y]
                    .iter()
                    .all(|value| value.is_finite()),
            "Invalid main-canvas draw geometry."
        );
    }
    ensure!(
        canvas.draw_rect.width() >= 320.0 && canvas.draw_rect.height() >= 180.0,
        "The main fixture is thumbnail-sized rather than a usable editor canvas."
    );
    ensure!(
        (canvas.draw_rect.height()
            - canvas.draw_rect.width() * SMOKE_HEIGHT as f32 / SMOKE_WIDTH as f32)
            .abs()
            <= 1.0,
        "The main fixture draw rectangle has the wrong aspect ratio."
    );
    let rounding = 0.5 / ppp;
    ensure!(
        canvas
            .clip_rect
            .expand(rounding)
            .contains_rect(canvas.draw_rect)
            && canvas
                .viewport
                .expand(rounding)
                .contains_rect(canvas.draw_rect),
        "The main fixture canvas is clipped or outside the root viewport."
    );
    ensure!(
        image.size[0] >= 800
            && image.size[1] >= 500
            && image.size[0].checked_mul(image.size[1]) == Some(image.pixels.len()),
        "Native framebuffer dimensions are invalid or unexpectedly small."
    );
    let expected_size = [
        (canvas.viewport.width() * ppp).round() as usize,
        (canvas.viewport.height() * ppp).round() as usize,
    ];
    ensure!(
        image.size[0].abs_diff(expected_size[0]) <= 1
            && image.size[1].abs_diff(expected_size[1]) <= 1,
        "Framebuffer dimensions do not match the requested main-canvas DPI/viewport geometry."
    );

    // Only samples inside the actual main painter.image rectangle count, never Recent or chrome.
    let mut samples = 0;
    for row in 0..6 {
        for column in 0..8 {
            let expected = smoke_tile_color(column, row);
            for dy in [-8.0, 0.0, 8.0] {
                for dx in [-10.0, 0.0, 10.0] {
                    let source = Vec2::new(
                        (column * SMOKE_TILE_WIDTH) as f32 + 40.0 + dx,
                        (row * SMOKE_TILE_HEIGHT) as f32 + 30.0 + dy,
                    );
                    let point = canvas.draw_rect.min
                        + Vec2::new(
                            source.x / SMOKE_WIDTH as f32 * canvas.draw_rect.width(),
                            source.y / SMOKE_HEIGHT as f32 * canvas.draw_rect.height(),
                        );
                    let pixel = (point - canvas.viewport.min) * ppp;
                    let x = pixel.x.floor() as usize;
                    let y = pixel.y.floor() as usize;
                    ensure!(
                        x < image.size[0] && y < image.size[1],
                        "A main-canvas fixture sample falls outside the framebuffer."
                    );
                    let actual = image.pixels[y * image.size[0] + x].to_array();
                    ensure!((0..4).all(|channel|
                        actual[channel].abs_diff(expected[channel]) <= SMOKE_COLOR_TOLERANCE),
                        "Main-canvas fixture tile ({column},{row}) at framebuffer ({x},{y}) was {actual:?}, expected {expected:?} +/- {SMOKE_COLOR_TOLERANCE}. Recent thumbnails and UI colors cannot satisfy this check.");
                    samples += 1;
                }
            }
        }
    }
    let rect_values = |rect: Rect| [rect.min.x, rect.min.y, rect.max.x, rect.max.y];
    let pixel_rect = Rect::from_min_max(
        Pos2::ZERO + (canvas.draw_rect.min - canvas.viewport.min) * ppp,
        Pos2::ZERO + (canvas.draw_rect.max - canvas.viewport.min) * ppp,
    );
    Ok(serde_json::json!({
        "oracle":"main-canvas-tile-pattern-v2", "capture_id":fixture_id,
        "generation":canvas.generation, "source_pixels":canvas.source_size,
        "texture_pixels":canvas.texture_size, "draw_rect_points":rect_values(canvas.draw_rect),
        "clip_rect_points":rect_values(canvas.clip_rect),
        "draw_rect_framebuffer_pixels":rect_values(pixel_rect), "pixels_per_point":ppp,
        "tiles_verified":48, "interior_samples_verified":samples,
        "channel_tolerance":SMOKE_COLOR_TOLERANCE, "asymmetric_corner_markers_verified":4,
        "recent_and_chrome_excluded":true
    }))
}

struct SmokeRequest {
    token: egui::UserData,
    canvas: SmokeCanvas,
    frame: u32,
}

struct GuiSmoke {
    editor: Editor,
    fixture_id: String,
    report: PathBuf,
    adapter: serde_json::Value,
    started: Instant,
    frames: u32,
    request: Option<SmokeRequest>,
    outcome: Arc<Mutex<Option<std::result::Result<serde_json::Value, String>>>>,
}

impl GuiSmoke {
    fn current_canvas(&self) -> Result<SmokeCanvas> {
        ensure!(
            self.editor.renderer_fixture
                && self
                    .editor
                    .capture_ui
                    .preferences
                    .capture
                    .effective_sources()
                    == Sources::default()
                && self.editor.recorder.is_none()
                && !self.editor.capture_ui.flow.active(),
            "GUI-only proof must not activate capture sources."
        );
        ensure!(
            !self.editor.page_library
                && self
                    .editor
                    .current
                    .as_ref()
                    .is_some_and(|capture| capture.id == self.fixture_id),
            "The generated fixture must be open on the main Editor page, not Library."
        );
        ensure!(
            self.editor.document.as_ref() == Some(&Document::new(SMOKE_WIDTH, SMOKE_HEIGHT)?)
                && !self.editor.unsaved()
                && self.editor.draft.is_none()
                && self.editor.selected.is_none(),
            "The renderer fixture was edited or replaced before main-canvas verification."
        );
        let canvas = self
            .editor
            .smoke_canvas
            .as_ref()
            .context("The main editor canvas did not draw its fixture image this frame")?;
        ensure!(
            canvas.capture_id == self.fixture_id
                && canvas.generation == self.editor.saved_generation,
            "The main-canvas draw is from another capture or revision."
        );
        Ok(canvas.clone())
    }
}

impl eframe::App for GuiSmoke {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, input: &mut egui::RawInput) {
        // Fixture-only verification cannot navigate/edit/import; OS close/resize events stay intact.
        input
            .events
            .retain(|event| matches!(event, egui::Event::Screenshot { .. }));
        input.dropped_files.clear();
        input.hovered_files.clear();
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.editor.update(ctx, frame);
        self.frames += 1;
        if self
            .outcome
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        let screenshot = ctx.input(|input| {
            input.events.iter().find_map(|event| {
                if let egui::Event::Screenshot {
                    viewport_id,
                    user_data,
                    image,
                } = event
                {
                    if *viewport_id == egui::ViewportId::ROOT
                        && self
                            .request
                            .as_ref()
                            .is_some_and(|request| request.token == *user_data)
                    {
                        return Some(image.clone());
                    }
                }
                None
            })
        });
        if let Some(image) = screenshot {
            let result=(||->Result<serde_json::Value>{
                let request=self.request.as_ref().context("No main-canvas screenshot was requested")?;
                ensure!(request.frame>=4,"GUI screenshot arrived before the rendering fixture was ready.");
                ensure!(self.current_canvas()?==request.canvas,
                    "Main-canvas layout, DPI, or fixture changed while readback was pending; repeat the test.");
                let canvas_proof=verify_smoke_canvas(&image,&request.canvas,&self.fixture_id)?;
                let pixels=image.pixels.iter().flat_map(|pixel|pixel.to_array()).collect::<Vec<_>>();
                let pixels=RgbaImage::from_raw(image.size[0] as u32,image.size[1] as u32,pixels)
                    .context("Invalid GUI framebuffer size")?;
                let snapshot=self.report.with_extension("png");
                library::export_image(&snapshot,&pixels)?;
                Ok(serde_json::json!({"passed":true,"kind":"native-window-and-framebuffer-smoke",
                    "build":media::selftest::build_identity()?,"renderer":self.adapter,
                    "frames":self.frames,"framebuffer_pixels":image.size,"snapshot":snapshot,
                    "screenshot_request_frame":request.frame,"main_canvas":canvas_proof,
                    "capture_sources_opened":false,"fixture_only":true,
                    "elapsed_ms":self.started.elapsed().as_millis()}))
            })().map_err(|error|format!("{error:#}"));
            *self.outcome.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if self.started.elapsed() > Duration::from_secs(20) {
            *self.outcome.lock().unwrap_or_else(|e|e.into_inner())=Some(Err(
                "Actual native-window/framebuffer proof exceeded 20 seconds; no capture device was activated.".into()));
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if self.request.is_none()
            && self.frames >= 4
            && !self.editor.unsaved()
            && !self.editor.busy()
            && self.editor.flattened.is_some()
        {
            match self.current_canvas() {
                Ok(canvas) => {
                    let token = egui::UserData::new(self.frames);
                    self.request = Some(SmokeRequest {
                        token: token.clone(),
                        canvas,
                        frame: self.frames,
                    });
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(token));
                }
                Err(error) => {
                    *self.outcome.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(Err(format!("{error:#}")));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        ctx.request_repaint_after(Duration::from_millis(30));
    }
}

pub fn gui_smoke_test(report: PathBuf) -> Result<()> {
    let report = if report.is_absolute() {
        report
    } else {
        std::env::current_dir()?.join(report)
    };
    let directory = report
        .parent()
        .context("GUI report needs a parent directory")?;
    std::fs::create_dir_all(directory)?;
    let root = directory.join(format!("gui-fixture-{}", library::id()));
    let library = Library::open(root.clone())?;
    let image = smoke_fixture_image();
    let capture = library.import_image(&image, "Generated GUI renderer fixture")?;
    let outcome = Arc::new(Mutex::new(None));
    let output = outcome.clone();
    let target = report.clone();
    let mut options = native_options();
    options.viewport = options
        .viewport
        .with_active(false)
        .with_title("Snapper renderer verification")
        .with_inner_size([1440.0, 940.0])
        .with_min_inner_size([1050.0, 720.0]);
    let started = Instant::now();
    let launch = eframe::run_native(
        "Snapper renderer verification",
        options,
        Box::new(move |cc| {
            let adapter = crate::graphics::adapter_report(
                &cc.wgpu_render_state
                    .as_ref()
                    .context("Direct3D renderer state missing")?
                    .adapter,
            );
            let mut editor = Editor::new(cc, root)?;
            editor.renderer_fixture = true;
            editor.pending_open = Some(capture.id.clone());
            Ok(Box::new(GuiSmoke {
                editor,
                fixture_id: capture.id,
                report: target,
                adapter,
                started,
                frames: 0,
                request: None,
                outcome: output,
            }))
        }),
    )
    .map_err(|error| anyhow::anyhow!("Actual native-window startup failed: {error}"));
    let result = launch.and_then(|_| {
        outcome
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .context("GUI verification window closed before a framebuffer was verified")?
            .map_err(anyhow::Error::msg)
    });
    match result {
        Ok(evidence) => {
            library::write_json(&report, &evidence)?;
            println!("{}", serde_json::to_string_pretty(&evidence)?);
            Ok(())
        }
        Err(error) => {
            library::write_json(
                &report,
                &serde_json::json!({"passed":false,"kind":"native-window-and-framebuffer-smoke",
                "build":media::selftest::build_identity()?,"error":format!("{error:#}"),"capture_sources_opened":false}),
            )?;
            Err(error)
        }
    }
}

fn checkerboard_cells(
    canvas: Rect,
    viewport: Rect,
) -> (std::ops::Range<i32>, std::ops::Range<i32>) {
    let visible = canvas.intersect(viewport);
    if !visible.is_positive() {
        return (0..0, 0..0);
    }
    let start = (visible.min - canvas.min) / 16.0;
    let end = (visible.max - canvas.min) / 16.0;
    (
        start.x.floor() as i32..end.x.ceil() as i32,
        start.y.floor() as i32..end.y.ceil() as i32,
    )
}

fn texture(ctx: &egui::Context, name: &str, image: &RgbaImage) -> TextureHandle {
    ctx.load_texture(
        name,
        egui::ColorImage::from_rgba_unmultiplied(
            [image.width() as usize, image.height() as usize],
            image.as_raw(),
        ),
        egui::TextureOptions::LINEAR,
    )
}
fn open_path(path: &Path) -> Result<()> {
    ensure!(path.exists(), "The selected local file no longer exists.");
    std::process::Command::new("explorer.exe")
        .arg(path)
        .spawn()
        .context("Opening the local file in Windows")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn smoke_test_canvas(ppp: f32, zoom: f32) -> SmokeCanvas {
        let viewport = Rect::from_min_size(Pos2::new(17.0, 23.0), Vec2::new(1280.0, 840.0));
        SmokeCanvas {
            capture_id: "generated-smoke-fixture".into(),
            generation: 3,
            source_size: [SMOKE_WIDTH, SMOKE_HEIGHT],
            texture_size: [SMOKE_WIDTH as usize, SMOKE_HEIGHT as usize],
            draw_rect: Rect::from_min_size(
                viewport.min + Vec2::new(269.25, 131.5),
                Vec2::new(SMOKE_WIDTH as f32, SMOKE_HEIGHT as f32) * zoom,
            ),
            clip_rect: Rect::from_min_size(
                viewport.min + Vec2::new(250.0, 120.0),
                Vec2::new(760.0, 450.0),
            ),
            viewport,
            pixels_per_point: ppp,
        }
    }

    fn smoke_test_frame(canvas: &SmokeCanvas) -> egui::ColorImage {
        egui::ColorImage::new(
            [
                (canvas.viewport.width() * canvas.pixels_per_point).round() as usize,
                (canvas.viewport.height() * canvas.pixels_per_point).round() as usize,
            ],
            Color32::from_rgb(238, 242, 246),
        )
    }

    fn paint_smoke_test_fixture(
        frame: &mut egui::ColorImage,
        canvas: &SmokeCanvas,
        rect: Rect,
        rotate: bool,
    ) {
        let fixture = smoke_fixture_image();
        let ppp = canvas.pixels_per_point;
        let min = (rect.min - canvas.viewport.min) * ppp;
        let max = (rect.max - canvas.viewport.min) * ppp;
        for y in min.y.floor() as usize..max.y.ceil() as usize {
            for x in min.x.floor() as usize..max.x.ceil() as usize {
                let point =
                    canvas.viewport.min + Vec2::new((x as f32 + 0.5) / ppp, (y as f32 + 0.5) / ppp);
                if !rect.contains(point) {
                    continue;
                }
                let u = ((point.x - rect.min.x) / rect.width()).clamp(0.0, 0.999999);
                let v = ((point.y - rect.min.y) / rect.height()).clamp(0.0, 0.999999);
                let sx = (u * SMOKE_WIDTH as f32) as u32;
                let sy = (v * SMOKE_HEIGHT as f32) as u32;
                let pixel = if rotate {
                    fixture.get_pixel(SMOKE_WIDTH - 1 - sx, SMOKE_HEIGHT - 1 - sy)
                } else {
                    fixture.get_pixel(sx, sy)
                };
                frame.pixels[y * frame.size[0] + x] =
                    Color32::from_rgba_unmultiplied(pixel[0], pixel[1], pixel[2], pixel[3]);
            }
        }
    }

    #[test]
    fn smoke_oracle_verifies_main_canvas_pattern_at_scaled_and_high_dpi_geometry() {
        let fixture = smoke_fixture_image();
        assert_eq!(fixture.get_pixel(40, 30).0, [224, 64, 72, 255]);
        assert_eq!(fixture.get_pixel(600, 30).0, [230, 174, 32, 255]);
        assert_eq!(fixture.get_pixel(40, 330).0, [136, 64, 208, 255]);
        assert_eq!(fixture.get_pixel(600, 330).0, [32, 180, 208, 255]);
        assert_eq!(fixture.get_pixel(120, 90).0, [30, 180, 120, 255]);
        assert_eq!(fixture.get_pixel(200, 90).0, [30, 80, 170, 255]);
        for (ppp, zoom) in [(1.0, 1.0), (1.5, 0.625), (2.0, 0.75)] {
            let canvas = smoke_test_canvas(ppp, zoom);
            let mut frame = smoke_test_frame(&canvas);
            paint_smoke_test_fixture(&mut frame, &canvas, canvas.draw_rect, false);
            let proof = verify_smoke_canvas(&frame, &canvas, &canvas.capture_id).unwrap();
            assert_eq!(proof["tiles_verified"], 48);
            assert_eq!(proof["interior_samples_verified"], 432);
            assert_eq!(proof["pixels_per_point"], ppp);
            assert_eq!(proof["recent_and_chrome_excluded"], true);
        }
    }

    #[test]
    fn smoke_oracle_rejects_missing_or_wrong_main_canvas_even_with_a_valid_recent_thumbnail() {
        let canvas = smoke_test_canvas(1.5, 0.75);
        let mut frame = smoke_test_frame(&canvas);
        let recent = Rect::from_min_size(
            canvas.viewport.min + Vec2::new(300.0, 700.0),
            Vec2::new(120.0, 75.0),
        );
        paint_smoke_test_fixture(&mut frame, &canvas, recent, false);
        let teal = frame
            .pixels
            .iter()
            .filter(|pixel| pixel.r() < 80 && pixel.g() > 90 && pixel.b() > 80)
            .count();
        let colorful = frame
            .pixels
            .iter()
            .filter(|pixel| pixel.r().abs_diff(pixel.b()) > 40)
            .count();
        assert!(
            teal > 100 && colorful > 1000,
            "The negative control must satisfy the rejected whole-window oracle."
        );
        assert!(verify_smoke_canvas(&frame, &canvas, &canvas.capture_id).is_err());

        let mut uniform_main = frame.clone();
        for pixel in &mut uniform_main.pixels {
            *pixel = Color32::from_rgb(30, 180, 120);
        }
        paint_smoke_test_fixture(&mut uniform_main, &canvas, recent, false);
        assert!(verify_smoke_canvas(&uniform_main, &canvas, &canvas.capture_id).is_err());

        paint_smoke_test_fixture(&mut frame, &canvas, canvas.draw_rect, true);
        assert!(
            verify_smoke_canvas(&frame, &canvas, &canvas.capture_id).is_err(),
            "A rotated checkerboard must fail the asymmetric main-canvas corner markers."
        );
    }

    #[test]
    fn smoke_oracle_rejects_thumbnail_geometry_clipping_and_mismatched_readback() {
        let canvas = smoke_test_canvas(1.5, 0.75);
        let mut frame = smoke_test_frame(&canvas);
        paint_smoke_test_fixture(&mut frame, &canvas, canvas.draw_rect, false);
        assert!(verify_smoke_canvas(&frame, &canvas, "different-capture").is_err());
        let mut clipped = canvas.clone();
        clipped.clip_rect.max.x = clipped.draw_rect.center().x;
        assert!(verify_smoke_canvas(&frame, &clipped, &canvas.capture_id).is_err());
        let mut thumbnail = canvas.clone();
        thumbnail.draw_rect = Rect::from_min_size(canvas.draw_rect.min, Vec2::new(120.0, 67.5));
        assert!(verify_smoke_canvas(&frame, &thumbnail, &canvas.capture_id).is_err());
        let mut incorrect_texture = canvas.clone();
        incorrect_texture.texture_size = [240, 150];
        assert!(verify_smoke_canvas(&frame, &incorrect_texture, &canvas.capture_id).is_err());
        let mut incorrect_dpi = canvas.clone();
        incorrect_dpi.pixels_per_point = 1.0;
        assert!(verify_smoke_canvas(&frame, &incorrect_dpi, &canvas.capture_id).is_err());
        let mut outside = canvas.clone();
        outside.draw_rect = outside.draw_rect.translate(Vec2::new(1000.0, 0.0));
        assert!(verify_smoke_canvas(&frame, &outside, &canvas.capture_id).is_err());
    }

    #[test]
    fn smoke_oracle_enforces_the_declared_per_channel_tolerance() {
        let canvas = smoke_test_canvas(1.5, 0.75);
        let mut frame = smoke_test_frame(&canvas);
        paint_smoke_test_fixture(&mut frame, &canvas, canvas.draw_rect, false);
        for pixel in &mut frame.pixels {
            *pixel = Color32::from_rgb(
                pixel.r().saturating_add(8),
                pixel.g().saturating_add(8),
                pixel.b().saturating_add(8),
            );
        }
        assert!(verify_smoke_canvas(&frame, &canvas, &canvas.capture_id).is_ok());
        let sample = (canvas.draw_rect.min - canvas.viewport.min
            + Vec2::new(
                canvas.draw_rect.width() * 120.0 / 640.0,
                canvas.draw_rect.height() * 90.0 / 360.0,
            ))
            * canvas.pixels_per_point;
        let index = sample.y.floor() as usize * frame.size[0] + sample.x.floor() as usize;
        frame.pixels[index] = Color32::from_rgb(39, 188, 128);
        assert!(verify_smoke_canvas(&frame, &canvas, &canvas.capture_id).is_err());
    }

    #[test]
    fn a_staged_revision_cannot_publish_after_supersession_even_when_the_new_render_fails() {
        use std::sync::Barrier;
        let temporary = tempfile::tempdir().unwrap();
        let library = Library::open(temporary.path().to_owned()).unwrap();
        let original = Arc::new(RgbaImage::from_pixel(
            120,
            80,
            image::Rgba([70, 120, 180, 255]),
        ));
        let capture = library.import_image(&original, "Last committed").unwrap();
        let baseline = library.load_document(&capture).unwrap();
        let baseline_capture =
            std::fs::read(library.capture_dir(&capture).unwrap().join("capture.json")).unwrap();
        let baseline_thumbnail =
            std::fs::read(library.capture_dir(&capture).unwrap().join("thumbnail.png")).unwrap();
        let publication = Arc::new(RenderPublication::default());
        publication.request(1);
        let staged = Arc::new(Barrier::new(2));
        let publish = Arc::new(Barrier::new(2));
        let worker_library = library.clone();
        let worker_capture = capture.clone();
        let worker_original = original.clone();
        let worker_publication = publication.clone();
        let prepared = staged.clone();
        let release = publish.clone();
        let worker = thread::spawn(move || {
            let mut document = Document::new(120, 80).unwrap();
            document.border = 7;
            let mut capture = worker_capture;
            capture.title = "Superseded title".into();
            let staged = stage_render(
                &Renderer::new().unwrap(),
                &worker_library,
                RenderRequest {
                    generation: 1,
                    capture,
                    document,
                    original: worker_original,
                },
                &worker_publication,
            )
            .unwrap();
            prepared.wait();
            release.wait();
            worker_publication.publish(staged).err().unwrap()
        });
        staged.wait();
        assert_eq!(library.load_document(&capture).unwrap(), baseline);
        publication.request(2);
        let mut invalid = baseline.clone();
        invalid.width = 0;
        assert!(stage_render(
            &Renderer::new().unwrap(),
            &library,
            RenderRequest {
                generation: 2,
                capture: capture.clone(),
                document: invalid,
                original,
            },
            &publication
        )
        .is_err());
        publish.wait();
        assert!(worker
            .join()
            .unwrap()
            .downcast_ref::<RenderCancelled>()
            .is_some());
        let directory = library.capture_dir(&capture).unwrap();
        assert_eq!(library.load_document(&capture).unwrap(), baseline);
        assert_eq!(
            std::fs::read(directory.join("capture.json")).unwrap(),
            baseline_capture
        );
        assert_eq!(
            std::fs::read(directory.join("thumbnail.png")).unwrap(),
            baseline_thumbnail
        );
        assert!(std::fs::read_dir(directory).unwrap().all(|entry| {
            entry
                .unwrap()
                .path()
                .extension()
                .is_none_or(|extension| extension != "tmp")
        }));
    }

    #[test]
    fn derivative_work_honors_supersession_between_rows() {
        use std::cell::Cell;
        let image = RgbaImage::new(2400, 1800);
        let checks = Cell::new(0);
        let result = derivative_image(&image, 2048, 2048, &|| {
            checks.set(checks.get() + 1);
            if checks.get() > 5 {
                Err(RenderCancelled.into())
            } else {
                Ok(())
            }
        });
        assert!(result
            .unwrap_err()
            .downcast_ref::<RenderCancelled>()
            .is_some());
        assert_eq!(checks.get(), 6);
    }

    #[test]
    fn switching_large_render_results_retains_only_thumbnail_sized_textures() {
        let temporary = tempfile::tempdir().unwrap();
        let library = Library::open(temporary.path().to_owned()).unwrap();
        let original = Arc::new(RgbaImage::from_pixel(
            2200,
            1800,
            image::Rgba([40, 100, 160, 255]),
        ));
        let capture = library.import_image(&original, "Large fixture").unwrap();
        let publication = RenderPublication::default();
        publication.request(1);
        let staged = stage_render(
            &Renderer::new().unwrap(),
            &library,
            RenderRequest {
                generation: 1,
                capture,
                document: Document::new(2200, 1800).unwrap(),
                original,
            },
            &publication,
        )
        .unwrap();
        let rendered = publication.publish(staged).unwrap();
        assert!(rendered.preview.width() > library::THUMBNAIL_WIDTH * 4);
        let ctx = egui::Context::default();
        let mut cache = ThumbnailCache::default();
        let mut current_preview = None;
        for index in 0..RECENT_TEXTURE_LIMIT + 5 {
            current_preview = Some(
                install_render_textures(&ctx, &format!("capture-{index}"), &rendered, &mut cache)
                    .unwrap(),
            );
            assert!(cache.textures.len() <= RECENT_TEXTURE_LIMIT);
            let retained = cache
                .textures
                .values()
                .map(|texture| {
                    let [width, height] = texture.size();
                    assert!(
                        width <= library::THUMBNAIL_WIDTH as usize
                            && height <= library::THUMBNAIL_HEIGHT as usize
                    );
                    assert_ne!(texture.id(), current_preview.as_ref().unwrap().id());
                    width * height * 4
                })
                .sum::<usize>();
            assert!(retained <= RECENT_TEXTURE_LIMIT * 240 * 150 * 4);
        }
        assert_eq!(cache.textures.len(), RECENT_TEXTURE_LIMIT);
        let before = cache.textures.len();
        assert!(cache
            .insert(&ctx, "incorrect-preview-input", &rendered.preview)
            .is_err());
        assert_eq!(cache.textures.len(), before);
        assert!(current_preview.unwrap().size()[0] > library::THUMBNAIL_WIDTH as usize);
    }

    #[test]
    fn rapid_render_supersession_saves_only_the_latest_completed_document() {
        let temporary = tempfile::tempdir().unwrap();
        let library = Library::open(temporary.path().to_owned()).unwrap();
        let original = Arc::new(RgbaImage::from_pixel(
            800,
            600,
            image::Rgba([255, 255, 255, 255]),
        ));
        let capture = library.import_image(&original, "Supersession").unwrap();
        let service = RenderService::new(library.clone(), egui::Context::default()).unwrap();
        let mut document = Document::new(800, 600).unwrap();
        document.grayscale = true;
        service.request(RenderRequest {
            generation: 1,
            capture: capture.clone(),
            document: document.clone(),
            original: original.clone(),
        });
        document.border = 8;
        service.request(RenderRequest {
            generation: 2,
            capture: capture.clone(),
            document: document.clone(),
            original: original.clone(),
        });
        document.border = 20;
        document.grayscale = false;
        service.request(RenderRequest {
            generation: 3,
            capture: capture.clone(),
            document: document.clone(),
            original: original.clone(),
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(result) = service.take() {
                let result = result.unwrap();
                if result.generation == 3 {
                    break;
                }
            }
            assert!(Instant::now() < deadline, "Latest render did not finish.");
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(library.load_document(&capture).unwrap(), document);
        assert_eq!(
            library::load_image(&library.capture_dir(&capture).unwrap().join("original.png"))
                .unwrap(),
            *original
        );
    }
    #[test]
    fn maximum_image_at_eight_hundred_percent_paints_only_visible_cells() {
        let canvas = Rect::from_min_size(
            Pos2::new(-20000.0, -16000.0),
            Vec2::new(6000.0, 4000.0) * 8.0,
        );
        let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
        let (x, y) = checkerboard_cells(canvas, viewport);
        assert!(x.len() * y.len() <= 51 * 39);
        let (x, y) = checkerboard_cells(
            canvas,
            Rect::from_min_size(Pos2::new(50000.0, 40000.0), Vec2::splat(100.0)),
        );
        assert!(x.is_empty() && y.is_empty());
    }
}
