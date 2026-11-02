use super::{encoder, selftest, supervisor::Recorder, Phase, Request, Sources, TestFault};
use crate::{
    library::{self, Library},
    platform::{process, Area},
};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::{
    fs,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

struct CpuPeer {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<f64>>>,
    started: Instant,
}
impl CpuPeer {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let cancel = stop.clone();
        let thread = thread::spawn(move || {
            let before = process::current_thread_cpu_100ns()?;
            let mut value = 1u64;
            while !cancel.load(Ordering::Acquire) {
                let until = Instant::now() + Duration::from_millis(5);
                while Instant::now() < until {
                    for _ in 0..1000 {
                        value = value.wrapping_mul(6364136223846793005).wrapping_add(1);
                    }
                    std::hint::black_box(value);
                }
                thread::sleep(Duration::from_millis(5));
            }
            Ok((process::current_thread_cpu_100ns()? - before) as f64 / 10_000_000.0)
        });
        Self {
            stop,
            thread: Some(thread),
            started: Instant::now(),
        }
    }
    fn finish(mut self) -> Result<Value> {
        self.stop.store(true, Ordering::Release);
        let cpu = self
            .thread
            .take()
            .context("CPU peer is already joined")?
            .join()
            .map_err(|_| anyhow::anyhow!("Bounded CPU peer panicked"))??;
        let seconds = self.started.elapsed().as_secs_f64();
        Ok(json!({"threads":1,"duty_policy":"5 ms work, 5 ms sleep",
            "thread_cpu_seconds":cpu,"wall_seconds":seconds,"logical_core_utilization":cpu/seconds,
            "logical_processors":thread::available_parallelism()?.get()}))
    }
}
impl Drop for CpuPeer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            match thread.join() {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => eprintln!("CPU peer shutdown: {error:#}"),
                Err(_) => eprintln!("CPU peer panicked during shutdown."),
            }
        }
    }
}

pub fn run(root: &Path, seconds: u64) -> Result<()> {
    ensure!(
        (12..=600).contains(&seconds),
        "Profile test must run 12-600 seconds."
    );
    ensure!(
        !cfg!(debug_assertions),
        "Performance/sync profiles require an optimized release build."
    );
    let root = if root.is_absolute() {
        root.to_owned()
    } else {
        std::env::current_dir()?.join(root)
    };
    fs::create_dir_all(&root)?;
    let run = root.join(format!("run-{}", library::id()));
    fs::create_dir(&run)?;
    let mut cases = Vec::new();
    for (name, duration, load) in [
        ("baseline", 20, false),
        ("bounded-load-and-video-gaps", seconds, true),
    ] {
        let result = record(&run.join(name), duration, load);
        let (passed, evidence, error) = match result {
            Ok(evidence) => (true, evidence, Value::Null),
            Err(error) => (false, Value::Null, json!(format!("{error:#}"))),
        };
        println!(
            "{} {name}: {}",
            if passed { "PASS" } else { "FAIL" },
            if passed {
                evidence.to_string()
            } else {
                error.to_string()
            }
        );
        cases.push(json!({"name":name,"passed":passed,"evidence":evidence,"error":error}));
        library::write_json(&run.join("results.json"), &cases)?;
    }
    let passed = cases.iter().filter(|case| case["passed"] == true).count();
    let summary = json!({"kind":"generated-media-1080p-time-localized-sync-and-bounded-load",
        "passed":passed,"total":cases.len(),"build":selftest::build_identity()?,"results":run.join("results.json"),
        "real_devices_tested":false,
        "limitations":"One bounded CPU peer is not host-wide heavy-load proof. Generated A/V markers do not calibrate camera optics, microphone or actual WASAPI/WGC delivery."});
    library::write_json(&root.join("latest.json"), &summary)?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    ensure!(
        passed == cases.len(),
        "Profile acceptance failed. See {}.",
        run.display()
    );
    Ok(())
}

