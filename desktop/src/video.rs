use crate::{
    library::{self, Capture, CaptureKind, Library, StagedFile},
    media::encoder,
    platform::process,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

pub const FPS: u32 = 30;
const MAX_FRAMES: u32 = 7201 * FPS;
const MAX_SECTIONS: usize = 64;
const PIPELINE: &str = "snapper-video-edits-v1-cfr30-x264-crf20-aac160k";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Section {
    pub start: u32,
    pub end: u32,
}
impl Section {
    pub fn frames(self) -> u32 {
        self.end - self.start
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoPlan {
    pub schema: u32,
    pub source_sha256: String,
    pub source_frames: u32,
    pub fps: u32,
    pub sections: Vec<Section>,
}

impl VideoPlan {
    pub fn new(source_sha256: String, source_frames: u32) -> Self {
        Self {
            schema: 1,
            source_sha256,
            source_frames,
            fps: FPS,
            sections: vec![Section {
                start: 0,
                end: source_frames,
            }],
        }
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == 1 && self.fps == FPS,
            "Unsupported video-edit timeline version."
        );
        ensure!(
            self.source_sha256.len() == 64
                && self.source_sha256.bytes().all(|c| c.is_ascii_hexdigit()),
            "Video edits have an invalid original fingerprint."
        );
        ensure!(
            (1..=MAX_FRAMES).contains(&self.source_frames),
            "Video edit duration exceeds its two-hour budget."
        );
        ensure!(
            !self.sections.is_empty() && self.sections.len() <= MAX_SECTIONS,
            "Keep at least one frame and at most 64 sections."
        );
        let mut previous = 0;
        for section in &self.sections {
            ensure!(section.start >= previous && section.start < section.end && section.end <= self.source_frames,
                "Video sections must be nonempty, ordered, non-overlapping and inside the original.");
            previous = section.end;
        }
        Ok(())
    }
    pub fn frames(&self) -> u32 {
        self.sections.iter().map(|section| section.frames()).sum()
    }
    pub fn source_frame(&self, mut frame: u32) -> Result<u32> {
        ensure!(
            frame < self.frames(),
            "Playhead is outside the edited timeline."
        );
        for section in &self.sections {
            if frame < section.frames() {
                return Ok(section.start + frame);
            }
            frame -= section.frames();
        }
        anyhow::bail!("Edited timeline has no frame at this position.")
    }
    pub fn selection(&self, start: u32, end: u32) -> Result<Self> {
        ensure!(
            start < end && end <= self.frames(),
            "Select at least one frame inside the edited timeline."
        );
        let mut offset = 0;
        let mut selected = Vec::new();
        for section in &self.sections {
            let a = start.saturating_sub(offset).min(section.frames());
            let b = end.saturating_sub(offset).min(section.frames());
            if a < b {
                selected.push(Section {
                    start: section.start + a,
                    end: section.start + b,
                });
            }
            offset += section.frames();
        }
        let plan = Self {
            sections: selected,
            ..self.clone()
        };
        plan.validate()?;
        Ok(plan)
    }
    pub fn remove(&mut self, start: u32, end: u32) -> Result<()> {
        ensure!(
            start < end && end <= self.frames(),
            "Select a nonempty interval to remove."
        );
        let mut sections = Vec::new();
        if start > 0 {
            sections.extend(self.selection(0, start)?.sections);
        }
        if end < self.frames() {
            sections.extend(self.selection(end, self.frames())?.sections);
        }
        let plan = Self {
            sections,
            ..self.clone()
        };
        plan.validate()?;
        *self = plan;
        Ok(())
    }
    pub fn split(&mut self, frame: u32) -> Result<bool> {
        ensure!(
            frame > 0 && frame < self.frames(),
            "Place the playhead inside a kept section."
        );
        let source = self.source_frame(frame)?;
        let Some(index) = self
            .sections
            .iter()
            .position(|section| section.start < source && source < section.end)
        else {
            return Ok(false);
        };
        ensure!(
            self.sections.len() < MAX_SECTIONS,
            "The 64-section edit budget is full."
        );
        let end = self.sections[index].end;
        self.sections[index].end = source;
        self.sections
            .insert(index + 1, Section { start: source, end });
        Ok(true)
    }
    pub fn effective_sections(&self) -> Vec<Section> {
        let mut result: Vec<Section> = Vec::new();
        for &section in &self.sections {
            if let Some(last) = result.last_mut() {
                if last.end == section.start {
                    last.end = section.end;
                    continue;
                }
            }
            result.push(section);
        }
        result
    }
    pub fn fingerprint(&self) -> Result<String> {
        self.validate()?;
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(
                PIPELINE,
                &self.source_sha256,
                self.source_frames,
                self.effective_sections()
            ))?)
        ))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub frames: u32,
    pub source_seconds: f64,
    pub origin_seconds: f64,
    pub audio: bool,
    pub source_sha256: String,
}

#[derive(Clone)]
pub struct VideoDocument {
    pub directory: PathBuf,
    pub source: PathBuf,
    pub info: VideoInfo,
    pub plan: VideoPlan,
    _source_lock: Arc<File>,
}

