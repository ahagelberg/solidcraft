//! Project folders and the designs in them: browsing, creating, renaming, moving, copying and
//! deleting, and their metadata (description and custom thumbnail).
//!
//! These commands work on the file system the user pointed the Designs panel at. They never
//! touch the open design, and none is undoable: a file operation cannot be undone. Paths are
//! taken as given (absolute, no `..`) and never canonicalised, so a move or rename reports and
//! changes exactly what the panel showed.

use std::cmp::Ordering;
use std::path::{Component, Path, PathBuf};

use serde_json::{Value, json};

use super::CommandSpec;
use crate::params::{bad, bool_, str_};
use crate::{EngineError, Result, Session};

pub static COMMANDS: &[CommandSpec] = &[
    CommandSpec::new("designs.list", "List Designs", list_command)
        .icon("folder")
        .noundo()
        .doc_only()
        .params("path: an absolute folder -> {path, folders: [{name, path, description, has_thumbnail}], designs: [{name, path, modified, kind, versions}], truncated}; at most 2000 entries. `designs` lists the designs and the files SolidCraft can open (`kind`: `design`, `step` for STEP, IGES, FreeCAD and Rhino, `mesh` for 3MF, STL, OBJ and glTF, `kicad` for a board or project); `versions` is how many versions a design file holds (1 when it has none)"),
    CommandSpec::new("designs.folder_new", "New Folder", folder_new)
        .icon("new")
        .noundo()
        .doc_only()
        .params("path: the absolute folder to create; parents?: bool (create the folders above it too)"),
    CommandSpec::new("designs.rename", "Rename", rename)
        .icon("rename")
        .noundo()
        .doc_only()
        .params("path: a design file or a folder; name: the new name (a design keeps its extension when the name has none) -> {path, old_path}"),
    CommandSpec::new("designs.move", "Move", move_path)
        .icon("move")
        .noundo()
        .doc_only()
        .params("path: a design file or a folder; destination: an existing absolute folder -> {path, old_path}"),
    CommandSpec::new("designs.copy", "Copy To", copy)
        .icon("copy")
        .noundo()
        .doc_only()
        .params("path: a design file or an importable file; destination: an existing absolute folder -> {path, old_path}. A file of that name there is never replaced"),
    CommandSpec::new("designs.duplicate", "Duplicate", duplicate)
        .icon("copy")
        .noundo()
        .doc_only()
        .params("path: a design file or an importable file -> {path, old_path}. The copy is `<name> copy` in the same folder (`copy 2`, `copy 3`, … when that is taken)"),
    CommandSpec::new("designs.delete", "Delete Design", delete)
        .icon("delete")
        .noundo()
        .doc_only()
        .params("path: a design file, or a folder that holds nothing but its metadata -> {deleted}"),
    CommandSpec::new("designs.project_get", "Project Properties", project_get)
        .icon("list")
        .noundo()
        .doc_only()
        .params("path: an absolute project folder -> {description, has_thumbnail}"),
    CommandSpec::new("designs.project_set", "Set Project Properties", project_set)
        .icon("list")
        .noundo()
        .doc_only()
        .params("path: an absolute project folder; description?: text (at most 2000 characters); thumbnail_source?: an image file to copy in (PNG/JPEG/WebP, at most 20 MB); remove_thumbnail?: bool (applied before thumbnail_source) -> {description, has_thumbnail}"),
    CommandSpec::new("designs.version_promote", "Promote Version", version_promote)
        .icon("checkpoint")
        .noundo()
        .doc_only()
        .params("path: a design file; version: the version to keep as the latest -> {path, version, versions}. The versions after it are deleted; the ones before it stay"),
    CommandSpec::new("designs.version_purge", "Purge Older Versions", version_purge)
        .icon("delete")
        .noundo()
        .doc_only()
        .params("path: a design file; version: the oldest version to keep -> {path, version, versions}. The versions before it are deleted; the ones kept renumber from 1"),
];

/// The `designs.list` answer for a folder, also used by the Designs panel on a worker thread
/// (no `Session` is involved).
pub fn list(path: &str) -> Result<Value> {
    if cfg!(target_arch = "wasm32") {
        return Err(unsupported());
    }
    list_at(&existing_dir(path, "designs.list")?)
}

fn list_command(_s: &mut Session, p: &Value) -> Result<Value> {
    let path = str_(p, "path").ok_or_else(|| bad("designs.list", "`path` is required"))?;
    list(path)
}

/// At most this many entries are listed; `truncated` says more were there.
const LIST_LIMIT: usize = 2000;

