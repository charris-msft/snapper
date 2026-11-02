mod audio_timeline;
pub mod encoder;
pub mod preview;
mod profile;
mod publication;
pub mod selftest;
pub mod supervisor;
pub mod worker;

use crate::{
    library::{self, Library},
    platform::Area,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, Read},
    path::PathBuf,
};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sources {
    pub cursor: bool,
    pub webcam: bool,
    pub system_audio: bool,
    pub microphone: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TestFault {
    BeforeFirstCommit,
    WorkerExit,
    EncoderExit,
    WriterStall,
    AfterSegmentRename,
    AfterFinalRename,
    FinalDrainStall,
    VideoDeliveryJitter,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub schema: u32,
    pub session_dir: PathBuf,
    pub library_root: PathBuf,
    pub area: Area,
    pub sources: Sources,
    pub camera_id: Option<String>,
    #[serde(default)]
    pub microphone_id: Option<String>,
    #[serde(default)]
    pub output_id: Option<String>,
    #[serde(default)]
    pub pip: Option<crate::capture::PipLayout>,
    pub video: bool,
    pub max_seconds: u64,
    pub synthetic: bool,
    #[serde(default)]
    pub test_fault: Option<TestFault>,
}
impl Request {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.schema == 1, "Unsupported media request version.");
        ensure!(
            self.test_fault.is_none() || (self.synthetic && self.video),
            "Failure injection is restricted to generated-media tests."
        );
        crate::model::validate_size(self.area.width, self.area.height)?;
        if let Some(pip) = self.pip {
            pip.validate()?;
        }
        for id in [&self.camera_id, &self.microphone_id, &self.output_id]
            .into_iter()
            .flatten()
        {
            ensure!(
                !id.is_empty() && id.len() <= 4096,
                "Selected device identifier is invalid."
            );
        }
        ensure!(
            (1..=7200).contains(&self.max_seconds),
            "Recording duration must be 1-7200 seconds."
        );
        ensure!(
            !self.sources.webcam
                || self.synthetic
                || self.camera_id.as_ref().is_some_and(|s| !s.is_empty()),
            "Choose a camera before enabling webcam capture."
        );
        ensure!(
            self.video || (!self.sources.system_audio && !self.sources.microphone),
            "Audio sources cannot be activated for a screenshot."
        );
        ensure!(
            self.session_dir.is_absolute() && self.library_root.is_absolute(),
            "Media storage paths must be absolute."
        );
        ensure!(
            self.session_dir.parent() == Some(self.library_root.join("sessions").as_path()),
            "Media session directory is outside this library."
        );
        Ok(())
    }
    pub fn canvas(&self) -> (u32, u32) {
        if !self.video {
            return (self.area.width, self.area.height);
        }
        let scale = (1920.0 / self.area.width as f64)
            .min(1080.0 / self.area.height as f64)
            .min(1.0);
        (
            ((self.area.width as f64 * scale) as u32 / 2 * 2).max(2),
            ((self.area.height as f64 * scale) as u32 / 2 * 2).max(2),
        )
    }
    pub fn audio_count(&self) -> u8 {
        u8::from(self.sources.system_audio) + u8::from(self.sources.microphone)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    #[default]
    Starting,
    Recording,
    Stopping,
    DevicesStopped,
    Finalizing,
    Saved,
    Interrupted,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Progress {
    pub phase: Phase,
    pub elapsed_ms: u64,
    pub frames: u64,
    pub repeated: u64,
    pub skipped: u64,
    pub audio_discontinuities: u64,
    pub audio_silence_frames: u64,
    pub camera_frames: u64,
    pub devices_opened: Sources,
    pub detail: String,
    pub capture_id: Option<String>,
    pub last_write_ms: u64,
    #[serde(default)]
    pub stop_requested_ms: Option<u64>,
    #[serde(default)]
    pub devices_stopped_ms: Option<u64>,
    #[serde(default)]
    pub screen_backend: String,
    #[serde(default)]
    pub peak_audio_queue_bytes: usize,
    #[serde(default)]
    pub peak_job_committed_bytes: usize,
}

pub(crate) fn mark_fault(request: &Request, fault: TestFault) -> Result<()> {
    ensure!(
        request.synthetic && request.test_fault == Some(fault),
        "Fault injection is limited to the requested generated-media test."
    );
    library::write_json(
        &request.session_dir.join("fault-fired.json"),
        &serde_json::json!({"fault":fault}),
    )
}
impl Progress {
    pub fn terminal(&self) -> bool {
        matches!(self.phase, Phase::Saved | Phase::Interrupted)
    }
}

pub fn entry() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--camera-preview") | Some("--generated-camera-preview") => {
            preview::worker(args.get(1).context("Preview camera identifier is missing")?,
                args[0] == "--generated-camera-preview")
        },
        Some("--worker")=>{
            let path=args.get(1).context("Worker request path is missing")?;
            let request:Request=library::read_json(std::path::Path::new(path))?;
            request.validate()?;
            // No activation, children, or capture before the parent establishes Job ownership.
            await_start_gate()?;
            worker::run(request)
        },
        Some("--codec-helper")=>{
            encoder::codec_helper(args.get(1).context("Codec helper kind is missing")?,
                args.get(2).context("Codec helper arguments are missing")?)
        },
        Some("--helper-owner-test")=>{
            await_start_gate()?;
            let mut command=crate::platform::process::command(&crate::platform::process::media_tool("ffmpeg")?);
            command.args(["-v","error","-re","-f","lavfi","-i","color=c=black:s=16x16:r=10","-f","null","NUL"]);
            encoder::run_bounded(command,std::time::Duration::from_secs(30))
        },
        Some("--self-test")=>{
            let directory=args.get(1).map(PathBuf::from).unwrap_or(std::env::current_dir()?.join("artifacts").join("acceptance"));
            selftest::run(&directory)
        },
        Some("--fault-test") => {
            let directory=args.get(1).map(PathBuf::from)
                .unwrap_or(std::env::current_dir()?.join("artifacts").join("fault-acceptance"));
            selftest::fault_tests(&directory)
        },
        Some("--profile-test")=>{
            let directory=args.get(1).map(PathBuf::from)
                .unwrap_or(std::env::current_dir()?.join("artifacts").join("profile-acceptance"));
            let seconds=args.get(2).map(|s|s.parse::<u64>()).transpose()?.unwrap_or(60);
            profile::run(&directory,seconds)
        },
        Some("--devices")=>{
            let monitors=crate::platform::screen::monitors()?;
            let cameras=crate::platform::camera::list()?;
            let screen_support=crate::platform::screen::support_report()?;
            println!("{}",serde_json::to_string_pretty(            &serde_json::json!({"monitors":monitors,"cameras":cameras,"screen_support":screen_support}))?);
            Ok(())
        },
        Some("--graphics-diagnostics")=>{
            println!("{}",serde_json::to_string_pretty(&crate::graphics::diagnostics()?)?);
            Ok(())
        },
        Some("--gui-smoke-test")=>{
            crate::editor::gui_smoke_test(PathBuf::from(args.get(1).context("GUI smoke report path is missing")?))
        },
        Some("--recover")=>{
            let directory=PathBuf::from(args.get(1).context("Recovery session directory is missing")?);
            encoder::recover(&directory)?;
            Ok(())
        },
        None|Some("--data-dir")=>{
            let root=if !args.is_empty(){PathBuf::from(args.get(1).context("Data directory is missing")?)}else{Library::default_root()?};
            crate::editor::run(root)
        },
        Some(_)=>anyhow::bail!("Unknown argument. Use --devices, --self-test <directory>, --recover <session>, or --data-dir <library>."),
    }
}

pub(crate) fn await_start_gate() -> Result<()> {
    let mut gate = String::new();
    std::io::stdin().lock().take(64).read_line(&mut gate)?;
    ensure!(gate == "start\n", "Supervised start gate was not granted.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_sixteen_flags_are_independent() {
        for mask in 0..16 {
            let s = Sources {
                cursor: mask & 1 != 0,
                webcam: mask & 2 != 0,
                system_audio: mask & 4 != 0,
                microphone: mask & 8 != 0,
            };
            let json = serde_json::to_vec(&s).unwrap();
            assert_eq!(serde_json::from_slice::<Sources>(&json).unwrap(), s);
            assert_eq!(
                u8::from(s.system_audio) + u8::from(s.microphone),
                ((mask & 4 != 0) as u8) + ((mask & 8 != 0) as u8)
            );
        }
    }
}
