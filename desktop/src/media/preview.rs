use crate::platform::{
    self,
    process::{self, Job},
    Frame,
};
use anyhow::{ensure, Context, Result};
use std::{
    io::{BufRead, Read, Write},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Default)]
struct State {
    frame: Option<Frame>,
    error: Option<String>,
}

pub struct CameraPreview {
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
}

impl CameraPreview {
    pub fn start(camera_id: String, synthetic: bool, repaint: impl Fn() + Send + 'static) -> Self {
        let state = Arc::new(Mutex::new(State::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let result = Self {
            state: state.clone(),
            stop: stop.clone(),
            done: done.clone(),
        };
        thread::spawn(move || {
            if let Err(error) = supervise(&camera_id, synthetic, &state, &stop) {
                state.lock().unwrap_or_else(|e| e.into_inner()).error = Some(format!("{error:#}"));
            }
            done.store(true, Ordering::Release);
            repaint();
        });
        result
    }
    pub fn take_frame(&self) -> Option<Frame> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .frame
            .take()
    }
    pub fn take_error(&self) -> Option<String> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .error
            .take()
    }
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
    pub fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
    pub fn finished(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }
}
impl Drop for CameraPreview {
    fn drop(&mut self) {
        self.stop();
    }
}

fn supervise(
    id: &str,
    synthetic: bool,
    state: &Arc<Mutex<State>>,
    stop: &AtomicBool,
) -> Result<()> {
    ensure!(
        synthetic || (!id.is_empty() && id.len() <= 4096),
        "Choose a camera before opening its preview."
    );
    ensure!(
        !stop.load(Ordering::Acquire),
        "Camera preview was cancelled before activation."
    );
    let job = Job::new()?;
    let mut child = process::command(&std::env::current_exe()?)
        .arg(if synthetic {
            "--generated-camera-preview"
        } else {
            "--camera-preview"
        })
        .arg(id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Starting the supervised camera preview")?;
    if let Err(error) = job.assign(&child) {
        child.kill()?;
        child.wait()?;
        return Err(error).context("Camera preview ownership failed; the camera was not opened");
    }
    let mut control = child
        .stdin
        .take()
        .context("Camera preview control pipe is missing")?;
    let output = child
        .stdout
        .take()
        .context("Camera preview output pipe is missing")?;
    let errors = child
        .stderr
        .take()
        .context("Camera preview diagnostics are missing")?;
    let last_frame = Arc::new(Mutex::new(Instant::now()));
    let arrival = last_frame.clone();
    let frames = state.clone();
    let reader = thread::spawn(move || -> Result<()> {
        let mut output = std::io::BufReader::new(output);
        while let Some(frame) = read_frame(&mut output)? {
            frames.lock().unwrap_or_else(|e| e.into_inner()).frame = Some(frame);
            *arrival.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
        }
        Ok(())
    });
    let diagnostics = thread::spawn(move || -> Result<String> {
        let mut text = String::new();
        errors.take(8192).read_to_string(&mut text)?;
        Ok(text)
    });
    if stop.load(Ordering::Acquire) {
        job.terminate()?;
        child.wait()?;
        return Ok(());
    }
    control.write_all(b"start\n")?;
    control.flush()?;
    let mut stop_time = None;
    let mut forced = false;
    let mut failure = None;
    loop {
        if let Some(exit) = child.try_wait()? {
            if job.active_processes()? > 0 {
                job.terminate()?;
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            while job.active_processes()? > 0 && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            ensure!(
                job.active_processes()? == 0,
                "Camera preview exited but device-process release is not confirmed."
            );
            let frame_result = reader
                .join()
                .map_err(|_| anyhow::anyhow!("Camera preview reader panicked"))?;
            let diagnostic = diagnostics
                .join()
                .map_err(|_| anyhow::anyhow!("Camera diagnostics reader panicked"))??;
            if let Some(failure) = failure {
                anyhow::bail!("{failure} The preview process and its camera handles have exited.");
            }
            if !stop.load(Ordering::Acquire) {
                frame_result?;
                anyhow::bail!("Camera preview ended ({exit}). {diagnostic}");
            }
            return Ok(());
        }
        if reader.is_finished() && !stop.load(Ordering::Acquire) {
            failure = Some("Camera preview stopped delivering valid frames.".to_owned());
            stop.store(true, Ordering::Release);
        }
        if last_frame
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .elapsed()
            > Duration::from_secs(5)
            && !stop.load(Ordering::Acquire)
        {
            failure = Some("Camera preview delivered no frame for 5 seconds.".to_owned());
            stop.store(true, Ordering::Release);
        }
        if stop.load(Ordering::Acquire) && stop_time.is_none() {
            if let Err(error) = control.write_all(b"stop\n").and_then(|_| control.flush()) {
                failure = Some(format!("Camera preview stop command failed: {error}."));
                job.terminate()?;
                forced = true;
            }
            stop_time = Some(Instant::now());
        }
        if stop_time.is_some_and(|at| at.elapsed() > Duration::from_millis(500)) && !forced {
            job.terminate()?;
            forced = true;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn read_frame(reader: &mut impl Read) -> Result<Option<Frame>> {
    let mut header = [0; 8];
    if reader.read(&mut header[..1])? == 0 {
        return Ok(None);
    }
    reader
        .read_exact(&mut header[1..])
        .context("Camera preview header is incomplete")?;
    let width = u32::from_le_bytes(header[..4].try_into()?);
    let height = u32::from_le_bytes(header[4..].try_into()?);
    ensure!(
        width > 0 && height > 0 && width <= 640 && height <= 360,
        "Camera preview exceeded its 640x360 frame budget."
    );
    let mut bgra = vec![0; width as usize * height as usize * 4];
    reader
        .read_exact(&mut bgra)
        .context("Camera preview pixels are incomplete")?;
    Ok(Some(Frame {
        width,
        height,
        bgra,
        timestamp: platform::clock_100ns()?,
    }))
}

pub fn worker(id: &str, synthetic: bool) -> Result<()> {
    super::await_start_gate()?;
    let stop = Arc::new(AtomicBool::new(false));
    let cancel = stop.clone();
    thread::spawn(move || {
        if let Some(command) = std::io::stdin().lock().lines().next() {
            match command {
                Ok(command) if command == "stop" => {}
                Ok(_) => eprintln!("Invalid camera preview control command; stopping."),
                Err(error) => eprintln!("Camera preview control pipe failed: {error}"),
            }
        }
        cancel.store(true, Ordering::Release);
    });
    let mut camera = if synthetic {
        None
    } else {
        Some(platform::camera::Camera::open(id)?)
    };
    let started = Instant::now();
    let mut output = std::io::stdout().lock();
    while !stop.load(Ordering::Acquire) {
        let source = if let Some(camera) = &camera {
            camera.read()?
        } else {
            Some(super::worker::synthetic_frame(
                320,
                180,
                started.elapsed().as_millis() as u64,
                false,
                true,
            )?)
        };
        if let Some(source) = source {
            let scale = (640.0 / source.width as f64)
                .min(360.0 / source.height as f64)
                .min(1.0);
            let width = (source.width as f64 * scale).max(1.0) as u32;
            let height = (source.height as f64 * scale).max(1.0) as u32;
            let mut pixels = vec![0; width as usize * height as usize * 4];
            super::worker::composite_frame(&source, None, width, height, &mut pixels);
            output.write_all(&width.to_le_bytes())?;
            output.write_all(&height.to_le_bytes())?;
            output.write_all(&pixels)?;
            output.flush()?;
        }
        thread::sleep(Duration::from_millis(33));
    }
    if let Some(camera) = camera.as_mut() {
        camera.close()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preview_protocol_rejects_large_and_partial_frames_without_allocating_them() {
        let mut oversized = Vec::new();
        oversized.extend(1280u32.to_le_bytes());
        oversized.extend(720u32.to_le_bytes());
        assert!(read_frame(&mut oversized.as_slice()).is_err());
        assert!(read_frame(&mut [1, 0, 0].as_slice()).is_err());
        assert!(read_frame(&mut [].as_slice()).unwrap().is_none());
        let mut valid = Vec::new();
        valid.extend(2u32.to_le_bytes());
        valid.extend(1u32.to_le_bytes());
        valid.extend([1u8; 8]);
        let frame = read_frame(&mut valid.as_slice()).unwrap().unwrap();
        assert_eq!((frame.width, frame.height, frame.bgra.len()), (2, 1, 8));
    }
}