#[derive(Clone, Default)]
pub struct VideoProgress {
    pub stage: String,
    pub fraction: f32,
    pub elapsed_ms: u64,
}
#[derive(Default)]
pub struct VideoControl {
    cancel: AtomicBool,
    publication: AtomicU8,
    progress: Mutex<VideoProgress>,
}
impl VideoControl {
    pub fn cancel(&self) -> bool {
        if self
            .publication
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            || self.publication.load(Ordering::Acquire) == 1
        {
            self.cancel.store(true, Ordering::Release);
            true
        } else {
            false
        }
    }
    pub fn check(&self) -> Result<()> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(encoder::Cancelled.into());
        }
        Ok(())
    }
    pub fn progress(&self) -> VideoProgress {
        self.progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn update(&self, stage: &str, fraction: f32, elapsed_ms: u64) {
        *self.progress.lock().unwrap_or_else(|e| e.into_inner()) = VideoProgress {
            stage: stage.into(),
            fraction: fraction.clamp(0.0, 1.0),
            elapsed_ms,
        };
    }
    fn commit<T>(&self, publish: impl FnOnce() -> Result<T>) -> Result<T> {
        if self
            .publication
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            self.check()?;
            anyhow::bail!("This video operation already reached its publication boundary.");
        }
        let result = publish();
        self.publication.store(3, Ordering::Release);
        result
    }
}

#[derive(Clone)]
pub struct EditedPreview {
    pub path: PathBuf,
    pub plan_sha256: String,
    pub frames: u32,
    pub width: u32,
    pub height: u32,
    pub output_sha256: String,
    _lock: Arc<File>,
}

#[derive(Serialize, Deserialize)]
struct CacheReceipt {
    schema: u32,
    filename: String,
    plan_sha256: String,
    output_sha256: String,
    frames: u32,
    width: u32,
    height: u32,
}

fn read_lock(path: &Path) -> Result<File> {
    use std::os::windows::fs::MetadataExt;
    use windows::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
    };
    let file = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)
        .with_context(|| format!("Opening read-locked local media {}", path.display()))?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0,
        "Video editing requires a regular local file, not a redirected media link."
    );
    Ok(file)
}

fn hash_file(file: &mut File, control: &VideoControl) -> Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut sha = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        control.check()?;
        ensure!(
            Instant::now() < deadline,
            "Video fingerprinting exceeded 120 seconds; the original is unchanged."
        );
        let length = file.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        sha.update(&buffer[..length]);
    }
    Ok(format!("{:x}", sha.finalize()))
}

fn probe(
    path: &Path,
    directory: &Path,
    control: &VideoControl,
    count_frames: bool,
) -> Result<Value> {
    let staged = StagedFile::target(&directory.join("video-probe.json"))?;
    let output = File::create(staged.path()?)?;
    let mut command = process::command(&process::media_tool("ffprobe")?);
    command.args([
        "-v",
        "error",
        "-protocol_whitelist",
        "file,pipe",
        "-show_streams",
        "-show_format",
        "-of",
        "json",
    ]);
    if count_frames {
        command.arg("-count_frames");
    }
    command.arg(path);
    encoder::run_bounded_cancellable(
        &mut command,
        Duration::from_secs(if count_frames { 180 } else { 20 }),
        Some(output),
        &control.cancel,
        || control.check(),
    )?;
    library::read_json(staged.path()?)
}

impl VideoDocument {
    #[cfg(test)]
    pub(crate) fn controller_fixture(library: &Library, id: &str) -> Result<(Capture, Self)> {
        let capture = Capture {
            schema: 1,
            id: id.into(),
            title: id.into(),
            kind: CaptureKind::Video,
            created_ms: 0,
            width: 320,
            height: 180,
            tags: vec![],
        };
        let directory = library.capture_dir(&capture)?;
        fs::create_dir(&directory)?;
        let source = directory.join("recording.mp4");
        fs::write(&source, b"controller-only fixture; never sent to a codec")?;
        library.save_capture(&capture)?;
        library::export_image(
            &directory.join("thumbnail.png"),
            &image::RgbaImage::new(1, 1),
        )?;
        let mut locked = read_lock(&source)?;
        let hash = hash_file(&mut locked, &VideoControl::default())?;
        let info = VideoInfo {
            width: 320,
            height: 180,
            frames: 90,
            source_seconds: 3.0,
            origin_seconds: 0.0,
            audio: false,
            source_sha256: hash.clone(),
        };
        let plan = VideoPlan::new(hash, 90);
        Ok((
            capture,
            Self {
                directory,
                source,
                info,
                plan,
                _source_lock: Arc::new(locked),
            },
        ))
    }

