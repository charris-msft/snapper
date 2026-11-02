use super::{
    publication::{self, FinalPublication, StableMedia},
    Request, Sources, TestFault,
};
use crate::{
    library::{self, Capture, CaptureKind, Library},
    platform::process,
    transport::Matroska,
};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Segment {
    pub file: String,
    pub start: f64,
    pub end: f64,
    pub bytes: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Journal {
    pub schema: u32,
    pub request: Request,
    pub segments: Vec<Segment>,
    pub completed: bool,
    pub recovered: bool,
    pub output: Option<PathBuf>,
    pub interrupted: Option<String>,
    #[serde(default)]
    pub(super) final_publication: Option<FinalPublication>,
}
impl Journal {
    pub fn validate(&self, directory: &Path) -> Result<()> {
        ensure!(
            matches!(self.schema, 1 | 2) && self.request.session_dir == directory,
            "Unsupported or mismatched recovery journal."
        );
        self.request.validate()?;
        ensure!(
            self.segments.len() <= 4000,
            "Journal exceeds its segment budget."
        );
        let mut previous = 0.0;
        for (index, segment) in self.segments.iter().enumerate() {
            ensure!(
                segment.bytes > 0 && segment.bytes <= 512 * 1024 * 1024,
                "Committed segment exceeds its size budget."
            );
            ensure!(
                segment.file == format!("segment-{index:05}.mp4"),
                "Invalid committed segment path."
            );
            ensure!(
                segment.start.is_finite()
                    && segment.end.is_finite()
                    && segment.start >= 0.0
                    && segment.end > segment.start
                    && segment.start >= previous - 0.1
                    && segment.end <= self.request.max_seconds as f64 + 10.0,
                "Invalid or inconsistent committed segment timestamps."
            );
            previous = segment.end;
        }
        if let Some(output) = &self.output {
            let id = directory
                .file_name()
                .context("Missing recovery session ID")?;
            ensure!(
                *output
                    == self
                        .request
                        .library_root
                        .join("captures")
                        .join(id)
                        .join("recording.mp4"),
                "Recovery output path is outside the expected capture folder."
            );
        }
        if let Some(publication) = &self.final_publication {
            publication.validate(self)?;
        }
        Ok(())
    }
}

pub struct Encoder {
    child: Child,
    stream: Option<Matroska<ChildStdin>>,
    log: Arc<Mutex<VecDeque<String>>>,
    directory: PathBuf,
}
fn drain_log(pipe: impl std::io::Read + Send + 'static, log: Arc<Mutex<VecDeque<String>>>) {
    thread::spawn(move || {
        let mut reader = BufReader::new(pipe);
        let mut chunk = Vec::with_capacity(4096);
        loop {
            chunk.clear();
            let line = match (&mut reader).take(4096).read_until(b'\n', &mut chunk) {
                Ok(0) => break,
                Ok(_) => String::from_utf8_lossy(&chunk).into_owned(),
                Err(error) => {
                    log.lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push_back(format!("Diagnostic pipe failed: {error}"));
                    break;
                }
            };
            let mut log = log.lock().unwrap_or_else(|e| e.into_inner());
            if log.len() == 80 {
                log.pop_front();
            }
            log.push_back(line.chars().take(1024).collect());
        }
    });
}
impl Encoder {
    pub fn start(request: &Request) -> Result<Self> {
        let (width, height) = request.canvas();
        let ffmpeg = process::media_tool("ffmpeg")?;
        let mut command = process::command(&ffmpeg);
        command.args([
            "-hide_banner",
            "-loglevel",
            "warning",
            "-nostdin",
            "-probesize",
            "32768",
            "-analyzeduration",
            "0",
            "-fpsprobesize",
            "0",
            "-thread_queue_size",
            "2",
            "-filter_threads",
            "1",
            "-f",
            "matroska",
            "-i",
            "pipe:0",
            "-map",
            "0:v:0",
        ]);
        match request.audio_count() {
            0 => {
                command.arg("-an");
            }
            1 => {
                command.args([
                    "-map",
                    "0:a:0",
                    "-af",
                    "aresample=48000:async=1000:min_hard_comp=0.05:first_pts=0",
                ]);
            }
            2 => {
                command.args(["-filter_complex_threads","1","-filter_complex",
                    "[0:a:0]aresample=48000:async=1000:min_hard_comp=0.05:first_pts=0[s];[0:a:1]aresample=48000:async=1000:min_hard_comp=0.05:first_pts=0[m];[s][m]amix=inputs=2:duration=longest:dropout_transition=0:normalize=1[mixed]",
                    "-map","[mixed]"]);
            }
            _ => unreachable!(),
        }
        command
            .args([
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-tune",
                "zerolatency",
                "-crf",
                "20",
                "-threads",
                "2",
                "-pix_fmt",
                "yuv420p",
                "-g",
                "60",
                "-bf",
                "0",
                "-flags",
                "+cgop",
                "-sc_threshold",
                "0",
                "-force_key_frames",
                "expr:gte(t,n_forced*2)",
                "-fps_mode",
                "vfr",
                "-enc_time_base:v",
                "1:1000",
                "-c:a",
                "aac",
                "-b:a",
                "160k",
                "-max_muxing_queue_size",
                "64",
                "-max_interleave_delta",
                "1000000",
                "-f",
                "segment",
                "-segment_time",
                "2",
                "-segment_time_delta",
                "0.02",
                "-reset_timestamps",
                "1",
                "-segment_format",
                "mp4",
                "-segment_format_options",
                "movflags=+faststart",
                "-segment_list_type",
                "csv",
                "-segment_list",
            ])
            .arg(request.session_dir.join("segments.csv"))
            .arg(request.session_dir.join("chunk-%05d.pending.mp4"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .context("Starting the pinned local encoder")?;
        let log = Arc::new(Mutex::new(VecDeque::new()));
        drain_log(
            child.stderr.take().context("Encoder stderr is missing")?,
            log.clone(),
        );
        let stdin = child
            .stdin
            .take()
            .context("Encoder media input is missing")?;
        let stream = Matroska::new(stdin, width, height, request.audio_count())?;
        Ok(Self {
            child,
            stream: Some(stream),
            log,
            directory: request.session_dir.clone(),
        })
    }
    pub fn video(&mut self, ms: u64, bytes: &[u8]) -> Result<()> {
        self.stream
            .as_mut()
            .context("Encoder is already finalizing")?
            .video(ms, bytes)
            .context("Writing timestamped video to the encoder")
    }
    pub fn audio(&mut self, index: u8, ms: u64, bytes: &[u8]) -> Result<()> {
        self.stream
            .as_mut()
            .context("Encoder is already finalizing")?
            .audio(index, ms, bytes)
    }
    pub fn check(&mut self) -> Result<()> {
        if let Some(code) = self.child.try_wait()? {
            bail!(
                "Encoder exited during capture ({code}). {}",
                self.diagnostics()
            );
        }
        Ok(())
    }
    pub fn abort_for_test(&mut self) -> Result<()> {
        self.child.kill()?;
        self.child.wait()?;
        bail!("Injected encoder exit in a generated-media test.")
    }
    pub fn diagnostics(&self) -> String {
        self.log
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    }
    pub fn finish(mut self) -> Result<()> {
        if let Some(stream) = self.stream.take() {
            drop(stream.finish()?);
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(code) = self.child.try_wait()? {
                library::atomic_write(
                    &self.directory.join("encoder.log"),
                    self.diagnostics().as_bytes(),
                )?;
                ensure!(
                    code.success(),
                    "Encoder finalization failed ({code}). {}",
                    self.diagnostics()
                );
                return Ok(());
            }
            if Instant::now() > deadline {
                self.child.kill()?;
                self.child.wait()?;
                bail!(
                    "Encoder finalization exceeded 30 seconds. Committed segments were retained."
                );
            }
            thread::sleep(Duration::from_millis(25));
        }
    }
}
impl Drop for Encoder {
    fn drop(&mut self) {
        self.stream.take();
        match self.child.try_wait() {
            Ok(Some(_)) => {}
            _ => {
                if let Err(e) = self.child.kill() {
                    eprintln!("Encoder cleanup: {e}");
                }
                if let Err(e) = self.child.wait() {
                    eprintln!("Encoder exit confirmation: {e}");
                }
            }
        }
    }
}

pub fn create_journal(request: &Request) -> Result<Journal> {
    let journal = Journal {
        schema: 2,
        request: request.clone(),
        segments: vec![],
        completed: false,
        recovered: false,
        output: None,
        interrupted: None,
        final_publication: None,
    };
    library::write_json(&request.session_dir.join("journal.json"), &journal)?;
    Ok(journal)
}

pub fn run_bounded(mut command: std::process::Command, timeout: Duration) -> Result<()> {
    run_bounded_with_output(&mut command, timeout, None)
}

pub fn run_bounded_with_output(
    command: &mut std::process::Command,
    timeout: Duration,
    output: Option<fs::File>,
) -> Result<()> {
    run_bounded_cancellable(
        command,
        timeout,
        output,
        &std::sync::atomic::AtomicBool::new(false),
        || Ok(()),
    )
}

#[derive(Debug)]
pub struct Cancelled;
impl std::fmt::Display for Cancelled {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(
            "Video work cancelled. The original and previously saved files are unchanged.",
        )
    }
}
impl std::error::Error for Cancelled {}

pub fn run_bounded_cancellable(
    command: &mut std::process::Command,
    timeout: Duration,
    output: Option<fs::File>,
    cancel: &std::sync::atomic::AtomicBool,
    mut tick: impl FnMut() -> Result<()>,
) -> Result<()> {
    if cancel.load(std::sync::atomic::Ordering::Acquire) {
        return Err(Cancelled.into());
    }
    let tool = if command.get_program() == process::media_tool("ffmpeg")?.as_os_str() {
        "ffmpeg"
    } else if command.get_program() == process::media_tool("ffprobe")?.as_os_str() {
        "ffprobe"
    } else {
        bail!("Only the pinned media tools can run in a codec helper.");
    };
    let arguments = command
        .get_args()
        .map(|value| {
            value
                .to_str()
                .context("Media argument is not valid Unicode")
        })
        .collect::<Result<Vec<_>>>()?;
    let encoded = serde_json::to_string(&arguments)?;
    ensure!(
        encoded.len() <= 24_000,
        "Media helper arguments exceed the 24 KiB command budget."
    );
    let job = process::Job::new()?;
    let log = Arc::new(Mutex::new(VecDeque::new()));
    let mut child = process::command(&std::env::current_exe()?)
        .args(["--codec-helper", tool, &encoded])
        .stdin(Stdio::piped())
        .stdout(output.map(Stdio::from).unwrap_or_else(Stdio::null))
        .stderr(Stdio::piped())
        .spawn()?;
    if let Err(error) = job.assign(&child) {
        child.kill()?;
        child.wait()?;
        return Err(error).context("Codec helper Job assignment failed; tool was not started");
    }
    drain_log(
        child
            .stderr
            .take()
            .context("Media validation log pipe missing")?,
        log.clone(),
    );
    child
        .stdin
        .take()
        .context("Codec helper start gate is missing")?
        .write_all(b"start\n")?;
    let start = Instant::now();
    loop {
        if cancel.load(std::sync::atomic::Ordering::Acquire) {
            end_codec_job(&job, &mut child)?;
            return Err(Cancelled.into());
        }
        if let Err(error) = tick() {
            end_codec_job(&job, &mut child)?;
            return Err(error);
        }
        if let Some(code) = child.try_wait()? {
            if job.active_processes()? > 0 {
                job.terminate()?;
            }
            ensure!(
                code.success(),
                "Media operation failed ({code}): {}",
                log.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            return Ok(());
        }
        if start.elapsed() > timeout {
            end_codec_job(&job, &mut child)?;
            bail!(
                "Media operation exceeded {} seconds; existing captures were preserved.",
                timeout.as_secs()
            );
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn end_codec_job(job: &process::Job, child: &mut std::process::Child) -> Result<()> {
    job.terminate()?;
    child.wait()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while job.active_processes()? > 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    ensure!(
        job.active_processes()? == 0,
        "Codec cancellation was requested, but owned-process release could not be confirmed."
    );
    Ok(())
}

pub fn codec_helper(tool: &str, encoded: &str) -> Result<()> {
    ensure!(
        encoded.len() <= 24_000,
        "Codec helper arguments exceed their size budget."
    );
    let arguments: Vec<String> = serde_json::from_str(encoded)?;
    ensure!(
        arguments.len() <= 128,
        "Codec helper argument count exceeds its budget."
    );
    super::await_start_gate()?;
    let mut child = process::command(&process::media_tool(tool)?)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;
    let status = child.wait()?;
    ensure!(status.success(), "{tool} exited with {status}.");
    Ok(())
}

pub fn validate_video(path: &Path) -> Result<()> {
    validate_video_with_timeout(path, Duration::from_secs(45))
}
fn validate_video_with_timeout(path: &Path, timeout: Duration) -> Result<()> {
    let mut command = process::command(&process::media_tool("ffmpeg")?);
    command
        .args([
            "-hide_banner",
            "-v",
            "error",
            "-xerror",
            "-threads",
            "1",
            "-i",
        ])
        .arg(path)
        .args(["-map", "0:v:0", "-map", "0:a?", "-f", "null", "NUL"]);
    run_bounded(command, timeout).context("Decoding all frames/audio in a committed segment")
}

pub fn publish_segments(journal: &mut Journal) -> Result<()> {
    journal.validate(&journal.request.session_dir)?;
    let list = journal.request.session_dir.join("segments.csv");
    if !list.exists() {
        return Ok(());
    }
    ensure!(
        fs::metadata(&list)?.len() <= 2 * 1024 * 1024,
        "Segment index exceeds its 2 MiB budget."
    );
    let text = fs::read_to_string(list)?;
    for (index, line) in text
        .split_inclusive('\n')
        .filter(|line| line.ends_with('\n'))
        .enumerate()
    {
        let line = line.trim_end();
        ensure!(index < 4000, "Session exceeds its segment-count budget.");
        if index < journal.segments.len() {
            continue;
        }
        let columns: Vec<_> = line.rsplitn(3, ',').collect();
        ensure!(columns.len() == 3, "Malformed completed segment index row.");
        let expected = format!("chunk-{index:05}.pending.mp4");
        let listed = Path::new(columns[2].trim_matches('"'))
            .file_name()
            .and_then(|n| n.to_str());
        ensure!(
            listed == Some(expected.as_str()),
            "Unexpected segment filename in the local journal."
        );
        let start: f64 = columns[1].parse()?;
        let end: f64 = columns[0].parse()?;
        ensure!(
            start.is_finite() && end.is_finite() && end > start,
            "Invalid segment time range."
        );
        let pending = journal.request.session_dir.join(&expected);
        let published = format!("segment-{index:05}.mp4");
        let target = journal.request.session_dir.join(&published);
        let file = if target.exists() { &target } else { &pending };
        let handoff = closed_segment(file, Duration::from_secs(3))?;
        validate_video(file)?;
        let bytes = fs::metadata(file)?.len();
        ensure!(bytes > 0, "Encoder produced an empty segment.");
        handoff.sync_all().context("Flushing completed segment")?;
        drop(handoff);
        if !target.exists() {
            fs::rename(&pending, &target).context("Publishing completed segment")?;
        }
        if journal.request.test_fault == Some(TestFault::AfterSegmentRename) {
            super::mark_fault(&journal.request, TestFault::AfterSegmentRename)?;
            std::process::exit(98);
        }
        journal.segments.push(Segment {
            file: published,
            start,
            end,
            bytes,
        });
        library::write_json(&journal.request.session_dir.join("journal.json"), journal)
            .context("Committing the segment manifest after media publication")?;
    }
    Ok(())
}

pub fn final_export(journal: &mut Journal) -> Result<Capture> {
    journal.validate(&journal.request.session_dir)?;
    ensure!(
        !journal.segments.is_empty(),
        "Nothing recoverable yet: no segment was finalized before capture stopped."
    );
    let library = Library::open(journal.request.library_root.clone())?;
    let id = journal
        .request
        .session_dir
        .file_name()
        .and_then(|n| n.to_str())
        .context("Session ID is invalid")?
        .to_owned();
    let (width, height) = journal.request.canvas();
    let mut capture = Capture {
        schema: 1,
        id,
        title: if journal.recovered {
            "Recovered recording".into()
        } else {
            "Screen recording".into()
        },
        kind: CaptureKind::Video,
        created_ms: library::now_ms(),
        width,
        height,
        tags: vec![],
    };
    let dir = library.capture_dir(&capture)?;
    fs::create_dir_all(&dir)?;
    let metadata = dir.join("capture.json");
    if metadata.exists() {
        let existing: Capture = library::read_json(&metadata)?;
        existing.validate()?;
        ensure!(
            existing.id == capture.id
                && existing.kind == CaptureKind::Video
                && existing.width == width
                && existing.height == height,
            "Existing capture metadata disagrees with recovery."
        );
        capture = existing;
    }
    let final_path = dir.join("recording.mp4");
    let adopting = final_path.exists();
    if !adopting {
        let temporary = dir.join("recording.partial.mp4");
        let mut concat = String::from("ffconcat version 1.0\n");
        for (index, segment) in journal.segments.iter().enumerate() {
            ensure!(
                segment.file.starts_with("segment-")
                    && segment.file.ends_with(".mp4")
                    && segment
                        .file
                        .bytes()
                        .all(|c| c.is_ascii_digit() || b"segment-.mp4".contains(&c)),
                "Unsafe segment name."
            );
            concat.push_str(&format!(
                "file '{}'\nduration {:.6}\n",
                segment.file,
                journal
                    .segments
                    .get(index + 1)
                    .map(|next| next.start)
                    .unwrap_or(segment.end)
                    - segment.start
            ));
        }
        let concat_path = journal.request.session_dir.join("concat.txt");
        library::atomic_write(&concat_path, concat.as_bytes())?;
        let required = journal.segments.iter().map(|s| s.bytes).sum::<u64>() + 128 * 1024 * 1024;
        ensure!(
            process::free_space(&dir)? > required,
            "Insufficient space for final MP4. Committed segments remain available for recovery."
        );
        let mut command = process::command(&process::media_tool("ffmpeg")?);
        command
            .args([
                "-hide_banner",
                "-v",
                "error",
                "-nostdin",
                "-f",
                "concat",
                "-safe",
                "1",
                "-i",
            ])
            .arg(&concat_path)
            .args(["-map", "0", "-c", "copy", "-movflags", "+faststart", "-y"])
            .arg(&temporary);
        run_bounded(command, Duration::from_secs(120))
            .context("Final MP4 remux; segments remain unchanged")?;
        OpenOptions::new()
            .write(true)
            .open(&temporary)?
            .sync_all()?;
        let mut prepared = StableMedia::open(&temporary)?;
        // Every source segment has already been fully decoded. Check the joined stream boundaries too.
        let duration = publication::timeline_seconds(journal)?;
        for at in [0.0, (duration - 1.0).max(0.0)] {
            let mut check = process::command(&process::media_tool("ffmpeg")?);
            check
                .args(["-v", "error", "-xerror", "-ss", &format!("{at:.3}"), "-i"])
                .arg(&temporary)
                .args([
                    "-t", "1", "-map", "0:v:0", "-map", "0:a?", "-f", "null", "NUL",
                ]);
            run_bounded(check, Duration::from_secs(30))?;
        }
        journal.schema = 2;
        journal.final_publication = Some(prepared.prepare_publication(journal)?);
        library::write_json(&journal.request.session_dir.join("journal.json"), journal)
            .context("Recording durable final-media identity before publication")?;
        drop(prepared);
        publication::publish_new_file(&temporary, &final_path)?;
        if journal.request.test_fault == Some(TestFault::AfterFinalRename) {
            super::mark_fault(&journal.request, TestFault::AfterFinalRename)?;
            std::process::exit(99);
        }
    }
    let mut final_input = StableMedia::open(&final_path)?;
    final_input.verify_publication(journal)?;
    if adopting {
        let seconds = publication::timeline_seconds(journal)?;
        validate_video_with_timeout(
            &final_path,
            Duration::from_secs((seconds / 5.0).ceil() as u64 + 45),
        )?;
    }
    library.save_capture(&capture).context("The final MP4 is saved, but library metadata publication failed; recover this session to adopt it")?;
    let mut thumb = process::command(&process::media_tool("ffmpeg")?);
    thumb
        .args(["-v", "error", "-i"])
        .arg(&final_path)
        .args([
            "-frames:v",
            "1",
            "-vf",
            "scale=240:150:force_original_aspect_ratio=decrease",
            "-y",
        ])
        .arg(dir.join("thumbnail.png"));
    run_bounded(thumb, Duration::from_secs(30))
        .context("The final MP4 and its library entry are saved, but thumbnail generation failed. Play it from Library; Recover retries finalization")?;
    journal.output = Some(final_path);
    journal.completed = true;
    library::write_json(&journal.request.session_dir.join("journal.json"), journal)?;
    drop(final_input);
    Ok(capture)
}

pub fn recover(directory: &Path) -> Result<Capture> {
    let _ownership = library::exclusive_lock(&directory.join("session.lock"))?;
    let mut journal: Journal = library::read_json(&directory.join("journal.json"))?;
    journal.validate(directory)?;
    ensure!(!journal.completed, "This recording is already complete.");
    journal.request.test_fault = None;
    publish_segments(&mut journal)?;
    // A published file can survive a crash before either the CSV or journal is durable.
    for index in journal.segments.len()..4000 {
        let name = format!("segment-{index:05}.mp4");
        let path = directory.join(&name);
        if !path.exists() {
            break;
        }
        validate_video(&path)?;
        let start = journal.segments.last().map(|s| s.end).unwrap_or(0.0);
        let duration = probe_duration(&path, directory)?;
        journal.segments.push(Segment {
            file: name,
            start,
            end: start + duration,
            bytes: fs::metadata(&path)?.len(),
        });
        journal.interrupted = Some(
            "An orphan committed segment was reconciled using its validated container duration."
                .into(),
        );
        library::write_json(&directory.join("journal.json"), &journal)?;
    }
    for segment in &journal.segments {
        validate_video(&directory.join(&segment.file))?;
    }
    journal.recovered = true;
    final_export(&mut journal)
}

pub fn incomplete(library: &Library) -> Result<(Vec<PathBuf>, Vec<String>)> {
    let mut output = Vec::new();
    let mut warnings = Vec::new();
    for (count, entry) in fs::read_dir(library.root.join("sessions"))?.enumerate() {
        ensure!(count < 5000, "Recovery session count exceeds 5,000.");
        let path = entry?.path();
        if path.join("journal.json").is_file() {
            match library::read_json::<Journal>(&path.join("journal.json")).and_then(|j| {
                j.validate(&path)?;
                Ok(j)
            }) {
                Ok(journal) => {
                    if !journal.completed {
                        output.push(path);
                    }
                }
                Err(error) => warnings.push(format!(
                    "Recovery journal needs attention at {}: {error:#}",
                    path.display()
                )),
            }
        }
    }
    Ok((output, warnings))
}

fn probe_duration(path: &Path, directory: &Path) -> Result<f64> {
    let temporary = directory.join(format!("duration-{}.tmp", library::id()));
    let output = fs::File::create(&temporary)?;
    let mut command = process::command(&process::media_tool("ffprobe")?);
    command
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path);
    let result = (|| -> Result<f64> {
        run_bounded_with_output(&mut command, Duration::from_secs(15), Some(output))?;
        ensure!(
            fs::metadata(&temporary)?.len() < 128,
            "Unexpected duration response."
        );
        let duration = fs::read_to_string(&temporary)?.trim().parse::<f64>()?;
        ensure!(
            duration.is_finite() && duration > 0.0 && duration < 30.0,
            "Invalid recovered segment duration."
        );
        Ok(duration)
    })();
    fs::remove_file(&temporary).context("Removing the session-owned duration scratch file")?;
    result
}

fn closed_segment(path: &Path, timeout: Duration) -> Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    let deadline = Instant::now() + timeout;
    loop {
        match OpenOptions::new().read(true).write(true).share_mode(1).open(path) {
            Ok(file)=>return Ok(file),
            Err(error) if matches!(error.raw_os_error(),Some(32|33))&&Instant::now()<deadline=>{
                thread::sleep(Duration::from_millis(10));
            },
            Err(error)=>return Err(error).with_context(||format!(
                "Segment producer did not release {} within {} ms, or the file could not be opened; it was not committed.",
                path.display(),timeout.as_millis())),
        }
    }
}

pub fn source_description(s: &Sources) -> String {
    format!(
        "Cursor {} | Webcam {} | System audio {} | Microphone {}",
        if s.cursor { "ON" } else { "OFF" },
        if s.webcam { "ON" } else { "OFF" },
        if s.system_audio { "ON" } else { "OFF" },
        if s.microphone { "ON" } else { "OFF" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn segment_handoff_waits_for_the_producer_to_close() {
        use std::os::windows::fs::OpenOptionsExt;
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("segment.mp4");
        let producer = OpenOptions::new()
            .create_new(true)
            .write(true)
            .share_mode(3)
            .open(&path)
            .unwrap();
        let writer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(80));
            drop(producer);
        });
        let begin = Instant::now();
        let file = closed_segment(&path, Duration::from_secs(2)).unwrap();
        assert!(begin.elapsed() >= Duration::from_millis(60));
        drop(file);
        writer.join().unwrap();
    }
    #[test]
    fn held_segment_times_out_without_deleting_it() {
        use std::os::windows::fs::OpenOptionsExt;
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("segment.mp4");
        let _producer = OpenOptions::new()
            .create_new(true)
            .write(true)
            .share_mode(3)
            .open(&path)
            .unwrap();
        assert!(closed_segment(&path, Duration::from_millis(30)).is_err());
        assert!(path.exists());
    }
    #[test]
    fn one_malformed_journal_does_not_hide_valid_captures_or_recovery_sessions() {
        let temporary = tempfile::tempdir().unwrap();
        let library = Library::open(temporary.path().to_owned()).unwrap();
        let capture = library
            .import_image(&image::RgbaImage::new(20, 20), "Visible")
            .unwrap();
        let session = library.root.join("sessions").join("valid");
        fs::create_dir(&session).unwrap();
        let request = Request {
            schema: 1,
            session_dir: session.clone(),
            library_root: library.root.clone(),
            area: crate::platform::Area {
                monitor: "generated".into(),
                width: 20,
                height: 20,
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
        create_journal(&request).unwrap();
        let bad = library.root.join("sessions").join("broken");
        fs::create_dir(&bad).unwrap();
        fs::write(bad.join("journal.json"), b"not valid JSON").unwrap();
        let (sessions, warnings) = incomplete(&library).unwrap();
        assert_eq!(sessions, [session]);
        assert_eq!(warnings.len(), 1);
        assert_eq!(library.list().unwrap().0[0].id, capture.id);
        assert_eq!(
            fs::read(bad.join("journal.json")).unwrap(),
            b"not valid JSON"
        );
    }
}
