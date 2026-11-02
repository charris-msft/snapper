use super::{encoder, encoder::Journal};
use crate::{library, platform::process};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    os::windows::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::Duration,
};
use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(super) struct FinalPublication {
    schema: u32,
    session_id: String,
    plan_sha256: String,
    bytes: u64,
    sha256: String,
    media: MediaShape,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct MediaShape {
    width: u32,
    height: u32,
    duration_seconds: f64,
    audio: Option<AudioShape>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct AudioShape {
    codec: String,
    sample_rate: u32,
    channels: u32,
}

#[derive(Deserialize)]
struct Probe {
    streams: Vec<ProbeStream>,
    format: ProbeFormat,
}

#[derive(Deserialize)]
struct ProbeStream {
    codec_type: String,
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    pix_fmt: Option<String>,
    sample_rate: Option<String>,
    channels: Option<u32>,
}

#[derive(Deserialize)]
struct ProbeFormat {
    format_name: String,
    duration: String,
}

fn session_id(journal: &Journal) -> Result<&str> {
    journal
        .request
        .session_dir
        .file_name()
        .and_then(|id| id.to_str())
        .context("Final publication has no valid session identity")
}

fn plan_hash(journal: &Journal) -> Result<String> {
    let request = &journal.request;
    let mut binding = serde_json::to_vec(&(
        "final-publication-v1",
        journal.schema,
        session_id(journal)?,
        request.schema,
        request.canvas(),
        &request.area,
        &request.sources,
        &request.camera_id,
        request.video,
        request.max_seconds,
        request.synthetic,
        &journal.segments,
    ))?;
    if request.microphone_id.is_some() || request.output_id.is_some() || request.pip.is_some() {
        binding.extend(serde_json::to_vec(&(
            "explicit-devices-and-layout-v1",
            &request.microphone_id,
            &request.output_id,
            request.pip,
        ))?);
    }
    Ok(format!("{:x}", Sha256::digest(binding)))
}

pub(super) fn timeline_seconds(journal: &Journal) -> Result<f64> {
    let first = journal
        .segments
        .first()
        .context("Final publication has no committed media")?;
    let last = journal
        .segments
        .last()
        .context("Final publication has no committed media")?;
    let seconds = last.end - first.start;
    ensure!(
        seconds.is_finite() && seconds > 0.0,
        "Invalid final media timeline."
    );
    Ok(seconds)
}

impl FinalPublication {
    pub(super) fn validate(&self, journal: &Journal) -> Result<()> {
        ensure!(journal.schema == 2 && self.schema == 1 && self.session_id == session_id(journal)?
            && self.plan_sha256 == plan_hash(journal)?,
            "Final publication identity does not match this session, its selected sources, or its committed segment timeline.");
        ensure!(
            self.bytes > 0
                && self.sha256.len() == 64
                && self.sha256.bytes().all(|value| value.is_ascii_hexdigit()),
            "Final publication fingerprint is invalid."
        );
        self.media.validate(journal)
    }
}

impl MediaShape {
    fn validate(&self, journal: &Journal) -> Result<()> {
        ensure!(
            (self.width, self.height) == journal.request.canvas(),
            "Final media dimensions do not match this recording."
        );
        let expected = timeline_seconds(journal)?;
        // AAC priming/padding and frame-duration rounding may extend a short MP4 slightly.
        ensure!(
            self.duration_seconds.is_finite() && (self.duration_seconds - expected).abs() <= 0.125,
            "Final media duration {:.6}s does not match this session's {:.6}s timeline.",
            self.duration_seconds,
            expected
        );
        ensure!(
            self.audio.is_some() == (journal.request.audio_count() > 0),
            "Final media audio streams do not match this recording's source selection."
        );
        if let Some(audio) = &self.audio {
            ensure!(
                audio.codec == "aac" && audio.sample_rate == 48_000 && audio.channels == 2,
                "Final media audio is not the expected AAC 48 kHz stereo mix."
            );
        }
        Ok(())
    }
}

fn inspect_probe(probe: Probe, journal: &Journal) -> Result<MediaShape> {
    ensure!(
        probe
            .format
            .format_name
            .split(',')
            .any(|name| name == "mp4"),
        "Final media is not an MP4 container."
    );
    let mut video = None;
    let mut audio = None;
    for stream in probe.streams {
        match stream.codec_type.as_str() {
            "video" => {
                ensure!(
                    video.is_none(),
                    "Final media has more than one video stream."
                );
                ensure!(
                    stream.codec_name.as_deref() == Some("h264")
                        && stream.pix_fmt.as_deref() == Some("yuv420p"),
                    "Final media is not the expected H.264/yuv420p video stream."
                );
                video = Some((
                    stream.width.context("Final video width is missing")?,
                    stream.height.context("Final video height is missing")?,
                ));
            }
            "audio" => {
                ensure!(
                    audio.is_none(),
                    "Final media has more than one audio stream."
                );
                audio = Some(AudioShape {
                    codec: stream.codec_name.context("Final audio codec is missing")?,
                    sample_rate: stream
                        .sample_rate
                        .context("Final audio sample rate is missing")?
                        .parse()?,
                    channels: stream
                        .channels
                        .context("Final audio channel count is missing")?,
                });
            }
            _ => anyhow::bail!(
                "Final media contains an unexpected {} stream.",
                stream.codec_type
            ),
        }
    }
    let (width, height) = video.context("Final media has no video stream")?;
    let media = MediaShape {
        width,
        height,
        duration_seconds: probe.format.duration.parse()?,
        audio,
    };
    media.validate(journal)?;
    Ok(media)
}

pub(super) struct StableMedia {
    path: PathBuf,
    input: File,
}

impl StableMedia {
    pub(super) fn open(path: &Path) -> Result<Self> {
        let input = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
            .open(path)
            .with_context(|| {
                format!(
                    "Locking {} against writes or replacement during final validation",
                    path.display()
                )
            })?;
        let metadata = input.metadata()?;
        ensure!(
            metadata.is_file() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0,
            "Final media must be a regular file, not a redirected/reparse-point path."
        );
        Ok(Self {
            path: path.to_owned(),
            input,
        })
    }

    fn fingerprint(&mut self, expected_bytes: Option<u64>) -> Result<(u64, String)> {
        let bytes = self.input.metadata()?.len();
        ensure!(
            bytes > 0 && expected_bytes.is_none_or(|expected| bytes == expected),
            "Final media byte length does not match its durable session publication identity."
        );
        self.input.seek(SeekFrom::Start(0))?;
        let mut digest = Sha256::new();
        let mut buffer = [0u8; 65_536];
        loop {
            let count = self.input.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
        Ok((bytes, format!("{:x}", digest.finalize())))
    }

    fn inspect(&self, journal: &Journal) -> Result<MediaShape> {
        let scratch = journal
            .request
            .session_dir
            .join(format!("final-probe-{}.tmp", library::id()));
        let output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&scratch)?;
        let mut command = process::command(&process::media_tool("ffprobe")?);
        command.args([
            "-v", "error", "-show_entries",
            "format=format_name,duration:stream=codec_type,codec_name,width,height,pix_fmt,sample_rate,channels",
            "-of", "json",
        ]).arg(&self.path);
        let result = (|| -> Result<MediaShape> {
            encoder::run_bounded_with_output(&mut command, Duration::from_secs(15), Some(output))?;
            inspect_probe(library::read_json(&scratch)?, journal)
        })();
        fs::remove_file(&scratch).context("Removing the session-owned final-media probe file")?;
        result
    }

    pub(super) fn prepare_publication(&mut self, journal: &Journal) -> Result<FinalPublication> {
        let media = self.inspect(journal)?;
        let (bytes, sha256) = self.fingerprint(None)?;
        Ok(FinalPublication {
            schema: 1,
            session_id: session_id(journal)?.to_owned(),
            plan_sha256: plan_hash(journal)?,
            bytes,
            sha256,
            media,
        })
    }

    pub(super) fn verify_publication(&mut self, journal: &Journal) -> Result<()> {
        let publication = journal.final_publication.as_ref().context(
            "Final MP4 has no durable publication identity for this recording. It was left unchanged; move the unbound final file aside before recovering the retained segments.",
        )?;
        publication.validate(journal)?;
        let (_, sha256) = self.fingerprint(Some(publication.bytes))?;
        ensure!(sha256 == publication.sha256,
            "Final MP4 does not match its durable session publication fingerprint. It was left unchanged; move the unrelated final file aside before recovering the retained segments.");
        let media = self.inspect(journal)?;
        ensure!(
            media == publication.media,
            "Final media stream shape or timing does not match its recorded publication identity."
        );
        Ok(())
    }
}

pub(crate) use crate::platform::process::publish_new_file;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        media::{Request, Sources},
        platform::Area,
    };

    fn journal(root: &Path) -> Journal {
        Journal {
            schema: 2,
            request: Request {
                schema: 1,
                session_dir: root.join("sessions").join("fixture"),
                library_root: root.to_owned(),
                area: Area {
                    monitor: "generated".into(),
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
                max_seconds: 3,
                synthetic: true,
                test_fault: None,
            },
            segments: vec![encoder::Segment {
                file: "segment-00000.mp4".into(),
                start: 0.0,
                end: 3.0,
                bytes: 100,
            }],
            completed: false,
            recovered: false,
            output: None,
            interrupted: None,
            final_publication: None,
        }
    }

    fn probe() -> Probe {
        serde_json::from_value(serde_json::json!({
            "streams":[{"codec_type":"video","codec_name":"h264","width":640,"height":360,"pix_fmt":"yuv420p"}],
            "format":{"format_name":"mov,mp4,m4a,3gp,3g2,mj2","duration":"3.0"}
        })).unwrap()
    }

    #[test]
    fn actual_stream_shape_and_duration_are_bound_to_the_recording() {
        let root = tempfile::tempdir().unwrap();
        let mut journal = journal(root.path());
        assert!(inspect_probe(probe(), &journal).is_ok());
        let mut wrong = probe();
        wrong.streams[0].width = Some(320);
        assert!(inspect_probe(wrong, &journal).is_err());
        let mut wrong = probe();
        wrong.format.duration = "1.0".into();
        assert!(inspect_probe(wrong, &journal).is_err());
        let mut edge = probe();
        edge.format.duration = "3.125".into();
        assert!(inspect_probe(edge, &journal).is_ok());
        let mut outside = probe();
        outside.format.duration = "3.126".into();
        assert!(inspect_probe(outside, &journal).is_err());
        let mut wrong = probe();
        wrong.streams[0].codec_name = Some("mpeg4".into());
        assert!(inspect_probe(wrong, &journal).is_err());
        let mut wrong = probe();
        wrong.streams.push(ProbeStream {
            codec_type: "audio".into(),
            codec_name: Some("aac".into()),
            width: None,
            height: None,
            pix_fmt: None,
            sample_rate: Some("48000".into()),
            channels: Some(2),
        });
        assert!(inspect_probe(wrong, &journal).is_err());
        journal.request.sources.microphone = true;
        assert!(inspect_probe(probe(), &journal).is_err());
    }

    #[test]
    fn durable_receipt_cannot_be_reused_for_another_session_or_source_plan() {
        let root = tempfile::tempdir().unwrap();
        let mut journal = journal(root.path());
        let receipt = FinalPublication {
            schema: 1,
            session_id: "fixture".into(),
            plan_sha256: plan_hash(&journal).unwrap(),
            bytes: 100,
            sha256: "a".repeat(64),
            media: inspect_probe(probe(), &journal).unwrap(),
        };
        assert!(receipt.validate(&journal).is_ok());
        journal.request.sources.cursor = true;
        assert!(receipt.validate(&journal).is_err());
        journal.request.sources.cursor = false;
        journal.segments[0].bytes += 1;
        assert!(receipt.validate(&journal).is_err());
        journal.segments[0].bytes -= 1;
        journal.request.session_dir.set_file_name("other-session");
        assert!(receipt.validate(&journal).is_err());
    }

    #[test]
    fn same_length_replacement_is_rejected_by_the_recorded_content_digest() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("recording.mp4");
        fs::write(&path, b"wrong-123").unwrap();
        let mut journal = journal(root.path());
        journal.final_publication = Some(FinalPublication {
            schema: 1,
            session_id: "fixture".into(),
            plan_sha256: plan_hash(&journal).unwrap(),
            bytes: 9,
            sha256: format!("{:x}", Sha256::digest(b"right-123")),
            media: inspect_probe(probe(), &journal).unwrap(),
        });
        let error = StableMedia::open(&path)
            .unwrap()
            .verify_publication(&journal)
            .unwrap_err();
        assert!(format!("{error:#}").contains("fingerprint"));
        assert_eq!(fs::read(path).unwrap(), b"wrong-123");
    }

    #[test]
    fn legacy_segment_journals_remain_readable_without_an_adoption_receipt() {
        let root = tempfile::tempdir().unwrap();
        let mut journal = journal(root.path());
        journal.schema = 1;
        assert!(journal.validate(&journal.request.session_dir).is_ok());
        assert!(journal.final_publication.is_none());
    }

    #[test]
    fn final_input_excludes_replacement_until_validation_and_publication_finish() {
        use std::{
            sync::{Arc, Barrier},
            thread,
        };
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("recording.mp4");
        let replacement = root.path().join("replacement.mp4");
        fs::write(&path, b"task-owned media").unwrap();
        fs::write(&replacement, b"replacement").unwrap();
        let opened = Arc::new(Barrier::new(2));
        let finish = Arc::new(Barrier::new(2));
        let held_path = path.clone();
        let opened_worker = opened.clone();
        let finish_worker = finish.clone();
        let worker = thread::spawn(move || {
            let mut input = StableMedia::open(&held_path).unwrap();
            opened_worker.wait();
            finish_worker.wait();
            assert_eq!(
                input.fingerprint(None).unwrap().1,
                format!("{:x}", Sha256::digest(b"task-owned media"))
            );
            input
        });
        opened.wait();
        assert!(fs::write(&path, b"changed").is_err());
        assert!(fs::rename(&replacement, &path).is_err());
        finish.wait();
        let held_until_completed = worker.join().unwrap();
        assert!(fs::rename(&replacement, &path).is_err());
        drop(held_until_completed);
        fs::rename(&replacement, &path).unwrap();
    }

    #[test]
    fn final_publication_never_overwrites_a_competing_final() {
        let root = tempfile::tempdir().unwrap();
        let staged = root.path().join("staged.mp4");
        let final_path = root.path().join("recording.mp4");
        fs::write(&staged, b"intended").unwrap();
        fs::write(&final_path, b"unrelated").unwrap();
        assert!(publish_new_file(&staged, &final_path).is_err());
        assert_eq!(fs::read(&staged).unwrap(), b"intended");
        assert_eq!(fs::read(&final_path).unwrap(), b"unrelated");
    }

    #[test]
    fn long_path_publication_succeeds_without_overwriting_existing_media() {
        use std::os::windows::ffi::OsStrExt;
        let root = tempfile::tempdir().unwrap();
        let mut directory = root.path().to_owned();
        for index in 0..4 {
            directory = directory.join(format!("segment-fixture-{index}-{}", "x".repeat(60)));
        }
        fs::create_dir_all(&directory).unwrap();
        let staged = directory.join("recording.partial.mp4");
        let final_path = directory.join("recording.mp4");
        assert!(staged.as_os_str().encode_wide().count() > 260);
        fs::write(&staged, b"intended media").unwrap();
        publish_new_file(&staged, &final_path).unwrap();
        assert!(!staged.exists());
        assert_eq!(fs::read(&final_path).unwrap(), b"intended media");
        fs::write(&staged, b"competing media").unwrap();
        assert!(publish_new_file(&staged, &final_path).is_err());
        assert_eq!(fs::read(&final_path).unwrap(), b"intended media");
        assert_eq!(fs::read(&staged).unwrap(), b"competing media");
    }
}
