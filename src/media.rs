use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use std::path::Path;

pub struct ImageData {
    pub base64: String,
}

pub struct AudioData {
    pub base64: String,
}

/// Detect a known image format from magic bytes.
fn detect_image_mime(bytes: &[u8]) -> Option<()> {
    match bytes {
        [0xFF, 0xD8, 0xFF, ..]                                            => Some(()),
        [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, ..]            => Some(()),
        [0x47, 0x49, 0x46, 0x38, ..]                                      => Some(()),
        [0x52, 0x49, 0x46, 0x46, _, _, _, _, 0x57, 0x45, 0x42, 0x50, ..] => Some(()),
        [0x42, 0x4D, ..]                                                  => Some(()),
        [0x49, 0x49, 0x2A, 0x00, ..] | [0x4D, 0x4D, 0x00, 0x2A, ..]     => Some(()),
        _ => None,
    }
}


/// Read an image file and return base64-encoded bytes.
/// Supports: jpeg, png, gif, webp, bmp, tiff, avif, heic, ico, svg
/// Falls back to magic byte detection when the extension is absent or unrecognised.
pub fn load_image(path: &str) -> Result<ImageData> {
    let p = Path::new(path);
    let ext = p.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let bytes = std::fs::read(p).with_context(|| format!("Cannot read image: {path}"))?;

    let known_ext = matches!(
        ext.as_str(),
        "jpg" | "jpeg" | "jfif" | "jpe"
        | "png" | "gif" | "webp"
        | "bmp" | "dib"
        | "tiff" | "tif"
        | "avif" | "heic" | "heif"
        | "ico" | "cur"
        | "svg" | "svgz"
    );

    if !known_ext && detect_image_mime(&bytes).is_none() {
        anyhow::bail!(
            "Unsupported or unrecognised image format: '{path}'\n\
             Supported: jpg/jpeg, png, gif, webp, bmp, tiff, avif, heic, ico, svg"
        );
    }

    Ok(ImageData { base64: STANDARD.encode(&bytes) })
}

/// Read an audio file and return base64-encoded WAV bytes (16 kHz mono).
/// Ollama detects audio via RIFF/WAVE magic bytes and requires 16 kHz mono WAV.
/// Uses ffmpeg to convert any supported format to the required spec.
pub fn load_audio(path: &str) -> Result<AudioData> {
    // Verify the file exists before invoking ffmpeg
    if !Path::new(path).exists() {
        anyhow::bail!("Cannot read audio: '{path}' — file not found");
    }

    // Convert to WAV 16 kHz mono in-memory via ffmpeg
    let output = std::process::Command::new("ffmpeg")
        .args([
            "-v", "error",
            "-i", path,
            "-ar", "16000",   // 16 kHz sample rate
            "-ac", "1",       // mono
            "-f", "wav",
            "pipe:1",         // write to stdout
        ])
        .output()
        .with_context(|| "ffmpeg not found — install ffmpeg to use audio input")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("ffmpeg conversion failed: {stderr}");
    }

    Ok(AudioData { base64: STANDARD.encode(&output.stdout) })
}

/// Read a text file (any extension) and return its contents as a String.
/// Warn if the file is large.
pub fn load_text_file(path: &str) -> Result<String> {
    let p = Path::new(path);
    let metadata = std::fs::metadata(p)
        .with_context(|| format!("Cannot stat file: {path}"))?;

    if metadata.len() > 200 * 1024 {
        eprintln!(
            "\x1b[33m[warn] File '{}' is {:.1} KB — may exceed context window.\x1b[0m",
            path,
            metadata.len() as f64 / 1024.0
        );
    }

    std::fs::read_to_string(p).with_context(|| format!("Cannot read file: {path}"))
}
