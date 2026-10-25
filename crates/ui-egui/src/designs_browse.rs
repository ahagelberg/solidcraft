//! The repositories and their folders, as Save As, Move and Copy show them.
//!
//! One browser serves all three: which repository, which folder in it, that folder's subfolders
//! (listed on a worker thread) and a new folder where it stands. Every link stays inside a
//! repository, so no design can be saved, moved or copied outside them, and the three windows
//! behave the same.

use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use egui::{RichText, vec2};
use serde_json::json;

use crate::SolveApp;
use crate::theme::Tokens;

/// The subfolders a dialog lists; a repository folder with more than this shows nothing (no one
/// picks from thousands).
const MAX_FOLDERS: usize = 500;

/// A repository, as the browser offers it.
struct Repository {
    id: String,
    name: String,
    root: PathBuf,
}

/// The repositories and the folder chosen in one of them.
pub(crate) struct Browser {
    repositories: Vec<Repository>,
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

impl Browser {
    /// The repositories to offer, opening on `start` when it is a folder inside one of them
    /// (else the active repository's root, else the first repository).
    pub fn new(app: &SolveApp, start: Option<&Path>) -> Option<Browser> {
        let repositories: Vec<Repository> = app
            .preferences
            .repositories
            .iter()
            .filter(|r| !r.path.is_empty())
            .map(|r| Repository { id: r.id.clone(), name: r.name.clone(), root: PathBuf::from(&r.path) })
            .collect();
        if repositories.is_empty() {
            return None;
        }
        let active = app.preferences.active_repository.clone();
        let chosen = start
            .and_then(|s| repositories.iter().find(|r| s.starts_with(&r.root)))
            .or_else(|| repositories.iter().find(|r| r.id == active))
            .or_else(|| repositories.first())?;
        let (repository, root, root_name) = (chosen.id.clone(), chosen.root.clone(), chosen.name.clone());
        let folder = start.filter(|s| s.starts_with(&root)).map(Path::to_path_buf).unwrap_or_else(|| root.clone());
        Some(Browser { repositories, repository, root, root_name, folder, folders: None, listing: None, new_folder: None, error: None })
    }

    /// The folder chosen in the repository.
    pub fn folder(&self) -> &Path {
        &self.folder
    }

    /// The repository chosen (its id), and its root (read by the Move/Copy tests).
    #[cfg(test)]
    pub fn repository(&self) -> &str {
        &self.repository
    }

    #[cfg(test)]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Say why the last action could not be done (shown under the folders).
    pub fn fail(&mut self, error: String) {
        self.error = Some(error);
    }

