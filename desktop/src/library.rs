use crate::model::{default_styles, validate_size, Document, QuickStyle, SCHEMA};
use anyhow::{ensure, Context, Result};
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, RgbaImage};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

const JSON_BUDGET: u64 = 8 * 1024 * 1024;
const LIBRARY_BUDGET: usize = 5000;
pub const THUMBNAIL_WIDTH: u32 = 240;
pub const THUMBNAIL_HEIGHT: u32 = 150;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn id() -> String {
    let ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "{ns:020}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub struct StagedFile {
    temporary: Option<PathBuf>,
    destination: PathBuf,
}

impl StagedFile {
    fn create(path: &Path) -> Result<(Self, File)> {
        let parent = path.parent().context("Output has no parent directory")?;
        fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(".{}.tmp", id()));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        Ok((
            Self {
                temporary: Some(temporary),
                destination: path.to_owned(),
            },
            file,
        ))
    }

    pub fn publish(mut self) -> Result<()> {
        let temporary = self
            .temporary
            .as_ref()
            .context("Staged output is already published")?;
        fs::rename(temporary, &self.destination).with_context(|| {
            format!(
                "Publishing {}; prior file was not deleted",
                self.destination.display()
            )
        })?;
        self.temporary = None;
        Ok(())
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        if let Some(path) = self.temporary.take() {
            if let Err(error) = fs::remove_file(&path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    eprintln!(
                        "Could not remove unpublished staging file {}: {error}",
                        path.display()
                    );
                }
            }
        }
    }
}

pub fn stage_bytes(path: &Path, bytes: &[u8]) -> Result<StagedFile> {
    let (staged, mut file) = StagedFile::create(path)?;
    file.write_all(bytes)
        .with_context(|| format!("Staging {}", path.display()))?;
    file.sync_all()
        .context("Flushing new data before publication")?;
    drop(file);
    Ok(staged)
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    stage_bytes(path, bytes)?.publish()
}

pub fn stage_json(path: &Path, value: &impl Serialize) -> Result<StagedFile> {
    let bytes = serde_json::to_vec_pretty(value)?;
    ensure!(
        bytes.len() as u64 <= JSON_BUDGET,
        "Metadata exceeds the 8 MiB budget."
    );
    stage_bytes(path, &bytes)
}

pub fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    stage_json(path, value)?.publish()
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let file = File::open(path)?;
    ensure!(
        file.metadata()?.len() <= JSON_BUDGET,
        "Metadata file exceeds the 8 MiB budget."
    );
    let mut bytes = Vec::new();
    file.take(JSON_BUDGET + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= JSON_BUDGET,
        "Metadata grew beyond its read budget."
    );
    serde_json::from_slice(&bytes).with_context(|| format!("Reading {}", path.display()))
}

pub fn exclusive_lock(path: &Path) -> Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    OpenOptions::new().create(true).truncate(false).read(true).write(true)
        .share_mode(0).open(path).with_context(||format!(
            "Cannot acquire exclusive local ownership at {}. Another app/session may still own it; no existing data was changed.",
            path.display()))
}

pub fn load_image(path: &Path) -> Result<RgbaImage> {
    decode_image(
        path,
        crate::model::MAX_DIMENSION,
        crate::model::MAX_DIMENSION,
        crate::model::MAX_PIXELS,
        128 * 1024 * 1024,
    )
}

pub fn load_thumbnail(path: &Path) -> Result<RgbaImage> {
    decode_image(
        path,
        THUMBNAIL_WIDTH,
        THUMBNAIL_HEIGHT,
        u64::from(THUMBNAIL_WIDTH) * u64::from(THUMBNAIL_HEIGHT),
        1024 * 1024,
    )
    .context("Reading a bounded library thumbnail")
}