    pub fn open(library: &Library, capture: &Capture, control: &VideoControl) -> Result<Self> {
        ensure!(
            capture.kind == CaptureKind::Video,
            "This capture is not a video."
        );
        control.check()?;
        let directory = library.capture_dir(capture)?;
        let source = directory.join("recording.mp4");
        let mut source_lock = read_lock(&source)?;
        control.update("Inspecting the original recording", 0.0, 0);
        let metadata = probe(&source, &directory, control, false)?;
        let streams = metadata["streams"]
            .as_array()
            .context("Video probe has no streams")?;
        let videos: Vec<_> = streams
            .iter()
            .filter(|s| s["codec_type"] == "video")
            .collect();
        let audio: Vec<_> = streams
            .iter()
            .filter(|s| s["codec_type"] == "audio")
            .collect();
        ensure!(
            videos.len() == 1 && videos[0]["codec_name"] == "h264" && audio.len() <= 1,
            "The focused editor supports recorded H.264 MP4 with zero or one mixed audio track."
        );
        let width = videos[0]["width"].as_u64().context("Video width missing")? as u32;
        let height = videos[0]["height"]
            .as_u64()
            .context("Video height missing")? as u32;
        crate::model::validate_size(width, height)?;
        ensure!(
            width <= 1920 && height <= 1080 && width % 2 == 0 && height % 2 == 0,
            "Video editing currently supports the recorder's even-sized 1920x1080 envelope."
        );
        let seconds = metadata["format"]["duration"]
            .as_str()
            .context("Video duration missing")?
            .parse::<f64>()?;
        let origin = metadata["format"]["start_time"]
            .as_str()
            .context("Video presentation origin missing")?
            .parse::<f64>()?;
        ensure!(
            seconds.is_finite() && seconds > 0.0 && seconds <= 7201.0,
            "Video duration is outside the two-hour edit budget."
        );
        ensure!(
            origin.is_finite() && origin.abs() <= 3600.0,
            "Video presentation origin is unsupported."
        );
        ensure!(
            audio.is_empty()
                || audio[0]["channels"]
                    .as_u64()
                    .is_some_and(|count| (1..=2).contains(&count)),
            "The focused editor supports mono or stereo recorded audio."
        );
        control.update("Verifying the immutable original", 0.1, 0);
        let sha = hash_file(&mut source_lock, control)?;
        let frames = (seconds * FPS as f64).ceil() as u32;
        let info = VideoInfo {
            width,
            height,
            frames,
            source_seconds: seconds,
            origin_seconds: origin,
            audio: !audio.is_empty(),
            source_sha256: sha.clone(),
        };
        let plan_path = directory.join("video-edits.json");
        let plan: VideoPlan = if plan_path.exists() {
            library::read_json(&plan_path)?
        } else {
            VideoPlan::new(sha, frames)
        };
        plan.validate()?;
        ensure!(plan.source_sha256 == info.source_sha256 && plan.source_frames == frames,
            "Saved video edits do not match this original. The original and saved edit file were not changed.");
        Ok(Self {
            directory,
            source,
            info,
            plan,
            _source_lock: Arc::new(source_lock),
        })
    }
    pub fn save_plan(&self, plan: &VideoPlan) -> Result<()> {
        self.check_plan(plan)?;
        library::write_json(&self.directory.join("video-edits.json"), plan)
    }
    fn check_plan(&self, plan: &VideoPlan) -> Result<()> {
        plan.validate()?;
        ensure!(
            plan.source_sha256 == self.info.source_sha256 && plan.source_frames == self.info.frames,
            "Video edits belong to a different original."
        );
        Ok(())
    }
}

fn filter_graph(plan: &VideoPlan, audio: bool, origin: f64) -> Result<String> {
    plan.validate()?;
    let sections = plan.effective_sections();
    let count = sections.len();
    let mut graph = format!("[0:v:0]setpts=PTS-({origin:.9})/TB,fps=fps=30:start_time=0:round=near,tpad=stop_mode=clone:stop_duration=1,trim=end_frame={},split={count}", plan.source_frames);
    for index in 0..count {
        graph.push_str(&format!("[v{index}]"));
    }
    graph.push(';');
    if audio {
        graph.push_str(&format!("[0:a:0]asetpts=PTS-({origin:.9})/TB,aresample=48000:async=1:min_hard_comp=0:first_pts=0,apad,atrim=end_sample={},asplit={count}", u64::from(plan.source_frames) * 1600));
        for index in 0..count {
            graph.push_str(&format!("[a{index}]"));
        }
        graph.push(';');
    }
    for (index, section) in sections.iter().enumerate() {
        graph.push_str(&format!(
            "[v{index}]trim=start_frame={}:end_frame={},setpts=PTS-STARTPTS[vc{index}];",
            section.start, section.end
        ));
        if audio {
            graph.push_str(&format!("[a{index}]atrim=start_sample={}:end_sample={},apad=whole_len={},atrim=end_sample={},asetpts=N/SR/TB[ac{index}];",
                u64::from(section.start)*1600, u64::from(section.end)*1600,
                u64::from(section.frames())*1600, u64::from(section.frames())*1600));
        }
    }
    for index in 0..count {
        graph.push_str(&format!("[vc{index}]"));
        if audio {
            graph.push_str(&format!("[ac{index}]"));
        }
    }
    graph.push_str(&format!(
        "concat=n={count}:v=1:a={}[joinedv]",
        u8::from(audio)
    ));
    if audio {
        graph.push_str("[joineda]");
    }
    graph.push_str(";[joinedv]settb=1/30,setpts=N[video]");
    if audio {
        graph.push_str(";[joineda]asetpts=N/SR/TB[audio]");
    }
    ensure!(
        graph.len() <= 20_000,
        "The edit filter exceeded its bounded command budget. Merge adjacent sections."
    );
    Ok(graph)
}

fn progress_frame(path: &Path) -> Result<Option<u32>> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    file.seek(SeekFrom::End(-(length.min(8192) as i64)))?;
    let mut tail = String::new();
    file.read_to_string(&mut tail)?;
    Ok(tail.lines().rev().find_map(|line| {
        line.trim()
            .strip_prefix("frame=")
            .and_then(|value| value.trim().parse().ok())
    }))
}