pub fn native(template: &Request, root: &Path, approved: bool) -> Result<()> {
    ensure!(approved, "Native profiling captures the explicitly selected screen rectangle twice. Explicit screen-capture approval is required.");
    template.validate()?;
    ensure!(template.video && !template.synthetic && template.test_fault.is_none(),
        "Native profiling requires a video request for an explicit real screen region, without fault injection.");
    ensure!(!template.sources.webcam && !template.sources.microphone && !template.sources.system_audio,
        "This native profile does not authorize private inputs. Leave camera, microphone and system audio OFF.");
    ensure!(
        (12..=120).contains(&template.max_seconds),
        "Each native profiling pass must be 12-120 seconds."
    );
    ensure!(
        !cfg!(debug_assertions),
        "Native efficiency measurements require an optimized release candidate."
    );
    let root = if root.is_absolute() {
        root.to_owned()
    } else {
        std::env::current_dir()?.join(root)
    };
    fs::create_dir_all(&root)?;
    let run = root.join(format!("run-{}", library::id()));
    fs::create_dir(&run)?;
    let mut cases = Vec::new();
    for (name, load) in [
        ("native-idle-load", false),
        ("native-bounded-competing-load", true),
    ] {
        let result = native_case(template, &run.join(name), load);
        let case = match result {
            Ok(measurements) => json!({"name":name,"measured":true,"measurements":measurements}),
            Err(error) => json!({"name":name,"measured":false,"error":format!("{error:#}")}),
        };
        println!("{}", serde_json::to_string(&case)?);
        cases.push(case);
        library::write_json(&run.join("results.json"), &cases)?;
    }
    let measured = cases.iter().filter(|case| case["measured"] == true).count();
    let summary = json!({
        "kind":"native-wgc-efficiency-measurements", "measured":measured,"total":cases.len(),
        "build":selftest::build_identity()?, "results":run.join("results.json"),
        "scope":"Actual WGC recorder and owned encoder/helper process family. CLI harness; GUI rendering overhead is not included.",
        "private_inputs_activated":false,
        "av_timing":"Native audio/camera calibration is not measured with sources OFF. Same-build generated A/V acceptance is separate.",
        "performance_verdict":"Measured data, not a universal performance pass or arbitrary-load guarantee."
    });
    library::write_json(&root.join("latest.json"), &summary)?;
    ensure!(
        measured == cases.len(),
        "Native profiling did not complete both passes. See {}.",
        run.display()
    );
    Ok(())
}

fn cpu_window(
    before: process::ProcessStats,
    after: process::ProcessStats,
    wall: f64,
    child_ticks: u64,
) -> Result<Value> {
    ensure!(
        wall > 0.0 && wall.is_finite(),
        "CPU measurement window is invalid."
    );
    let parent = after
        .cpu_100ns
        .checked_sub(before.cpu_100ns)
        .context("Parent CPU counter regressed")?;
    let system = after
        .system_total_100ns
        .checked_sub(before.system_total_100ns)
        .context("System CPU counter regressed")?;
    let idle = after
        .system_idle_100ns
        .checked_sub(before.system_idle_100ns)
        .context("System idle counter regressed")?;
    ensure!(
        system > 0 && idle <= system,
        "System CPU sample is incomplete."
    );
    let cores = thread::available_parallelism()?.get();
    let cpu_seconds = (parent + child_ticks) as f64 / 10_000_000.0;
    Ok(json!({
        "window_seconds":wall,"logical_processors":cores,"parent_cpu_seconds":parent as f64/10_000_000.0,
        "worker_family_cpu_seconds":child_ticks as f64/10_000_000.0,"family_cpu_seconds":cpu_seconds,
        "family_logical_core_percent":cpu_seconds/wall*100.0,
        "family_machine_percent":cpu_seconds/wall/cores as f64*100.0,
        "system_busy_percent":(system-idle) as f64/system as f64*100.0
    }))
}