fn decode_image(
    path: &Path,
    max_width: u32,
    max_height: u32,
    max_pixels: u64,
    max_file_bytes: u64,
) -> Result<RgbaImage> {
    let file = File::open(path)?;
    ensure!(
        file.metadata()?.len() <= max_file_bytes,
        "Compressed image exceeds its {}-byte file budget.",
        max_file_bytes
    );
    let mut reader = ImageReader::new(BufReader::new(file)).with_guessed_format()?;
    ensure!(
        matches!(reader.format(), Some(ImageFormat::Png | ImageFormat::Jpeg)),
        "Import supports PNG and JPEG images."
    );
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(max_width);
    limits.max_image_height = Some(max_height);
    limits.max_alloc = Some(max_pixels * 8);
    reader.limits(limits);
    let decoder = reader.into_decoder()?;
    let (width, height) = decoder.dimensions();
    ensure!(
        width > 0
            && height > 0
            && width <= max_width
            && height <= max_height
            && u64::from(width) * u64::from(height) <= max_pixels,
        "Image dimensions exceed this surface's pixel budget."
    );
    ensure!(
        decoder.total_bytes() <= max_pixels * 8,
        "Decoded image exceeds this surface's allocation budget."
    );
    Ok(DynamicImage::from_decoder(decoder)?.into_rgba8())
}

pub fn export_image(path: &Path, image: &RgbaImage) -> Result<()> {
    stage_image(path, image)?.publish()
}

pub fn stage_image(path: &Path, image: &RgbaImage) -> Result<StagedFile> {
    validate_size(image.width(), image.height())?;
    let extension = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    ensure!(
        matches!(extension.as_str(), "png" | "jpg" | "jpeg"),
        "Choose a .png or .jpg filename."
    );
    let (staged, file) = StagedFile::create(path)?;
    let mut writer = BufWriter::new(file);
    if extension == "png" {
        DynamicImage::ImageRgba8(image.clone()).write_to(&mut writer, ImageFormat::Png)?;
    } else {
        let mut opaque = image::RgbImage::new(image.width(), image.height());
        for (src, dst) in image.pixels().zip(opaque.pixels_mut()) {
            let a = u32::from(src[3]);
            for c in 0..3 {
                dst[c] = ((u32::from(src[c]) * a + 255 * (255 - a) + 127) / 255) as u8;
            }
        }
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut writer, 95)
            .encode_image(&DynamicImage::ImageRgb8(opaque))?;
    }
    writer.flush()?;
    writer.get_ref().sync_all()?;
    drop(writer);
    Ok(staged)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum CaptureKind {
    Image,
    Video,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Capture {
    pub schema: u32,
    pub id: String,
    pub title: String,
    pub kind: CaptureKind,
    pub created_ms: u64,
    pub width: u32,
    pub height: u32,
    pub tags: Vec<String>,
}

impl Capture {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == SCHEMA,
            "Unsupported capture metadata version."
        );
        ensure!(
            !self.id.is_empty()
                && self.id.len() <= 100
                && self
                    .id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-'),
            "Invalid capture ID."
        );
        ensure!(self.title.len() <= 400, "Capture title is too long.");
        ensure!(
            self.tags.len() <= 32 && self.tags.iter().all(|t| t.len() <= 80),
            "Tag budget exceeded."
        );
        validate_size(self.width, self.height)
    }
}

#[derive(Clone)]
pub struct Library {
    pub root: PathBuf,
}