fn decoded_shape(path: &Path, directory: &Path, control: &VideoControl) -> Result<(u32, u64)> {
    let rows = StagedFile::target(&directory.join("decoded-frame-shape.txt"))?;
    let mut command = process::command(&process::media_tool("ffprobe")?);
    command
        .args([
            "-v",
            "error",
            "-protocol_whitelist",
            "file,pipe",
            "-show_frames",
            "-show_entries",
            "frame=media_type,best_effort_timestamp_time,nb_samples:frame_side_data=",
            "-of",
            "compact=p=0:nk=0",
        ])
        .arg(path);
    encoder::run_bounded_cancellable(
        &mut command,
        Duration::from_secs(180),
        Some(File::create(rows.path()?)?),
        &control.cancel,
        || control.check(),
    )?;
    ensure!(
        fs::metadata(rows.path()?)?.len() <= 64 * 1024 * 1024,
        "Decoded frame metadata exceeded its 64 MiB budget."
    );
    let mut frames = 0u32;
    let mut samples = 0u64;
    for line in BufReader::new(File::open(rows.path()?)?).lines() {
        control.check()?;
        let line = line?;
        let fields: std::collections::HashMap<_, _> = line
            .split('|')
            .filter_map(|field| field.split_once('='))
            .collect();
        match fields.get("media_type").copied() {
            Some("video") => {
                let stamp = fields
                    .get("best_effort_timestamp_time")
                    .context("Decoded video timestamp missing")?
                    .parse::<f64>()?;
                ensure!(stamp.is_finite() && (stamp-frames as f64/FPS as f64).abs() <= 0.0001,
                    "Edited frame {frames} does not have the promised 30 fps presentation timestamp.");
                frames = frames
                    .checked_add(1)
                    .context("Decoded frame count overflow")?;
                ensure!(
                    frames <= MAX_FRAMES,
                    "Edited output exceeded its frame budget."
                );
            }
            Some("audio") => {
                let count = fields
                    .get("nb_samples")
                    .context("Decoded audio sample count missing")?
                    .parse::<u64>()?;
                ensure!(count <= 48_000, "Decoded audio frame exceeded one second.");
                samples = samples
                    .checked_add(count)
                    .context("Audio sample count overflow")?;
            }
            None if line.trim().is_empty() => {}
            _ => anyhow::bail!("Unexpected decoded frame metadata."),
        }
    }
    Ok((frames, samples))
}

fn validate_edit(
    path: &Path,
    directory: &Path,
    document: &VideoDocument,
    plan: &VideoPlan,
    control: &VideoControl,
) -> Result<()> {
    let metadata = probe(path, directory, control, false)?;
    let streams = metadata["streams"]
        .as_array()
        .context("Edited output has no streams")?;
    let videos: Vec<_> = streams
        .iter()
        .filter(|s| s["codec_type"] == "video")
        .collect();
    let audio: Vec<_> = streams
        .iter()
        .filter(|s| s["codec_type"] == "audio")
        .collect();
    ensure!(
        videos.len() == 1
            && videos[0]["codec_name"] == "h264"
            && videos[0]["width"] == document.info.width
            && videos[0]["height"] == document.info.height,
        "Edited output changed the video shape."
    );
    let (frames, samples) = decoded_shape(path, directory, control)?;
    ensure!(
        frames == plan.frames(),
        "Edited output has {frames} frames; {} were selected.",
        plan.frames()
    );
    ensure!(
        audio.len() == usize::from(document.info.audio),
        "Edited output changed audio presence."
    );
    if document.info.audio {
        let expected = u64::from(plan.frames()) * 1600;
        ensure!(samples >= expected && samples <= expected + 1024,
            "Edited audio has {samples} effective samples, expected {expected} plus at most one final AAC frame.");
    }
    let duration = metadata["format"]["duration"]
        .as_str()
        .context("Edited duration missing")?
        .parse::<f64>()?;
    ensure!(
        duration.is_finite() && (duration - plan.frames() as f64 / FPS as f64).abs() <= 0.08,
        "Edited duration does not match the selected frame timeline."
    );
    let mut command = process::command(&process::media_tool("ffmpeg")?);
    command
        .args([
            "-v",
            "error",
            "-xerror",
            "-protocol_whitelist",
            "file,pipe",
            "-i",
        ])
        .arg(path)
        .args(["-f", "null", "NUL"]);
    encoder::run_bounded_cancellable(
        &mut command,
        Duration::from_secs(180),
        None,
        &control.cancel,
        || control.check(),
    )?;
    Ok(())
}

fn cached_preview(
    document: &VideoDocument,
    plan: &VideoPlan,
    control: &VideoControl,
) -> Result<Option<EditedPreview>> {
    let folder = document.directory.join("video-preview");
    let receipt_path = folder.join("preview.json");
    if !receipt_path.exists() {
        return Ok(None);
    }
    let receipt: CacheReceipt = library::read_json(&receipt_path)?;
    let fingerprint = plan.fingerprint()?;
    if receipt.plan_sha256 != fingerprint {
        return Ok(None);
    }
    ensure!(
        receipt.schema == 1
            && receipt.filename.starts_with("preview-")
            && receipt.filename.ends_with(".mp4")
            && receipt
                .filename
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-.".contains(&c)),
        "Edited-preview receipt has an invalid filename."
    );
    let path = folder.join(&receipt.filename);
    let mut file = read_lock(&path)?;
    ensure!(receipt.frames == plan.frames() && receipt.width == document.info.width && receipt.height == document.info.height
        && hash_file(&mut file, control)? == receipt.output_sha256,
        "Edited preview was changed outside Snapper. Original recording and edit metadata remain unchanged.");
    Ok(Some(EditedPreview {
        path,
        plan_sha256: fingerprint,
        frames: receipt.frames,
        width: receipt.width,
        height: receipt.height,
        output_sha256: receipt.output_sha256,
        _lock: Arc::new(file),
    }))
}

fn make_cache_room(folder: &Path) -> Result<()> {
    fs::create_dir_all(folder)?;
    let receipt_path = folder.join("preview.json");
    let protected = if receipt_path.is_file() {
        Some(library::read_json::<CacheReceipt>(&receipt_path)?.filename)
    } else {
        None
    };
    let mut previews = Vec::new();
    for (index, entry) in fs::read_dir(folder)?.enumerate() {
        ensure!(index < 128, "Preview cache is unexpectedly large. Close preview windows and inspect the cache folder.");
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if entry.file_type()?.is_file() && name.starts_with("preview-") && name.ends_with(".mp4") {
            previews.push(entry.path());
        }
    }
    previews.sort();
    let mut remaining = previews.len();
    for old in previews {
        if remaining < 3 {
            break;
        }
        if old.file_name().and_then(|name| name.to_str()) == protected.as_deref() {
            continue;
        }
        match fs::remove_file(&old) {
            Ok(()) => remaining -= 1,
            Err(error) if matches!(error.raw_os_error(), Some(32 | 33)) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => remaining -= 1,
            Err(error) => return Err(error).context("Cleaning an owned edited-preview cache file"),
        }
    }
    ensure!(
        remaining < 3,
        "Close old edited-preview player windows to free the three-preview cache budget."
    );
    Ok(())
}