fn list_at(root: &Path) -> Result<Value> {
    let entries = std::fs::read_dir(root).map_err(|e| bad("designs.list", format!("{}: {e}", root.display())))?;
    let mut folders = Vec::new();
    let mut designs = Vec::new();
    let mut truncated = false;
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let Some(name) = entry.file_name().to_str().map(str::to_string) else { continue };
        if name.starts_with('.') {
            continue;
        }
        if folders.len() + designs.len() >= LIST_LIMIT {
            truncated = true;
            break;
        }
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            let project = solidcraft_io::project::read(&path);
            folders.push(json!({
                "name": name,
                "path": path.to_string_lossy(),
                "description": project.description,
                "has_thumbnail": solidcraft_io::project::has_thumbnail(&path),
            }));
        } else if meta.is_file()
            && let Some(kind) = file_kind(path.to_string_lossy().as_ref())
            && !project_beside_its_board(&path)
        {
            let modified = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(&name).to_string();
            // A design's versions (the directory only: no payload is read).
            let versions = if kind == "design" { solidcraft_io::container_versions_file(&path).map(|v| v.current).unwrap_or(1) } else { 1 };
            designs.push(json!({ "name": stem, "path": path.to_string_lossy(), "modified": modified, "kind": kind, "versions": versions }));
        }
    }
    folders.sort_by(|a, b| natural_cmp(a["name"].as_str().unwrap_or(""), b["name"].as_str().unwrap_or("")));
    designs.sort_by(|a, b| natural_cmp(a["name"].as_str().unwrap_or(""), b["name"].as_str().unwrap_or("")));
    Ok(json!({ "path": root.to_string_lossy(), "folders": folders, "designs": designs, "truncated": truncated }))
}

fn folder_new(_s: &mut Session, p: &Value) -> Result<Value> {
    if cfg!(target_arch = "wasm32") {
        return Err(unsupported());
    }
    let cmd = "designs.folder_new";
    let path = given_path(p, cmd)?;
    let parents = bool_(p, "parents").unwrap_or(false);
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return Err(bad(cmd, "`path` must end in a folder name"));
    };
    checked_name(name, cmd)?;
    if std::fs::symlink_metadata(&path).is_ok() {
        return Err(bad(cmd, "that folder already exists"));
    }
    if parents {
        std::fs::create_dir_all(&path).map_err(|e| bad(cmd, format!("{}: {e}", path.display())))?;
    } else {
        std::fs::create_dir(&path).map_err(|e| bad(cmd, format!("{}: {e}", path.display())))?;
    }
    Ok(json!({ "path": path.to_string_lossy() }))
}

fn rename(_s: &mut Session, p: &Value) -> Result<Value> {
    if cfg!(target_arch = "wasm32") {
        return Err(unsupported());
    }
    let cmd = "designs.rename";
    let path = given_path(p, cmd)?;
    let meta = std::fs::symlink_metadata(&path).map_err(|e| bad(cmd, format!("{}: {e}", path.display())))?;
    let parent = path.parent().ok_or_else(|| bad(cmd, "the path has no folder to rename it in"))?;
    let Some(old_name) = path.file_name().and_then(|n| n.to_str()) else {
        return Err(bad(cmd, "the path has no name to rename"));
    };
    let mut name = checked_name(str_(p, "name").unwrap_or_default(), cmd)?;
    if meta.is_file() {
        let text = path.to_string_lossy();
        if file_kind(text.as_ref()).is_none() {
            return Err(bad(cmd, "only designs and the files SolidCraft can import can be renamed here"));
        }
        // The name typed is the stem: a design keeps its `kind`'s extension.
        let extension = path.extension().and_then(|e| e.to_str()).unwrap_or(solidcraft_io::DESIGN_EXT);
        if !name_has_design_extension(&name) && !name.ends_with(&format!(".{extension}")) {
            name = format!("{name}.{extension}");
        }
    } else if !meta.is_dir() {
        return Err(bad(cmd, "only designs, importable files and folders can be renamed"));
    }
    let target = parent.join(&name);
    if target == path {
        return Ok(json!({ "path": path.to_string_lossy(), "old_path": path.to_string_lossy() }));
    }
    let case_only = old_name.eq_ignore_ascii_case(&name);
    if std::fs::symlink_metadata(&target).is_ok() {
        // The same file reached through a case-insensitive file system: rename through a
        // temporary name so the operation does not replace itself. On a case-sensitive file
        // system this is a different entry and the rename is refused.
        let same = std::fs::canonicalize(&path).ok().zip(std::fs::canonicalize(&target).ok()).is_some_and(|(a, b)| a == b);
        if !(case_only && same) {
            return Err(bad(cmd, "a file or folder with that name already exists"));
        }
        let temp = parent.join(format!(".solidcraft-rename-{}", std::process::id()));
        std::fs::rename(&path, &temp).map_err(|e| bad(cmd, format!("{}: {e}", path.display())))?;
        std::fs::rename(&temp, &target).map_err(|e| bad(cmd, format!("{}: {e}", target.display())))?;
    } else {
        std::fs::rename(&path, &target).map_err(|e| bad(cmd, format!("{}: {e}", path.display())))?;
    }
    Ok(json!({ "path": target.to_string_lossy(), "old_path": path.to_string_lossy() }))
}

