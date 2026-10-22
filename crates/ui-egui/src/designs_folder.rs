//! Move or copy a design into a folder of a repository.
//!
//! The dialog walks the repositories and their own folders; the system's folder picker is never
//! used, so a design cannot be moved or copied somewhere no repository covers. It starts on the
//! repository the design is in (else the one the panel shows) and in the design's own folder.

use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use egui::{RichText, vec2};
use serde_json::json;

use crate::SolveApp;
use crate::theme::Tokens;

/// The subfolders a dialog lists; a repository folder with more than this shows nothing (no one
/// picks from thousands).
const MAX_FOLDERS: usize = 500;

/// A repository, as the dialog offers it.
struct Repository {
    id: String,
    name: String,
    root: PathBuf,
}

/// An open Move or Copy window.
pub struct Pick {
    /// The file to move or copy, its name, and which of the two this is.
    source: String,
    source_name: String,
    copy: bool,
    repositories: Vec<Repository>,
    /// The repository chosen and the folder in it the file goes to.
    repository: String,
    root: PathBuf,
    root_name: String,
    folder: PathBuf,
    folders: Option<Vec<(String, PathBuf)>>,
    listing: Option<Receiver<Vec<(String, PathBuf)>>>,
    /// A new folder being named.
    new_folder: Option<String>,
    error: Option<String>,
}

/// Open the window for a design or an importable file.
pub fn open(app: &mut SolveApp, path: &str, copy: bool) {
    let repositories: Vec<Repository> = app
        .preferences
        .repositories
        .iter()
        .filter(|r| !r.path.is_empty())
        .map(|r| Repository { id: r.id.clone(), name: r.name.clone(), root: PathBuf::from(&r.path) })
        .collect();
    if repositories.is_empty() {
        app.set_status(crate::i18n::tr("designs.no_repositories"), true);
        return;
    }
    let source = PathBuf::from(path);
    // The repository holding the file comes first: a copy stays where the design lives unless
    // the user says otherwise.
    let active = app.preferences.active_repository.clone();
    let chosen = repositories.iter().find(|r| source.starts_with(&r.root)).or_else(|| repositories.iter().find(|r| r.id == active));
    let Some(chosen) = chosen else { return };
    let (repository, root, root_name) = (chosen.id.clone(), chosen.root.clone(), chosen.name.clone());
    let folder = source.parent().filter(|p| p.starts_with(&root)).map(Path::to_path_buf).unwrap_or_else(|| root.clone());
    let source_name = source.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    app.designs_pick = Some(Pick {
        source: path.to_string(),
        source_name,
        copy,
        repositories,
        repository,
        root,
        root_name,
        folder,
        folders: None,
        listing: None,
        new_folder: None,
        error: None,
    });
}

impl Pick {
    /// List the folder shown's subfolders on a worker thread: a repository may be a network
    /// folder, and the window must not wait for it.
    fn sync(&mut self, ctx: &egui::Context) {
        let received = self.listing.as_ref().and_then(|rx| match rx.try_recv() {
            Ok(v) => Some(v),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Vec::new()),
        });
        if let Some(folders) = received {
            self.listing = None;
            self.folders = Some(folders);
        }
        if self.folders.is_none() && self.listing.is_none() {
            self.listing = spawn_folders(&self.folder);
        }
        if self.listing.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    /// Go into a folder of the repository shown (and never above its root).
    fn enter(&mut self, folder: &Path) {
        if !folder.starts_with(&self.root) || !folder.is_dir() {
            return;
        }
        self.folder = folder.to_path_buf();
        self.folders = None;
        self.listing = None;
        self.error = None;
    }

    /// Choose a repository: the walk starts again at its root.
    fn choose(&mut self, id: &str) {
        let Some(repository) = self.repositories.iter().find(|r| r.id == id) else { return };
        self.repository = repository.id.clone();
        self.root = repository.root.clone();
        self.root_name = repository.name.clone();
        self.folder = repository.root.clone();
        self.folders = None;
        self.listing = None;
        self.error = None;
    }
}

