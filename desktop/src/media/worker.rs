use super::{encoder, Phase, Progress, Request, TestFault};
use crate::{
    library::{self, Capture, Library},
    platform::{self, Frame},
};
use anyhow::{bail, ensure, Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender};
use std::{
    io::BufRead,
    sync::{
        atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

type FrameSlot = Arc<Mutex<Option<Arc<Frame>>>>;
#[derive(Clone)]
struct Shared {
    stop: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    origin: Arc<AtomicI64>,
    error: Arc<Mutex<Option<String>>>,
    progress: Arc<Mutex<Progress>>,
    audio_bytes: Arc<AtomicUsize>,
    audio_limit: usize,
}
impl Shared {
    fn fail(&self, message: String) {
        let mut error = self.error.lock().unwrap_or_else(|e| e.into_inner());
        if error.is_none() {
            *error = Some(message);
        }
        self.stop.store(true, Ordering::Release);
    }
    fn update(&self, f: impl FnOnce(&mut Progress)) {
        f(&mut self.progress.lock().unwrap_or_else(|e| e.into_inner()));
    }
    fn error(&self) -> Option<String> {
        self.error.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    fn ms(&self) -> u64 {
        let origin = self.origin.load(Ordering::Acquire);
        if origin == 0 {
            return 0;
        }
        match platform::clock_100ns() {
            Ok(t) => ((t - origin).max(0) / 10_000) as u64,
            Err(error) => {
                self.fail(format!("Monotonic media clock failed: {error:#}"));
                0
            }
        }
    }
}
struct AudioPacket {
    index: u8,
    ms: u64,
    pcm: Vec<u8>,
}
struct Devices {
    shared: Shared,
    threads: Vec<JoinHandle<()>>,
    screen: FrameSlot,
    camera: FrameSlot,
    audio: Receiver<AudioPacket>,
}
fn spawn_device(
    shared: &Shared,
    threads: &mut Vec<JoinHandle<()>>,
    name: &'static str,
    run: impl FnOnce() -> Result<()> + Send + 'static,
) {
    let state = shared.clone();
    threads.push(thread::spawn(move || {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => state.fail(format!("{name}: {e:#}")),
            Err(_) => state.fail(format!("{name} worker panicked. Capture was interrupted.")),
        }
    }));
}
impl Devices {
    fn start(request: &Request, shared: Shared) -> Result<Self> {
        let screen: FrameSlot = Arc::new(Mutex::new(None));
        let camera: FrameSlot = Arc::new(Mutex::new(None));
        let (ready_tx, ready_rx) = bounded(4);
        let (audio_tx, audio_rx) = bounded(512);
        let mut devices = Self {
            shared: shared.clone(),
            threads: vec![],
            screen: screen.clone(),
            camera: camera.clone(),
            audio: audio_rx,
        };
        let req = request.clone();
        let state = shared.clone();
        let ready = ready_tx.clone();
        spawn_device(&shared, &mut devices.threads, "Screen", move || {
            if req.synthetic {
                let frame = synthetic_frame(
                    req.area.width,
                    req.area.height,
                    0,
                    req.sources.cursor,
                    false,
                )?;
                *screen.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(frame));
                ready.send(())?;
                while !state.stop.load(Ordering::Acquire) {
                    let frame = synthetic_frame(
                        req.area.width,
                        req.area.height,
                        state.ms(),
                        req.sources.cursor,
                        false,
                    )?;
                    *screen.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(frame));
                    thread::sleep(Duration::from_millis(20));
                }
            } else {
                let mut capture = platform::screen::Screen::open(&req.area, req.sources.cursor)?;
                state.update(|p| {
                    p.devices_opened.cursor = req.sources.cursor;
                    p.screen_backend = capture.path_label().into();
                });
                let mut first = true;
                while !state.stop.load(Ordering::Acquire) {
                    if let Some(frame) = capture.next(Duration::from_millis(100))? {
                        *screen.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(frame));
                        if first {
                            ready.send(())?;
                            first = false;
                        }
                    }
                }
                capture.close()?;
            }
            Ok(())
        });
        if request.sources.webcam {
            let req = request.clone();
            let state = shared.clone();
            let ready = ready_tx.clone();
            spawn_device(&shared, &mut devices.threads, "Camera", move || {
                if req.synthetic {
                    let frame = synthetic_frame(320, 180, 0, false, true)?;
                    *camera.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(frame));
                    ready.send(())?;
                    while !state.stop.load(Ordering::Acquire) {
                        let frame = synthetic_frame(320, 180, state.ms(), false, true)?;
                        *camera.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(frame));
                        state.update(|p| p.camera_frames += 1);
                        thread::sleep(Duration::from_millis(33));
                    }
                } else {
                    let mut capture = platform::camera::Camera::open(
                        req.camera_id.as_deref().context("No selected camera")?,
                    )?;
                    state.update(|p| p.devices_opened.webcam = true);
                    let mut first = true;
                    let mut clock = CameraClock::default();
                    while !state.stop.load(Ordering::Acquire) {
                        if let Some(mut frame) = capture.read()? {
                            frame.timestamp =
                                clock.map(frame.timestamp, platform::clock_100ns()?)?;
                            *camera.lock().unwrap_or_else(|e| e.into_inner()) =
                                Some(Arc::new(frame));
                            state.update(|p| p.camera_frames += 1);
                            if first {
                                ready.send(())?;
                                first = false;
                            }
                        }
                    }
                    capture.close()?;
                }
                Ok(())
            });
        }
        let mut index = 0;
        for system in [true, false] {
            if !(if system {
                request.sources.system_audio
            } else {
                request.sources.microphone
            }) {
                continue;
            }
            let state = shared.clone();
            let ready = ready_tx.clone();
            let tx = audio_tx.clone();
            let synthetic = request.synthetic;
            let selected_id = if system {
                request.output_id.clone()
            } else {
                request.microphone_id.clone()
            };
            let this_index = index;
            index += 1;
            spawn_device(
                &shared,
                &mut devices.threads,
                if system { "System audio" } else { "Microphone" },
                move || {
                    let mut native = if synthetic {
                        None
                    } else {
                        Some(platform::audio::Audio::open_selected(
                            system,
                            selected_id.as_deref(),
                        )?)
                    };
                    if !synthetic {
                        state.update(|p| {
                            if system {
                                p.devices_opened.system_audio = true
                            } else {
                                p.devices_opened.microphone = true
                            }
                        });
                    }
                    if let Some(native) = native.as_mut() {
                        native.start()?;
                        if !system {
                            let deadline = Instant::now() + Duration::from_secs(1);
                            loop {
                                ensure!(
                                    !state.stop.load(Ordering::Acquire),
                                    "Microphone startup cancelled."
                                );
                                if native.packet()?.is_some() {
                                    break;
                                }
                                ensure!(Instant::now()<deadline,"The selected microphone started but delivered no packet within 1 second.");
                                thread::sleep(Duration::from_millis(5));
                            }
                        }
                    }
                    ready.send(())?;
                    while state.origin.load(Ordering::Acquire) == 0
                        && !state.stop.load(Ordering::Acquire)
                    {
                        if let Some(native) = native.as_ref() {
                            for _ in 0..64 {
                                if native.packet()?.is_none() {
                                    break;
                                }
                            }
                        }
                        thread::sleep(Duration::from_millis(2));
                    }
                    if state.stop.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    let origin = state.origin.load(Ordering::Acquire);
                    let mut timeline = super::audio_timeline::AudioTimeline::new(system, origin);
                    let mut next_frame = 0u64;
                    while !state.stop.load(Ordering::Acquire) {
                        if let Some(native) = native.as_ref() {
                            while let Some(packet) = native.packet()? {
                                let plan = timeline.packet(packet, platform::clock_100ns()?)?;
                                state.update(|p| {
                                    p.audio_silence_frames += plan.silence_frames;
                                    p.audio_discontinuities += u64::from(plan.discontinuity);
                                });
                                for chunk in plan.chunks {
                                    send_audio(
                                        &state,
                                        &tx,
                                        this_index,
                                        chunk.sample / 48,
                                        chunk.pcm,
                                    )?;
                                }
                                if state.stop.load(Ordering::Acquire) {
                                    break;
                                }
                            }
                            if let Some(chunk) = timeline.idle(platform::clock_100ns()?)? {
                                state.update(|p| {
                                    p.audio_silence_frames += chunk.pcm.len() as u64 / 4
                                });
                                send_audio(&state, &tx, this_index, chunk.sample / 48, chunk.pcm)?;
                            }
                        } else {
                            let due = state.ms() * 48;
                            while next_frame + 480 <= due {
                                let mut pcm = Vec::with_capacity(1920);
                                let frequency = if system { 440.0 } else { 880.0 };
                                for frame in next_frame..next_frame + 480 {
                                    let value = ((frame as f64 / 48000.0
                                        * frequency
                                        * std::f64::consts::TAU)
                                        .sin()
                                        * 8000.0
                                        + if frame % 48000 < 4800 {
                                            (frame as f64 / 48000.0
                                                * if system { 1200.0 } else { 2400.0 }
                                                * std::f64::consts::TAU)
                                                .sin()
                                                * 6000.0
                                        } else {
                                            0.0
                                        }) as i16;
                                    pcm.extend(value.to_le_bytes());
                                    pcm.extend(value.to_le_bytes());
                                }
                                send_audio(&state, &tx, this_index, next_frame / 48, pcm)?;
                                next_frame += 480;
                            }
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                    if let Some(native) = native.as_mut() {
                        native.stop()?;
                    }
                    Ok(())
                },
            );
        }
        let expected = 1 + usize::from(request.sources.webcam) + usize::from(request.audio_count());
        let deadline = Instant::now() + Duration::from_secs(12);
        let mut ready_count = 0;
        while ready_count < expected {
            if let Some(error) = shared.error() {
                bail!("{error}");
            }
            ensure!(
                !shared.stop.load(Ordering::Acquire),
                "Capture cancelled before sources were ready."
            );
            ensure!(
                Instant::now() < deadline,
                "Selected source startup exceeded 12 seconds."
            );
            if ready_rx.recv_timeout(Duration::from_millis(30)).is_ok() {
                ready_count += 1;
            }
        }
        Ok(devices)
    }
    fn stop(&mut self) -> Result<()> {
        self.shared.stop.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_millis(2500);
        while self.threads.iter().any(|t| !t.is_finished()) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        ensure!(self.threads.iter().all(JoinHandle::is_finished),
            "A device did not release within 2.5 seconds. The supervised process will exit; committed segments are retained.");
        for t in self.threads.drain(..) {
            t.join()
                .map_err(|_| anyhow::anyhow!("Device shutdown thread panicked"))?;
        }
        Ok(())
    }
}
impl Drop for Devices {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
    }
}

