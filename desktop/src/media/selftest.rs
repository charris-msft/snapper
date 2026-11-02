use super::{
    encoder, publication::StableMedia, supervisor::Recorder, Phase, Request, Sources, TestFault,
};
use crate::{
    library::{self, Library},
    platform::{self, process, Area},
};
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::Path,
    process::Stdio,
    thread,
    time::{Duration, Instant},
};

#[derive(Serialize)]
struct Case {
    name: String,
    passed: bool,
    evidence: Value,
    error: Option<String>,
}

pub fn run(root: &Path) -> Result<()> {
    let root = if root.is_absolute() {
        root.to_owned()
    } else {
        std::env::current_dir()?.join(root)
    };
    fs::create_dir_all(&root)?;
    let run = root.join(format!("run-{}", library::id()));
    fs::create_dir(&run)?;
    let mut cases = Vec::new();
    for mask in 0..16 {
        let name = format!("synthetic-source-flags-{mask:02}");
        let result = case(&run.join(&name), mask);
        match result {
            Ok(evidence) => {
                println!("PASS {name}");
                cases.push(Case {
                    name,
                    passed: true,
                    evidence,
                    error: None,
                });
            }
            Err(error) => {
                eprintln!("FAIL {name}: {error:#}");
                cases.push(Case {
                    name,
                    passed: false,
                    evidence: Value::Null,
                    error: Some(format!("{error:#}")),
                });
            }
        }
        library::write_json(&run.join("results.json"), &cases)?;
    }
    let gap = timestamp_gap(&run.join("timestamp-gap"));
    cases.push(match gap {
        Ok(evidence) => Case {
            name: "timestamp-preserving-video-gap".into(),
            passed: true,
            evidence,
            error: None,
        },
        Err(e) => Case {
            name: "timestamp-preserving-video-gap".into(),
            passed: false,
            evidence: Value::Null,
            error: Some(format!("{e:#}")),
        },
    });
    library::write_json(&run.join("results.json"), &cases)?;
    let passed = cases.iter().filter(|c| c.passed).count();
    let summary = serde_json::json!({
        "kind":"synthetic-pipeline-acceptance","passed":passed,"total":cases.len(),"results":run.join("results.json"),"build":build_identity()?,
        "real_screen_tested":false,"real_microphone_tested":false,"real_camera_tested":false,
        "note":"No real capture or personal device was activated by these cases."
    });
    library::write_json(&root.join("latest.json"), &summary)?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    ensure!(
        passed == cases.len(),
        "{passed} of {} synthetic pipeline cases passed. See {}.",
        cases.len(),
        run.display()
    );
    Ok(())
}
pub fn pause_tests(root: &Path) -> Result<()> {
    let root = if root.is_absolute() {
        root.to_owned()
    } else {
        std::env::current_dir()?.join(root)
    };
    fs::create_dir_all(&root)?;
    let run = root.join(format!("run-{}", library::id()));
    fs::create_dir(&run)?;
    let mut cases = Vec::new();
    for (name, mask, stop_paused) in [
        ("pause-resume-all-sources", 15, false),
        ("pause-resume-no-optional-sources", 0, false),
        ("stop-while-paused", 12, true),
    ] {
        let result = pause_capture(&run.join(name), mask, stop_paused);
        cases.push(acceptance_case(name, result));
        library::write_json(&run.join("results.json"), &cases)?;
    }
    for (name, result) in [
        (
            "video-only-paused-tail-after-slow-write",
            paused_video_tail(&run.join("paused-tail")),
        ),
        (
            "pause-stalled-writer-then-stop",
            pause_stalled_writer(&run.join("stalled-writer")),
        ),
        ("generated-camera-preview-release", generated_preview()),
        ("adjustable-camera-placement", pip_capture(&run.join("pip"))),
    ] {
        cases.push(acceptance_case(name, result));
        library::write_json(&run.join("results.json"), &cases)?;
    }
    let passed = cases.iter().filter(|case| case.passed).count();
    let summary = json!({
        "kind":"synthetic-pause-and-capture-acceptance", "passed":passed, "total":cases.len(),
        "results":run.join("results.json"), "build":build_identity()?,
        "real_screen_tested":false, "real_camera_tested":false, "real_microphone_tested":false,
    });
    library::write_json(&root.join("latest.json"), &summary)?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    ensure!(
        passed == cases.len(),
        "{passed}/{} pause/capture cases passed. See {}.",
        cases.len(),
        run.display()
    );
    Ok(())
}

fn acceptance_case(name: &str, result: Result<Value>) -> Case {
    match result {
        Ok(evidence) => {
            println!("PASS {name}");
            Case {
                name: name.into(),
                passed: true,
                evidence,
                error: None,
            }
        }
        Err(error) => {
            eprintln!("FAIL {name}: {error:#}");
            Case {
                name: name.into(),
                passed: false,
                evidence: Value::Null,
                error: Some(format!("{error:#}")),
            }
        }
    }
}