/// The window. Called once per frame while it is open.
pub fn show(app: &mut SolveApp, ctx: &egui::Context) {
    let Some(mut pick) = app.designs_pick.take() else { return };
    pick.sync(ctx);
    let t = Tokens::get();
    let mut open = true;
    let (mut go, mut cancel, mut create) = (false, false, false);
    let mut browse: Option<PathBuf> = None;
    let mut switch: Option<String> = None;
    let title = if pick.copy {
        crate::i18n::format("designs.copy_title", &[("name", &pick.source_name)])
    } else {
        crate::i18n::format("designs.move_title", &[("name", &pick.source_name)])
    };
    crate::frame::window(ctx, RichText::new(title).strong().size(13.0), crate::frame::Width::Normal)
        .id(egui::Id::new("sc_designs_folder"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.set_min_width(460.0);
            ui.label(RichText::new(crate::i18n::tr("designs.folder_pick_note")).size(11.0).color(t.text_dim));
            ui.add_space(4.0);
            // The repository, then its folders down to the one shown: every link is a repository
            // folder, so nothing outside one can be reached.
            let combo = egui::ComboBox::from_id_salt("designs_pick_repository")
                .width(ui.available_width() - 4.0)
                .selected_text(RichText::new(pick.root_name.clone()).size(12.5).strong())
                .show_ui(ui, |ui| {
                    for repository in &pick.repositories {
                        if ui.selectable_label(repository.id == pick.repository, &repository.name).clicked() {
                            switch = Some(repository.id.clone());
                        }
                    }
                });
            crate::scenario::publish_handle("designs:pick:repository", combo.response.rect.center());
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                if ui.link(RichText::new(&pick.root_name).size(12.0)).on_hover_text(pick.root.to_string_lossy()).clicked() {
                    browse = Some(pick.root.clone());
                }
                let mut path = pick.root.clone();
                for part in pick.folder.strip_prefix(&pick.root).into_iter().flat_map(|p| p.components()) {
                    let Component::Normal(part) = part else { continue };
                    path.push(part);
                    ui.label(RichText::new(">").size(12.0).color(t.text_dim));
                    let target = path.clone();
                    if ui.link(RichText::new(part.to_string_lossy()).size(12.0)).clicked() {
                        browse = Some(target);
                    }
                }
            });
            ui.add_space(4.0);
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::vertical().max_height(160.0).auto_shrink([false, false]).show(ui, |ui| {
                    let mut shown = 0;
                    for (name, path) in pick.folders.iter().flatten() {
                        shown += 1;
                        let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), egui::Sense::click());
                        ui.painter().text(
                            egui::pos2(rect.left() + 8.0, rect.center().y),
                            egui::Align2::LEFT_CENTER,
                            name,
                            egui::TextStyle::Button.resolve(ui.style()),
                            t.text,
                        );
                        crate::scenario::publish_handle(&format!("designs:pick:folder:{name}"), rect.center());
                        if response.clicked() {
                            browse = Some(path.clone());
                        }
                    }
                    if shown == 0 {
                        let text = if pick.listing.is_some() { "…" } else { crate::i18n::tr("designs.no_folders") };
                        ui.label(RichText::new(text).size(11.0).color(t.text_dim));
                    }
                });
            });
            ui.add_space(6.0);
            if let Some(mut field) = pick.new_folder.take() {
                let mut cancelled = false;
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut field).desired_width(180.0).hint_text(crate::i18n::tr("designs.name")));
                    if ui.button(crate::i18n::tr("designs.create")).clicked() {
                        create = true;
                    }
                    if ui.button("×").on_hover_text(crate::i18n::tr("designs.cancel")).clicked() {
                        cancelled = true;
                    }
                });
                if !cancelled {
                    pick.new_folder = Some(field);
                }
            } else if ui.button(crate::i18n::tr("designs.new_folder")).clicked() {
                pick.new_folder = Some(String::new());
            }
            if let Some(error) = &pick.error {
                ui.add_space(2.0);
                ui.label(RichText::new(crate::designs_panel::plain(error)).size(11.0).color(t.error));
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let label = if pick.copy { crate::i18n::tr("designs.copy_here") } else { crate::i18n::tr("designs.move_here") };
                let button = ui.add(egui::Button::new(label).min_size(vec2(100.0, 0.0)));
                crate::scenario::publish_handle("designs:pick:here", button.rect.center());
                if button.clicked() {
                    go = true;
                }
                let cancel_button = ui.button(crate::i18n::tr("designs.cancel"));
                crate::scenario::publish_handle("designs:pick:cancel", cancel_button.rect.center());
                if cancel_button.clicked() {
                    cancel = true;
                }
            });
        });
    if let Some(id) = switch {
        pick.choose(&id);
    }
    if let Some(folder) = browse {
        pick.enter(&folder);
    }
    if create {
        let name = pick.new_folder.clone().unwrap_or_default();
        let path = pick.folder.join(name.trim());
        match app.run("designs.folder_new", json!({ "path": path.to_string_lossy() })) {
            Ok(_) => {
                pick.new_folder = None;
                pick.folder = path;
                pick.folders = None;
                pick.listing = None;
            }
            Err(e) => pick.error = Some(e.to_string()),
        }
    }
    if go {
        match perform(app, &pick.source, pick.copy, &pick.folder) {
            Ok(v) => {
                crate::designs_panel::reload(app);
                match (v.get("old_path").and_then(|p| p.as_str()), v.get("path").and_then(|p| p.as_str())) {
                    (Some(_), Some(new)) if pick.copy => {
                        app.designs.selected = Some(new.to_string());
                        app.set_status(crate::i18n::format("designs.copied", &[("name", &pick.source_name)]), false);
                        cancel = true;
                    }
                    (Some(old), Some(new)) => {
                        crate::documents::retarget(app, old, new);
                        app.set_status(crate::i18n::tr("designs.links_warning"), false);
                        cancel = true;
                    }
                    _ => {}
                }
            }
            Err(e) => pick.error = Some(e),
        }
    }
    if !open || cancel {
        return;
    }
    app.designs_pick = Some(pick);
}