fn send_audio(
    state: &Shared,
    tx: &Sender<AudioPacket>,
    index: u8,
    ms: u64,
    pcm: Vec<u8>,
) -> Result<()> {
    let bytes = pcm.len();
    let previous = state.audio_bytes.fetch_add(bytes, Ordering::AcqRel);
    if previous + bytes > state.audio_limit {
        state.audio_bytes.fetch_sub(bytes, Ordering::AcqRel);
        bail!("The bounded two-seconds-per-source audio queue is full. Capture stopped rather than growing memory or dropping audio.");
    }
    state.update(|p| p.peak_audio_queue_bytes = p.peak_audio_queue_bytes.max(previous + bytes));
    if let Err(e) = tx.try_send(AudioPacket { index, ms, pcm }) {
        state.audio_bytes.fetch_sub(bytes, Ordering::AcqRel);
        bail!("Bounded audio channel cannot accept a packet: {e}");
    }
    Ok(())
}

#[derive(Default)]
struct CameraClock {
    source: Option<i64>,
    origin: i64,
    last: i64,
}
impl CameraClock {
    fn map(&mut self, source: i64, arrival: i64) -> Result<i64> {
        let first = *self.source.get_or_insert_with(|| {
            self.origin = arrival;
            source
        });
        let mapped = self.origin + (source - first);
        ensure!(mapped >= self.last, "Camera timestamp moved backwards.");
        let error = arrival - mapped;
        ensure!(
            error.abs() <= 5_000_000,
            "Camera clock/arrival divergence exceeded 500 ms."
        );
        self.origin += (error / 1000).clamp(-1000, 1000);
        self.last = mapped;
        Ok(mapped)
    }
}