fn move_path(_s: &mut Session, p: &Value) -> Result<Value> {
    if cfg!(target_arch = "wasm32") {
        return Err(unsupported());
    }
    let cmd = "designs.move";
    let path = given_path(p, cmd)?;
    let destination = existing_dir(str_(p, "destination").ok_or_else(|| bad(cmd, "`destination` is required"))?, cmd)?;
    let meta = std::fs::symlink_metadata(&path).map_err(|e| bad(cmd, format!("{}: {e}", path.display())))?;
    let Some(name) = path.file_name() else {
        return Err(bad(cmd, "the path has no name to move"));
    };
    let target = destination.join(name);
    if target == path {
        return Ok(json!({ "path": path.to_string_lossy(), "old_path": path.to_string_lossy() }));
    }
    if meta.is_dir() && is_inside(&destination, &path) {
        return Err(bad(cmd, "a folder cannot be moved into itself"));
    }
    if std::fs::symlink_metadata(&target).is_ok() {
        return Err(bad(cmd, "a file or folder with that name already exists there"));
    }
    std::fs::rename(&path, &target).map_err(|e| bad(cmd, format!("{}: {e}", path.display())))?;
    Ok(json!({ "path": target.to_string_lossy(), "old_path": path.to_string_lossy() }))
}

/// The file a copy works on: a design or a file SolidCraft can open, never a folder (a folder
/// carries its own sidecar, and is moved instead).
fn copy_source(p: &Value, cmd: &str) -> Result<PathBuf> {
    let path = given_path(p, cmd)?;
    let meta = std::fs::symlink_metadata(&path).map_err(|e| bad(cmd, format!("{}: {e}", path.display())))?;
    if !meta.is_file() {
        return Err(bad(cmd, "only a design or an importable file can be copied; move a folder instead"));
    }
    if file_kind(path.to_string_lossy().as_ref()).is_none() {
        return Err(bad(cmd, "only designs and the files SolidCraft can open can be copied here"));
    }
    Ok(path)
}

fn copy(_s: &mut Session, p: &Value) -> Result<Value> {
    if cfg!(target_arch = "wasm32") {
        return Err(unsupported());
    }
    let cmd = "designs.copy";
    let path = copy_source(p, cmd)?;
    let destination = existing_dir(str_(p, "destination").ok_or_else(|| bad(cmd, "`destination` is required"))?, cmd)?;
    let Some(name) = path.file_name() else {
        return Err(bad(cmd, "the path has no name to copy"));
    };
    let target = destination.join(name);
    if target == path {
        return Err(bad(cmd, "the file is already in that folder; Duplicate makes a second copy"));
    }
    if std::fs::symlink_metadata(&target).is_ok() {
        return Err(bad(cmd, format!("{} is already there", target.display())));
    }
    std::fs::copy(&path, &target).map_err(|e| bad(cmd, format!("{}: {e}", target.display())))?;
    Ok(json!({ "path": target.to_string_lossy(), "old_path": path.to_string_lossy() }))
}

fn duplicate(_s: &mut Session, p: &Value) -> Result<Value> {
    if cfg!(target_arch = "wasm32") {
        return Err(unsupported());
    }
    let cmd = "designs.duplicate";
    let path = copy_source(p, cmd)?;
    let parent = path.parent().ok_or_else(|| bad(cmd, "the path has no folder to copy it in"))?;
    let stem = path.file_stem().and_then(|s| s.to_str()).ok_or_else(|| bad(cmd, "the path has no name"))?;
    let extension = path.extension().and_then(|e| e.to_str()).ok_or_else(|| bad(cmd, "the file has no extension"))?;
    // `<name> copy`, then `<name> copy 2`, `<name> copy 3`, …: the first free name wins.
    let mut n = 1u32;
    let target = loop {
        let name = if n == 1 { format!("{stem} copy.{extension}") } else { format!("{stem} copy {n}.{extension}") };
        let candidate = parent.join(name);
        if std::fs::symlink_metadata(&candidate).is_err() {
            break candidate;
        }
        n += 1;
        if n > MAX_COPIES {
            return Err(bad(cmd, "there are too many copies of that file in this folder"));
        }
    };
    std::fs::copy(&path, &target).map_err(|e| bad(cmd, format!("{}: {e}", target.display())))?;
    Ok(json!({ "path": target.to_string_lossy(), "old_path": path.to_string_lossy() }))
}