/// Copy (or move) `path` into `folder`: what the window's button runs, and what a test drives
/// when it checks the dialog's own bookkeeping (which repository, which folder).
pub(crate) fn perform(app: &mut SolveApp, path: &str, copy: bool, folder: &Path) -> Result<serde_json::Value, String> {
    let command = if copy { "designs.copy" } else { "designs.move" };
    app.run(command, json!({ "path": path, "destination": folder.to_string_lossy() })).map_err(|e| e.to_string())
}

/// The subfolders of a folder, on a worker thread (as the panel lists them).
fn spawn_folders(folder: &Path) -> Option<Receiver<Vec<(String, PathBuf)>>> {
    let dir = folder.to_path_buf();
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new().name("designs-pick-list".into()).spawn(move || {
        let mut folders: Vec<(String, PathBuf)> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_str()?.to_string();
                if name.starts_with('.') || !entry.file_type().ok()?.is_dir() {
                    return None;
                }
                Some((name, entry.path()))
            })
            .collect();
        folders.sort_by_key(|(name, _)| name.to_lowercase());
        folders.truncate(MAX_FOLDERS);
        let _ = tx.send(folders);
    });
    spawned.ok().map(|_| rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Two repositories: a design in the first can be copied into the second, and the dialog
    /// starts on the design's own repository and folder.
    #[test]
    fn a_design_copies_into_another_repository() {
        let mut h = crate::scenario::Harness::new();
        let (first, second) = (h.dir.join("first"), h.dir.join("second"));
        std::fs::create_dir_all(first.join("parts")).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let repositories = vec![
            crate::prefs::Repository { id: "a".into(), name: "A".into(), description: String::new(), path: first.to_string_lossy().into_owned() },
            crate::prefs::Repository { id: "b".into(), name: "B".into(), description: String::new(), path: second.to_string_lossy().into_owned() },
        ];
        h.app.preferences.repositories = repositories;
        h.app.preferences.active_repository = "a".into();
        let plate = first.join("parts").join("plate.solidcraft");
        h.app.run("file.save_as", json!({ "path": plate.to_string_lossy() })).unwrap();

        open(&mut h.app, &plate.to_string_lossy(), true);
        h.frames(1);
        let pick = h.app.designs_pick.as_ref().expect("the copy window did not open");
        assert_eq!(pick.repository, "a", "the dialog did not start on the design's repository");
        assert_eq!(pick.folder, first.join("parts"), "the dialog did not start in the design's folder");

        // Choosing the other repository moves the walk to its root, and the copy lands there.
        {
            let pick = h.app.designs_pick.as_mut().expect("the copy window did not open");
            pick.choose("b");
            assert_eq!((pick.root.as_path(), pick.folder.as_path()), (second.as_path(), second.as_path()));
        }
        perform(&mut h.app, &plate.to_string_lossy(), true, &second).expect("the copy failed");
        assert!(second.join("plate.solidcraft").is_file(), "the copy did not reach the other repository");
        assert!(plate.is_file(), "copying removed the original");
    }

    /// Duplicate makes a second copy beside the design, and the next one is `copy 2`.
    #[test]
    fn duplicate_copies_beside_the_design() {
        let mut h = crate::scenario::Harness::new();
        let plate = h.dir.join("plate.solidcraft");
        h.app.run("file.save_as", json!({ "path": plate.to_string_lossy() })).unwrap();
        let once = h.app.run("designs.duplicate", json!({ "path": plate.to_string_lossy() })).unwrap();
        let twice = h.app.run("designs.duplicate", json!({ "path": plate.to_string_lossy() })).unwrap();
        assert!(once["path"].as_str().unwrap_or_default().ends_with("plate copy.solidcraft"), "{once}");
        assert!(twice["path"].as_str().unwrap_or_default().ends_with("plate copy 2.solidcraft"), "{twice}");
        assert!(h.dir.join("plate copy.solidcraft").is_file() && h.dir.join("plate copy 2.solidcraft").is_file());
    }
}
