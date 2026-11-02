use super::{Phase, Progress, Request};
use crate::{
    library,
    platform::process::{self, Job},
};
use anyhow::{bail, Context, Result};
use std::{
    io::{BufRead, BufReader, Read, Write},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub struct Recorder {
    progress: Arc<Mutex<Progress>>,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    abort: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Recorder {
    pub fn start(request: Request) -> Self {
        Self::start_with_stop(request, Arc::new(AtomicBool::new(false)))
    }
    pub fn start_with_stop(request: Request, stop: Arc<AtomicBool>) -> Self {
        let progress = Arc::new(Mutex::new(Progress {
            detail: "Starting supervised media worker".into(),
            ..Default::default()
        }));
        let state = progress.clone();
        let cancel = stop.clone();
        let done = Arc::new(AtomicBool::new(false));
        let abort = Arc::new(AtomicBool::new(false));
        let abandon = abort.clone();
        let completed = done.clone();
        let handle = thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                supervise(&request, &state, &cancel, &abandon)
            }))
            .unwrap_or_else(|_| {
                Err(anyhow::anyhow!(
                    "Media supervisor panicked. The owned worker lifetime was ended."
                ))
            });
            if let Err(error) = result {
                let mut p = state.lock().unwrap_or_else(|e| e.into_inner());
                p.phase = Phase::Interrupted;
                p.detail = format!(
                    "{error:#}\nRecovery files: {}",
                    request.session_dir.display()
                );
            }
            completed.store(true, Ordering::Release);
        });
        Self {
            progress,
            stop,
            done,
            abort,
            thread: Some(handle),
        }
    }
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        let mut p = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        if !p.terminal() && !matches!(p.phase, Phase::Finalizing | Phase::DevicesStopped) {
            p.phase = Phase::Stopping;
            p.detail = "Stop requested. Waiting for device release.".into();
        }
    }
    pub fn cancel_finalization(&self) {
        self.abort.store(true, Ordering::Release);
        self.stop.store(true, Ordering::Release);
        let mut p = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        p.detail =
            "Cancelling finalization. Completed segments will be retained for recovery.".into();
    }
    pub fn progress(&self) -> Progress {
        self.progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn finished(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
    }
    pub fn completion_signal(&self) -> Arc<AtomicBool> {
        self.done.clone()
    }
}
impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.abort.store(true, Ordering::Release);
        if self.thread.as_ref().is_some_and(|t| t.is_finished()) {
            if let Some(t) = self.thread.take() {
                if t.join().is_err() {
                    eprintln!("Media supervisor panicked.");
                }
            }
        }
    }
}

fn merge_progress(mut progress: Progress, stop_requested: bool) -> Progress {
    if stop_requested && matches!(progress.phase, Phase::Starting | Phase::Recording) {
        progress.phase = Phase::Stopping;
        progress.detail = "Stop requested. Waiting for confirmed device release.".into();
    }
    progress
}

fn publish_progress(state: &Mutex<Progress>, stop: &AtomicBool, incoming: Progress) {
    let mut current = state.lock().unwrap_or_else(|e| e.into_inner());
    // Stop acknowledgement and heartbeat publication share this linearization point.
    *current = merge_progress(incoming, stop.load(Ordering::Acquire));
}

fn persist_exit_progress(
    state: &Mutex<Progress>,
    stop: &AtomicBool,
    peak_memory: usize,
    path: &std::path::Path,
    before_persist: impl FnOnce(),
) -> Result<Progress> {
    let mut current = state.lock().unwrap_or_else(|e| e.into_inner());
    current.peak_job_committed_bytes = peak_memory;
    before_persist();
    // Stop, the exit snapshot and its durable publication share one boundary.
    // Never copy an earlier heartbeat back over an acknowledged Stop.
    if stop.load(Ordering::Acquire) && matches!(current.phase, Phase::Starting | Phase::Recording) {
        current.phase = Phase::Stopping;
        current.detail = "Stop requested. Waiting for confirmed device release.".into();
    }
    library::write_json(path, &*current)?;
    Ok(current.clone())
}