/// How many `<name> copy N` names Duplicate tries before it gives up.
const MAX_COPIES: u32 = 999;

fn delete(_s: &mut Session, p: &Value) -> Result<Value> {
    if cfg!(target_arch = "wasm32") {
        return Err(unsupported());
    }
    let cmd = "designs.delete";
    let path = given_path(p, cmd)?;
    let meta = std::fs::symlink_metadata(&path).map_err(|e| bad(cmd, format!("{}: {e}", path.display())))?;
    if meta.is_dir() {
        let sidecar = solidcraft_io::project::dir(&path);
        let only_metadata =
            std::fs::read_dir(&path).map_err(|e| bad(cmd, format!("{}: {e}", path.display())))?.all(|e| e.is_ok_and(|e| e.path() == sidecar));
        if !only_metadata {
            return Err(bad(cmd, "only an empty folder can be deleted"));
        }
        if std::fs::symlink_metadata(&sidecar).is_ok() {
            std::fs::remove_dir_all(&sidecar).map_err(|e| bad(cmd, format!("{}: {e}", sidecar.display())))?;
        }
        std::fs::remove_dir(&path).map_err(|e| bad(cmd, format!("{}: {e}", path.display())))?;
    } else if meta.is_file() {
        let text = path.to_string_lossy();
        if file_kind(text.as_ref()).is_none() {
            return Err(bad(cmd, "only designs and the files SolidCraft can import can be deleted here"));
        }
        std::fs::remove_file(&path).map_err(|e| bad(cmd, format!("{}: {e}", path.display())))?;
    } else {
        return Err(bad(cmd, "only files and folders can be deleted"));
    }
    Ok(json!({ "deleted": path.to_string_lossy() }))
}

fn project_get(_s: &mut Session, p: &Value) -> Result<Value> {
    if cfg!(target_arch = "wasm32") {
        return Err(unsupported());
    }
    let cmd = "designs.project_get";
    let path = existing_dir(str_(p, "path").ok_or_else(|| bad(cmd, "`path` is required"))?, cmd)?;
    let meta = solidcraft_io::project::read(&path);
    Ok(json!({ "path": path.to_string_lossy(), "description": meta.description, "has_thumbnail": solidcraft_io::project::has_thumbnail(&path) }))
}

fn project_set(_s: &mut Session, p: &Value) -> Result<Value> {
    if cfg!(target_arch = "wasm32") {
        return Err(unsupported());
    }
    let cmd = "designs.project_set";
    let path = existing_dir(str_(p, "path").ok_or_else(|| bad(cmd, "`path` is required"))?, cmd)?;
    if let Some(Value::String(description)) = p.get("description") {
        if description.chars().count() > solidcraft_io::project::MAX_DESCRIPTION {
            return Err(bad(cmd, format!("`description` is longer than {} characters", solidcraft_io::project::MAX_DESCRIPTION)));
        }
        solidcraft_io::project::write_description(&path, description).map_err(|e| bad(cmd, e.to_string()))?;
    } else if p.get("description").is_some() {
        return Err(bad(cmd, "`description` must be text"));
    }
    if bool_(p, "remove_thumbnail").unwrap_or(false) {
        solidcraft_io::project::remove_thumbnail(&path).map_err(|e| bad(cmd, e.to_string()))?;
    }
    if let Some(source) = str_(p, "thumbnail_source").filter(|s| !s.trim().is_empty()) {
        let source = existing_file(source, cmd)?;
        solidcraft_io::project::write_thumbnail(&path, &source).map_err(|e| bad(cmd, e.to_string()))?;
    }
    let meta = solidcraft_io::project::read(&path);
    Ok(json!({
        "path": path.to_string_lossy(),
        "description": meta.description,
        "has_thumbnail": solidcraft_io::project::has_thumbnail(&path),
    }))
}

/// The version a version command asks for, checked against the file's own count.
fn asked_version(p: &Value, cmd: &str, current: u32) -> Result<u32> {
    let version = p
        .get("version")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| bad(cmd, "`version` must be a version number (1 is the first)"))?;
    if version == 0 || version > current {
        return Err(bad(cmd, format!("there is no version {version} (the design has {current})")));
    }
    Ok(version)
}