pub fn save_preview(
    preview: &EditedPreview,
    destination: &Path,
    control: &VideoControl,
) -> Result<()> {
    control.check()?;
    ensure!(
        destination.is_absolute()
            && destination
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("mp4")),
        "Choose an absolute .mp4 output filename."
    );
    ensure!(
        !destination.exists(),
        "The destination exists. Choose a new filename; originals are never overwritten."
    );
    control.update("Copying the exact verified edited preview", 0.96, 0);
    let staged = library::stage_file_copy(&preview.path, destination, || control.check())?;
    let mut copied = File::open(staged.path()?)?;
    ensure!(
        hash_file(&mut copied, control)? == preview.output_sha256,
        "The staged export does not match the previewed bytes. No MP4 was published."
    );
    drop(copied);
    control.commit(|| staged.publish_new())?;
    control.update("MP4 saved; original unchanged", 1.0, 0);
    Ok(())
}

pub fn render_edit(
    document: &VideoDocument,
    plan: &VideoPlan,
    destination: Option<&Path>,
    control: &VideoControl,
) -> Result<EditedPreview> {
    control.check()?;
    document.check_plan(plan)?;
    if let Some(path) = destination {
        ensure!(
            path.is_absolute()
                && path
                    .extension()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.eq_ignore_ascii_case("mp4")),
            "Choose an absolute .mp4 output filename."
        );
        ensure!(!path.exists(), "The destination already exists. Choose a new filename; originals are never overwritten.");
    }
    let started = Instant::now();
    control.update("Checking edited-preview identity", 0.0, 0);
    let preview = match cached_preview(document, plan, control)? {
        Some(preview) => preview,
        None => {
            let folder = document.directory.join("video-preview");
            make_cache_room(&folder)?;
            ensure!(
                process::free_space(&folder)? > 512 * 1024 * 1024,
                "Less than 512 MiB is free. Video rendering was not started."
            );
            let filename = format!("preview-{}.mp4", library::id());
            let output = folder.join(&filename);
            let staged = StagedFile::target(&output)?;
            let progress = StagedFile::target(&folder.join("progress.txt"))?;
            let progress_file = File::create(progress.path()?)?;
            let mut command = process::command(&process::media_tool("ffmpeg")?);
            command
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-nostdin",
                    "-protocol_whitelist",
                    "file,pipe",
                    "-threads",
                    "2",
                    "-copyts",
                    "-i",
                ])
                .arg(&document.source)
                .args([
                    "-filter_complex_threads",
                    "1",
                    "-filter_complex",
                    &filter_graph(plan, document.info.audio, document.info.origin_seconds)?,
                    "-map",
                    "[video]",
                ]);
            if document.info.audio {
                command.args([
                    "-map", "[audio]", "-c:a", "aac", "-b:a", "160k", "-ar", "48000", "-ac", "2",
                ]);
            } else {
                command.arg("-an");
            }
            command
                .args([
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-crf",
                    "20",
                    "-threads",
                    "2",
                    "-pix_fmt",
                    "yuv420p",
                    "-r",
                    "30",
                    "-fps_mode",
                    "cfr",
                    "-g",
                    "60",
                    "-bf",
                    "0",
                    "-sc_threshold",
                    "0",
                    "-frames:v",
                    &plan.frames().to_string(),
                    "-movflags",
                    "+faststart",
                    "-progress",
                    "pipe:1",
                    "-stats_period",
                    "0.5",
                    "-f",
                    "mp4",
                    "-y",
                ])
                .arg(staged.path()?);
            let timeout =
                Duration::from_secs(u64::from(plan.frames()).div_ceil(u64::from(FPS)) * 3 + 60);
            let mut last_poll = Instant::now() - Duration::from_secs(1);
            encoder::run_bounded_cancellable(
                &mut command,
                timeout,
                Some(progress_file),
                &control.cancel,
                || {
                    control.check()?;
                    if last_poll.elapsed() >= Duration::from_millis(250) {
                        if let Some(frame) = progress_frame(progress.path()?)? {
                            control.update(
                                "Accurately re-encoding edited video",
                                frame as f32 / plan.frames() as f32 * 0.85,
                                started.elapsed().as_millis() as u64,
                            );
                        }
                        ensure!(
                            process::free_space(&folder)? > 256 * 1024 * 1024,
                            "Free space fell below 256 MiB. Export stopped; original retained."
                        );
                        last_poll = Instant::now();
                    }
                    Ok(())
                },
            )?;
            control.update(
                "Decoding and verifying edited output",
                0.88,
                started.elapsed().as_millis() as u64,
            );
            validate_edit(staged.path()?, &folder, document, plan, control)?;
            let flush = OpenOptions::new()
                .read(true)
                .write(true)
                .open(staged.path()?)?;
            flush.sync_all()?;
            drop(flush);
            let mut verified = read_lock(staged.path()?)?;
            let output_sha256 = hash_file(&mut verified, control)?;
            drop(verified);
            control.check()?;
            staged.publish_new()?;
            let mut output_lock = read_lock(&output)?;
            ensure!(
                hash_file(&mut output_lock, control)? == output_sha256,
                "Edited output identity changed during cache publication."
            );
            let preview = EditedPreview {
                path: output.clone(),
                plan_sha256: plan.fingerprint()?,
                frames: plan.frames(),
                width: document.info.width,
                height: document.info.height,
                output_sha256: output_sha256.clone(),
                _lock: Arc::new(output_lock),
            };
            library::write_json(
                &folder.join("preview.json"),
                &CacheReceipt {
                    schema: 1,
                    filename,
                    plan_sha256: preview.plan_sha256.clone(),
                    output_sha256,
                    frames: preview.frames,
                    width: preview.width,
                    height: preview.height,
                },
            )?;
            preview
        }
    };
    control.check()?;
    if let Some(destination) = destination {
        save_preview(&preview, destination, control)?;
    } else {
        control.commit(|| Ok(()))?;
    }
    control.update(
        if destination.is_some() {
            "MP4 saved; original unchanged"
        } else {
            "Edited preview ready"
        },
        1.0,
        started.elapsed().as_millis() as u64,
    );
    Ok(preview)
}