pub fn run(request: Request) -> Result<()> {
    let _ownership = library::exclusive_lock(&request.session_dir.join("session.lock"))?;
    let shared = Shared {
        stop: Arc::new(AtomicBool::new(false)),
        finished: Arc::new(AtomicBool::new(false)),
        origin: Arc::new(AtomicI64::new(0)),
        error: Arc::new(Mutex::new(None)),
        progress: Arc::new(Mutex::new(Progress {
            detail: encoder::source_description(&request.sources),
            ..Default::default()
        })),
        audio_bytes: Arc::new(AtomicUsize::new(0)),
        audio_limit: 192_000 * 2 * usize::from(request.audio_count().max(1)),
    };
    let state = shared.clone();
    thread::spawn(move || {
        if let Some(line) = std::io::stdin().lock().lines().next() {
            match line {
                Ok(line) if line == "stop" => {
                    state.stop.store(true, Ordering::Release);
                    let ms = state.ms();
                    state.update(|p| {
                        p.stop_requested_ms = Some(ms);
                        if matches!(p.phase, Phase::Starting | Phase::Recording) {
                            p.phase = Phase::Stopping;
                            p.detail = "Stop received by the media control thread.".into();
                        }
                    });
                }
                Ok(_) => {
                    state.fail("Invalid media control command.".into());
                }
                Err(e) => {
                    state.fail(format!("Media control channel failed: {e}"));
                }
            }
        }
        state.stop.store(true, Ordering::Release);
    });
    let state = shared.clone();
    let heartbeat = thread::spawn(move || {
        while !state.finished.load(Ordering::Acquire) {
            state.update(|p| {
                if matches!(
                    p.phase,
                    Phase::Starting | Phase::Recording | Phase::Stopping
                ) {
                    p.elapsed_ms = state.ms();
                }
            });
            let progress = state
                .progress
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            match serde_json::to_string(&progress) {
                Ok(json) => println!("{json}"),
                Err(e) => {
                    eprintln!("Status serialization: {e}");
                    state.stop.store(true, Ordering::Release);
                }
            }
            thread::sleep(Duration::from_millis(100));
        }
        let progress = state
            .progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Ok(json) = serde_json::to_string(&progress) {
            println!("{json}");
        }
    });
    let result = run_capture(&request, &shared);
    match &result {
        Ok(capture) => shared.update(|p| {
            p.phase = Phase::Saved;
            p.capture_id = Some(capture.id.clone());
            p.detail = "Saved locally. All capture devices released.".into();
        }),
        Err(e) => shared.update(|p| {
            p.phase = Phase::Interrupted;
            p.detail = format!("{e:#}");
        }),
    }
    shared.stop.store(true, Ordering::Release);
    shared.finished.store(true, Ordering::Release);
    heartbeat
        .join()
        .map_err(|_| anyhow::anyhow!("Heartbeat thread failed"))?;
    library::write_json(
        &request.session_dir.join("progress.json"),
        &*shared.progress.lock().unwrap_or_else(|e| e.into_inner()),
    )?;
    result.map(|_| ())
}

