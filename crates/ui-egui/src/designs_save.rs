//! Save As inside a repository: the panel's own dialog instead of the system file dialog, so a
//! design can only be written in the repository it browses.
//!
//! It opens on the folder the panel shows and lets the repository's own folders be walked into
//! (the breadcrumb never goes above the repository), with the design's name ready to edit and a
//! choice between the container format and plain JSON. A name already taken asks for Replace.

use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use egui::{RichText, vec2};
use serde_json::json;

use crate::SolveApp;
use crate::theme::Tokens;

/// The folders a save dialog lists: more than this and it says nothing is shown (a repository
/// folder with thousands of subfolders is not a save target anyone can pick from).
const MAX_FOLDERS: usize = 500;

/// The open Save As window.
pub struct Dialog {
    /// The repository's folder: nothing outside it can be saved to.
    root: PathBuf,
    /// The repository's name, for the breadcrumb's first link.
    root_name: String,
    /// The folder Save writes into.
    folder: PathBuf,
    name: String,
    /// Plain JSON instead of the container format.
    json: bool,
    /// The name field still has to select all (it is focused on the first frame).
    select_all: bool,
    folders: Option<Vec<(String, PathBuf)>>,
    listing: Option<Receiver<Vec<(String, PathBuf)>>>,
    /// A name that is already taken: the next Save with it replaces the file.
    replace: Option<PathBuf>,
    /// A new folder being named.
    new_folder: Option<String>,
    error: Option<String>,
}

/// Open it for the open design; false (and nothing opened) when there is no repository to save in,
/// so the caller falls back to the host's picker.
pub fn open(app: &mut SolveApp, json: bool) -> bool {
    let Some(root) = crate::designs_panel::repository_root(app) else { return false };
    let root_name = crate::designs_panel::active_repository_name(app).unwrap_or_default();
    let shown = crate::designs_panel::save_folder(app).map(PathBuf::from).filter(|p| p.starts_with(&root) && p.is_dir());
    let folder = shown.unwrap_or_else(|| root.clone());
    let name = app.session.doc.name.clone();
    app.save_to_repo = Some(Dialog {
        root,
        root_name,
        folder,
        name,
        json,
        select_all: true,
        folders: None,
        listing: None,
        replace: None,
        new_folder: None,
        error: None,
    });
    true
}

impl Dialog {
    /// The file Save writes, when the name can be one: a plain name in the folder shown.
    fn target(&self) -> Option<PathBuf> {
        let stem = self.name.trim();
        let stem = strip_extension(stem, self.json);
        if stem.is_empty() || !is_plain_name(stem) {
            return None;
        }
        Some(self.folder.join(format!("{stem}.{}", if self.json { "json" } else { solidcraft_engine::io::DESIGN_EXT })))
    }

    /// The name as it will be saved, for the "already there" question.
    fn file_name(&self) -> String {
        self.target().and_then(|t| t.file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_default()
    }

    /// The name typed is one already there: the dialog is asking before it replaces the file.
    pub fn asks_to_replace(&self) -> bool {
        self.replace.is_some() && self.replace.as_deref() == self.target().as_deref()
    }

    /// List the subfolders of the folder shown (on a worker thread: a repository may be a network
    /// folder, and the window must not wait for it).
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
            self.listing = spawn_listing(&self.folder);
        }
        if self.listing.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    /// Go into a folder of the repository (never above its root).
    fn enter(&mut self, folder: &Path) {
        if !folder.starts_with(&self.root) || !folder.is_dir() {
            return;
        }
        self.folder = folder.to_path_buf();
        self.folders = None;
        self.listing = None;
        self.replace = None;
    }
}

