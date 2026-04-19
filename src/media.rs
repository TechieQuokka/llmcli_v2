use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use std::path::Path;

pub struct ImageData {
    pub base64: String,
}

pub struct AudioData {
    pub base64: String,
    pub mime: &'static str,
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

/// Detect audio MIME type from magic bytes.
fn detect_audio_mime(bytes: &[u8]) -> Option<&'static str> {
    match bytes {
        [0xFF, 0xFB, ..] | [0xFF, 0xF3, ..] | [0xFF, 0xF2, ..]
        | [0x49, 0x44, 0x33, ..]                                        => Some("audio/mpeg"),
        [0x52, 0x49, 0x46, 0x46, _, _, _, _, 0x57, 0x41, 0x56, 0x45, ..] => Some("audio/wav"),
        [0x4F, 0x67, 0x67, 0x53, ..]                                   => Some("audio/ogg"),
        [0x66, 0x4C, 0x61, 0x43, ..]                                   => Some("audio/flac"),
        // MP4/M4A ftyp box
        [_, _, _, _, 0x66, 0x74, 0x79, 0x70, ..]                      => Some("audio/mp4"),
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

/// Read an audio file and return base64-encoded bytes + MIME type.
/// Supports: mp3, wav, ogg, flac, m4a, aac, opus, aiff, wma, amr, mp4
/// Falls back to magic byte detection when the extension is absent or unrecognised.
pub fn load_audio(path: &str) -> Result<AudioData> {
    let p = Path::new(path);
    let ext = p.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let bytes = std::fs::read(p).with_context(|| format!("Cannot read audio: {path}"))?;

    let mime: &'static str = match ext.as_str() {
        "mp3" | "mp2"          => "audio/mpeg",
        "wav" | "wave"         => "audio/wav",
        "ogg" | "oga"          => "audio/ogg",
        "flac"                 => "audio/flac",
        "m4a" | "mp4" | "m4b" => "audio/mp4",
        "aac"                  => "audio/aac",
        "opus"                 => "audio/opus",
        "aiff" | "aif" | "aifc" => "audio/aiff",
        "wma"                  => "audio/x-ms-wma",
        "amr"                  => "audio/amr",
        "weba"                 => "audio/webm",
        "mid" | "midi"         => "audio/midi",
        _ => {
            // Extension missing or unknown — try magic bytes
            detect_audio_mime(&bytes).ok_or_else(|| {
                anyhow::anyhow!(
                    "Unsupported or unrecognised audio format: '{path}'\n\
                     Supported: mp3, wav, ogg, flac, m4a, aac, opus, aiff, wma, amr, weba, midi"
                )
            })?
        }
    };

    Ok(AudioData {
        base64: STANDARD.encode(&bytes),
        mime,
    })
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