fn native_case(template: &Request, root: &Path, load: bool) -> Result<Value> {
    let library = Library::open(root.to_owned())?;
    let mut request = template.clone();
    request.library_root = library.root.clone();
    request.session_dir = library.root.join("sessions").join(library::id());
    request.max_seconds = template.max_seconds + 5;
    let baseline_start = process::current_process_stats()?;
    let baseline_clock = Instant::now();
    thread::sleep(Duration::from_secs(2));
    let baseline_end = process::current_process_stats()?;
    let baseline = cpu_window(
        baseline_start,
        baseline_end,
        baseline_clock.elapsed().as_secs_f64(),
        0,
    )?;
    let started = Instant::now();
    let recorder = Recorder::start(request.clone());
    let active = loop {
        let progress = recorder.progress();
        if progress.phase == Phase::Recording {
            break progress;
        }
        ensure!(
            !recorder.finished() && started.elapsed() < Duration::from_secs(30),
            "Native recorder did not start: {}",
            progress.detail
        );
        thread::sleep(Duration::from_millis(25));
    };
    let startup_ms = started.elapsed().as_millis();
    let active_start = process::current_process_stats()?;
    let active_clock = Instant::now();
    let peer = if load { Some(CpuPeer::start()) } else { None };
    let mut parent_peak = active_start.private_bytes;
    let stop_progress = loop {
        let progress = recorder.progress();
        parent_peak = parent_peak.max(process::current_process_stats()?.private_bytes);
        ensure!(
            !recorder.finished(),
            "Native recording ended early: {}",
            progress.detail
        );
        if progress.elapsed_ms >= template.max_seconds * 1000 {
            break progress;
        }
        ensure!(
            active_clock.elapsed() < Duration::from_secs(template.max_seconds + 20),
            "Native media clock did not reach the requested duration."
        );
        thread::sleep(Duration::from_millis(25));
    };
    let active_end = process::current_process_stats()?;
    let active_seconds = active_clock.elapsed().as_secs_f64();
    let recording_cpu = cpu_window(
        active_start,
        active_end,
        active_seconds,
        stop_progress
            .worker_cpu_100ns
            .checked_sub(active.worker_cpu_100ns)
            .context("Owned-family CPU counter regressed")?,
    )?;
    let peer = peer
        .map(CpuPeer::finish)
        .transpose()?
        .unwrap_or(Value::Null);
    let stop_clock = crate::platform::clock_100ns()?;
    let stop_started = Instant::now();
    recorder.stop();
    while !recorder.finished() && stop_started.elapsed() < Duration::from_secs(185) {
        parent_peak = parent_peak.max(process::current_process_stats()?.private_bytes);
        thread::sleep(Duration::from_millis(25));
    }
    ensure!(
        recorder.finished(),
        "Native Stop/finalization exceeded 185 seconds."
    );
    let saved_ms = stop_started.elapsed().as_millis();
    let progress = recorder.progress();
    ensure!(
        progress.phase == Phase::Saved,
        "Native recording did not save: {}",
        progress.detail
    );
    let journal: encoder::Journal = library::read_json(&request.session_dir.join("journal.json"))?;
    let output = journal
        .output
        .context("Native recording has no published output")?;
    encoder::validate_video(&output)?;
    let counted_path = root.join("counted-probe.json");
    let mut command = process::command(&process::media_tool("ffprobe")?);
    command
        .args([
            "-v",
            "error",
            "-count_frames",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
        ])
        .arg(&output);
    encoder::run_bounded_with_output(
        &mut command,
        Duration::from_secs(30),
        Some(fs::File::create(&counted_path)?),
    )?;
    let probe: Value = library::read_json(&counted_path)?;
    let video = probe["streams"]
        .as_array()
        .context("Native probe has no streams")?
        .iter()
        .find(|stream| stream["codec_type"] == "video")
        .context("Native output has no video stream")?;
    let frames = video["nb_read_frames"]
        .as_str()
        .context("Native frame count missing")?
        .parse::<u64>()?;
    let duration = probe["format"]["duration"]
        .as_str()
        .context("Native duration missing")?
        .parse::<f64>()?;
    ensure!(
        duration > 0.0 && duration.is_finite(),
        "Native output duration is invalid."
    );
    let bytes = fs::metadata(&output)?.len();
    let stopped = progress
        .devices_stopped_ms
        .context("Device-release time was not reported")?;
    ensure!(
        progress.timeline_origin_100ns > 0,
        "Native media origin is missing."
    );
    let release_ms = ((i128::from(progress.timeline_origin_100ns) + i128::from(stopped) * 10_000
        - i128::from(stop_clock))
    .max(0)
        / 10_000) as u64;
    Ok(json!({
        "output":output,"source_region":request.area,"output_dimensions":[video["width"],video["height"]],
        "target_fps":30,"encoded_frames":frames,"encoded_average_fps":frames as f64/duration,
        "encoded_seconds":duration,"elapsed_media_ms":progress.elapsed_ms,
        "skipped_ticks":progress.skipped,"repeated_frames":progress.repeated,
        "output_bytes":bytes,"average_bitrate_bits_per_second":bytes as f64*8.0/duration,
        "cpu_baseline":baseline,"cpu_recording":recording_cpu,"bounded_cpu_peer":peer,
        "parent_peak_private_bytes":parent_peak,"worker_family_peak_committed_bytes":progress.peak_job_committed_bytes,
        "family_commit_upper_bound_bytes":parent_peak.saturating_add(progress.peak_job_committed_bytes),
        "memory_scope":"Sum of parent sampled private-byte peak and whole worker Job peak commit; peaks need not coincide. Driver/kernel GPU allocations are excluded.",
        "audio_queue_peak_bytes":progress.peak_audio_queue_bytes,
        "startup_ms":startup_ms,"stop_to_device_release_ms":release_ms,"stop_to_saved_ms":saved_ms,
        "device_release_to_saved_ms":saved_ms.saturating_sub(u128::from(release_ms)),
        "backend":progress.screen_backend,"encoder":"libx264 software",
        "native_audio_video_sync_measured":false
    }))
}