/// Read a design file's bytes, handing them to `rewrite`, and put the result back atomically
/// (keeping the file being replaced as `<file>.bak`).
fn rewrite_design_versions(p: &Value, cmd: &str, rewrite: impl FnOnce(&[u8], u32) -> Result<(Vec<u8>, u32)>) -> Result<Value> {
    let path = given_path(p, cmd)?;
    let bytes = std::fs::read(&path).map_err(|e| bad(cmd, format!("{}: {e}", path.display())))?;
    if !solidcraft_io::is_design_container(&bytes) {
        return Err(bad(cmd, format!("{} is not a design container: only a design has versions", path.display())));
    }
    let current = solidcraft_io::container_versions(&bytes).current;
    let (bytes, version) = rewrite(&bytes, current)?;
    solidcraft_io::write_atomic(&path, &bytes, true).map_err(|e| bad(cmd, format!("{}: {e}", path.display())))?;
    Ok(json!({ "path": path.to_string_lossy(), "version": version, "versions": version }))
}

fn version_promote(_s: &mut Session, p: &Value) -> Result<Value> {
    if cfg!(target_arch = "wasm32") {
        return Err(unsupported());
    }
    let cmd = "designs.version_promote";
    rewrite_design_versions(p, cmd, |bytes, current| {
        let version = asked_version(p, cmd, current)?;
        // The promoted version becomes the latest; everything after it goes.
        let keep: Vec<u32> = (1..=version).collect();
        let bytes = solidcraft_io::rewrite_versions(bytes, &keep).map_err(|e| bad(cmd, format!("{}: {e}", str_(p, "path").unwrap_or_default())))?;
        Ok((bytes, version))
    })
}

fn version_purge(_s: &mut Session, p: &Value) -> Result<Value> {
    if cfg!(target_arch = "wasm32") {
        return Err(unsupported());
    }
    let cmd = "designs.version_purge";
    rewrite_design_versions(p, cmd, |bytes, current| {
        let version = asked_version(p, cmd, current)?;
        // The versions before it go; the ones kept renumber from 1, so the current one moves down.
        let keep: Vec<u32> = (version..=current).collect();
        let bytes = solidcraft_io::rewrite_versions(bytes, &keep).map_err(|e| bad(cmd, format!("{}: {e}", str_(p, "path").unwrap_or_default())))?;
        Ok((bytes, current - version + 1))
    })
}

fn unsupported() -> EngineError {
    EngineError::Other("designs: file browsing is not supported in the browser".into())
}

/// An absolute path from `path` (no `.` resolution, no `..`, at most 4096 bytes).
fn given_path(p: &Value, cmd: &str) -> Result<PathBuf> {
    let raw = str_(p, "path")
        .filter(|s| !s.trim().is_empty() && s.len() <= 4096)
        .ok_or_else(|| bad(cmd, "`path` is required: an absolute path of at most 4096 bytes"))?;
    checked_path(raw, cmd)
}

fn checked_path(raw: &str, cmd: &str) -> Result<PathBuf> {
    let path = Path::new(raw);
    if !path.is_absolute() {
        return Err(bad(cmd, "`path` must be absolute"));
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(bad(cmd, "`path` must not contain `..`"));
    }
    if raw.chars().any(|c| c == '\0' || c.is_control()) {
        return Err(bad(cmd, "`path` contains a control character"));
    }
    Ok(path.to_path_buf())
}

fn existing_dir(raw: &str, cmd: &str) -> Result<PathBuf> {
    let path = checked_path(raw, cmd)?;
    let meta = std::fs::metadata(&path).map_err(|e| bad(cmd, format!("{}: {e}", path.display())))?;
    if !meta.is_dir() {
        return Err(bad(cmd, format!("{} is not a folder", path.display())));
    }
    Ok(path)
}

fn existing_file(raw: &str, cmd: &str) -> Result<PathBuf> {
    let path = checked_path(raw, cmd)?;
    let meta = std::fs::metadata(&path).map_err(|e| bad(cmd, format!("{}: {e}", path.display())))?;
    if !meta.is_file() {
        return Err(bad(cmd, format!("{} is not a file", path.display())));
    }
    Ok(path)
}

