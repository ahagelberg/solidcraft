//! Project folders and the designs in them: browsing, creating, renaming, moving and deleting,
//! and their metadata (description and custom thumbnail).
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
        .params("path: an absolute folder -> {path, folders: [{name, path, description, has_thumbnail}], designs: [{name, path, modified}], truncated}; at most 2000 entries"),
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
        } else if meta.is_file() && is_design_file(&path) {
            let modified = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(&name).to_string();
            designs.push(json!({ "name": stem, "path": path.to_string_lossy(), "modified": modified }));
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
        if !is_design_file(&path) {
            return Err(bad(cmd, "only design files can be renamed here"));
        }
        if !name_has_design_extension(&name) {
            let extension = path.extension().and_then(|e| e.to_str()).unwrap_or(solidcraft_io::DESIGN_EXT);
            name = format!("{name}.{extension}");
        }
    } else if !meta.is_dir() {
        return Err(bad(cmd, "only designs and folders can be renamed"));
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
        if !is_design_file(&path) {
            return Err(bad(cmd, "only design files can be deleted here"));
        }
        std::fs::remove_file(&path).map_err(|e| bad(cmd, format!("{}: {e}", path.display())))?;
    } else {
        return Err(bad(cmd, "only design files and folders can be deleted"));
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

fn name_has_design_extension(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| solidcraft_io::DESIGN_EXTS.iter().any(|x| e.eq_ignore_ascii_case(x)) || e.eq_ignore_ascii_case("json"))
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