pub fn preview_frame(
    preview: &EditedPreview,
    frame: u32,
    control: &VideoControl,
) -> Result<image::RgbaImage> {
    ensure!(
        frame < preview.frames,
        "Preview playhead is outside the edited video."
    );
    control.check()?;
    let folder = preview
        .path
        .parent()
        .context("Edited preview has no parent")?;
    let image = StagedFile::target(&folder.join("frame.png"))?;
    let mut command = process::command(&process::media_tool("ffmpeg")?);
    command
        .args([
            "-v",
            "error",
            "-nostdin",
            "-protocol_whitelist",
            "file,pipe",
            "-ss",
            &(frame / FPS).to_string(),
            "-i",
        ])
        .arg(&preview.path)
        .args([
            "-vf",
            &format!("select=eq(n\\,{})", frame % FPS),
            "-frames:v",
            "1",
            "-an",
            "-c:v",
            "png",
            "-f",
            "image2",
            "-y",
        ])
        .arg(image.path()?);
    encoder::run_bounded_cancellable(
        &mut command,
        Duration::from_secs(20),
        None,
        &control.cancel,
        || control.check(),
    )?;
    let frame = library::load_image(image.path()?)?;
    Ok(image::imageops::thumbnail(&frame, 1280, 720))
}

pub fn self_test(root: &Path) -> Result<()> {
    let root = if root.is_absolute() {
        root.to_owned()
    } else {
        std::env::current_dir()?.join(root)
    };
    fs::create_dir_all(&root)?;
    let run = root.join(format!("run-{}", library::id()));
    fs::create_dir(&run)?;
    let mut cases = Vec::new();
    for (name, result) in [
        (
            "one-frame-cuts-preview-identity-and-no-overwrite",
            edit_fixture_case(&run.join("silent"), false),
        ),
        (
            "delayed-gapped-short-audio-keeps-common-timeline",
            edit_fixture_case(&run.join("audio"), true),
        ),
        (
            "recording-original-and-recovery-segments-unchanged",
            recording_edit_case(&run.join("recorded")),
        ),
        ("cancel-releases-owned-codec-job", cancel_codec_case()),
    ] {
        let case = match result {
            Ok(evidence) => {
                println!("PASS {name}");
                serde_json::json!({"name":name,"passed":true,"evidence":evidence})
            }
            Err(error) => {
                eprintln!("FAIL {name}: {error:#}");
                serde_json::json!({"name":name,"passed":false,"error":format!("{error:#}")})
            }
        };
        cases.push(case);
        library::write_json(&run.join("results.json"), &cases)?;
    }
    let passed = cases.iter().filter(|case| case["passed"] == true).count();
    let summary = serde_json::json!({"kind":"generated-nondestructive-video-edit-acceptance","passed":passed,"total":cases.len(),
        "results":run.join("results.json"),"build":crate::media::selftest::build_identity()?,
        "real_inputs_activated":false,"gui_launched":false});
    library::write_json(&root.join("latest.json"), &summary)?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    ensure!(
        passed == cases.len(),
        "{passed}/{} focused video-edit cases passed.",
        cases.len()
    );
    Ok(())
}

fn fixture(root: &Path, audio: bool) -> Result<(Library, Capture)> {
    let library = Library::open(root.to_owned())?;
    let capture = Capture {
        schema: 1,
        id: library::id(),
        title: "Generated editing fixture".into(),
        kind: CaptureKind::Video,
        created_ms: library::now_ms(),
        width: 320,
        height: 180,
        tags: vec![],
    };
    let directory = library.capture_dir(&capture)?;
    fs::create_dir(&directory)?;
    let staged = StagedFile::target(&directory.join("recording.mp4"))?;
    let mut command = process::command(&process::media_tool("ffmpeg")?);
    command.args(["-v","error","-nostdin","-f","lavfi","-i",
        "color=red:s=320x180:r=30:d=1[r];color=green:s=320x180:r=30:d=1[g];color=blue:s=320x180:r=30:d=1[b];color=yellow:s=320x180:r=30:d=1[y];[r][g][b][y]concat=n=4:v=1:a=0"]);
    if audio {
        command.args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=1000:sample_rate=48000:duration=3.2",
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-af",
            "aselect=not(between(t\\,1\\,1.25)),asetpts=PTS+0.1/TB",
            "-c:a",
            "aac",
            "-b:a",
            "160k",
            "-ac",
            "2",
        ]);
    } else {
        command.arg("-an");
    }
    command
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-crf",
            "18",
            "-threads",
            "2",
            "-pix_fmt",
            "yuv420p",
            "-r",
            "30",
            "-g",
            "60",
            "-bf",
            "0",
            "-sc_threshold",
            "0",
            "-movflags",
            "+faststart",
            "-f",
            "mp4",
            "-y",
        ])
        .arg(staged.path()?);
    encoder::run_bounded(command, Duration::from_secs(30))?;
    staged.publish_new()?;
    library.save_capture(&capture)?;
    Ok((library, capture))
}