fn supervise(
    request: &Request,
    progress: &Arc<Mutex<Progress>>,
    stop: &Arc<AtomicBool>,
    abort: &Arc<AtomicBool>,
) -> Result<()> {
    request.validate()?;
    anyhow::ensure!(
        !stop.load(Ordering::Acquire),
        "Capture cancelled before device activation."
    );
    std::fs::create_dir(&request.session_dir)
        .context("Creating a new, non-overwriting capture session")?;
    let path = request.session_dir.join("request.json");
    library::write_json(&path, request)?;
    let job = Job::new().context("Creating the mandatory worker lifetime boundary")?;
    let mut child = process::command(&std::env::current_exe()?)
        .arg("--worker")
        .arg(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Starting native media worker")?;
    // The worker blocks at a no-device/no-child gate until it belongs to this kill-on-close job.
    // Parent death before assignment closes stdin, so that path also cannot activate a source.
    if let Err(e) = job.assign(&child) {
        child.kill()?;
        child.wait()?;
        return Err(e).context("Job assignment failed; recording was not started");
    }
    let mut control = child
        .stdin
        .take()
        .context("Worker control channel is missing")?;
    let output = child
        .stdout
        .take()
        .context("Worker heartbeat channel is missing")?;
    let errors = child
        .stderr
        .take()
        .context("Worker diagnostic channel is missing")?;
    let latest = Arc::new(Mutex::new(Instant::now()));
    let latest_reader = latest.clone();
    let state = progress.clone();
    let cancel_reader = stop.clone();
    let reader = thread::spawn(move || {
        let mut reader = BufReader::new(output);
        let mut bytes = Vec::new();
        loop {
            bytes.clear();
            let result = (&mut reader).take(16_385).read_until(b'\n', &mut bytes);
            match result {
                Ok(0) => break,
                Ok(_) if bytes.len() <= 16_384 => {
                    match serde_json::from_slice::<Progress>(&bytes) {
                        Ok(p) => {
                            publish_progress(&state, &cancel_reader, p);
                            *latest_reader.lock().unwrap_or_else(|e| e.into_inner()) =
                                Instant::now();
                        }

                        Err(e) => {
                            let mut p = state.lock().unwrap_or_else(|e| e.into_inner());
                            p.phase = Phase::Interrupted;
                            p.detail = format!("Invalid worker status: {e}");
                            break;
                        }
                    }
                }
                Ok(_) | Err(_) => {
                    let mut p = state.lock().unwrap_or_else(|e| e.into_inner());
                    p.phase = Phase::Interrupted;
                    p.detail =
                        "Worker status channel failed or exceeded its message budget.".into();
                    break;
                }
            }
        }
    });
    let diagnostics = Arc::new(Mutex::new(String::new()));
    let diagnostics_reader = diagnostics.clone();
    thread::spawn(move || {
        for line in BufReader::new(errors).lines() {
            let line = match line {
                Ok(l) => l,
                Err(e) => format!("Diagnostic read failed: {e}"),
            };
            let mut d = diagnostics_reader.lock().unwrap_or_else(|e| e.into_inner());
            if d.len() > 8192 {
                d.clear();
            }
            d.extend(line.chars().take(1024));
            d.push('\n');
        }
    });
    if stop.load(Ordering::Acquire) {
        job.terminate()?;
        child.wait()?;
        reader
            .join()
            .map_err(|_| anyhow::anyhow!("Cancelled worker status reader panicked"))?;
        anyhow::bail!("Capture cancelled before device activation.");
    }
    control.write_all(b"start\n")?;
    control.flush()?;
    let started = Instant::now();
    let mut stop_time = None;
    let mut finalize_time = None;
    let mut forced = None;
    let mut peak_memory = 0usize;
    loop {
        peak_memory = peak_memory.max(job.peak_committed_bytes()?);
        {
            let mut p = progress.lock().unwrap_or_else(|e| e.into_inner());
            p.peak_job_committed_bytes = peak_memory;
        }
        if let Some(code) = child.try_wait()? {
            if job.active_processes()? > 0 {
                job.terminate()?;
            }
            let deadline = Instant::now() + Duration::from_secs(3);
            while job.active_processes()? > 0 && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(20));
            }
            anyhow::ensure!(
                job.active_processes()? == 0,
                "Worker exited but descendant shutdown could not be confirmed."
            );
            reader
                .join()
                .map_err(|_| anyhow::anyhow!("Worker status reader panicked"))?;
            let p = persist_exit_progress(
                progress,
                stop,
                peak_memory,
                &request.session_dir.join("last-progress.json"),
                || {},
            )?;
            if let Some(reason) = forced {
                bail!("{reason} Devices/processes have now exited. Last committed segments were retained.");
            }
            if !code.success() {
                bail!(
                    "Media worker exited ({code}). {} {}",
                    p.detail,
                    diagnostics.lock().unwrap_or_else(|e| e.into_inner())
                );
            }
            anyhow::ensure!(p.terminal(),"Worker exited without a saved/interrupted result. Files are retained for recovery.");
            return Ok(());
        }
        let p = progress.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if matches!(p.phase, Phase::DevicesStopped | Phase::Finalizing) && finalize_time.is_none() {
            finalize_time = Some(Instant::now());
        }
        if abort.load(Ordering::Acquire) && forced.is_none() {
            forced = Some("Finalization cancelled by request".into());
            job.terminate()?;
        }
        let stale =
            latest.lock().unwrap_or_else(|e| e.into_inner()).elapsed() > Duration::from_secs(5);
        let stalled =
            p.phase == Phase::Recording && p.elapsed_ms.saturating_sub(p.last_write_ms) > 5000;
        if (stop.load(Ordering::Acquire)
            || p.phase == Phase::Stopping
            || p.phase == Phase::Interrupted
            || stale
            || stalled)
            && stop_time.is_none()
            && !matches!(
                p.phase,
                Phase::Saved | Phase::Finalizing | Phase::DevicesStopped
            )
        {
            // Only a tiny control command crosses this dedicated pipe, never media payload.
            if let Err(e) = control.write_all(b"stop\n") {
                forced = Some(format!("Stop control failed: {e}"));
                job.terminate()?;
            }
            stop_time = Some(Instant::now());
        }
        let timed_out = stop_time.is_some_and(|t| t.elapsed() > Duration::from_secs(5))
            && !matches!(
                p.phase,
                Phase::Finalizing | Phase::DevicesStopped | Phase::Saved
            );
        let startup_timeout =
            p.phase == Phase::Starting && started.elapsed() > Duration::from_secs(20);
        let final_timeout = finalize_time.is_some_and(|t| t.elapsed() > Duration::from_secs(180));
        if forced.is_none() && (timed_out || startup_timeout || final_timeout) {
            let stage = if startup_timeout {
                "Device startup exceeded 20 seconds"
            } else if final_timeout {
                "Finalization exceeded 180 seconds"
            } else {
                "Device shutdown exceeded 5 seconds"
            };
            forced = Some(stage.to_owned());
            job.terminate()?;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stop_at_worker_exit_cannot_regress_visible_or_durable_progress() {
        use std::sync::Barrier;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("last-progress.json");
        let progress = Arc::new(Mutex::new(Progress {
            phase: Phase::Recording,
            ..Default::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let exit_locked = Arc::new(Barrier::new(2));
        let stop_requested = Arc::new(Barrier::new(2));
        let state = progress.clone();
        let flag = stop.clone();
        let entered = exit_locked.clone();
        let release = stop_requested.clone();
        let output = path.clone();
        let exit = thread::spawn(move || {
            persist_exit_progress(&state, &flag, 12345, &output, || {
                entered.wait();
                release.wait();
            })
            .unwrap()
        });
        exit_locked.wait();
        assert!(
            progress.try_lock().is_err(),
            "The exit snapshot must retain the state lock until publication."
        );
        stop.store(true, Ordering::Release);
        stop_requested.wait();
        assert_eq!(exit.join().unwrap().phase, Phase::Stopping);
        assert_eq!(progress.lock().unwrap().phase, Phase::Stopping);
        let saved: Progress = library::read_json(&path).unwrap();
        assert_eq!(saved.phase, Phase::Stopping);
        assert_eq!(saved.peak_job_committed_bytes, 12345);
    }
    #[test]
    fn stop_acknowledged_after_a_reader_snapshot_cannot_be_overwritten() {
        use std::sync::Barrier;
        let progress = Arc::new(Mutex::new(Progress {
            phase: Phase::Recording,
            ..Default::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let snapshot_taken = Arc::new(Barrier::new(2));
        let publish_allowed = Arc::new(Barrier::new(2));
        let state = progress.clone();
        let reader_stop = stop.clone();
        let reader_snapshot = snapshot_taken.clone();
        let reader_release = publish_allowed.clone();
        let reader = thread::spawn(move || {
            assert!(!reader_stop.load(Ordering::Acquire));
            let incoming = Progress {
                phase: Phase::Recording,
                ..Default::default()
            };
            reader_snapshot.wait();
            reader_release.wait();
            publish_progress(&state, &reader_stop, incoming);
        });
        let recorder = Recorder {
            progress,
            stop,
            done: Arc::new(AtomicBool::new(true)),
            abort: Arc::new(AtomicBool::new(false)),
            thread: None,
        };
        snapshot_taken.wait();
        recorder.stop();
        assert_eq!(recorder.progress().phase, Phase::Stopping);
        publish_allowed.wait();
        reader.join().unwrap();
        assert_eq!(recorder.progress().phase, Phase::Stopping);
        assert!(recorder.progress().detail.contains("Stop requested"));
    }

    #[test]
    fn delayed_heartbeat_cannot_revert_an_acknowledged_stop() {
        for phase in [Phase::Starting, Phase::Recording] {
            let result = merge_progress(
                Progress {
                    phase,
                    ..Default::default()
                },
                true,
            );
            assert_eq!(result.phase, Phase::Stopping);
        }
        assert_eq!(
            merge_progress(
                Progress {
                    phase: Phase::Finalizing,
                    ..Default::default()
                },
                true
            )
            .phase,
            Phase::Finalizing
        );
    }
}