/// A new file or folder name: no separators, control characters or Windows-reserved names.
fn checked_name(name: &str, cmd: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 128 {
        return Err(bad(cmd, "`name` is required: 1 to 128 characters"));
    }
    if name.contains(['/', '\\']) || name.chars().any(|c| c == '\0' || c.is_control()) {
        return Err(bad(cmd, "the name must not contain a path separator or control character"));
    }
    if name.contains(['<', '>', ':', '"', '|', '?', '*']) {
        return Err(bad(cmd, "the name must not contain < > : \" | ? *"));
    }
    if name == "." || name == ".." {
        return Err(bad(cmd, "the name cannot be `.` or `..`"));
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Err(bad(cmd, "the name cannot end with a dot or a space"));
    }
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5",
        "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.contains(&stem.as_str()) {
        return Err(bad(cmd, "that name is reserved on Windows"));
    }
    Ok(name.to_string())
}

fn is_design_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| solidcraft_io::DESIGN_EXTS.iter().any(|x| e.eq_ignore_ascii_case(x)) || e.eq_ignore_ascii_case("json"))
}

/// What a listed file is (`design`), or which importer opens it: `step` for STEP, IGES, FreeCAD
/// and Rhino (`file.insert_step`), `mesh` for 3MF, STL, OBJ and glTF (`file.insert_mesh`), and
/// `kicad` for a KiCad board or project, which opens with its import settings. `None`: SolidCraft
/// can't open it, so it is not listed.
fn file_kind(path: &str) -> Option<&'static str> {
    if is_design_file(Path::new(path)) {
        Some("design")
    } else if solidcraft_io::is_step_path(path) {
        Some("step")
    } else if solidcraft_io::is_mesh_path(path) {
        Some("mesh")
    } else if solidcraft_io::is_kicad_path(path) {
        Some("kicad")
    } else {
        None
    }
}

fn name_has_design_extension(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| solidcraft_io::DESIGN_EXTS.iter().any(|x| e.eq_ignore_ascii_case(x)) || e.eq_ignore_ascii_case("json"))
}

/// A KiCad project is not a second row when its board sits next to it: the board is what opens
/// (the importer resolves a project to its board) and the row would carry the same name.
fn project_beside_its_board(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("kicad_pro")) && path.with_extension("kicad_pcb").is_file()
}

/// Whether `candidate` is `folder` or below it, by components; both are made absolute first,
/// and canonicalised where the file system allows it (case-insensitive file systems).
fn is_inside(candidate: &Path, folder: &Path) -> bool {
    let full = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let (candidate, folder) = (full(candidate), full(folder));
    candidate == folder || candidate.starts_with(&folder)
}

/// Case-insensitive, numbers compared by value (`Part 2` before `Part 10`).
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ac), Some(bc)) if ac.is_ascii_digit() && bc.is_ascii_digit() => {
                let an = digits(&mut ai);
                let bn = digits(&mut bi);
                let cmp = an.len().cmp(&bn.len()).then_with(|| an.cmp(&bn));
                if cmp != Ordering::Equal {
                    return cmp;
                }
            }
            (Some(ac), Some(bc)) => {
                let al = ac.to_ascii_lowercase();
                let bl = bc.to_ascii_lowercase();
                if al != bl {
                    return al.cmp(&bl);
                }
                ai.next();
                bi.next();
            }
        }
    }
}