fn edit_fixture_case(root: &Path, audio: bool) -> Result<Value> {
    let (library, capture) = fixture(root, audio)?;
    let document = VideoDocument::open(&library, &capture, &VideoControl::default())?;
    let original = document.info.source_sha256.clone();
    let mut plan = document.plan.clone();
    plan.sections = if audio {
        vec![
            Section { start: 0, end: 45 },
            Section {
                start: 90,
                end: 120,
            },
        ]
    } else {
        vec![
            Section { start: 0, end: 1 },
            Section { start: 30, end: 31 },
            Section { start: 60, end: 61 },
        ]
    };
    document.save_plan(&plan)?;
    let reopened = VideoDocument::open(&library, &capture, &VideoControl::default())?;
    ensure!(reopened.plan == plan, "Video cuts did not survive reopen.");
    let preview = render_edit(&document, &plan, None, &VideoControl::default())?;
    let expected = if audio {
        vec![(15, [255, 0, 0]), (40, [0, 128, 0]), (60, [255, 255, 0])]
    } else {
        vec![(0, [255, 0, 0]), (1, [0, 128, 0]), (2, [0, 0, 255])]
    };
    for (frame, color) in expected {
        let image = preview_frame(&preview, frame, &VideoControl::default())?;
        let actual = image.get_pixel(image.width() / 2, image.height() / 2).0;
        ensure!(
            (0..3).all(|c| actual[c].abs_diff(color[c]) <= 22),
            "Edited preview frame {frame} has {actual:?}; expected source section {color:?}."
        );
    }
    let destination = root.join("edited-export.mp4");
    let saved = VideoControl::default();
    let exported = render_edit(&document, &plan, Some(&destination), &saved)?;
    ensure!(
        hash_file(&mut File::open(&destination)?, &VideoControl::default())?
            == preview.output_sha256
            && exported.output_sha256 == preview.output_sha256,
        "Saved MP4 differs from the previewed edit."
    );
    ensure!(
        !saved.cancel(),
        "Cancellation after publication must report too late."
    );
    ensure!(
        fs::write(&preview.path, b"replacement").is_err(),
        "Preview ownership did not prevent mutation."
    );
    ensure!(
        render_edit(
            &document,
            &plan,
            Some(&document.source),
            &VideoControl::default()
        )
        .is_err(),
        "Export accepted an original-overwrite destination."
    );
    ensure!(
        hash_file(&mut File::open(&document.source)?, &VideoControl::default())? == original,
        "Editing mutated the original."
    );
    let cancelled = VideoControl::default();
    cancelled.cancel();
    let cancelled_path = root.join("must-not-exist.mp4");
    ensure!(
        render_edit(&document, &plan, Some(&cancelled_path), &cancelled).is_err()
            && !cancelled_path.exists(),
        "Cancelled edit published an export."
    );
    let mut alignment = Value::Null;
    if audio {
        let raw = StagedFile::target(&root.join("short-test-audio.pcm"))?;
        let mut command = process::command(&process::media_tool("ffmpeg")?);
        command
            .args(["-v", "error", "-i"])
            .arg(&destination)
            .args([
                "-t", "3", "-vn", "-ac", "1", "-ar", "48000", "-f", "f32le", "-y",
            ])
            .arg(raw.path()?);
        encoder::run_bounded(command, Duration::from_secs(20))?;
        let bytes = fs::read(raw.path()?)?;
        ensure!(
            bytes.len() <= 3 * 48000 * 4,
            "Short audio test exceeded its fixed decode budget."
        );
        let samples: Vec<_> = bytes
            .chunks_exact(4)
            .map(|p| f32::from_le_bytes([p[0], p[1], p[2], p[3]]))
            .collect();
        let rms = |seconds: f64| -> Result<f64> {
            let start = (seconds * 48000.0) as usize;
            let slice = samples
                .get(start..start + 960)
                .context("Edited audio test window missing")?;
            Ok(
                (slice.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / slice.len() as f64)
                    .sqrt(),
            )
        };
        let lead = rms(0.03)?;
        let tone = rms(0.20)?;
        let gap = rms(1.20)?;
        let resumed = rms(1.70)?;
        let tail = rms(2.15)?;
        ensure!(lead<0.01&&gap<0.01&&tail<0.01&&tone>0.02&&resumed>0.02,
            "Audio origin/gap/tail changed: lead={lead:.4},tone={tone:.4},gap={gap:.4},resumed={resumed:.4},tail={tail:.4}.");
        alignment = serde_json::json!({"leading_silence_rms":lead,"tone_rms":tone,"retained_gap_rms":gap,
            "post_cut_tone_rms":resumed,"tail_silence_rms":tail});
    }
    Ok(
        serde_json::json!({"original_sha256":original,"preview_export_sha256":preview.output_sha256,
        "frames":preview.frames,"output":destination,"cuts":plan.sections,"audio_alignment":alignment}),
    )
}

