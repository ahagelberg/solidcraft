//! Project folders: a folder that holds designs is a project, and may carry its own description
//! and picture. They live in a hidden `.solidcraft-project` folder inside it:
//!
//! - `project.json`: `{"format":"solidcraft-project/1","description":"…"}`
//! - `thumbnail.png`: the custom thumbnail, at most 480x300.
//!
//! The folder's own name is the project's name; there is no name in the file. Missing,
//! unreadable or malformed metadata is not an error: a project without it simply has no
//! description or custom picture.

use std::io::{Cursor, Error, ErrorKind, Result};
use std::path::{Path, PathBuf};

use image::imageops::FilterType;
use serde::{Deserialize, Serialize};

/// The folder inside a project that holds its metadata (dot-prefixed: the file browser skips it).
pub const PROJECT_DIR: &str = ".solidcraft-project";
/// The metadata file's format tag.
pub const FORMAT: &str = "solidcraft-project/1";
/// Longest description kept.
pub const MAX_DESCRIPTION: usize = 2000;
/// Largest `project.json` read.
pub const MAX_META_BYTES: u64 = 64 << 10;
/// The custom thumbnail's largest size (it is scaled to fit).
pub const THUMBNAIL_SIZE: [u32; 2] = [480, 300];
/// Largest thumbnail file kept.
pub const MAX_THUMBNAIL_BYTES: usize = 256 << 10;
/// Largest image accepted as a custom thumbnail.
pub const MAX_SOURCE_BYTES: u64 = 20 << 20;
/// Largest source image side.
pub const MAX_SOURCE_SIDE: u32 = 8192;

/// A project's own metadata.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectMeta {
    pub description: String,
}

/// What `project.json` holds. The format tag is checked on read, so a future version can change
/// the shape without older builds misreading it.
#[derive(Deserialize, Serialize)]
struct Stored {
    format: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    description: String,
}

/// The project's metadata folder.
pub fn dir(project: &Path) -> PathBuf {
    project.join(PROJECT_DIR)
}

/// The project's metadata file.
pub fn meta_path(project: &Path) -> PathBuf {
    dir(project).join("project.json")
}

/// The project's custom thumbnail.
pub fn thumbnail_path(project: &Path) -> PathBuf {
    dir(project).join("thumbnail.png")
}

/// The project's metadata. Missing, unreadable or malformed files read as empty: this is used
/// while listing folders, where one bad file must not hide the project.
pub fn read(project: &Path) -> ProjectMeta {
    let path = meta_path(project);
    let Ok(meta) = std::fs::metadata(&path) else { return ProjectMeta::default() };
    if !meta.is_file() || meta.len() > MAX_META_BYTES {
        return ProjectMeta::default();
    }
    let Ok(bytes) = std::fs::read(&path) else { return ProjectMeta::default() };
    let Ok(stored) = serde_json::from_slice::<Stored>(&bytes) else { return ProjectMeta::default() };
    if stored.format != FORMAT {
        return ProjectMeta::default();
    }
    ProjectMeta { description: stored.description.chars().take(MAX_DESCRIPTION).collect() }
}

/// Whether the project has a usable custom thumbnail.
pub fn has_thumbnail(project: &Path) -> bool {
    std::fs::metadata(thumbnail_path(project)).is_ok_and(|m| m.is_file() && m.len() > 0 && m.len() <= MAX_THUMBNAIL_BYTES as u64)
}

/// Write the description (trimmed and bounded), creating the metadata folder.
pub fn write_description(project: &Path, description: &str) -> Result<()> {
    let description: String = description.trim().chars().take(MAX_DESCRIPTION).collect();
    let bytes = serde_json::to_vec_pretty(&Stored { format: FORMAT.into(), description }).map_err(|e| Error::new(ErrorKind::InvalidData, e))?;
    std::fs::create_dir_all(dir(project))?;
    crate::write_atomic(&meta_path(project), &bytes, false)
}

