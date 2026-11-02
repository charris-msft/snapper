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
        let (system, microphone) =
            audio_edges(&output, start, &root.join(format!("sound-{start}.pcm")))?;
        let system_offset = system - screen;
        let microphone_offset = microphone - screen;
        let camera_offset = camera - screen;
        ensure!(system_offset.abs()<=0.1&&microphone_offset.abs()<=0.1&&camera_offset.abs()<=0.1,
            "A/V marker mismatch at {start}s: screen {screen:.3}, camera {camera:.3}, system {system:.3}, mic {microphone:.3}.");
        offsets.push(json!({"window_start":start,"screen_edge":screen,"camera_edge":camera,
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

fn video_edge(path: &Path, start: f64, x: u32, y: u32, target: &Path) -> Result<f64> {
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

fn audio_edges(path: &Path, start: f64, target: &Path) -> Result<(f64, f64)> {
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
    Ok((onset(1200.0)?, onset(2400.0)?))
}