fn await_progress(
    recorder: &Recorder,
    timeout: Duration,
    predicate: impl Fn(&super::Progress) -> bool,
) -> Result<super::Progress> {
    let deadline = Instant::now() + timeout;
    loop {
        let progress = recorder.progress();
        if predicate(&progress) {
            return Ok(progress);
        }
        ensure!(
            !recorder.finished(),
            "Recording ended before the expected transition: {}",
            progress.detail
        );
        ensure!(
            Instant::now() < deadline,
            "Expected media transition exceeded {} seconds: {}",
            timeout.as_secs(),
            progress.detail
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn await_saved(recorder: &Recorder) -> Result<super::Progress> {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !recorder.finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    ensure!(
        recorder.finished(),
        "Synthetic media completion exceeded 30 seconds."
    );
    let progress = recorder.progress();
    ensure!(
        progress.phase == Phase::Saved,
        "Synthetic capture did not save: {}",
        progress.detail
    );
    ensure!(
        !progress.devices_opened.webcam
            && !progress.devices_opened.microphone
            && !progress.devices_opened.system_audio,
        "Synthetic capture must not activate personal devices."
    );
    Ok(progress)
}

fn pause_capture(root: &Path, mask: u8, stop_paused: bool) -> Result<Value> {
    let mut request = request(root, mask)?;
    request.max_seconds = 5;
    let recorder = Recorder::start(request.clone());
    await_progress(&recorder, Duration::from_secs(15), |p| {
        p.phase == Phase::Recording && p.elapsed_ms >= if stop_paused { 1300 } else { 820 }
    })?;
    recorder.pause()?;
    let paused = await_progress(&recorder, Duration::from_secs(3), |p| {
        p.phase == Phase::Paused && p.control_ack == 1
    })?;
    let hold = Instant::now();
    while hold.elapsed() < Duration::from_millis(if stop_paused { 450 } else { 1350 }) {
        let progress = recorder.progress();
        ensure!(
            progress.phase == Phase::Paused
                && progress.elapsed_ms == paused.control_media_sample / 48,
            "The acknowledged paused media clock advanced or changed phase."
        );
        ensure!(
            progress.frames == paused.frames,
            "Video frames were admitted after pause acknowledgement."
        );
        thread::sleep(Duration::from_millis(25));
    }
    let resumed = if stop_paused {
        recorder.stop();
        None
    } else {
        recorder.resume()?;
        Some(await_progress(&recorder, Duration::from_secs(3), |p| {
            p.phase == Phase::Recording && p.control_ack == 2
        })?)
    };
    let progress = await_saved(&recorder)?;
    let journal: encoder::Journal = library::read_json(&request.session_dir.join("journal.json"))?;
    let output = journal
        .output
        .context("Paused capture has no saved output")?;
    encoder::validate_video(&output)?;
    let probe = probe(&output, root)?;
    let duration = probe["format"]["duration"]
        .as_str()
        .context("Paused output duration missing")?
        .parse::<f64>()?;
    let active = if stop_paused {
        paused.control_media_sample as f64 / 48_000.0
    } else {
        5.0
    };
    ensure!((duration - active).abs() <= 0.20,
        "Encoded duration {duration:.3}s differs from active duration {active:.3}s by more than 200 ms.");
    let expected_elapsed = if stop_paused {
        paused.control_media_sample / 48
    } else {
        request.max_seconds * 1000
    };
    ensure!(
        progress.elapsed_ms == expected_elapsed,
        "Final media elapsed changed during device release or finalization."
    );
    let mut sync = Value::Null;
    if let Some(resumed) = resumed {
        let removed = (resumed.control_wall_sample - paused.control_wall_sample) as f64 / 48_000.0;
        ensure!(
            removed >= 1.30,
            "The synthetic pause did not last the requested 1.35 wall seconds."
        );
        let next_wall_pulse = (resumed.control_wall_sample / 48_000 + 1) as f64;
        let expected = next_wall_pulse - removed;
        let start = expected - 0.5;
        let video =
            super::profile::video_edge(&output, start, 20, 240, &root.join("resume-video.gray"))?;
        ensure!(
            (video - 0.5).abs() <= 0.12,
            "Resumed video pulse offset was {video:.3}s; expected 0.500s +/-120 ms."
        );
        if mask & 12 != 0 {
            let (system, mic) = super::profile::audio_edges(
                &output,
                start,
                next_wall_pulse as u64,
                &root.join("resume-audio.f32le"),
            )?;
            ensure!((system - video).abs() <= 0.12 && (mic - video).abs() <= 0.12,
                "A/V timing changed across resume: video={video:.3}s system={system:.3}s microphone={mic:.3}s.");
            sync = json!({"video":video,"system":system,"microphone":mic,"tolerance_seconds":0.12});
        }
        let quiet_at = paused.control_media_sample as f64 / 48_000.0 + 0.20;
        if quiet_at < expected - 0.15 {
            let image = frame(&output, quiet_at, &root.join("paused-marker-absent.png"))?;
            assert_pixel(&image, 20, 240, [20, 35, 60], 20)?;
        }
        ensure!(
            progress.wall_elapsed_ms as f64 / 1000.0 >= active + removed,
            "Wall and active-media durations were not kept distinct."
        );
    }
    Ok(
        json!({"output":output,"encoded_seconds":duration,"active_seconds":active,
        "elapsed_media_ms":progress.elapsed_ms,"wall_elapsed_ms":progress.wall_elapsed_ms,
        "pause_wall_sample":paused.control_wall_sample,"pause_media_sample":paused.control_media_sample,
        "sync":sync,"audio_queue_peak_bytes":progress.peak_audio_queue_bytes}),
    )
}

fn paused_video_tail(root: &Path) -> Result<Value> {
    let mut request = request(root, 0)?;
    request.max_seconds = 5;
    request.test_fault = Some(TestFault::SlowVideoWrite);
    let recorder = Recorder::start(request.clone());
    await_progress(&recorder, Duration::from_secs(15), |p| {
        p.phase == Phase::Recording && request.session_dir.join("fault-fired.json").is_file()
    })?;
    recorder.pause()?;
    let paused = await_progress(&recorder, Duration::from_secs(3), |p| {
        p.phase == Phase::Paused
    })?;
    ensure!(
        paused.elapsed_ms.saturating_sub(paused.last_write_ms) >= 350,
        "Controlled slow-write fixture did not create its intended held-frame gap."
    );
    recorder.stop();
    let saved = await_saved(&recorder)?;
    let journal: encoder::Journal = library::read_json(&request.session_dir.join("journal.json"))?;
    let output = journal
        .output
        .clone()
        .context("Video-only paused output missing")?;
    let data = probe(&output, root)?;
    let stream = data["streams"]
        .as_array()
        .context("No paused output streams")?
        .iter()
        .find(|s| s["codec_type"] == "video")
        .context("No paused output video")?;
    let video_end = stream["duration"]
        .as_str()
        .context("Paused video duration missing")?
        .parse::<f64>()?;
    let container_end = data["format"]["duration"]
        .as_str()
        .context("Paused container duration missing")?
        .parse::<f64>()?;
    let expected = paused.control_media_sample as f64 / 48000.0;
    ensure!((video_end-expected).abs()<=0.04 && (container_end-expected).abs()<=0.04,
        "Held frame was not sealed: active={expected:.3}s, video={video_end:.3}s, container={container_end:.3}s.");
    let tail = frame(
        &output,
        (expected - 0.05).max(0.0),
        &root.join("held-tail.png"),
    )?;
    let identity = super::profile::marker_second(&tail, false)?;
    ensure!(
        u64::from(identity) == paused.last_write_ms / 1000,
        "Finalization sampled a paused source instead of retaining the last admitted pixels."
    );
    // Replay copied committed segments as an interrupted session. The completed
    // source session must remain complete and must not be rewritten for this test.
    let replay_library = Library::open(root.join("recovery-copy"))?;
    let replay_directory = replay_library.root.join("sessions").join(library::id());
    fs::create_dir(&replay_directory)?;
    for segment in &journal.segments {
        library::copy_file_new(
            &request.session_dir.join(&segment.file),
            &replay_directory.join(&segment.file),
        )?;
    }
    let mut replay = journal.clone();
    replay.request.library_root = replay_library.root.clone();
    replay.request.session_dir = replay_directory.clone();
    replay.request.test_fault = None;
    replay.completed = false;
    replay.recovered = false;
    replay.output = None;
    replay.final_publication = None;
    replay.interrupted =
        Some("Generated held-tail recovery replay from copied committed segments.".into());
    library::write_json(&replay_directory.join("journal.json"), &replay)?;
    let recovered = encoder::recover(&replay_directory)?;
    let recovered_path = replay_library
        .capture_dir(&recovered)?
        .join("recording.mp4");
    let recovered_probe = probe(&recovered_path, root)?;
    let recovered_end = recovered_probe["format"]["duration"]
        .as_str()
        .context("Recovered duration missing")?
        .parse::<f64>()?;
    ensure!(
        (recovered_end - expected).abs() <= 0.04,
        "Recovered held-frame endpoint changed."
    );
    ensure!(
        saved.elapsed_ms == paused.control_media_sample / 48,
        "Finalization changed acknowledged media elapsed."
    );
    Ok(
        json!({"active_end_seconds":expected,"video_end_seconds":video_end,"container_end_seconds":container_end,
        "recovered_end_seconds":recovered_end,"held_source_second":identity,"output":output}),
    )
}

fn pause_stalled_writer(root: &Path) -> Result<Value> {
    let mut request = request(root, 0)?;
    request.max_seconds = 20;
    request.test_fault = Some(TestFault::WriterStall);
    let recorder = Recorder::start(request.clone());
    await_progress(&recorder, Duration::from_secs(15), |p| {
        p.phase == Phase::Recording
            && p.elapsed_ms >= 3500
            && request.session_dir.join("fault-fired.json").is_file()
    })?;
    recorder.pause()?;
    thread::sleep(Duration::from_millis(100));
    ensure!(
        recorder.progress().phase == Phase::Pausing,
        "A blocked writer falsely acknowledged Pause."
    );
    let stop = Instant::now();
    recorder.stop();
    while !recorder.finished() && stop.elapsed() < Duration::from_secs(7) {
        thread::sleep(Duration::from_millis(20));
    }
    ensure!(
        recorder.finished(),
        "Stop during a blocked Pause exceeded its seven-second test limit."
    );
    let progress = recorder.progress();
    ensure!(
        progress.phase == Phase::Interrupted
            && progress
                .detail
                .contains("Devices/processes have now exited"),
        "Forced writer shutdown did not confirm owned-process release: {}",
        progress.detail
    );
    Ok(
        json!({"stop_wall_ms":stop.elapsed().as_millis(),"phase":progress.phase,"detail":progress.detail}),
    )
}

fn generated_preview() -> Result<Value> {
    let preview = super::preview::CameraPreview::start("generated".into(), true, || {});
    let deadline = Instant::now() + Duration::from_secs(8);
    let frame = loop {
        if let Some(error) = preview.take_error() {
            anyhow::bail!("{error}");
        }
        if let Some(frame) = preview.take_frame() {
            break frame;
        }
        ensure!(
            !preview.finished() && Instant::now() < deadline,
            "Generated camera preview did not produce a frame."
        );
        thread::sleep(Duration::from_millis(20));
    };
    ensure!(
        frame.width <= 640 && frame.height <= 360,
        "Preview frame exceeded the bounded transport."
    );
    let stop = Instant::now();
    preview.stop();
    while !preview.finished() && stop.elapsed() < Duration::from_secs(3) {
        thread::sleep(Duration::from_millis(20));
    }
    ensure!(
        preview.finished(),
        "Generated preview release exceeded 3 seconds."
    );
    if let Some(error) = preview.take_error() {
        anyhow::bail!("{error}");
    }
    Ok(
        json!({"width":frame.width,"height":frame.height,"release_ms":stop.elapsed().as_millis(),"real_camera":false}),
    )
}

fn pip_capture(root: &Path) -> Result<Value> {
    let mut request = request(root, 2)?;
    request.pip = Some(crate::capture::PipLayout {
        x: 0,
        y: 0,
        width: 500,
    });
    let recorder = Recorder::start(request.clone());
    await_saved(&recorder)?;
    let journal: encoder::Journal = library::read_json(&request.session_dir.join("journal.json"))?;
    let output = journal.output.context("PiP capture output missing")?;
    let image = frame(&output, 1.5, &root.join("pip-position.png"))?;
    assert_pixel(&image, 100, 100, [35, 210, 25], 20)?;
    assert_pixel(&image, 560, 300, [20, 35, 60], 20)?;
    Ok(
        json!({"output":output,"pip":request.pip,"verified":"top-left camera; former bottom-right position contains screen content"}),
    )
}

fn request(root: &Path, mask: u8) -> Result<Request> {
    let library = Library::open(root.to_owned())?;
    let request = Request {
        schema: 1,
        session_dir: library.root.join("sessions").join(library::id()),
        library_root: library.root,
        area: Area {
            monitor: "synthetic".into(),
            x: 0,
            y: 0,
            width: 640,
            height: 360,
        },
        sources: Sources {
            cursor: mask & 1 != 0,
            webcam: mask & 2 != 0,
            system_audio: mask & 4 != 0,
            microphone: mask & 8 != 0,
        },
        camera_id: None,
        microphone_id: None,
        output_id: None,
        pip: None,
        video: true,
        max_seconds: 3,
        synthetic: true,
        test_fault: None,
    };
    Ok(request)
}
fn case(root: &Path, mask: u8) -> Result<Value> {
    let request = request(root, mask)?;
    let started = Instant::now();
    let recorder = Recorder::start(request.clone());
    while !recorder.finished() {
        ensure!(
            started.elapsed() < Duration::from_secs(40),
            "Synthetic recording exceeded the 40-second test budget."
        );
        thread::sleep(Duration::from_millis(20));
    }
    let progress = recorder.progress();
    ensure!(
        progress.phase == Phase::Saved,
        "Synthetic recording failed: {}",
        progress.detail
    );
    let journal: encoder::Journal = library::read_json(&request.session_dir.join("journal.json"))?;
    let output = journal
        .output
        .context("Completed recording has no output path")?;
    let probe = probe(&output, root)?;
    let streams = probe["streams"]
        .as_array()
        .context("Probe returned no streams")?;
    let videos: Vec<_> = streams
        .iter()
        .filter(|s| s["codec_type"] == "video")
        .collect();
    let audio: Vec<_> = streams
        .iter()
        .filter(|s| s["codec_type"] == "audio")
        .collect();
    ensure!(
        videos.len() == 1 && videos[0]["codec_name"] == "h264",
        "Output must contain one H.264 stream."
    );
    ensure!(
        videos[0]["width"] == 640 && videos[0]["height"] == 360,
        "Output dimensions changed."
    );
    ensure!(
        audio.len() == usize::from(mask & 12 != 0),
        "OFF audio must produce no audio track."
    );
    let duration = probe["format"]["duration"]
        .as_str()
        .context("Probe duration is missing")?
        .parse::<f64>()?;
    ensure!(
        (2.8..3.3).contains(&duration),
        "Elapsed video duration {duration:.3}s is inconsistent with a 3s capture."
    );
    let image = frame(&output, 1.0, &root.join("content.png"))?;
    assert_pixel(&image, 50, 50, [20, 45, 200], 12)?;
    assert_pixel(
        &image,
        326,
        190,
        if mask & 1 != 0 {
            [255, 255, 255]
        } else {
            [20, 35, 60]
        },
        20,
    )?;
    assert_pixel(
        &image,
        560,
        300,
        if mask & 2 != 0 {
            [35, 210, 25]
        } else {
            [20, 35, 60]
        },
        20,
    )?;
    let mut amplitudes = serde_json::json!({});
    if mask & 12 != 0 {
        let samples = audio_samples(&output, &root.join("audio.f32le"))?;
        let values = &samples[12_000..samples.len().min(120_000)];
        let system = amplitude(values, 440.0);
        let microphone = amplitude(values, 880.0);
        ensure!(
            if mask & 4 != 0 {
                system > 0.08
            } else {
                system < 0.008
            },
            "System-audio content toggle failed: amplitude {system}."
        );
        ensure!(
            if mask & 8 != 0 {
                microphone > 0.08
            } else {
                microphone < 0.008
            },
            "Microphone content toggle failed: amplitude {microphone}."
        );
        amplitudes = serde_json::json!({"system_440_hz":system,"microphone_880_hz":microphone});
    }
    encoder::validate_video(&output)?;
    ensure!(
        !progress.devices_opened.webcam
            && !progress.devices_opened.microphone
            && !progress.devices_opened.system_audio,
        "Synthetic tests must never open real devices."
    );
    Ok(
        serde_json::json!({"sources":request.sources,"duration_seconds":duration,"frames":progress.frames,
        "skipped":progress.skipped,"segments":journal.segments.len(),"audio_amplitudes":amplitudes,"output":output}),
    )
}
fn timestamp_gap(root: &Path) -> Result<Value> {
    let mut request = request(root, 0)?;
    request.area.width = 64;
    request.area.height = 64;
    fs::create_dir_all(&request.session_dir)?;
    let mut journal = encoder::create_journal(&request)?;
    let mut encoder = encoder::Encoder::start(&request)?;
    let pixels = platform::Frame {
        width: 64,
        height: 64,
        bgra: [0, 0, 255, 255].repeat(64 * 64),
        timestamp: 0,
    };
    for ms in [0, 33, 1333, 1367] {
        encoder.video(ms, &pixels.bgra)?;
    }
    encoder.finish()?;
    encoder::publish_segments(&mut journal)?;
    encoder::final_export(&mut journal)?;
    let output = journal.output.context("Gap fixture output missing")?;
    let probe = probe(&output, root)?;
    let duration = probe["format"]["duration"]
        .as_str()
        .context("Gap fixture duration missing")?
        .parse::<f64>()?;
    ensure!(
        (1.35..1.5).contains(&duration),
        "Dropped-frame gap compressed the timeline to {duration}s."
    );
    Ok(
        serde_json::json!({"input_timestamps_ms":[0,33,1333,1367],"output_duration_seconds":duration,"output":output}),
    )
}
pub fn probe(path: &Path, directory: &Path) -> Result<Value> {
    let target = directory.join("probe.json");
    let file = File::create(&target)?;
    let mut command = process::command(&process::media_tool("ffprobe")?);
    command
        .args([
            "-v",
            "error",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
        ])
        .arg(path);
    encoder::run_bounded_with_output(&mut command, Duration::from_secs(15), Some(file))?;
    library::read_json(&target)
}
pub fn frame(path: &Path, seconds: f64, target: &Path) -> Result<image::RgbaImage> {
    let mut command = process::command(&process::media_tool("ffmpeg")?);
    command
        .args(["-v", "error", "-ss", &format!("{seconds:.3}"), "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-y"])
        .arg(target);
    encoder::run_bounded(command, Duration::from_secs(15))?;
    library::load_image(target)
}
fn assert_pixel(
    image: &image::RgbaImage,
    x: u32,
    y: u32,
    expected: [u8; 3],
    tolerance: u8,
) -> Result<()> {
    let actual = image.get_pixel(x, y);
    ensure!(
        (0..3).all(|c| actual[c].abs_diff(expected[c]) <= tolerance),
        "Content pixel ({x},{y}) was {:?}, expected {expected:?} +/- {tolerance}.",
        actual.0
    );
    Ok(())
}
fn audio_samples(path: &Path, target: &Path) -> Result<Vec<f32>> {
    let mut command = process::command(&process::media_tool("ffmpeg")?);
    command
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-vn", "-ac", "1", "-ar", "48000", "-f", "f32le", "-y"])
        .arg(target);
    encoder::run_bounded(command, Duration::from_secs(15))?;
    let bytes = fs::read(target)?;
    ensure!(
        bytes.len() >= 48_000 * 4 && bytes.len() < 192_000 * 16,
        "Unexpected audio payload length."
    );
    Ok(bytes
        .chunks_exact(4)
        .map(|p| f32::from_le_bytes([p[0], p[1], p[2], p[3]]))
        .collect())
}
pub(crate) fn amplitude(samples: &[f32], frequency: f64) -> f64 {
    let mut sin = 0.0;
    let mut cos = 0.0;
    for (i, sample) in samples.iter().enumerate() {
        let angle = i as f64 / 48000.0 * frequency * std::f64::consts::TAU;
        sin += f64::from(*sample) * angle.sin();
        cos += f64::from(*sample) * angle.cos();
    }
    2.0 * sin.hypot(cos) / samples.len() as f64
}

pub fn fault_tests(root: &Path) -> Result<()> {
    let root = if root.is_absolute() {
        root.to_owned()
    } else {
        std::env::current_dir()?.join(root)
    };
    fs::create_dir_all(&root)?;
    let run = root.join(format!("run-{}", library::id()));
    fs::create_dir(&run)?;
    let mut cases = Vec::new();
    for (name, fault) in [
        ("before-first-commit", TestFault::BeforeFirstCommit),
        ("worker-abrupt-exit", TestFault::WorkerExit),
        ("encoder-abrupt-exit", TestFault::EncoderExit),
        ("blocked-writer-stop", TestFault::WriterStall),
        (
            "segment-rename-before-manifest",
            TestFault::AfterSegmentRename,
        ),
        ("final-rename-before-library", TestFault::AfterFinalRename),
        (
            "device-stopped-drain-cancellation",
            TestFault::FinalDrainStall,
        ),
    ] {
        let result = fault_case(&run.join(name), fault);
        let case = match result {
            Ok(evidence) => Case {
                name: name.into(),
                passed: true,
                evidence,
                error: None,
            },
            Err(error) => Case {
                name: name.into(),
                passed: false,
                evidence: Value::Null,
                error: Some(format!("{error:#}")),
            },
        };
        println!(
            "{} {name}{}",
            if case.passed { "PASS" } else { "FAIL" },
            case.error
                .as_ref()
                .map(|e| format!(": {e}"))
                .unwrap_or_default()
        );
        cases.push(case);
        library::write_json(&run.join("results.json"), &cases)?;
    }
    let result = normal_stop(&run.join("normal-stop"));
    cases.push(match result {
        Ok(evidence) => Case {
            name: "normal-stop".into(),
            passed: true,
            evidence,
            error: None,
        },
        Err(error) => Case {
            name: "normal-stop".into(),
            passed: false,
            evidence: Value::Null,
            error: Some(format!("{error:#}")),
        },
    });
    let result = helper_parent_death();
    cases.push(match result {
        Ok(evidence) => Case {
            name: "recovery-helper-parent-death".into(),
            passed: true,
            evidence,
            error: None,
        },
        Err(error) => Case {
            name: "recovery-helper-parent-death".into(),
            passed: false,
            evidence: Value::Null,
            error: Some(format!("{error:#}")),
        },
    });
    library::write_json(&run.join("results.json"), &cases)?;
    let passed = cases.iter().filter(|c| c.passed).count();
    library::write_json(
        &root.join("latest.json"),
        &serde_json::json!({
            "kind":"synthetic-fault-and-recovery-acceptance","passed":passed,"total":cases.len(),"build":build_identity()?,
            "results":run.join("results.json"),"real_devices_tested":false
        }),
    )?;
    println!(
        "{passed}/{} fault/recovery cases passed: {}",
        cases.len(),
        run.display()
    );
    ensure!(
        passed == cases.len(),
        "Some fault/recovery cases did not pass."
    );
    Ok(())
}

fn fault_case(root: &Path, fault: TestFault) -> Result<Value> {
    let mut request = request(root, 15)?;
    request.max_seconds = if matches!(
        fault,
        TestFault::AfterFinalRename | TestFault::FinalDrainStall
    ) {
        3
    } else {
        12
    };
    request.test_fault = Some(fault);
    let recorder = Recorder::start(request.clone());
    let start = Instant::now();
    let mut requested_stop = None;
    let mut acknowledgement_ms = None;
    while !recorder.finished() {
        if fault == TestFault::FinalDrainStall
            && requested_stop.is_none()
            && recorder.progress().phase == Phase::DevicesStopped
            && request.session_dir.join("fault-fired.json").exists()
        {
            let now = Instant::now();
            recorder.cancel_finalization();
            acknowledgement_ms = Some(now.elapsed().as_secs_f64() * 1000.0);
            requested_stop = Some(now);
        }
        if fault == TestFault::WriterStall
            && requested_stop.is_none()
            && recorder.progress().elapsed_ms >= 4100
        {
            let now = Instant::now();
            recorder.stop();
            acknowledgement_ms = Some(now.elapsed().as_secs_f64() * 1000.0);
            requested_stop = Some(now);
        }
        if start.elapsed() > Duration::from_secs(30) {
            recorder.stop();
            bail_timeout(&recorder)?;
            anyhow::bail!("Fault test exceeded 30 seconds; capture workers were asked to stop.");
        }
        thread::sleep(Duration::from_millis(10));
    }
    let progress = recorder.progress();
    ensure!(
        request.session_dir.join("fault-fired.json").is_file(),
        "Requested fault stage was never reached. Capture ended earlier: {}",
        progress.detail
    );
    ensure!(
        progress.phase == Phase::Interrupted,
        "Injected fault was incorrectly reported as a successful capture."
    );
    let exit_after_stop_ms = requested_stop.map(|s| s.elapsed().as_secs_f64() * 1000.0);
    if let Some(ack) = acknowledgement_ms {
        ensure!(ack < 250.0, "Stop acknowledgement took {ack:.1}ms.");
    }
    if let Some(exit) = exit_after_stop_ms {
        ensure!(exit < 7000.0, "Forced worker exit took {exit:.1}ms.");
    }
    let before: encoder::Journal = library::read_json(&request.session_dir.join("journal.json"))?;
    if fault == TestFault::BeforeFirstCommit {
        ensure!(
            before.segments.is_empty(),
            "Pre-commit fault unexpectedly has a committed segment."
        );
        let error = encoder::recover(&request.session_dir)
            .err()
            .context("Pre-commit recovery must not invent playable media")?;
        ensure!(
            format!("{error:#}").contains("Nothing recoverable yet"),
            "Unexpected empty recovery response: {error:#}"
        );
        return Ok(
            serde_json::json!({"committed_segments":0,"recoverable":false,"honest_empty_recovery":true}),
        );
    }
    let output_path = request
        .library_root
        .join("captures")
        .join(request.session_dir.file_name().unwrap())
        .join("recording.mp4");
    let existing = if output_path.exists() {
        Some(fs::read(&output_path)?)
    } else {
        None
    };
    let adoption_rejections = if fault == TestFault::AfterFinalRename {
        verify_adoption_rejections(root, &request, &before, &output_path)?
    } else {
        Value::Null
    };
    let capture = encoder::recover(&request.session_dir)?;
    let after: encoder::Journal = library::read_json(&request.session_dir.join("journal.json"))?;
    let output = after.output.context("Recovery did not publish an MP4")?;
    ensure!(
        after.completed && after.recovered,
        "Recovery result was not durably marked complete."
    );
    ensure!(
        !after.segments.is_empty(),
        "Recovery reported success without committed media."
    );
    encoder::validate_video(&output)?;
    let probe = probe(&output, root)?;
    let duration = probe["format"]["duration"]
        .as_str()
        .context("Recovered duration missing")?
        .parse::<f64>()?;
    ensure!(
        duration > 1.8,
        "Recovered media did not retain the first full segment."
    );
    let image = frame(&output, 0.5, &root.join("recovered-content.png"))?;
    assert_pixel(&image, 50, 50, [20, 45, 200], 12)?;
    assert_pixel(&image, 560, 300, [35, 210, 25], 20)?;
    let audio = audio_samples(&output, &root.join("recovered-audio.f32le"))?;
    ensure!(
        amplitude(&audio[12000..], 440.0) > 0.08 && amplitude(&audio[12000..], 880.0) > 0.08,
        "Recovered media lost selected generated audio content."
    );
    if let Some(existing) = existing {
        ensure!(
            fs::read(&output)? == existing,
            "Recovery overwrote a previously published final MP4."
        );
    }
    let library = Library::open(request.library_root.clone())?;
    ensure!(
        library.list()?.0.iter().any(|c| c.id == capture.id),
        "Recovered media is missing from the durable library."
    );
    let lost_ms = progress
        .elapsed_ms
        .saturating_sub((duration * 1000.0) as u64);
    Ok(
        serde_json::json!({"committed_segments":after.segments.len(),"recovered_seconds":duration,
        "last_observed_elapsed_ms":progress.elapsed_ms,"unretained_observed_interval_ms":lost_ms,
        "stop_acknowledgement_ms":acknowledgement_ms,"confirmed_process_exit_after_stop_ms":exit_after_stop_ms,
        "adoption_rejections":adoption_rejections,
        "output":output}),
    )
}

fn verify_adoption_rejections(
    root: &Path,
    request: &Request,
    before: &encoder::Journal,
    output: &Path,
) -> Result<Value> {
    use std::sync::mpsc;
    ensure!(
        before.final_publication.is_some(),
        "The pre-rename durable media identity is missing."
    );
    let metadata = output
        .parent()
        .context("Final media directory is missing")?
        .join("capture.json");
    ensure!(
        !metadata.exists(),
        "Adoption regression must exercise missing capture metadata."
    );
    let journal_path = request.session_dir.join("journal.json");
    let original_journal = fs::read(&journal_path)?;
    let intended = fs::read(output)?;
    let unrelated = root.join("independently-valid-unrelated.mp4");
    let (width, height) = request.canvas();
    let mut command = process::command(&process::media_tool("ffmpeg")?);
    command.args([
        "-v",
        "error",
        "-f",
        "lavfi",
        "-i",
        &format!("color=c=green:s={width}x{height}:r=30"),
    ]);
    if request.audio_count() > 0 {
        command.args(["-f", "lavfi", "-i", "sine=frequency=1000:sample_rate=48000"]);
    }
    command
        .args([
            "-t",
            "3",
            "-c:v",
            "libx264",
            "-threads",
            "1",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-ar",
            "48000",
            "-ac",
            "2",
            "-n",
        ])
        .arg(&unrelated);
    encoder::run_bounded(command, Duration::from_secs(30))?;
    encoder::validate_video(&unrelated)?;
    let mut same_shape = StableMedia::open(&unrelated)?;
    same_shape.prepare_publication(before).context(
        "The unrelated fixture must independently pass the expected stream-shape checks",
    )?;
    let mut audio_off = before.clone();
    audio_off.request.sources.system_audio = false;
    audio_off.request.sources.microphone = false;
    let unexpected_audio = same_shape
        .prepare_publication(&audio_off)
        .err()
        .context("Actual unexpected audio was accepted for an audio-OFF recording")?;
    ensure!(
        format!("{unexpected_audio:#}").contains("audio streams"),
        "Unexpected audio-stream rejection: {unexpected_audio:#}"
    );
    let mut wrong_size = before.clone();
    wrong_size.request.area.width = (width / 2).max(2);
    let dimensions = same_shape
        .prepare_publication(&wrong_size)
        .err()
        .context("Actual MP4 dimensions were not checked against the session")?;
    ensure!(
        format!("{dimensions:#}").contains("dimensions"),
        "Unexpected media-dimension rejection: {dimensions:#}"
    );
    let mut wrong_time = before.clone();
    wrong_time
        .segments
        .last_mut()
        .context("Adoption test has no committed segment")?
        .end += 1.0;
    let timing = same_shape
        .prepare_publication(&wrong_time)
        .err()
        .context("Actual MP4 duration was not checked against the session timeline")?;
    ensure!(
        format!("{timing:#}").contains("duration"),
        "Unexpected media-duration rejection: {timing:#}"
    );
    drop(same_shape);
    let unrelated_bytes = fs::read(&unrelated)?;
    ensure!(
        intended != unrelated_bytes,
        "The negative adoption fixture must contain different media."
    );
    fs::copy(&unrelated, output)?;
    let rejected = encoder::recover(&request.session_dir)
        .err()
        .context("An independently valid but unrelated final MP4 was incorrectly adopted")?;
    ensure!(
        format!("{rejected:#}").contains("publication"),
        "Unexpected rejection stage: {rejected:#}"
    );
    ensure!(
        fs::read(output)? == unrelated_bytes && !metadata.exists(),
        "Unrelated media was modified or published into this session's library."
    );
    ensure!(
        !library::read_json::<encoder::Journal>(&journal_path)?.completed,
        "Rejected adoption incorrectly marked the recording complete."
    );
    fs::write(output, &intended)?;

    let mut unbound = before.clone();
    unbound.final_publication = None;
    library::write_json(&journal_path, &unbound)?;
    let unbound_error = encoder::recover(&request.session_dir)
        .err()
        .context("An unbound final MP4 was adopted without durable session identity")?;
    ensure!(
        format!("{unbound_error:#}").contains("no durable publication identity"),
        "Unexpected missing-identity response: {unbound_error:#}"
    );
    ensure!(
        fs::read(output)? == intended && !metadata.exists(),
        "Unbound media was changed or exposed as this recording."
    );
    library::atomic_write(&journal_path, &original_journal)?;

    let (locked_tx, locked_rx) = mpsc::sync_channel(1);
    let (verify_tx, verify_rx) = mpsc::sync_channel(1);
    let path = output.to_owned();
    let journal = before.clone();
    let verifier = thread::spawn(move || -> Result<StableMedia> {
        let mut locked = StableMedia::open(&path)?;
        locked_tx.send(())?;
        verify_rx.recv_timeout(Duration::from_secs(5))?;
        locked.verify_publication(&journal)?;
        Ok(locked)
    });
    locked_rx
        .recv_timeout(Duration::from_secs(10))
        .context("Final-media validation did not acquire its stable input handle")?;
    let replace_blocked = fs::rename(&unrelated, output).is_err();
    let write_blocked = fs::OpenOptions::new().write(true).open(output).is_err();
    verify_tx.send(())?;
    let held_through_completion = verifier
        .join()
        .map_err(|_| anyhow::anyhow!("Final validation thread panicked"))??;
    ensure!(
        replace_blocked && write_blocked,
        "The final input could be replaced or modified while session-media validation held it."
    );
    ensure!(
        fs::rename(&unrelated, output).is_err(),
        "The final input lock ended before the completion publication boundary."
    );
    drop(held_through_completion);
    ensure!(
        fs::read(output)? == intended,
        "Final media changed during adoption validation."
    );
    Ok(json!({
        "unrelated_valid_same_shape_final_rejected":true,
        "missing_capture_metadata_tested":true,
        "missing_publication_identity_rejected":true,
        "actual_unexpected_audio_dimensions_and_duration_rejected":true,
        "replacement_and_writes_blocked_during_validation":true,
        "completion_lock_retained":true,
        "intended_media_and_unrelated_fixture_preserved":true
    }))
}

fn bail_timeout(recorder: &Recorder) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !recorder.finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    ensure!(
        recorder.finished(),
        "Test-owned media worker shutdown could not be confirmed."
    );
    Ok(())
}
pub(crate) fn build_identity() -> Result<Value> {
    use sha2::{Digest, Sha256};
    let executable = std::env::current_exe()?;
    let mut file = File::open(&executable)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let length = file.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        hasher.update(&buffer[..length]);
    }
    let manifest = executable
        .parent()
        .context("Executable directory unavailable")?
        .join("build-manifest.json");
    let package = if manifest.is_file() {
        library::read_json::<Value>(&manifest)?
    } else {
        Value::Null
    };
    Ok(
        json!({"executable":executable,"sha256":format!("{:x}",hasher.finalize()),
        "profile":if cfg!(debug_assertions){"debug"}else{"release"},"package":package}),
    )
}

fn helper_parent_death() -> Result<Value> {
    let outer = process::Job::new()?;
    let mut parent = process::command(&std::env::current_exe()?)
        .arg("--helper-owner-test")
        .stdin(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    if let Err(error) = outer.assign(&parent) {
        parent.kill()?;
        parent.wait()?;
        return Err(error);
    }
    parent
        .stdin
        .take()
        .context("Test owner gate missing")?
        .write_all(b"start\n")?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let children = loop {
        let children = process::descendants(parent.id())?;
        if children.len() >= 2 {
            break children;
        }
        ensure!(
            Instant::now() < deadline,
            "Recovery helper tree did not start."
        );
        thread::sleep(Duration::from_millis(20));
    };
    let handles = children
        .iter()
        .map(|pid| process::ProcessExit::open(*pid))
        .collect::<Result<Vec<_>>>()?;
    let killed = Instant::now();
    parent.kill()?;
    parent.wait()?;
    for handle in handles {
        handle.wait(Duration::from_secs(5))?;
    }
    ensure!(
        outer.active_processes()? == 0,
        "A codec descendant survived its owner before outer cleanup."
    );
    Ok(
        serde_json::json!({"owned_descendants":children.len(),"confirmed_exit_ms":killed.elapsed().as_secs_f64()*1000.0}),
    )
}

fn normal_stop(root: &Path) -> Result<Value> {
    let mut request = request(root, 0)?;
    request.max_seconds = 30;
    let recorder = Recorder::start(request.clone());
    let deadline = Instant::now() + Duration::from_secs(20);
    while recorder.progress().elapsed_ms < 1200 {
        ensure!(
            !recorder.finished() && Instant::now() < deadline,
            "Normal-stop fixture did not reach recording state."
        );
        thread::sleep(Duration::from_millis(10));
    }
    let call = Instant::now();
    recorder.stop();
    let ack = call.elapsed().as_secs_f64() * 1000.0;
    bail_timeout(&recorder)?;
    let progress = recorder.progress();
    ensure!(
        progress.phase == Phase::Saved,
        "Normal stop did not save its short final segment: {}",
        progress.detail
    );
    let received = progress
        .stop_requested_ms
        .context("Control-thread Stop receipt was not recorded")?;
    let released = progress
        .devices_stopped_ms
        .context("Device release was not confirmed")?;
    ensure!(
        ack < 250.0 && released.saturating_sub(received) < 3000,
        "Normal Stop exceeded its acknowledgement/release target."
    );
    Ok(
        serde_json::json!({"stop_acknowledgement_ms":ack,"device_release_after_receipt_ms":released.saturating_sub(received),
        "save_completed_after_stop_ms":call.elapsed().as_secs_f64()*1000.0}),
    )
}