/// Copy `source` into the project as its custom thumbnail: decoded with the same limits the
/// design thumbnails use, scaled down to fit [`THUMBNAIL_SIZE`] (never up), re-encoded as PNG.
pub fn write_thumbnail(project: &Path, source: &Path) -> Result<()> {
    let meta = std::fs::metadata(source)?;
    if !meta.is_file() {
        return Err(Error::new(ErrorKind::InvalidInput, "the chosen image is not a file"));
    }
    if meta.len() > MAX_SOURCE_BYTES {
        return Err(Error::new(ErrorKind::InvalidData, "the image is larger than 20 MB"));
    }
    let bytes = std::fs::read(source)?;
    let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format().map_err(invalid)?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_SIDE);
    limits.max_image_height = Some(MAX_SOURCE_SIDE);
    limits.max_alloc = Some(u64::from(MAX_SOURCE_SIDE) * u64::from(MAX_SOURCE_SIDE) * 4);
    reader.limits(limits);
    let image = reader.decode().map_err(invalid)?;
    let (w, h) = (image.width(), image.height());
    if w == 0 || h == 0 {
        return Err(Error::new(ErrorKind::InvalidData, "the image has no pixels"));
    }
    let scale = (f64::from(THUMBNAIL_SIZE[0]) / f64::from(w)).min(f64::from(THUMBNAIL_SIZE[1]) / f64::from(h)).min(1.0);
    let tw = ((f64::from(w) * scale).round() as u32).max(1);
    let th = ((f64::from(h) * scale).round() as u32).max(1);
    let rgba = image.to_rgba8();
    let scaled = if (tw, th) == (w, h) { rgba } else { image::imageops::resize(&rgba, tw, th, FilterType::Lanczos3) };
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(scaled).write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png).map_err(invalid)?;
    if out.len() > MAX_THUMBNAIL_BYTES {
        return Err(Error::new(ErrorKind::InvalidData, "the thumbnail is larger than 256 KB"));
    }
    std::fs::create_dir_all(dir(project))?;
    crate::write_atomic(&thumbnail_path(project), &out, false)
}

/// Remove the custom thumbnail. The metadata folder goes too when nothing else is in it.
pub fn remove_thumbnail(project: &Path) -> Result<()> {
    let path = thumbnail_path(project);
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let dir = dir(project);
    let empty = std::fs::read_dir(&dir).is_ok_and(|mut entries| entries.next().is_none());
    if empty {
        let _ = std::fs::remove_dir(&dir);
    }
    Ok(())
}

fn invalid(e: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::InvalidData, e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("solidcraft-project-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn description_round_trips_and_missing_metadata_is_empty() {
        let d = temp("description");
        assert_eq!(read(&d), ProjectMeta::default());
        write_description(&d, "  Parts for the rover  ").unwrap();
        assert_eq!(read(&d).description, "Parts for the rover");
        let text = std::fs::read_to_string(meta_path(&d)).unwrap();
        assert!(text.contains("\"format\": \"solidcraft-project/1\""));
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn a_broken_or_unknown_metadata_file_reads_as_empty() {
        let d = temp("broken");
        std::fs::create_dir_all(dir(&d)).unwrap();
        std::fs::write(meta_path(&d), b"not json").unwrap();
        assert_eq!(read(&d), ProjectMeta::default());
        std::fs::write(meta_path(&d), br#"{"format":"solidcraft-project/99","description":"new"}"#).unwrap();
        assert_eq!(read(&d), ProjectMeta::default());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn thumbnail_is_scaled_and_reencoded_as_png() {
        let d = temp("thumbnail");
        let source = d.join("source.png");
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(1000, 400, image::Rgba([10, 20, 30, 255])))
            .save_with_format(&source, image::ImageFormat::Png)
            .unwrap();
        write_thumbnail(&d, &source).unwrap();
        assert!(has_thumbnail(&d));
        let image = image::open(thumbnail_path(&d)).unwrap();
        assert!(image.width() <= THUMBNAIL_SIZE[0] && image.height() <= THUMBNAIL_SIZE[1]);
        assert_eq!(image.width(), 480);
        assert_eq!(image.height(), 192);
        remove_thumbnail(&d).unwrap();
        assert!(!has_thumbnail(&d));
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn a_file_that_is_not_an_image_is_refused() {
        let d = temp("not-an-image");
        let source = d.join("notes.txt");
        std::fs::write(&source, b"hello").unwrap();
        assert!(write_thumbnail(&d, &source).is_err());
        std::fs::remove_dir_all(d).unwrap();
    }
}