/// The window. Called once per frame while it is open.
pub fn show(app: &mut SolveApp, ctx: &egui::Context) {
    let Some(mut dialog) = app.save_to_repo.take() else { return };
    dialog.sync(ctx);
    let t = Tokens::get();
    let mut open = true;
    let (mut save, mut cancel, mut create) = (false, false, false);
    let mut browse: Option<PathBuf> = None;
    crate::frame::window(ctx, RichText::new(crate::i18n::tr("designs.save_title")).strong().size(13.0), crate::frame::Width::Normal)
        .id(egui::Id::new("sc_designs_save"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.set_min_width(460.0);
            // Where it saves: the repository, then its folders down to this one. Only the
            // repository's own links are offered.
            ui.horizontal_wrapped(|ui| {
                if ui.link(RichText::new(&dialog.root_name).size(12.0)).on_hover_text(dialog.root.to_string_lossy()).clicked() {
                    browse = Some(dialog.root.clone());
                }
                let mut path = dialog.root.clone();
                for part in dialog.folder.strip_prefix(&dialog.root).into_iter().flat_map(|p| p.components()) {
                    let Component::Normal(part) = part else { continue };
                    path.push(part);
                    ui.label(RichText::new("/").size(12.0).color(t.text_dim));
                    let target = path.clone();
                    if ui.link(RichText::new(part.to_string_lossy()).size(12.0)).clicked() {
                        browse = Some(target);
                    }
                }
            });
            ui.add_space(4.0);
            // The folders here: a double click (or a click) goes in.
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::vertical().max_height(140.0).auto_shrink([false, false]).show(ui, |ui| {
                    let mut shown = 0;
                    for (name, path) in dialog.folders.iter().flatten() {
                        shown += 1;
                        let response = ui.add_sized(vec2(ui.available_width(), 20.0), egui::Button::new(name).frame(false));
                        if response.clicked() {
                            browse = Some(path.clone());
                        }
                    }
                    if shown == 0 {
                        let text = if dialog.listing.is_some() { "…" } else { crate::i18n::tr("designs.no_folders") };
                        ui.label(RichText::new(text).size(11.0).color(t.text_dim));
                    }
                });
            });
            ui.add_space(6.0);
            if let Some(mut field) = dialog.new_folder.take() {
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
                    dialog.new_folder = Some(field);
                }
            } else if ui.button(crate::i18n::tr("designs.new_folder")).clicked() {
                dialog.new_folder = Some(String::new());
            }
            ui.add_space(4.0);
            let id = egui::Id::new("designs-save-name");
            ui.horizontal(|ui| {
                ui.label(crate::i18n::tr("designs.name"));
                let response = ui.add(egui::TextEdit::singleline(&mut dialog.name).id(id).desired_width(240.0));
                crate::scenario::publish_handle("designs:save:name", response.rect.center());
                if dialog.select_all {
                    response.request_focus();
                    let mut state = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
                    let all = egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(dialog.name.chars().count()));
                    state.cursor.set_char_range(Some(all));
                    state.store(ui.ctx(), id);
                    dialog.select_all = false;
                }
                // Enter in the name saves (Esc closes, as a window's Escape does).
                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    save = true;
                }
                ui.checkbox(&mut dialog.json, crate::i18n::tr("designs.save_json"));
            });
            if let Some(target) = dialog.target() {
                ui.label(RichText::new(target.to_string_lossy()).size(10.5).color(t.text_dim));
            }
            if let Some(error) = &dialog.error {
                ui.label(RichText::new(error).size(11.0).color(t.error));
            }
            if dialog.asks_to_replace() {
                let text = crate::i18n::format("designs.save_exists", &[("name", &dialog.file_name())]);
                ui.label(RichText::new(text).size(11.5).color(t.warning));
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let ready = dialog.target().is_some();
                let label = if dialog.asks_to_replace() { crate::i18n::tr("designs.replace") } else { crate::i18n::tr("ui.save") };
                let button = ui.add_enabled(ready, egui::Button::new(label).min_size(vec2(90.0, 0.0)));
                crate::scenario::publish_handle("designs:save:save", button.rect.center());
                if button.clicked() {
                    save = true;
                }
                let cancel_button = ui.button(crate::i18n::tr("designs.cancel"));
                crate::scenario::publish_handle("designs:save:cancel", cancel_button.rect.center());
                if cancel_button.clicked() {
                    cancel = true;
                }
            });
        });
    if let Some(folder) = browse {
        dialog.enter(&folder);
    }
    if create {
        let name = dialog.new_folder.clone().unwrap_or_default();
        let path = dialog.folder.join(name.trim());
        match app.run("designs.folder_new", json!({ "path": path.to_string_lossy() })) {
            Ok(_) => {
                dialog.new_folder = None;
                dialog.folder = path;
                dialog.folders = None;
                dialog.listing = None;
            }
            Err(e) => dialog.error = Some(e.to_string()),
        }
    }
    if save && let Some(target) = dialog.target() {
        if target.exists() && !dialog.asks_to_replace() {
            // Ask first: the next click with the same name replaces it.
            dialog.replace = Some(target);
        } else {
            let path = target.to_string_lossy().into_owned();
            match app.run("file.save_as", json!({ "path": path, "cache": !dialog.json })) {
                Ok(_) => {
                    crate::documents::saved_for_close(app);
                    cancel = true;
                }
                Err(e) => {
                    dialog.error = Some(e.to_string());
                    dialog.replace = None;
                }
            }
        }
    }
    if !open || cancel {
        return;
    }
    app.save_to_repo = Some(dialog);
}

/// A name for a folder, chosen on a worker thread (as the panel lists them).
fn spawn_listing(folder: &Path) -> Option<Receiver<Vec<(String, PathBuf)>>> {
    let dir = folder.to_path_buf();
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new().name("designs-save-list".into()).spawn(move || {
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

/// The name without the extension the dialog adds: `Part.solidcraft` saves as `Part`.
fn strip_extension(name: &str, json: bool) -> &str {
    let extension = if json { "json" } else { solidcraft_engine::io::DESIGN_EXT };
    match name.len().checked_sub(extension.len() + 1) {
        Some(cut) if name.is_char_boundary(cut) && name[cut..].eq_ignore_ascii_case(&format!(".{extension}")) => &name[..cut],
        _ => name,
    }
}

/// Whether a typed name can be a file name in the folder shown: no separators, no `..`, no
/// control characters (a save never reaches a folder the dialog doesn't show).
fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name.chars().count() <= 120
        && !name.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        && !name.ends_with('.')
        && !name.ends_with(' ')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typed_name_cannot_leave_the_folder_it_is_saved_in() {
        for bad in ["", " ", "..", ".", "a/b", "a\\b", "C:evil", "a*b", "a?b", "a\"b", "a<b", "end.", "end ", "a\u{7}"] {
            assert!(!is_plain_name(bad), "{bad:?}");
        }
        for good in ["part", "Bracket plate", "a.b", "Ünïcode"] {
            assert!(is_plain_name(good), "{good:?}");
        }
    }

    #[test]
    fn the_extension_the_dialog_adds_is_not_doubled() {
        assert_eq!(strip_extension("part.solidcraft", false), "part");
        assert_eq!(strip_extension("part.SOLIDCRAFT", false), "part");
        assert_eq!(strip_extension("part.json", true), "part");
        assert_eq!(strip_extension("part.json", false), "part.json");
    }
}