fn run_capture(request: &Request, shared: &Shared) -> Result<Capture> {
    ensure!(
        crate::platform::process::free_space(&request.session_dir)? > 512 * 1024 * 1024,
        "Less than 512 MiB is free. Capture was not started."
    );
    let mut journal = if request.video {
        Some(encoder::create_journal(request)?)
    } else {
        None
    };
    let mut devices = Devices::start(request, shared.clone())?;
    if request.test_fault == Some(TestFault::BeforeFirstCommit) {
        super::mark_fault(request, TestFault::BeforeFirstCommit)?;
        std::process::exit(97);
    }
    if !request.video {
        shared
            .origin
            .store(platform::clock_100ns()?, Ordering::Release);
        let screen = devices
            .screen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .context("No screen image")?;
        let camera = devices
            .camera
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let mut bgra = vec![0; screen.width as usize * screen.height as usize * 4];
        composite_frame_with_layout(
            &screen,
            camera.as_deref(),
            screen.width,
            screen.height,
            &mut bgra,
            request.pip,
        );
        shared.update(|p| {
            p.phase = Phase::Stopping;
            p.detail = "Screenshot captured; releasing devices.".into();
        });
        devices.stop()?;
        shared.update(|p| {
            p.devices_stopped_ms = Some(shared.ms());
            p.phase = Phase::Finalizing;
            p.detail = "Saving screenshot locally.".into();
        });
        let rgba = Frame {
            width: screen.width,
            height: screen.height,
            bgra,
            timestamp: screen.timestamp,
        }
        .into_rgba()?;
        let library = Library::open(request.library_root.clone())?;
        let capture = library.import_image(&rgba, "Screenshot")?;
        library::write_json(
            &library.capture_dir(&capture)?.join("sources.json"),
            &request.sources,
        )?;
        return Ok(capture);
    }
    let mut encoder = encoder::Encoder::start(request)?;
    ensure!(
        !shared.stop.load(Ordering::Acquire),
        "Capture cancelled before the media timeline started."
    );
    shared
        .origin
        .store(platform::clock_100ns()?, Ordering::Release);
    let (width, height) = request.canvas();
    let mut output = vec![0; width as usize * height as usize * 4];
    let mut next_tick = 0;
    let mut previous_stamp = None;
    let journal = Arc::new(Mutex::new(
        journal.take().context("Recording journal missing")?,
    ));
    let publisher_done = Arc::new(AtomicBool::new(false));
    let publish_done = publisher_done.clone();
    let published = journal.clone();
    let state = shared.clone();
    let publisher = thread::spawn(move || {
        while !publish_done.load(Ordering::Acquire) {
            if let Err(e) =
                encoder::publish_segments(&mut published.lock().unwrap_or_else(|e| e.into_inner()))
            {
                state.fail(format!("Segment publication: {e:#}"));
                break;
            }
            thread::sleep(Duration::from_millis(150));
        }
    });
    shared.update(|p| {
        p.phase = Phase::Recording;
        p.detail = encoder::source_description(&request.sources);
    });
    let result = (|| -> Result<()> {
        let mut last_disk_check = Instant::now();
        let mut next_jitter = 5000;
        while !shared.stop.load(Ordering::Acquire) {
            let ms = shared.ms();
            if request.test_fault == Some(TestFault::VideoDeliveryJitter) && ms >= next_jitter {
                thread::sleep(Duration::from_millis(180));
                next_jitter = ms + 5000;
            }
            if ms >= 3500 {
                match request.test_fault {
                    Some(TestFault::WorkerExit) => {
                        super::mark_fault(request, TestFault::WorkerExit)?;
                        std::process::exit(97);
                    }
                    Some(TestFault::EncoderExit) => {
                        super::mark_fault(request, TestFault::EncoderExit)?;
                        encoder.abort_for_test()?;
                    }
                    Some(TestFault::WriterStall) => {
                        super::mark_fault(request, TestFault::WriterStall)?;
                        thread::sleep(Duration::from_secs(60));
                    }
                    _ => {}
                }
            }
            if ms >= request.max_seconds * 1000 {
                break;
            }
            while let Ok(packet) = devices.audio.try_recv() {
                shared
                    .audio_bytes
                    .fetch_sub(packet.pcm.len(), Ordering::AcqRel);
                encoder.audio(packet.index, packet.ms, &packet.pcm)?;
            }
            let ms = shared.ms();
            let tick = ms * 30 / 1000;
            if tick >= next_tick {
                let screen = devices
                    .screen
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
                    .context("Screen frame missing")?;
                let camera = devices
                    .camera
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                if let Some(camera) = &camera {
                    ensure!(
                        platform::clock_100ns()? - camera.timestamp < 20_000_000,
                        "Camera stopped delivering frames for 2 seconds."
                    );
                }
                let repeated = previous_stamp == Some(screen.timestamp);
                previous_stamp = Some(screen.timestamp);
                composite_frame_with_layout(
                    &screen,
                    camera.as_deref(),
                    width,
                    height,
                    &mut output,
                    request.pip,
                );
                encoder.video(tick * 1000 / 30, &output)?;
                shared.update(|p| {
                    p.frames += 1;
                    p.repeated += u64::from(repeated);
                    p.skipped += tick.saturating_sub(next_tick);
                    p.last_write_ms = ms;
                });
                next_tick = tick + 1;
            }
            encoder.check()?;
            if last_disk_check.elapsed() > Duration::from_secs(1) {
                ensure!(crate::platform::process::free_space(&request.session_dir)?>256*1024*1024,
                    "Free space fell below 256 MiB. Recording stopped; committed segments are retained.");
                last_disk_check = Instant::now();
            }
            thread::sleep(Duration::from_millis(2));
        }
        Ok(())
    })();
    shared.update(|p| {
        p.phase = Phase::Stopping;
        p.stop_requested_ms.get_or_insert(shared.ms());
        p.detail = "Releasing selected capture devices.".into();
    });
    let device_result = devices.stop();
    if let Err(error) = device_result {
        publisher_done.store(true, Ordering::Release);
        drop(encoder);
        publisher
            .join()
            .map_err(|_| anyhow::anyhow!("Segment publisher panicked during shutdown"))?;
        return Err(error)
            .context("Device release was not confirmed; the media process is exiting.");
    }
    if device_result.is_ok() {
        shared.update(|p| {
            p.devices_stopped_ms = Some(shared.ms());
            p.elapsed_ms = shared.ms();
            p.phase = Phase::DevicesStopped;
            p.detail = "Devices released; finalizing media.".into();
        });
    }
    if request.test_fault == Some(TestFault::FinalDrainStall) {
        super::mark_fault(request, TestFault::FinalDrainStall)?;
        thread::sleep(Duration::from_secs(60));
    }
    while let Ok(packet) = devices.audio.try_recv() {
        shared
            .audio_bytes
            .fetch_sub(packet.pcm.len(), Ordering::AcqRel);
        if let Err(e) = encoder.audio(packet.index, packet.ms, &packet.pcm) {
            shared.fail(format!("Final audio packet: {e:#}"));
            break;
        }
    }
    shared.update(|p| {
        p.phase = Phase::Finalizing;
        p.detail = "Committing playable segments and building the final MP4.".into();
    });
    let encoder_result = encoder.finish();
    publisher_done.store(true, Ordering::Release);
    publisher
        .join()
        .map_err(|_| anyhow::anyhow!("Segment publisher panicked"))?;
    let mut journal = journal.lock().unwrap_or_else(|e| e.into_inner());
    encoder::publish_segments(&mut journal)?;
    let failure = result
        .err()
        .or(encoder_result.err())
        .or_else(|| shared.error().map(anyhow::Error::msg));
    if let Some(failure) = failure {
        journal.interrupted = Some(format!("{failure:#}"));
        library::write_json(&request.session_dir.join("journal.json"), &*journal)?;
        return Err(failure).context("Recording interrupted. Use Recover in the library to export the verified completed segments.");
    }
    encoder::final_export(&mut journal)
}

