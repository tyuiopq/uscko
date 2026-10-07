use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use image::{ImageFormat, ImageReader, codecs::jpeg::JpegEncoder};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempGuard {
    path: PathBuf,
    committed: bool,
}

impl TempGuard {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            committed: false,
        }
    }

    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn decode(source: &Path) -> Result<image::DynamicImage, String> {
    ImageReader::open(source)
        .map_err(|error| format!("Could not open source image: {error}"))?
        .with_guessed_format()
        .map_err(|error| format!("Could not detect image format: {error}"))?
        .decode()
        .map_err(|error| format!("Could not decode image: {error}"))
}

fn available_output_path(source: &Path, extension: &str) -> Result<PathBuf, String> {
    let parent = source.parent().ok_or("Source has no parent directory")?;

    let stem = source.file_stem().ok_or("Source has no filename")?;

    for number in 0..100_000u32 {
        let mut name = OsString::from(stem);

        if number > 0 {
            name.push(format!(" ({number})"));
        }

        name.push(".");
        name.push(extension);

        let candidate = parent.join(name);

        if !candidate.exists() {
            return Ok(candidate);
        }
    }

    Err("Could not find available output filename".to_string())
}

fn create_temp_file(parent: &Path) -> Result<(PathBuf, File), String> {
    let pid = std::process::id();

    for _ in 0..100 {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);

        let path = parent.join(format!(".filehalo-{pid}-{id}.tmp"));

        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => {
                return Ok((path, file));
            }

            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                continue;
            }

            Err(error) => {
                return Err(format!("Could not create temporary file: {error}"));
            }
        }
    }

    Err("Could not create temporary filename".to_string())
}

fn finalize_temp(temp_path: &Path, output: &Path, guard: &mut TempGuard) -> Result<(), String> {
    fs::rename(temp_path, output)
        .map_err(|error| format!("Could not finalize output file: {error}"))?;

    guard.commit();

    Ok(())
}

pub fn to_png(source: &Path) -> Result<PathBuf, String> {
    let image = decode(source)?;

    let output = available_output_path(source, "png")?;

    let parent = output.parent().ok_or("Invalid output directory")?;

    let (temp_path, temp_file) = create_temp_file(parent)?;

    let mut guard = TempGuard::new(temp_path.clone());

    {
        let mut writer = BufWriter::new(temp_file);

        image
            .write_to(&mut writer, ImageFormat::Png)
            .map_err(|error| format!("PNG encoding failed: {error}"))?;

        writer
            .flush()
            .map_err(|error| format!("Could not flush PNG: {error}"))?;

        writer
            .get_ref()
            .sync_all()
            .map_err(|error| format!("Could not sync PNG: {error}"))?;
    }

    finalize_temp(&temp_path, &output, &mut guard)?;

    Ok(output)
}

pub fn to_jpeg(source: &Path) -> Result<PathBuf, String> {
    let image = decode(source)?;

    let output = available_output_path(source, "jpg")?;

    let parent = output.parent().ok_or("Invalid output directory")?;

    let (temp_path, temp_file) = create_temp_file(parent)?;

    let mut guard = TempGuard::new(temp_path.clone());

    {
        let mut writer = BufWriter::new(temp_file);

        let mut encoder = JpegEncoder::new_with_quality(&mut writer, 92);

        encoder
            .encode_image(&image)
            .map_err(|error| format!("JPEG encoding failed: {error}"))?;

        writer
            .flush()
            .map_err(|error| format!("Could not flush JPEG: {error}"))?;

        writer
            .get_ref()
            .sync_all()
            .map_err(|error| format!("Could not sync JPEG: {error}"))?;
    }

    finalize_temp(&temp_path, &output, &mut guard)?;

    Ok(output)
}

pub fn to_webp(source: &Path) -> Result<PathBuf, String> {
    let image = decode(source)?;

    let output = available_output_path(source, "webp")?;

    let parent = output.parent().ok_or("Invalid output directory")?;

    let (temp_path, temp_file) = create_temp_file(parent)?;

    let mut guard = TempGuard::new(temp_path.clone());

    {
        let mut writer = BufWriter::new(temp_file);

        image
            .write_to(&mut writer, ImageFormat::WebP)
            .map_err(|error| format!("WebP encoding failed: {error}"))?;

        writer
            .flush()
            .map_err(|error| format!("Could not flush WebP: {error}"))?;

        writer
            .get_ref()
            .sync_all()
            .map_err(|error| format!("Could not sync WebP: {error}"))?;
    }

    finalize_temp(&temp_path, &output, &mut guard)?;

    Ok(output)
}