pub fn encoder_diagnostics(root: &Path) -> Result<()> {
    let root = if root.is_absolute() {
        root.to_owned()
    } else {
        std::env::current_dir()?.join(root)
    };
    fs::create_dir_all(&root)?;
    let mut results = Vec::new();
    for codec in ["libx264", "h264_qsv", "h264_nvenc", "h264_amf"] {
        let mut command = process::command(&process::media_tool("ffmpeg")?);
        command.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=1280x720:rate=30",
            "-frames:v",
            "30",
            "-an",
            "-c:v",
            codec,
            "-f",
            "null",
            "NUL",
        ]);
        let started = Instant::now();
        let result = encoder::run_bounded(command, Duration::from_secs(15));
        results.push(json!({"encoder":codec,"generated_encode_succeeded":result.is_ok(),
            "elapsed_ms":started.elapsed().as_millis(),"failure":result.err().map(|error|format!("{error:#}"))}));
    }
    let report = json!({
        "kind":"bounded-generated-frame-encoder-diagnostics","build":selftest::build_identity()?,
        "selected_recording_encoder":"libx264 software","automatic_encoder_switching":false,"results":results,
        "scope":"Generated frames only; no screen/camera/audio source was opened. Named encoder success does not prove hardware execution, WGC performance, or segmented recovery compatibility."
    });
    library::write_json(&root.join("encoders.json"), &report)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn record(root: &Path, seconds: u64, load: bool) -> Result<Value> {
    let library = Library::open(root.to_owned())?;
    let request = Request {
        schema: 1,
        session_dir: library.root.join("sessions").join(library::id()),
        library_root: library.root,
        area: Area {
            monitor: "generated-profile".into(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        },
        sources: Sources {
            cursor: true,
            webcam: true,
            system_audio: true,
            microphone: true,
        },
        camera_id: None,
        microphone_id: None,
        output_id: None,
        pip: None,
        video: true,
        max_seconds: seconds,
        synthetic: true,
        test_fault: if load {
            Some(TestFault::VideoDeliveryJitter)
        } else {
            None
        },
    };
    let peer = if load { Some(CpuPeer::start()) } else { None };
    let recorder = Recorder::start(request.clone());
    let deadline = Instant::now() + Duration::from_secs(seconds + 180);
    while !recorder.finished() {
        if Instant::now() > deadline {
            recorder.cancel_finalization();
            anyhow::bail!(
                "Generated profile exceeded its duration plus 180-second finalization budget."
            );
        }
        thread::sleep(Duration::from_millis(25));
    }
    let peer = peer
        .map(CpuPeer::finish)
        .transpose()?
        .unwrap_or(Value::Null);
    let progress = recorder.progress();
    ensure!(
        progress.phase == Phase::Saved,
        "Generated profile did not save: {}",
        progress.detail
    );
    let journal: encoder::Journal = library::read_json(&request.session_dir.join("journal.json"))?;
    let output = journal.output.context("Profile output is missing")?;
    let probe = selftest::probe(&output, root)?;
    let duration = probe["format"]["duration"]
        .as_str()
        .context("Profile duration missing")?
        .parse::<f64>()?;
    ensure!(
        (duration - seconds as f64).abs() < 0.15,
        "Profile duration changed: requested {seconds}s, encoded {duration:.6}s."
    );
    let mut offsets = Vec::new();
    for start in [0.5, (seconds / 2) as f64 + 0.5, seconds as f64 - 2.5] {
        let screen = video_edge(
            &output,
            start,
            40,
            600,
            &root.join(format!("screen-{start}.gray")),
        )?;
        let camera = video_edge(
            &output,
            start,
            1568,
            886,
            &root.join(format!("camera-{start}.gray")),
        )?;
        let screen_image = selftest::frame(
            &output,
            start + screen + 0.02,
            &root.join(format!("screen-identity-{start}.png")),
        )?;
        let camera_image = selftest::frame(
            &output,
            start + camera + 0.02,
            &root.join(format!("camera-identity-{start}.png")),
        )?;
        let marker = marker_second(&screen_image, false)?;
        ensure!(
            marker == marker_second(&camera_image, true)?,
            "Screen and camera pulses identify different source seconds."
        );
        let (system, microphone) = audio_edges(
            &output,
            start,
            u64::from(marker),
            &root.join(format!("sound-{start}.pcm")),
        )?;
        let system_offset = system - screen;
        let microphone_offset = microphone - screen;
        let camera_offset = camera - screen;
        ensure!(system_offset.abs()<=0.1&&microphone_offset.abs()<=0.1&&camera_offset.abs()<=0.1,
            "A/V marker mismatch at {start}s: screen {screen:.3}, camera {camera:.3}, system {system:.3}, mic {microphone:.3}.");
        offsets.push(json!({"window_start":start,"marker_source_second":marker,
            "missed_visual_pulses":(start.ceil() as u16..marker).collect::<Vec<_>>(),
            "screen_edge":screen,"camera_edge":camera,
            "system_audio_edge":system,"microphone_edge":microphone,
            "system_offset_ms":system_offset*1000.0,"microphone_offset_ms":microphone_offset*1000.0,"camera_offset_ms":camera_offset*1000.0}));
    }
    for key in [
        "system_offset_ms",
        "microphone_offset_ms",
        "camera_offset_ms",
    ] {
        let minimum = offsets
            .iter()
            .map(|v| v[key].as_f64().unwrap())
            .fold(f64::INFINITY, f64::min);
        let maximum = offsets
            .iter()
            .map(|v| v[key].as_f64().unwrap())
            .fold(f64::NEG_INFINITY, f64::max);
        ensure!(
            maximum - minimum <= 50.0,
            "{key} drift changed by {:.1}ms, above 50ms.",
            maximum - minimum
        );
    }
    ensure!(
        progress.peak_audio_queue_bytes <= 768000,
        "Audio queue exceeded its configured bound."
    );
    let fps = progress.frames as f64 / seconds as f64;
    ensure!(
        fps >= if load { 27.0 } else { 28.5 },
        "1080p delivered {fps:.2} fps, below this profile's acceptance target."
    );
    Ok(
        json!({"requested_seconds":seconds,"encoded_seconds":duration,"frames":progress.frames,
        "delivered_fps":fps,"skipped_ticks":progress.skipped,"peak_audio_queue_bytes":progress.peak_audio_queue_bytes,
        "job_peak_committed_bytes":progress.peak_job_committed_bytes,"sync_markers":offsets,"cpu_peer":peer,
        "output":output,"segments":journal.segments.len()}),
    )
}

pub(super) fn video_edge(path: &Path, start: f64, x: u32, y: u32, target: &Path) -> Result<f64> {
    let mut command = process::command(&process::media_tool("ffmpeg")?);
    command
        .args(["-v", "error", "-ss", &format!("{start:.3}"), "-i"])
        .arg(path)
        .args([
            "-t",
            "2",
            "-an",
            "-vf",
            &format!("crop=2:2:{x}:{y},scale=1:1,format=gray,fps=100"),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "gray",
            "-y",
        ])
        .arg(target);
    encoder::run_bounded(command, Duration::from_secs(30))?;
    let bytes = fs::read(target)?;
    ensure!(
        (180..=210).contains(&bytes.len()),
        "Video marker window has {} samples, expected about 200.",
        bytes.len()
    );
    let index = bytes
        .iter()
        .enumerate()
        .skip(20)
        .find(|(_, value)| **value > 200)
        .map(|(index, _)| index)
        .context("The expected visual flash was not decoded")?;
    Ok(index as f64 / 100.0)
}

pub(super) fn marker_second(image: &image::RgbaImage, camera: bool) -> Result<u16> {
    let width = image.width();
    let height = image.height();
    ensure!(
        width >= 80 && height >= 32,
        "Generated identity image is too small."
    );
    let read = |x: u32| {
        let (x, y) = if camera {
            let w = (width / 4).clamp(1, 360);
            let h = (w * 180 / 320).min(height).max(1);
            (
                width.saturating_sub(w + 12) + x * w / 320,
                height.saturating_sub(h + 12) + 168 * h / 180,
            )
        } else {
            (x, height - 12)
        };
        image.get_pixel(x, y).0
    };
    ensure!(
        read(66)[..3].iter().all(|c| *c > 190) && read(72)[..3].iter().all(|c| *c < 60),
        "Generated frame identity sentinels are missing."
    );
    let mut second = 0u16;
    for bit in 0..16 {
        if read(bit * 4 + 2)[..3].iter().all(|c| *c > 160) {
            second |= 1 << bit;
        }
    }
    Ok(second)
}

pub(super) fn audio_edges(
    path: &Path,
    start: f64,
    marker: u64,
    target: &Path,
) -> Result<(f64, f64)> {
    let mut command = process::command(&process::media_tool("ffmpeg")?);
    command
        .args(["-v", "error", "-ss", &format!("{start:.3}"), "-i"])
        .arg(path)
        .args([
            "-t", "2", "-vn", "-ac", "1", "-ar", "48000", "-f", "f32le", "-y",
        ])
        .arg(target);
    encoder::run_bounded(command, Duration::from_secs(30))?;
    let bytes = fs::read(target)?;
    ensure!(
        (80000 * 4..=100000 * 4).contains(&bytes.len()),
        "Audio marker window has an unexpected length."
    );
    let samples = bytes
        .chunks_exact(4)
        .map(|p| f32::from_le_bytes([p[0], p[1], p[2], p[3]]))
        .collect::<Vec<_>>();
    let onset = |frequency: f64| -> Result<f64> {
        for ms in 200..1800 {
            let offset = ms * 48;
            if selftest::amplitude(&samples[offset..offset + 480], frequency) > 0.05 {
                return Ok(ms as f64 / 1000.0);
            }
        }
        anyhow::bail!("The {frequency} Hz timing pulse was not decoded.")
    };
    Ok((
        onset(super::worker::pulse_frequency(true, marker))?,
        onset(super::worker::pulse_frequency(false, marker))?,
    ))
}

#[cfg(test)]
mod measurement_tests {
    use super::*;
    #[test]
    fn generated_marker_identity_is_independent_of_a_missing_previous_flash() {
        for second in [1, 10, 11, 257, 599] {
            let screen =
                super::super::worker::synthetic_frame(640, 360, second * 1000, false, false)
                    .unwrap();
            let camera =
                super::super::worker::synthetic_frame(320, 180, second * 1000, false, true)
                    .unwrap();
            let mut pixels = vec![0; 640 * 360 * 4];
            super::super::worker::composite_frame(&screen, Some(&camera), 640, 360, &mut pixels);
            let image = crate::platform::Frame {
                width: 640,
                height: 360,
                bgra: pixels,
                timestamp: 0,
            }
            .into_rgba()
            .unwrap();
            assert_eq!(marker_second(&image, false).unwrap(), second as u16);
            assert_eq!(marker_second(&image, true).unwrap(), second as u16);
        }
        assert!(marker_second(&image::RgbaImage::new(640, 360), false).is_err());
    }

    #[test]
    fn audio_identity_does_not_match_an_adjacent_second() {
        for system in [false, true] {
            let pcm = super::super::worker::generated_pcm(11 * 48000, 4800, system);
            let samples: Vec<_> = pcm
                .chunks_exact(4)
                .map(|p| i16::from_le_bytes([p[0], p[1]]) as f32 / 32768.0)
                .collect();
            assert!(
                selftest::amplitude(&samples, super::super::worker::pulse_frequency(system, 11))
                    > 0.15
            );
            assert!(
                selftest::amplitude(&samples, super::super::worker::pulse_frequency(system, 10))
                    < 0.01
            );
        }
    }
    #[test]
    fn native_profile_refuses_unapproved_screen_or_private_inputs_before_creating_evidence() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("not-created");
        let mut request = Request {
            schema: 1,
            session_dir: directory.path().join("sessions").join("test"),
            library_root: directory.path().to_path_buf(),
            area: Area {
                monitor: "test".into(),
                width: 640,
                height: 360,
                ..Default::default()
            },
            sources: Sources::default(),
            camera_id: None,
            microphone_id: None,
            output_id: None,
            pip: None,
            video: true,
            max_seconds: 12,
            synthetic: false,
            test_fault: None,
        };
        assert!(native(&request, &root, false)
            .unwrap_err()
            .to_string()
            .contains("approval"));
        request.sources.microphone = true;
        assert!(native(&request, &root, true)
            .unwrap_err()
            .to_string()
            .contains("private inputs"));
        assert!(!root.exists());
        assert!(!request.session_dir.exists());
    }

    #[test]
    fn cpu_accounting_includes_descendants_and_keeps_system_baseline_distinct() {
        let before = process::ProcessStats {
            cpu_100ns: 10_000_000,
            private_bytes: 20,
            system_idle_100ns: 400,
            system_total_100ns: 1000,
        };
        let after = process::ProcessStats {
            cpu_100ns: 20_000_000,
            private_bytes: 21,
            system_idle_100ns: 900,
            system_total_100ns: 3000,
        };
        let measured = cpu_window(before, after, 2.0, 10_000_000).unwrap();
        assert_eq!(measured["family_cpu_seconds"], 2.0);
        assert_eq!(measured["family_logical_core_percent"], 100.0);
        assert_eq!(measured["system_busy_percent"], 75.0);
        assert!(cpu_window(after, before, 2.0, 0).is_err());
    }
}
