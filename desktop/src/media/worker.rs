use super::{
    audio_timeline::AudioTimeline,
    encoder,
    pause::{PauseTimeline, SAMPLE_RATE},
    Phase, Progress, Request, TestFault,
};
use crate::{
    library::{self, Capture, Library},
    platform::{self, Frame},
};
use anyhow::{bail, ensure, Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender};
use std::{
    io::{BufRead, Read},
    sync::{
        atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering},
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
    media_sample: Arc<AtomicU64>,
    control: Arc<AtomicU64>,
    stop_clock: Arc<AtomicI64>,
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
        self.media_sample.load(Ordering::Acquire) / 48
    }
    fn wall_sample(&self) -> Result<u64> {
        let origin = self.origin.load(Ordering::Acquire);
        if origin == 0 {
            return Ok(0);
        }
        Ok(
            ((i128::from(platform::clock_100ns()?) - i128::from(origin)).max(0)
                * i128::from(SAMPLE_RATE)
                / 10_000_000) as u64,
        )
    }
    fn wall_ms(&self) -> Result<u64> {
        Ok(self.wall_sample()? / 48)
    }
}
enum AudioEvent {
    Packet {
        index: u8,
        packet: platform::audio::Packet,
        observed: i64,
    },
    Drained {
        index: u8,
        observed: i64,
    },
}
impl AudioEvent {
    fn bytes(&self) -> usize {
        match self {
            Self::Packet { packet, .. } => packet.pcm.len(),
            Self::Drained { .. } => 0,
        }
    }
}
struct Devices {
    shared: Shared,
    threads: Vec<JoinHandle<()>>,
    screen: FrameSlot,
    camera: FrameSlot,
    audio: Receiver<AudioEvent>,
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
                        state.wall_ms()?,
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
                        let frame = synthetic_frame(320, 180, state.wall_ms()?, false, true)?;
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
                                if native.packet(this_index)?.is_some() {
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
                        if let Some(native) = native.as_mut() {
                            for _ in 0..64 {
                                if native.packet(this_index)?.is_none() {
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
                    let mut next_frame = 0u64;
                    let mut last_packet = Instant::now();
                    let mut next_drained = Instant::now();
                    while !state.stop.load(Ordering::Acquire) {
                        if let Some(native) = native.as_mut() {
                            while let Some(packet) = native.packet(this_index)? {
                                last_packet = Instant::now();
                                send_audio(
                                    &state,
                                    &tx,
                                    AudioEvent::Packet {
                                        index: this_index,
                                        packet,
                                        observed: platform::clock_100ns()?,
                                    },
                                )?;
                                if state.stop.load(Ordering::Acquire) {
                                    break;
                                }
                            }
                            ensure!(system || last_packet.elapsed() <= Duration::from_secs(1),
                                "The selected microphone stopped delivering packets for 1 second, including while paused.");
                        } else {
                            let due = state.wall_sample()?;
                            while next_frame + 480 <= due {
                                let pcm = generated_pcm(next_frame, 480, system);
                                send_audio(
                                    &state,
                                    &tx,
                                    AudioEvent::Packet {
                                        index: this_index,
                                        observed: platform::clock_100ns()?,
                                        packet: platform::audio::Packet {
                                            timing: platform::audio::PacketTiming {
                                                sequence: next_frame / 480 + 1,
                                                timestamp: origin
                                                    + (i128::from(next_frame) * 10_000_000
                                                        / i128::from(SAMPLE_RATE))
                                                        as i64,
                                                device_position: next_frame,
                                                frames: 480,
                                                flags: 0,
                                                client_rate_hz: SAMPLE_RATE as u32,
                                                engine_mix_rate_hz: None,
                                            },
                                            pcm,
                                        },
                                    },
                                )?;
                                next_frame += 480;
                            }
                        }
                        if Instant::now() >= next_drained {
                            send_audio(
                                &state,
                                &tx,
                                AudioEvent::Drained {
                                    index: this_index,
                                    observed: platform::clock_100ns()?,
                                },
                            )?;
                            next_drained = Instant::now() + Duration::from_millis(50);
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

fn send_audio(state: &Shared, tx: &Sender<AudioEvent>, event: AudioEvent) -> Result<()> {
    let bytes = event.bytes();
    let previous = state.audio_bytes.fetch_add(bytes, Ordering::AcqRel);
    if previous + bytes > state.audio_limit {
        state.audio_bytes.fetch_sub(bytes, Ordering::AcqRel);
        bail!("The bounded two-seconds-per-source audio queue is full. Capture stopped rather than growing memory or dropping audio.");
    }
    state.update(|p| p.peak_audio_queue_bytes = p.peak_audio_queue_bytes.max(previous + bytes));
    if let Err(e) = tx.try_send(event) {
        state.audio_bytes.fetch_sub(bytes, Ordering::AcqRel);
        bail!("Bounded audio channel cannot accept a packet: {e}");
    }
    Ok(())
}

fn read_controls(state: &Shared) -> Result<()> {
    let input = std::io::stdin();
    let mut reader = input.lock();
    loop {
        let mut line = String::new();
        if (&mut reader).take(65).read_line(&mut line)? == 0 {
            return Ok(());
        }
        ensure!(
            line.len() <= 64 && line.ends_with('\n'),
            "Media command exceeded its 64-byte budget."
        );
        let line = line.trim_end_matches('\n');
        if line == "stop" {
            let now = platform::clock_100ns()?;
            let _ = state
                .stop_clock
                .compare_exchange(0, now, Ordering::AcqRel, Ordering::Acquire);
            state.stop.store(true, Ordering::Release);
            let wall = state.wall_ms()?;
            state.update(|p| {
                p.stop_requested_ms = Some(wall);
                if matches!(
                    p.phase,
                    Phase::Starting
                        | Phase::Recording
                        | Phase::Pausing
                        | Phase::Paused
                        | Phase::Resuming
                ) {
                    p.phase = Phase::Stopping;
                    p.detail = "Stop received by the media control thread.".into();
                }
            });
            return Ok(());
        }
        let (command, sequence) = line
            .split_once(' ')
            .context("Invalid media control command")?;
        ensure!(
            command == "pause" || command == "resume",
            "Unknown media control command."
        );
        let sequence: u64 = sequence.parse()?;
        ensure!(
            sequence > 0 && sequence <= u64::MAX >> 1,
            "Invalid media control sequence."
        );
        let next = (sequence << 1) | u64::from(command == "pause");
        let previous = state.control.load(Ordering::Acquire);
        ensure!(
            sequence > previous >> 1 || next == previous,
            "Stale or conflicting media control sequence."
        );
        state.control.store(next, Ordering::Release);
    }
}

fn apply_control(
    state: &Shared,
    clock: &mut PauseTimeline,
    wall: u64,
    applied: &mut u64,
) -> Result<()> {
    if state.stop.load(Ordering::Acquire) {
        return Ok(());
    }
    let requested = state.control.load(Ordering::Acquire);
    if requested == *applied {
        return Ok(());
    }
    let paused = requested & 1 != 0;
    clock.set_paused(wall, paused)?;
    *applied = requested;
    state
        .media_sample
        .store(clock.media_at(wall), Ordering::Release);
    state.update(|p| {
        if !state.stop.load(Ordering::Acquire) {
            p.phase = if paused {
                Phase::Paused
            } else {
                Phase::Recording
            };
            p.detail = if paused {
                "Paused. Selected devices remain on; paused pixels and audio are excluded."
            } else {
                "Recording resumed on the same media timeline."
            }
            .into();
        }
        p.control_ack = requested >> 1;
        p.control_wall_sample = wall;
        p.control_media_sample = clock.media_at(wall);
        p.last_write_wall_ms = wall / 48;
    });
    Ok(())
}

fn write_media(state: &Shared, write: impl FnOnce() -> Result<()>) -> Result<()> {
    let started = state.wall_ms()?;
    state.update(|p| p.write_started_wall_ms = Some(started));
    let result = write();
    let finished = state.wall_ms()?;
    state.update(|p| {
        p.write_started_wall_ms = None;
        if result.is_ok() {
            p.last_write_wall_ms = finished;
        }
    });
    result
}

fn receive_audio(devices: &Devices, timelines: &mut [AudioTimeline], state: &Shared) -> Result<()> {
    for _ in 0..512 {
        let event = match devices.audio.try_recv() {
            Ok(event) => event,
            Err(crossbeam_channel::TryRecvError::Empty) => break,
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                ensure!(
                    timelines.is_empty() || state.stop.load(Ordering::Acquire),
                    "Selected audio sources closed their channel before Stop."
                );
                break;
            }
        };
        let (index, packet, observed) = match event {
            AudioEvent::Packet {
                index,
                packet,
                observed,
            } => (index, Some(packet), observed),
            AudioEvent::Drained { index, observed } => (index, None, observed),
        };
        let timeline = timelines
            .get_mut(index as usize)
            .context("Audio event has an unselected source index")?;
        if let Some(packet) = packet {
            let discontinuity = timeline
                .observe(packet, observed)
                .with_context(|| format!("Admitting selected audio stream index={index}"))?;
            state.update(|p| p.audio_discontinuities += u64::from(discontinuity));
        } else {
            timeline.observe_drained(observed)?;
        }
        state
            .audio_bytes
            .fetch_sub(timeline.take_released_bytes(), Ordering::AcqRel);
    }
    Ok(())
}

fn write_audio_until(
    timelines: &mut [AudioTimeline],
    clock: &PauseTimeline,
    sealed_wall: u64,
    media_limit: u64,
    final_drain: bool,
    mut write: impl FnMut(u8, u64, &[u8]) -> Result<()>,
    state: &Shared,
) -> Result<()> {
    for (index, timeline) in timelines.iter_mut().enumerate() {
        let budget = if final_drain { usize::MAX } else { 32 };
        for _ in 0..budget {
            let prepared = timeline
                .prepare_next(clock, sealed_wall, media_limit, final_drain)
                .with_context(|| format!("Preparing selected audio stream index={index}"))?;
            state
                .audio_bytes
                .fetch_sub(timeline.take_released_bytes(), Ordering::AcqRel);
            let Some(prepared) = prepared else { break };
            write_media(state, || {
                write(index as u8, prepared.sample / 48, &prepared.pcm)
            })?;
            timeline.commit(&prepared)?;
            state
                .audio_bytes
                .fetch_sub(timeline.take_released_bytes(), Ordering::AcqRel);
            state.update(|p| p.audio_silence_frames += prepared.silence_frames);
        }
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
        media_sample: Arc::new(AtomicU64::new(0)),
        control: Arc::new(AtomicU64::new(0)),
        stop_clock: Arc::new(AtomicI64::new(0)),
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
        let result = read_controls(&state);
        if let Err(error) = result {
            state.fail(format!("Media control channel: {error:#}"));
        }
        match platform::clock_100ns() {
            Ok(clock) => {
                let _ = state.stop_clock.compare_exchange(
                    0,
                    clock,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
            }
            Err(error) => state.fail(format!("Stop clock failed: {error:#}")),
        }
        state.stop.store(true, Ordering::Release);
    });
    let state = shared.clone();
    let heartbeat = thread::spawn(move || {
        while !state.finished.load(Ordering::Acquire) {
            let wall = state.wall_ms();
            if let Err(error) = &wall {
                state.fail(format!("Heartbeat clock failed: {error:#}"));
            }
            state.update(|p| {
                if matches!(
                    p.phase,
                    Phase::Starting
                        | Phase::Recording
                        | Phase::Pausing
                        | Phase::Paused
                        | Phase::Resuming
                        | Phase::Stopping
                ) {
                    p.elapsed_ms = state.ms();
                }
                if let Ok(wall) = wall {
                    p.wall_elapsed_ms = wall;
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
        let stopped = shared.wall_ms()?;
        shared.update(|p| {
            p.devices_stopped_ms = Some(stopped);
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
    let origin = shared.origin.load(Ordering::Acquire);
    shared.update(|p| p.timeline_origin_100ns = origin);
    let mut audio_timelines = Vec::new();
    if request.sources.system_audio {
        audio_timelines.push(AudioTimeline::new(true, origin));
    }
    if request.sources.microphone {
        audio_timelines.push(AudioTimeline::new(false, origin));
    }
    let mut clock = PauseTimeline::default();
    let mut applied_control = 0;
    let media_limit = request.max_seconds * SAMPLE_RATE;
    let (width, height) = request.canvas();
    let mut output = vec![0; width as usize * height as usize * 4];
    let mut next_tick = 0;
    let mut last_encoded_ms = None;
    let mut slow_write_fired = false;
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
            let wall = shared.wall_sample()?;
            // This loop is the only media-admission and pause-boundary owner. No native
            // acquisition thread can commit output or acknowledge a transition.
            apply_control(shared, &mut clock, wall, &mut applied_control)?;
            let sample = clock.media_at(wall).min(media_limit);
            shared.media_sample.store(sample, Ordering::Release);
            let ms = sample / 48;
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
                clock.freeze(clock.wall_at(media_limit));
                break;
            }
            receive_audio(&devices, &mut audio_timelines, shared)?;
            write_audio_until(
                &mut audio_timelines,
                &clock,
                wall,
                media_limit,
                false,
                |index, ms, pcm| encoder.audio(index, ms, pcm),
                shared,
            )?;
            let video_sample = clock.media_at(shared.wall_sample()?);
            let video_ms = video_sample / 48;
            let tick = video_ms * 30 / 1000;
            if !clock.paused()
                && video_sample < media_limit
                && tick >= next_tick
                && !shared.stop.load(Ordering::Acquire)
            {
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
                let stamp = tick * 1000 / 30;
                let delay = request.test_fault == Some(TestFault::SlowVideoWrite)
                    && !slow_write_fired
                    && video_ms >= 800;
                write_media(shared, || {
                    encoder.video(stamp, &output)?;
                    if delay {
                        super::mark_fault(request, TestFault::SlowVideoWrite)?;
                        thread::sleep(Duration::from_millis(400));
                    }
                    Ok(())
                })?;
                slow_write_fired |= delay;
                last_encoded_ms = Some(stamp);
                shared.update(|p| {
                    p.frames += 1;
                    p.repeated += u64::from(repeated);
                    p.skipped += tick.saturating_sub(next_tick);
                    p.last_write_ms = video_ms;
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
    if clock.frozen().is_none() {
        let stopped = shared.stop_clock.load(Ordering::Acquire);
        let wall = if stopped != 0 {
            ((i128::from(stopped) - i128::from(origin)).max(0) * i128::from(SAMPLE_RATE)
                / 10_000_000) as u64
        } else {
            shared.wall_sample()?
        };
        clock.freeze(wall.min(clock.wall_at(media_limit)));
    }
    let final_wall = clock
        .frozen()
        .context("Final media endpoint was not frozen")?;
    shared.media_sample.store(
        clock.media_at(final_wall).min(media_limit),
        Ordering::Release,
    );
    let stop_ms = shared.wall_ms()?;
    shared.update(|p| {
        p.phase = Phase::Stopping;
        p.stop_requested_ms.get_or_insert(stop_ms);
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
        let stopped = shared.wall_ms()?;
        shared.update(|p| {
            p.devices_stopped_ms = Some(stopped);
            p.elapsed_ms = shared.ms();
            p.phase = Phase::DevicesStopped;
            p.detail = "Devices released; finalizing media.".into();
        });
    }
    if result.is_ok() && shared.error().is_none() {
        if let Some(stamp) = last_encoded_ms.and_then(|last| held_tail_timestamp(last, shared.ms()))
        {
            // Seal the presentation of the last admitted pixels, never sample paused sources.
            if let Err(error) = write_media(shared, || encoder.video(stamp, &output)) {
                shared.fail(format!("Sealing the held final frame: {error:#}"));
            } else {
                shared.update(|p| {
                    p.frames += 1;
                    p.repeated += 1;
                    p.last_write_ms = stamp;
                });
            }
        }
    }
    if request.test_fault == Some(TestFault::FinalDrainStall) {
        super::mark_fault(request, TestFault::FinalDrainStall)?;
        thread::sleep(Duration::from_secs(60));
    }
    if let Err(error) = receive_audio(&devices, &mut audio_timelines, shared).and_then(|_| {
        write_audio_until(
            &mut audio_timelines,
            &clock,
            final_wall,
            media_limit,
            true,
            |index, ms, pcm| encoder.audio(index, ms, pcm),
            shared,
        )
    }) {
        shared.fail(format!("Final audio admission: {error:#}"));
    }
    for timeline in &mut audio_timelines {
        shared
            .audio_bytes
            .fetch_sub(timeline.discard_pending(), Ordering::AcqRel);
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

fn held_tail_timestamp(last: u64, end: u64) -> Option<u64> {
    (end > last.saturating_add(34)).then(|| end.saturating_sub(33))
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

pub(super) fn pulse_frequency(system: bool, second: u64) -> f64 {
    (if system { 1200.0 } else { 4800.0 }) + (second % 16) as f64 * 200.0
}

pub(super) fn generated_pcm(start: u64, frames: u32, system: bool) -> Vec<u8> {
    let mut pcm = Vec::with_capacity(frames as usize * 4);
    for frame in start..start + u64::from(frames) {
        let time = frame as f64 / 48000.0;
        let tone =
            (time * if system { 440.0 } else { 880.0 } * std::f64::consts::TAU).sin() * 8000.0;
        let marker = if frame % 48000 < 4800 {
            (time * pulse_frequency(system, frame / 48000) * std::f64::consts::TAU).sin() * 6000.0
        } else {
            0.0
        };
        let value = (tone + marker) as i16;
        pcm.extend(value.to_le_bytes());
        pcm.extend(value.to_le_bytes());
    }
    pcm
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
    if width >= 80 && height >= 16 {
        let second = (ms / 1000) as u16;
        for y in height - 16..height - 8 {
            for x in 0..76u32 {
                let white = if x < 64 {
                    second & (1 << (x / 4)) != 0
                } else {
                    x < 68
                };
                let offset = (y as usize * width as usize + x as usize) * 4;
                pixels[offset..offset + 4].copy_from_slice(if white {
                    &[255, 255, 255, 255]
                } else {
                    &[0, 0, 0, 255]
                });
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
    fn audio_test_state() -> Shared {
        Shared {
            stop: Arc::new(AtomicBool::new(false)),
            finished: Arc::new(AtomicBool::new(false)),
            origin: Arc::new(AtomicI64::new(1)),
            media_sample: Arc::new(AtomicU64::new(0)),
            control: Arc::new(AtomicU64::new(0)),
            stop_clock: Arc::new(AtomicI64::new(0)),
            error: Arc::new(Mutex::new(None)),
            progress: Arc::new(Mutex::new(Progress::default())),
            audio_bytes: Arc::new(AtomicUsize::new(0)),
            audio_limit: 768_000,
        }
    }

    #[test]
    fn audio_queue_preserves_source_order_and_discontinuity_boundaries() {
        use platform::audio::{Packet, PacketTiming};
        use windows::Win32::Media::Audio::AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY;
        for sources in [vec![true], vec![false], vec![true, false]] {
            let shared = audio_test_state();
            let (tx, rx) = bounded(512);
            let devices = Devices {
                shared: shared.clone(),
                threads: vec![],
                screen: Arc::new(Mutex::new(None)),
                camera: Arc::new(Mutex::new(None)),
                audio: rx,
            };
            let mut timelines = sources
                .iter()
                .map(|&system| AudioTimeline::new(system, 0))
                .collect::<Vec<_>>();
            for step in 0..3 {
                for (index, _) in sources.iter().enumerate().rev() {
                    send_audio(
                        &shared,
                        &tx,
                        AudioEvent::Packet {
                            index: index as u8,
                            observed: (step + 1) as i64 * 100_000,
                            packet: Packet {
                                timing: PacketTiming {
                                    sequence: step + 1,
                                    timestamp: step as i64 * 100_000,
                                    device_position: match step {
                                        0 => 9600,
                                        1 => 0,
                                        _ => 480,
                                    },
                                    frames: 480,
                                    flags: if step == 1 {
                                        AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32
                                    } else {
                                        0
                                    },
                                    client_rate_hz: SAMPLE_RATE as u32,
                                    engine_mix_rate_hz: Some(if sources[index] {
                                        48_000
                                    } else {
                                        16_000
                                    }),
                                },
                                pcm: vec![index as u8 * 10 + step as u8 + 1; 1920],
                            },
                        },
                    )
                    .unwrap();
                }
            }
            receive_audio(&devices, &mut timelines, &shared).unwrap();
            assert_eq!(
                shared.progress.lock().unwrap().audio_discontinuities,
                sources.len() as u64
            );
            for (index, timeline) in timelines.iter_mut().enumerate() {
                for step in 0..3u64 {
                    let prepared = timeline
                        .prepare_next(&PauseTimeline::default(), 1440, 1440, true)
                        .unwrap()
                        .unwrap();
                    assert_eq!(prepared.sample, step * 480);
                    assert_eq!(prepared.silence_frames, 0);
                    assert_eq!(prepared.pcm, vec![index as u8 * 10 + step as u8 + 1; 1920]);
                    timeline.commit(&prepared).unwrap();
                    shared
                        .audio_bytes
                        .fetch_sub(timeline.take_released_bytes(), Ordering::AcqRel);
                }
                assert!(timeline
                    .prepare_next(&PauseTimeline::default(), 1440, 1440, true)
                    .unwrap()
                    .is_none());
            }
            assert_eq!(shared.audio_bytes.load(Ordering::Acquire), 0);
            assert_eq!(
                shared.progress.lock().unwrap().peak_audio_queue_bytes,
                sources.len() * 5760
            );
        }
    }

    #[test]
    fn audio_queue_preserves_the_observed_converted_microphone_pair_in_client_time() {
        use platform::audio::{Packet, PacketTiming};
        let shared = audio_test_state();
        let (tx, rx) = bounded(512);
        let devices = Devices {
            shared: shared.clone(),
            threads: vec![],
            screen: Arc::new(Mutex::new(None)),
            camera: Arc::new(Mutex::new(None)),
            audio: rx,
        };
        let mut timelines = vec![AudioTimeline::new(true, 0), AudioTimeline::new(false, 0)];
        for (sequence, position, timestamp, marker) in [(1, 0, 0, 7), (2, 441, 100_000, 11)] {
            send_audio(
                &shared,
                &tx,
                AudioEvent::Packet {
                    index: 1,
                    observed: 1_000_000,
                    packet: Packet {
                        timing: PacketTiming {
                            sequence,
                            device_position: position,
                            timestamp,
                            frames: 480,
                            flags: 0,
                            client_rate_hz: 48_000,
                            engine_mix_rate_hz: Some(44_100),
                        },
                        pcm: vec![marker; 1920],
                    },
                },
            )
            .unwrap();
        }
        receive_audio(&devices, &mut timelines, &shared).unwrap();
        let mut written = Vec::new();
        write_audio_until(
            &mut timelines,
            &PauseTimeline::default(),
            960,
            960,
            false,
            |index, ms, pcm| {
                written.push((index, ms, pcm.to_vec()));
                Ok(())
            },
            &shared,
        )
        .unwrap();
        assert!(written == vec![(1, 0, vec![7; 1920]), (1, 10, vec![11; 1920])]);
        assert_eq!(shared.audio_bytes.load(Ordering::Acquire), 0);
        let progress = shared.progress.lock().unwrap();
        assert_eq!(progress.peak_audio_queue_bytes, 3840);
        assert_eq!(progress.audio_silence_frames, 0);
        assert_eq!(progress.audio_discontinuities, 0);
    }

    #[test]
    fn deferred_audio_failures_keep_the_correct_queued_pair_and_selected_index() {
        use platform::audio::{Packet, PacketTiming};
        use windows::Win32::Media::Audio::AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY;
        for (case, first_frames, position, sample, discontinuity, expected) in [
            ("timestamp-gap", 480, 0, 28_800, true, "gap_frames=28320"),
            (
                "converted-gap",
                480,
                32_091,
                24_481,
                false,
                "missing_frames=24001",
            ),
            (
                "device-gap",
                480,
                34_081,
                480,
                false,
                "missing_frames=24001",
            ),
            ("overlap", 1920, 0, 480, true, "overlap_frames=1440"),
            ("first-packet", 0, 0, 28_800, true, "previous=[none]"),
        ] {
            let shared = audio_test_state();
            let (tx, rx) = bounded(512);
            let devices = Devices {
                shared: shared.clone(),
                threads: vec![],
                screen: Arc::new(Mutex::new(None)),
                camera: Arc::new(Mutex::new(None)),
                audio: rx,
            };
            let mut timelines = vec![AudioTimeline::new(true, 0), AudioTimeline::new(false, 0)];
            let packet = |sequence, position, sample, frames, flags| Packet {
                timing: PacketTiming {
                    sequence,
                    device_position: position,
                    frames,
                    flags,
                    timestamp: (sample as i128 * 10_000_000 / i128::from(SAMPLE_RATE)) as i64,
                    client_rate_hz: SAMPLE_RATE as u32,
                    engine_mix_rate_hz: Some(if case == "converted-gap" {
                        44_100
                    } else {
                        SAMPLE_RATE as u32
                    }),
                },
                pcm: vec![sequence as u8; frames as usize * 4],
            };
            let mut writes = 0;
            if first_frames != 0 {
                send_audio(
                    &shared,
                    &tx,
                    AudioEvent::Packet {
                        index: 1,
                        observed: 400_000,
                        packet: packet(1, 9600, 0, first_frames, 0),
                    },
                )
                .unwrap();
                receive_audio(&devices, &mut timelines, &shared).unwrap();
                write_audio_until(
                    &mut timelines,
                    &PauseTimeline::default(),
                    u64::from(first_frames),
                    u64::MAX,
                    false,
                    |index, ms, pcm| {
                        assert_eq!((index, ms, pcm.len()), (1, 0, first_frames as usize * 4));
                        writes += 1;
                        Ok(())
                    },
                    &shared,
                )
                .unwrap();
            }
            let current_sequence = if first_frames == 0 { 1 } else { 2 };
            for (sequence, position, sample, flags) in [
                (
                    current_sequence,
                    position,
                    sample,
                    if discontinuity {
                        AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32
                    } else {
                        0
                    },
                ),
                (
                    current_sequence + 1,
                    position + if case == "converted-gap" { 441 } else { 480 },
                    sample + 480,
                    0,
                ),
            ] {
                let queued = packet(sequence, position, sample, 480, flags);
                let observed = queued.timing.timestamp + 100_000;
                send_audio(
                    &shared,
                    &tx,
                    AudioEvent::Packet {
                        index: 1,
                        observed,
                        packet: queued,
                    },
                )
                .unwrap();
            }
            receive_audio(&devices, &mut timelines, &shared).unwrap();
            let error = write_audio_until(
                &mut timelines,
                &PauseTimeline::default(),
                30_000,
                u64::MAX,
                false,
                |_, _, _| panic!("A rejected packet must not reach the encoder."),
                &shared,
            )
            .unwrap_err();
            let error = format!("{error:#}");
            for field in [
                "Preparing selected audio stream index=1",
                "Microphone",
                expected,
            ] {
                assert!(error.contains(field), "{case}: {error}");
            }
            assert!(
                error.contains(&format!("current=[sequence={current_sequence} ")),
                "{error}"
            );
            if first_frames != 0 {
                assert!(
                    error.contains("previous=[sequence=1 position=9600"),
                    "{error}"
                );
            }
            assert!(
                !error.contains(&format!("sequence={} ", current_sequence + 1)),
                "{error}"
            );
            assert_eq!(writes, usize::from(first_frames != 0));
            assert!(error.len() < 1024, "{error}");
        }
    }

    #[test]
    fn audio_queue_failure_names_the_actual_selected_source_index() {
        use platform::audio::{Packet, PacketTiming};
        let shared = audio_test_state();
        let (tx, rx) = bounded(512);
        let devices = Devices {
            shared: shared.clone(),
            threads: vec![],
            screen: Arc::new(Mutex::new(None)),
            camera: Arc::new(Mutex::new(None)),
            audio: rx,
        };
        let mut timelines = vec![AudioTimeline::new(true, 0), AudioTimeline::new(false, 0)];
        for step in 0..2 {
            send_audio(
                &shared,
                &tx,
                AudioEvent::Packet {
                    index: 1,
                    observed: (step + 1) as i64 * 100_000,
                    packet: Packet {
                        timing: PacketTiming {
                            sequence: step + 1,
                            timestamp: step as i64 * 100_000,
                            device_position: if step == 0 { 9600 } else { 0 },
                            frames: 480,
                            flags: 0,
                            client_rate_hz: SAMPLE_RATE as u32,
                            engine_mix_rate_hz: Some(16_000),
                        },
                        pcm: vec![17; 1920],
                    },
                },
            )
            .unwrap();
        }
        let error = format!(
            "{:#}",
            receive_audio(&devices, &mut timelines, &shared).unwrap_err()
        );
        for field in [
            "index=1",
            "Microphone",
            "Unflagged",
            "previous_position=9600",
            "flags=0x0",
            "engine_mix_rate_hz=Some(16000)",
        ] {
            assert!(error.contains(field), "{error}");
        }
        assert!(!error.contains("System audio"));
        assert_eq!(shared.progress.lock().unwrap().audio_discontinuities, 0);
    }

    #[test]
    fn held_frame_sealing_closes_a_slow_write_gap_without_extra_normal_frames() {
        assert_eq!(held_tail_timestamp(800, 1200), Some(1167));
        assert_eq!(held_tail_timestamp(19_966, 20_000), None);
        assert_eq!(held_tail_timestamp(1000, 1010), None);
    }
    #[test]
    fn pause_acknowledgement_waits_for_inflight_write_and_stop_can_preempt_it() {
        use std::sync::Barrier;
        for stop_during_write in [false, true] {
            let shared = Shared {
                stop: Arc::new(AtomicBool::new(false)),
                finished: Arc::new(AtomicBool::new(false)),
                origin: Arc::new(AtomicI64::new(platform::clock_100ns().unwrap())),
                media_sample: Arc::new(AtomicU64::new(0)),
                control: Arc::new(AtomicU64::new(0)),
                stop_clock: Arc::new(AtomicI64::new(0)),
                error: Arc::new(Mutex::new(None)),
                progress: Arc::new(Mutex::new(Progress {
                    phase: Phase::Recording,
                    ..Default::default()
                })),
                audio_bytes: Arc::new(AtomicUsize::new(0)),
                audio_limit: 384_000,
            };
            let writing = Arc::new(Barrier::new(2));
            let release = Arc::new(Barrier::new(2));
            let state = shared.clone();
            let entered = writing.clone();
            let resume = release.clone();
            let worker = thread::spawn(move || {
                let mut clock = PauseTimeline::default();
                write_media(&state, || {
                    entered.wait();
                    resume.wait();
                    Ok(())
                })
                .unwrap();
                apply_control(&state, &mut clock, state.wall_sample().unwrap(), &mut 0).unwrap();
                clock.paused()
            });
            writing.wait();
            shared.control.store(3, Ordering::Release);
            assert_eq!(shared.progress.lock().unwrap().control_ack, 0);
            assert!(shared
                .progress
                .lock()
                .unwrap()
                .write_started_wall_ms
                .is_some());
            if stop_during_write {
                shared.stop.store(true, Ordering::Release);
            }
            release.wait();
            assert_eq!(worker.join().unwrap(), !stop_during_write);
            let progress = shared.progress.lock().unwrap();
            assert_eq!(progress.control_ack, u64::from(!stop_during_write));
            assert!(progress.write_started_wall_ms.is_none());
        }
    }

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