pub fn composite_frame(
    screen: &Frame,
    camera: Option<&Frame>,
    width: u32,
    height: u32,
    output: &mut [u8],
) {
    composite_frame_with_layout(screen, camera, width, height, output, None);
}

pub fn composite_frame_with_layout(
    screen: &Frame,
    camera: Option<&Frame>,
    width: u32,
    height: u32,
    output: &mut [u8],
    layout: Option<crate::capture::PipLayout>,
) {
    for y in 0..height {
        let sy = y as usize * screen.height as usize / height as usize;
        for x in 0..width {
            let sx = x as usize * screen.width as usize / width as usize;
            let src = (sy * screen.width as usize + sx) * 4;
            let dst = (y as usize * width as usize + x as usize) * 4;
            output[dst..dst + 4].copy_from_slice(&screen.bgra[src..src + 4]);
            output[dst + 3] = 255;
        }
    }
    if let Some(camera) = camera {
        let (left, top, w, h) = if let Some(layout) = layout {
            let r = layout.rect(width, height, camera.width, camera.height);
            (r.x, r.y, r.width, r.height)
        } else {
            let w = (width / 4).clamp(1, 360);
            let h = (w * camera.height / camera.width).min(height).max(1);
            (
                width.saturating_sub(w + 12),
                height.saturating_sub(h + 12),
                w,
                h,
            )
        };
        for y in 0..h {
            for x in 0..w {
                let sx = x * camera.width / w;
                let sy = y * camera.height / h;
                let src = (sy as usize * camera.width as usize + sx as usize) * 4;
                let dst = ((top + y) as usize * width as usize + (left + x) as usize) * 4;
                output[dst..dst + 4].copy_from_slice(&camera.bgra[src..src + 4]);
                output[dst + 3] = 255;
            }
        }
    }
}