impl Library {
    pub fn open(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(root.join("captures")).context("Creating local capture library")?;
        fs::create_dir_all(root.join("sessions"))?;
        Ok(Self { root })
    }
    pub fn default_root() -> Result<PathBuf> {
        Ok(
            PathBuf::from(std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is unavailable")?)
                .join("SteadyCapture")
                .join("Library"),
        )
    }
    pub fn capture_dir(&self, capture: &Capture) -> Result<PathBuf> {
        capture.validate()?;
        Ok(self.root.join("captures").join(&capture.id))
    }
    pub fn list(&self) -> Result<(Vec<Capture>, Vec<String>)> {
        let mut captures = Vec::new();
        let mut warnings = Vec::new();
        for (index, entry) in fs::read_dir(self.root.join("captures"))?.enumerate() {
            ensure!(
                index < LIBRARY_BUDGET,
                "Library exceeds 5,000 captures. Move an older capture folder out of this library."
            );
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let path = entry.path().join("capture.json");
            if !path.exists() {
                let intent = entry.path().join("import.json");
                if intent.exists() {
                    match self.recover_image_import(&entry.path()) {
                        Ok(capture) => {
                            warnings.push(format!(
                                "Recovered interrupted image import: {}",
                                capture.title
                            ));
                            captures.push(capture);
                        }
                        Err(error) => warnings.push(format!(
                            "Image import retained at {}: {error:#}",
                            entry.path().display()
                        )),
                    }
                } else {
                    warnings.push(format!(
                        "Incomplete capture retained: {}",
                        entry.file_name().to_string_lossy()
                    ));
                }
                continue;
            }
            match read_json::<Capture>(&path).and_then(|c| {
                c.validate()?;
                Ok(c)
            }) {
                Ok(c) if entry.file_name() == c.id.as_str() => captures.push(c),
                Ok(_) => warnings.push(format!(
                    "Capture ID does not match folder: {}",
                    path.display()
                )),
                Err(e) => warnings.push(format!("Unreadable capture: {e:#}")),
            }
        }
        captures.sort_by_key(|c| std::cmp::Reverse(c.created_ms));
        Ok((captures, warnings))
    }
    pub fn save_capture(&self, capture: &Capture) -> Result<()> {
        let dir = self.capture_dir(capture)?;
        write_json(&dir.join("capture.json"), capture)
    }
    pub fn import_image(&self, pixels: &RgbaImage, title: &str) -> Result<Capture> {
        validate_size(pixels.width(), pixels.height())?;
        let capture = Capture {
            schema: SCHEMA,
            id: id(),
            title: title.chars().take(100).collect(),
            kind: CaptureKind::Image,
            created_ms: now_ms(),
            width: pixels.width(),
            height: pixels.height(),
            tags: vec![],
        };
        let dir = self.capture_dir(&capture)?;
        fs::create_dir(&dir)?;
        let _ownership = exclusive_lock(&dir.join("import.lock"))?;
        write_json(&dir.join("import.json"), &capture)?;
        export_image(&dir.join("original.png"), pixels)?;
        self.save_document(&capture, &Document::new(pixels.width(), pixels.height())?)?;
        self.save_capture(&capture)?;
        let thumb = image::imageops::thumbnail(pixels, 240, 150);
        export_image(&dir.join("thumbnail.png"), &thumb)
            .context("The original and editable image are saved, but thumbnail generation failed. Open Library and Refresh to access the image")?;
        Ok(capture)
    }
    fn recover_image_import(&self, dir: &Path) -> Result<Capture> {
        let _ownership = exclusive_lock(&dir.join("import.lock"))?;
        let capture: Capture = read_json(&dir.join("import.json"))?;
        capture.validate()?;
        ensure!(
            capture.kind == CaptureKind::Image && self.capture_dir(&capture)? == dir,
            "Interrupted image intent has a mismatched capture identity."
        );
        let original = load_image(&dir.join("original.png"))
            .context("No complete original is recoverable yet; existing files were retained")?;
        ensure!(
            original.dimensions() == (capture.width, capture.height),
            "Original does not match the import intent."
        );
        if dir.join("document.json").exists() {
            self.load_document(&capture)?;
        } else {
            self.save_document(&capture, &Document::new(capture.width, capture.height)?)?;
        }
        self.save_capture(&capture)?;
        Ok(capture)
    }
    pub fn load_document(&self, capture: &Capture) -> Result<Document> {
        let d: Document = read_json(&self.capture_dir(capture)?.join("document.json"))?;
        d.validate()?;
        ensure!(
            (d.width, d.height) == (capture.width, capture.height),
            "Document size does not match its original."
        );
        Ok(d)
    }
    pub fn save_document(&self, capture: &Capture, document: &Document) -> Result<()> {
        document.validate()?;
        write_json(&self.capture_dir(capture)?.join("document.json"), document)
    }
    pub fn styles(&self) -> Result<Vec<QuickStyle>> {
        let path = self.root.join("styles.json");
        if !path.exists() {
            return Ok(default_styles());
        }
        let styles: Vec<QuickStyle> = read_json(&path)?;
        ensure!(styles.len() <= 128, "The 128-style budget was exceeded.");
        for s in &styles {
            s.validate()?;
        }
        Ok(styles)
    }
    pub fn save_styles(&self, styles: &[QuickStyle]) -> Result<()> {
        ensure!(styles.len() <= 128, "The 128-style budget was exceeded.");
        for s in styles {
            s.validate()?;
        }
        write_json(&self.root.join("styles.json"), &styles)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn image_document_and_style_persist() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = Library::open(tmp.path().to_owned()).unwrap();
        let pixels = RgbaImage::from_pixel(100, 50, image::Rgba([32, 100, 200, 255]));
        let mut c = lib.import_image(&pixels, "Fixture").unwrap();
        c.tags = vec!["work".into()];
        lib.save_capture(&c).unwrap();
        let d = lib.load_document(&c).unwrap();
        assert_eq!(d.width, 100);
        assert_eq!(lib.list().unwrap().0[0].tags, ["work"]);
        let mut styles = lib.styles().unwrap();
        styles[0].style.width = 13.0;
        styles[0].favorite = false;
        lib.save_styles(&styles).unwrap();
        assert_eq!(lib.styles().unwrap(), styles);
        assert_eq!(
            load_image(&lib.capture_dir(&c).unwrap().join("original.png")).unwrap(),
            pixels
        );
    }
    #[test]
    fn atomic_replace_preserves_valid_document() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a.json");
        write_json(&path, &vec![1, 2]).unwrap();
        write_json(&path, &vec![3, 4]).unwrap();
        assert_eq!(read_json::<Vec<u8>>(&path).unwrap(), [3, 4]);
    }
    #[test]
    fn failed_publication_retains_previous_document() {
        use std::os::windows::fs::OpenOptionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("document.json");
        write_json(&path, &vec![1, 2]).unwrap();
        let lock = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&path)
            .unwrap();
        assert!(write_json(&path, &vec![3, 4]).is_err());
        assert_eq!(read_json::<Vec<u8>>(&path).unwrap(), [1, 2]);
        drop(lock);
    }
    #[test]
    fn exclusive_ownership_prevents_concurrent_recovery_or_editors() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("owner.lock");
        let first = exclusive_lock(&path).unwrap();
        assert!(exclusive_lock(&path).is_err());
        drop(first);
        assert!(exclusive_lock(&path).is_ok());
    }
    #[test]
    fn capture_path_traversal_is_rejected() {
        let c = Capture {
            schema: SCHEMA,
            id: "..\\escape".into(),
            title: "x".into(),
            kind: CaptureKind::Image,
            created_ms: 0,
            width: 1,
            height: 1,
            tags: vec![],
        };
        assert!(c.validate().is_err());
    }
    #[test]
    fn thumbnail_header_limits_prevent_full_resolution_cache_allocations() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("thumbnail.png");
        export_image(&path, &RgbaImage::new(241, 150)).unwrap();
        assert!(load_thumbnail(&path).is_err());
        export_image(&path, &RgbaImage::new(240, 150)).unwrap();
        assert_eq!(load_thumbnail(&path).unwrap().dimensions(), (240, 150));
    }
    #[test]
    fn original_published_before_metadata_is_adopted_without_thumbnail() {
        let temporary = tempfile::tempdir().unwrap();
        let library = Library::open(temporary.path().to_owned()).unwrap();
        let original = RgbaImage::from_pixel(20, 15, image::Rgba([1, 2, 3, 255]));
        let capture = Capture {
            schema: 1,
            id: id(),
            title: "Interrupted".into(),
            kind: CaptureKind::Image,
            created_ms: now_ms(),
            width: 20,
            height: 15,
            tags: vec![],
        };
        let dir = library.capture_dir(&capture).unwrap();
        fs::create_dir(&dir).unwrap();
        write_json(&dir.join("import.json"), &capture).unwrap();
        export_image(&dir.join("original.png"), &original).unwrap();
        let (listed, warnings) = library.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, capture.id);
        assert!(warnings[0].contains("Recovered"));
        assert!(library.load_document(&capture).is_ok());
        assert_eq!(load_image(&dir.join("original.png")).unwrap(), original);
    }
    #[test]
    fn jpeg_flattens_alpha_on_white_with_loss_tolerance() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.jpg");
        let image = RgbaImage::from_pixel(20, 20, image::Rgba([255, 0, 0, 128]));
        export_image(&p, &image).unwrap();
        let loaded = load_image(&p).unwrap();
        let px = loaded.get_pixel(10, 10);
        for (actual, expected) in px.0.into_iter().zip([255i16, 127, 127, 255]) {
            assert!((i16::from(actual) - expected).abs() <= 4);
        }
    }
}