    /// List the folder shown's subfolders on a worker thread: a repository may be a network
    /// folder, and the window must not wait for it.
    pub fn sync(&mut self, ctx: &egui::Context) {
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
    pub fn enter(&mut self, folder: &Path) {
        if !folder.starts_with(&self.root) || !folder.is_dir() {
            return;
        }
        self.folder = folder.to_path_buf();
        self.folders = None;
        self.listing = None;
        self.error = None;
    }

    /// Choose a repository: the walk starts again at its root.
    pub fn choose(&mut self, id: &str) {
        let Some(repository) = self.repositories.iter().find(|r| r.id == id) else { return };
        self.repository = repository.id.clone();
        self.root = repository.root.clone();
        self.root_name = repository.name.clone();
        self.folder = repository.root.clone();
        self.folders = None;
        self.listing = None;
        self.error = None;
    }

    /// Draw the repository picker, the breadcrumb and the folders here, and make a folder when
    /// one is asked for. The caller draws its own fields and buttons after it.
    pub fn show(&mut self, app: &mut SolveApp, ui: &mut egui::Ui) {
        let t = Tokens::get();
        let mut switch = None;
        let mut browse = None;
        let mut create = false;
        let mut combo_center = None;
        // The repository, then its folders down to the one shown: every link is a repository
        // folder, so nothing outside one can be reached.
        ui.horizontal(|ui| {
            ui.label(RichText::new(crate::i18n::tr("designs.repository")).size(12.0).color(t.text_dim));
            let combo = egui::ComboBox::from_id_salt("designs_browse_repository")
                .width(ui.available_width() - 4.0)
                .selected_text(RichText::new(self.root_name.clone()).size(12.5).strong())
                .show_ui(ui, |ui| {
                    for repository in &self.repositories {
                        if ui.selectable_label(repository.id == self.repository, &repository.name).clicked() {
                            switch = Some(repository.id.clone());
                        }
                    }
                });
            combo_center = Some(combo.response.rect.center());
        });
        if let Some(center) = combo_center {
            crate::scenario::publish_handle("designs:browse:repository", center);
        }
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            if ui.link(RichText::new(&self.root_name).size(12.0)).on_hover_text(self.root.to_string_lossy()).clicked() {
                browse = Some(self.root.clone());
            }
            let mut path = self.root.clone();
            for part in self.folder.strip_prefix(&self.root).into_iter().flat_map(|p| p.components()) {
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
                for (name, path) in self.folders.iter().flatten() {
                    shown += 1;
                    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), egui::Sense::click());
                    ui.painter().text(
                        egui::pos2(rect.left() + 8.0, rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        name,
                        egui::TextStyle::Button.resolve(ui.style()),
                        t.text,
                    );
                    crate::scenario::publish_handle(&format!("designs:browse:folder:{name}"), rect.center());
                    if response.clicked() {
                        browse = Some(path.clone());
                    }
                }
                if shown == 0 {
                    let text = if self.listing.is_some() { "…" } else { crate::i18n::tr("designs.no_folders") };
                    ui.label(RichText::new(text).size(11.0).color(t.text_dim));
                }
            });
        });
        ui.add_space(6.0);
        if let Some(mut field) = self.new_folder.take() {
            let mut cancelled = false;
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut field).desired_width(180.0).hint_text(crate::i18n::tr("designs.name")));
                let button = ui.button(crate::i18n::tr("designs.create"));
                crate::scenario::publish_handle("designs:browse:create", button.rect.center());
                if button.clicked() {
                    create = true;
                }
                if ui.button("×").on_hover_text(crate::i18n::tr("designs.cancel")).clicked() {
                    cancelled = true;
                }
            });
            if !cancelled {
                self.new_folder = Some(field);
            }
        } else {
            let button = ui.button(crate::i18n::tr("designs.new_folder"));
            crate::scenario::publish_handle("designs:browse:new", button.rect.center());
            if button.clicked() {
                self.new_folder = Some(String::new());
            }
        }
        if let Some(error) = &self.error {
            ui.add_space(2.0);
            ui.label(RichText::new(crate::designs_panel::plain(error)).size(11.0).color(t.error));
        }
        if let Some(id) = switch {
            self.choose(&id);
        }
        if let Some(folder) = browse {
            self.enter(&folder);
        }
        if create {
            let name = self.new_folder.clone().unwrap_or_default();
            let path = self.folder.join(name.trim());
            match app.run("designs.folder_new", json!({ "path": path.to_string_lossy() })) {
                Ok(_) => {
                    self.new_folder = None;
                    self.folder = path;
                    self.folders = None;
                    self.listing = None;
                    crate::designs_panel::reload(app);
                }
                Err(e) => self.error = Some(e.to_string()),
            }
        }
    }
}

/// The subfolders of a folder, on a worker thread (as the panel lists them).
fn spawn_folders(folder: &Path) -> Option<Receiver<Vec<(String, PathBuf)>>> {
    let dir = folder.to_path_buf();
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new().name("designs-browse-list".into()).spawn(move || {
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

    /// It opens on the active repository and, given a folder inside one, on that repository
    /// and folder; switching repositories goes back to a root.
    #[test]
    fn the_browser_opens_where_asked_and_switches_repositories() {
        let mut h = crate::scenario::Harness::new();
        let (first, second) = (h.dir.join("first"), h.dir.join("second"));
        std::fs::create_dir_all(first.join("parts")).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        h.app.preferences.repositories = vec![
            crate::prefs::Repository { id: "a".into(), name: "A".into(), description: String::new(), path: first.to_string_lossy().into_owned() },
            crate::prefs::Repository { id: "b".into(), name: "B".into(), description: String::new(), path: second.to_string_lossy().into_owned() },
        ];
        h.app.preferences.active_repository = "a".into();

        let mut browser = Browser::new(&h.app, None).expect("no repository offered");
        assert_eq!((browser.repository.as_str(), browser.folder().to_path_buf()), ("a", first.clone()));
        browser.enter(&first.join("parts"));
        assert_eq!(browser.folder(), first.join("parts"));

        let mut browser = Browser::new(&h.app, Some(&second)).expect("no repository offered");
        assert_eq!((browser.repository.as_str(), browser.folder().to_path_buf()), ("b", second.clone()));
        browser.choose("a");
        assert_eq!((browser.repository.as_str(), browser.folder().to_path_buf()), ("a", first.clone()));
    }
}