pub fn synthetic_frame(
    width: u32,
    height: u32,
    ms: u64,
    cursor: bool,
    camera: bool,
) -> Result<Frame> {
    let mut pixels = vec![0; width as usize * height as usize * 4];
    for y in 0..height {
        for x in 0..width {
            let i = (y as usize * width as usize + x as usize) * 4;
            pixels[i..i + 4].copy_from_slice(if camera {
                &[25, 210, 35, 255]
            } else {
                &[60, 35, 20, 255]
            });
            if !camera && x < width / 2 && y < height / 2 {
                pixels[i..i + 4].copy_from_slice(&[200, 45, 20, 255]);
            }
            if !camera && y < 12 && x < (ms / 10 % u64::from(width)) as u32 {
                pixels[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
            if cursor
                && x > width / 2
                && x < width / 2 + 12
                && y > height / 2
                && y < height / 2 + 28
            {
                pixels[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
            if ms % 1000 < 100
                && x < 80
                && if camera {
                    y < 32
                } else {
                    y >= height / 2 + 40 && y < height / 2 + 80
                }
            {
                pixels[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
    }
    Ok(Frame {
        width,
        height,
        bgra: pixels,
        timestamp: platform::clock_100ns()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn camera_epoch_mapping_and_regression() {
        let mut c = CameraClock::default();
        assert_eq!(c.map(1000, 50_000_000).unwrap(), 50_000_000);
        assert_eq!(c.map(334_333, 50_333_333).unwrap(), 50_333_333);
        assert!(c.map(20, 50_666_666).is_err());
    }
    #[test]
    fn microphone_off_and_camera_off_do_not_fabricate_content() {
        let screen = synthetic_frame(100, 80, 0, false, false).unwrap();
        let camera = synthetic_frame(20, 20, 0, false, true).unwrap();
        let mut without = vec![0; 100 * 80 * 4];
        let mut with = without.clone();
        composite_frame(&screen, None, 100, 80, &mut without);
        composite_frame(&screen, Some(&camera), 100, 80, &mut with);
        assert_ne!(without, with);
        assert_eq!(
            &without[(60 * 100 + 80) * 4..(60 * 100 + 80) * 4 + 4],
            &[60, 35, 20, 255]
        );
    }
}