fn digits(it: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut out = String::new();
    while let Some(c) = it.peek().copied() {
        if !c.is_ascii_digit() {
            break;
        }
        out.push(c);
        it.next();
    }
    let trimmed = out.trim_start_matches('0');
    if trimmed.is_empty() { "0".to_string() } else { trimmed.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("solidcraft-designs-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn listed(path: &Path) -> Value {
        list(path.to_str().unwrap()).unwrap()
    }

    fn names(v: &Value, key: &str) -> Vec<String> {
        v[key].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn listing_sorts_folders_and_designs_naturally_and_skips_hidden() {
        let d = temp("list");
        for folder in ["Part 10", "Part 2", "alpha"] {
            std::fs::create_dir(d.join(folder)).unwrap();
        }
        for file in ["b.solidcraft", "a10.solidcraft", "a2.solidcraft", "notes.txt", ".hidden.solidcraft"] {
            std::fs::write(d.join(file), b"x").unwrap();
        }
        std::fs::create_dir(d.join(".solidcraft-project")).unwrap();
        std::fs::create_dir(d.join(".git")).unwrap();
        let v = listed(&d);
        assert_eq!(names(&v, "folders"), ["alpha", "Part 2", "Part 10"]);
        assert_eq!(names(&v, "designs"), ["a2", "a10", "b"]);
        assert_eq!(v["truncated"], json!(false));
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn promoting_a_version_and_purging_the_older_ones_rewrite_the_file() {
        let d = temp("versions");
        let path = d.join("part.solidcraft");
        let path_text = path.to_string_lossy().into_owned();
        let mut doc = solidcraft_doc::Document::new("Versioned");
        doc.name = "first".into();
        let (v1, _) =
            solidcraft_io::write_design_container_versioned(&doc, solidcraft_io::DesignCompression::Fast, None, None, None, None, None).unwrap();
        std::fs::write(&path, &v1).unwrap();
        let mut s = Session::default();
        for name in ["second", "third"] {
            s.execute("doc.open", &json!({"path": path_text})).unwrap();
            s.doc_mut().name = name.into();
            s.execute("file.save", &json!({})).unwrap();
        }
        assert_eq!(solidcraft_io::container_versions(&std::fs::read(&path).unwrap()).current, 3);

        // Promote version 1: it is the latest, and 2 and 3 are gone.
        let promoted = version_promote(&mut s, &json!({"path": path_text, "version": 1})).unwrap();
        assert_eq!(promoted["versions"], json!(1));
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(solidcraft_io::container_versions(&bytes).current, 1);
        assert_eq!(solidcraft_io::read_design(&bytes).unwrap().name, "first");
        assert!(version_promote(&mut s, &json!({"path": path_text, "version": 2})).is_err());

        // Two more saves, then purge everything before the last one.
        for name in ["fourth", "fifth"] {
            s.execute("doc.open", &json!({"path": path_text})).unwrap();
            s.doc_mut().name = name.into();
            s.execute("file.save", &json!({})).unwrap();
        }
        assert_eq!(solidcraft_io::container_versions(&std::fs::read(&path).unwrap()).current, 3);
        let purged = version_purge(&mut s, &json!({"path": path_text, "version": 3})).unwrap();
        assert_eq!(purged["versions"], json!(1), "only the current one stays");
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(solidcraft_io::read_design(&bytes).unwrap().name, "fifth");
        assert!(solidcraft_io::read_design_version(&bytes, 2).is_err());
        assert!(version_purge(&mut s, &json!({"path": path_text, "version": 9})).is_err());
        // Something that is not a design container is refused.
        let json_design = d.join("plain.json");
        std::fs::write(&json_design, b"{}").unwrap();
        assert!(version_promote(&mut s, &json!({"path": json_design.to_string_lossy(), "version": 1})).is_err());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn listing_includes_the_files_solidcraft_can_import_and_says_what_they_are() {
        let d = temp("kinds");
        for file in [
            "plate.solidcraft",
            "bracket.STEP",
            "import.stp",
            "iges.igs",
            "freecad.fcstd",
            "mesh.stl",
            "part.OBJ",
            "print.3mf",
            "gltf.glb",
            "board.kicad_pcb",
            "project.kicad_pro",
            "pair.kicad_pcb",
            "pair.kicad_pro",
            "notes.txt",
            "sheet.pdf",
        ] {
            std::fs::write(d.join(file), b"x").unwrap();
        }
        let v = listed(&d);
        let rows: Vec<(String, String)> = v["designs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| (e["name"].as_str().unwrap().to_string(), e["kind"].as_str().unwrap().to_string()))
            .collect();
        let kind = |name: &str| rows.iter().find(|(n, _)| n == name).map(|(_, k)| k.as_str()).unwrap_or("missing");
        assert_eq!(kind("plate"), "design");
        for step in ["bracket", "import", "iges", "freecad"] {
            assert_eq!(kind(step), "step", "{step}");
        }
        for mesh in ["mesh", "part", "print", "gltf"] {
            assert_eq!(kind(mesh), "mesh", "{mesh}");
        }
        // A board and a project open through the KiCad settings window, so both are listed. A
        // project beside its board is not a second row: the board is the one that opens.
        for board in ["board", "project", "pair"] {
            assert_eq!(kind(board), "kicad", "{board}");
        }
        assert_eq!(rows.iter().filter(|(n, _)| n == "pair").count(), 1, "{rows:?}");
        // Files SolidCraft cannot open are not listed.
        assert!(!rows.iter().any(|(n, _)| n == "notes" || n == "sheet"), "{rows:?}");
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn folder_names_are_checked() {
        let d = temp("names");
        for name in ["a/b", "CON", "x.", "..", "", "a:b"] {
            let mut s = Session::default();
            let path = d.join(name);
            assert!(folder_new(&mut s, &json!({ "path": path.to_string_lossy() })).is_err(), "{name}");
        }
        let mut s = Session::default();
        folder_new(&mut s, &json!({ "path": d.join("Parts").to_string_lossy() })).unwrap();
        assert!(d.join("Parts").is_dir());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn rename_keeps_a_design_extension_and_refuses_collisions() {
        let d = temp("rename");
        let part = d.join("part.solidcraft");
        std::fs::write(&part, b"x").unwrap();
        std::fs::write(d.join("taken.solidcraft"), b"x").unwrap();
        let mut s = Session::default();
        let renamed = rename(&mut s, &json!({ "path": part.to_string_lossy(), "name": "bracket" })).unwrap();
        assert!(d.join("bracket.solidcraft").is_file());
        assert_eq!(renamed["path"], json!(d.join("bracket.solidcraft").to_string_lossy()));
        let bracket = d.join("bracket.solidcraft");
        assert!(rename(&mut s, &json!({ "path": bracket.to_string_lossy(), "name": "taken.solidcraft" })).is_err());
        assert!(bracket.is_file());
        // An importable file keeps its own extension (a STEP stays a STEP), and is renamed and
        // deleted like a design; a file SolidCraft cannot open is refused.
        let step = d.join("bracket.step");
        std::fs::write(&step, b"x").unwrap();
        let renamed = rename(&mut s, &json!({ "path": step.to_string_lossy(), "name": "dish" })).unwrap();
        assert_eq!(renamed["path"], json!(d.join("dish.step").to_string_lossy()));
        assert!(d.join("dish.step").is_file() && !step.exists());
        delete(&mut s, &json!({ "path": d.join("dish.step").to_string_lossy() })).unwrap();
        assert!(!d.join("dish.step").exists());
        let notes = d.join("notes.txt");
        std::fs::write(&notes, b"x").unwrap();
        assert!(rename(&mut s, &json!({ "path": notes.to_string_lossy(), "name": "memo" })).is_err());
        assert!(delete(&mut s, &json!({ "path": notes.to_string_lossy() })).is_err());
        assert!(notes.is_file(), "a file SolidCraft cannot open was touched");
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn moving_refuses_a_collision_and_a_folder_into_itself() {
        let d = temp("move");
        let sub = d.join("sub");
        std::fs::create_dir(&sub).unwrap();
        let part = d.join("part.solidcraft");
        std::fs::write(&part, b"x").unwrap();
        std::fs::write(sub.join("part.solidcraft"), b"x").unwrap();
        let mut s = Session::default();
        assert!(
            move_path(&mut s, &json!({ "path": part.to_string_lossy(), "destination": sub.to_string_lossy() })).is_err(),
            "an existing target must be refused"
        );
        assert!(
            move_path(&mut s, &json!({ "path": sub.to_string_lossy(), "destination": sub.to_string_lossy() })).is_err(),
            "a folder cannot move into itself"
        );
        let dest = d.join("dest");
        std::fs::create_dir(&dest).unwrap();
        move_path(&mut s, &json!({ "path": part.to_string_lossy(), "destination": dest.to_string_lossy() })).unwrap();
        assert!(dest.join("part.solidcraft").is_file());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn only_empty_folders_are_deleted_and_metadata_does_not_count() {
        let d = temp("delete");
        let empty = d.join("empty");
        let project = d.join("project");
        std::fs::create_dir(&empty).unwrap();
        std::fs::create_dir(&project).unwrap();
        std::fs::create_dir(solidcraft_io::project::dir(&project)).unwrap();
        std::fs::write(solidcraft_io::project::meta_path(&project), b"{}").unwrap();
        let full = d.join("full");
        std::fs::create_dir(&full).unwrap();
        std::fs::write(full.join("part.solidcraft"), b"x").unwrap();
        let mut s = Session::default();
        assert!(delete(&mut s, &json!({ "path": full.to_string_lossy() })).is_err());
        delete(&mut s, &json!({ "path": empty.to_string_lossy() })).unwrap();
        assert!(!empty.exists());
        delete(&mut s, &json!({ "path": project.to_string_lossy() })).unwrap();
        assert!(!project.exists());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn project_metadata_round_trips_through_the_commands() {
        let d = temp("project");
        let mut s = Session::default();
        project_set(&mut s, &json!({ "path": d.to_string_lossy(), "description": "Rover parts" })).unwrap();
        let got = project_get(&mut s, &json!({ "path": d.to_string_lossy() })).unwrap();
        assert_eq!(got["description"], json!("Rover parts"));
        assert_eq!(got["has_thumbnail"], json!(false));
        let v = listed(&d);
        assert_eq!(v["folders"].as_array().unwrap().len(), 0);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn hostile_paths_are_refused() {
        let mut s = Session::default();
        for path in ["relative", "/tmp/../etc", "bad\u{0}path", ""] {
            assert!(list(path).is_err(), "{path:?}");
            assert!(delete(&mut s, &json!({ "path": path })).is_err(), "{path:?}");
        }
    }
}