fn recording_edit_case(root: &Path) -> Result<Value> {
    use crate::{
        media::{supervisor::Recorder, Phase, Request, Sources},
        platform::Area,
    };
    let library = Library::open(root.to_owned())?;
    let request = Request {
        schema: 1,
        session_dir: library.root.join("sessions").join(library::id()),
        library_root: library.root.clone(),
        area: Area {
            monitor: "generated".into(),
            width: 320,
            height: 180,
            ..Default::default()
        },
        sources: Sources::default(),
        camera_id: None,
        microphone_id: None,
        output_id: None,
        pip: None,
        video: true,
        max_seconds: 3,
        synthetic: true,
        test_fault: None,
    };
    let recorder = Recorder::start(request.clone());
    let deadline = Instant::now() + Duration::from_secs(45);
    while !recorder.finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    ensure!(
        recorder.finished() && recorder.progress().phase == Phase::Saved,
        "Generated recording fixture failed: {}",
        recorder.progress().detail
    );
    let id = recorder
        .progress()
        .capture_id
        .context("Generated recording ID missing")?;
    let capture: Capture =
        library::read_json(&library.root.join("captures").join(&id).join("capture.json"))?;
    let document = VideoDocument::open(&library, &capture, &VideoControl::default())?;
    let journal: encoder::Journal = library::read_json(&request.session_dir.join("journal.json"))?;
    let mut before = Vec::new();
    for segment in &journal.segments {
        let path = request.session_dir.join(&segment.file);
        before.push((
            path.clone(),
            hash_file(&mut File::open(path)?, &VideoControl::default())?,
        ));
    }
    let plan = document.plan.selection(5, 35)?;
    render_edit(
        &document,
        &plan,
        Some(&root.join("selected-clip.mp4")),
        &VideoControl::default(),
    )?;
    for (path, hash) in &before {
        ensure!(
            &hash_file(&mut File::open(path)?, &VideoControl::default())? == hash,
            "Editing changed a committed recovery segment."
        );
    }
    ensure!(
        hash_file(&mut File::open(&document.source)?, &VideoControl::default())?
            == document.info.source_sha256,
        "Editing changed a captured original."
    );
    Ok(
        serde_json::json!({"original_unchanged":true,"committed_segments_unchanged":before.len(),"selected_frames":plan.frames()}),
    )
}

fn cancel_codec_case() -> Result<Value> {
    let cancel = Arc::new(AtomicBool::new(false));
    let request = cancel.clone();
    let controller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        request.store(true, Ordering::Release);
    });
    let mut command = process::command(&process::media_tool("ffmpeg")?);
    command.args([
        "-v",
        "error",
        "-nostdin",
        "-re",
        "-f",
        "lavfi",
        "-i",
        "color=black:s=32x32:r=30:d=30",
        "-an",
        "-f",
        "null",
        "NUL",
    ]);
    let start = Instant::now();
    let outcome = encoder::run_bounded_cancellable(
        &mut command,
        Duration::from_secs(10),
        None,
        &cancel,
        || Ok(()),
    );
    controller
        .join()
        .map_err(|_| anyhow::anyhow!("Cancellation fixture thread panicked"))?;
    ensure!(
        outcome
            .err()
            .is_some_and(|error| error.downcast_ref::<encoder::Cancelled>().is_some()),
        "Codec cancellation did not report confirmed cancellation."
    );
    ensure!(
        start.elapsed() < Duration::from_secs(3),
        "Owned codec cancellation exceeded its three-second test budget."
    );
    Ok(
        serde_json::json!({"confirmed_owned_job_exit":true,"cancel_elapsed_ms":start.elapsed().as_millis()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn plan() -> VideoPlan {
        VideoPlan::new("a".repeat(64), 120)
    }
    #[test]
    fn split_remove_trim_and_selected_clip_map_the_edited_timeline() {
        let mut plan = plan();
        assert!(plan.split(60).unwrap());
        assert_eq!(plan.sections.len(), 2);
        plan.remove(30, 60).unwrap();
        assert_eq!(
            plan.sections,
            vec![
                Section { start: 0, end: 30 },
                Section {
                    start: 60,
                    end: 120
                }
            ]
        );
        assert_eq!(plan.source_frame(30).unwrap(), 60);
        let clip = plan.selection(15, 45).unwrap();
        assert_eq!(
            clip.sections,
            vec![
                Section { start: 15, end: 30 },
                Section { start: 60, end: 75 }
            ]
        );
        assert_eq!(clip.frames(), 30);
        assert!(plan.remove(0, plan.frames()).is_err());
        assert_eq!(plan.frames(), 90);
    }
    #[test]
    fn splitting_without_removing_content_preserves_preview_identity() {
        let mut plan = plan();
        let identity = plan.fingerprint().unwrap();
        plan.split(60).unwrap();
        assert_eq!(identity, plan.fingerprint().unwrap());
    }
    #[test]
    fn audio_and_video_cuts_use_the_same_thirty_fps_sample_grid() {
        let plan = plan().selection(1, 31).unwrap();
        let graph = filter_graph(&plan, true, 0.1).unwrap();
        assert!(graph.contains("start_frame=1:end_frame=31"));
        assert!(graph.contains("start_sample=1600:end_sample=49600"));
        assert!(graph.contains("min_hard_comp=0:first_pts=0"));
        assert!(graph.contains("[joinedv]settb=1/30,setpts=N[video]"));
        assert!(!graph.contains("copy"));
        assert!(!filter_graph(&plan, false, 0.0).unwrap().contains("[0:a"));
    }
    #[test]
    fn cancel_and_final_publication_have_one_linearization_boundary() {
        let cancelled = VideoControl::default();
        assert!(cancelled.cancel());
        let mut wrote = false;
        assert!(cancelled
            .commit(|| {
                wrote = true;
                Ok(())
            })
            .is_err());
        assert!(!wrote);
        let saved = VideoControl::default();
        saved.commit(|| Ok(())).unwrap();
        assert!(!saved.cancel());
    }
}
