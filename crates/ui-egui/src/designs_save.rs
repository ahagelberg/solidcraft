//! Save As inside a repository: the panel's own dialog instead of the system file dialog, so a
//! design can only be written in the repository it browses.
//!
//! It opens on the folder the panel shows and lets the repository's own folders be walked into
//! (the breadcrumb never goes above the repository), with the design's name ready to edit and a
//! choice between the container format and plain JSON. A name already taken asks for Replace.

use std::path::PathBuf;

use egui::{RichText, vec2};
use serde_json::json;

use crate::SolveApp;
use crate::theme::Tokens;

/// The open Save As window.
pub struct Dialog {
    /// The repositories and the folder in one of them (the same browser Move and Copy show).
    browser: crate::designs_browse::Browser,
    name: String,
    /// Plain JSON instead of the container format.
    json: bool,
    /// The name field still has to select all (it is focused on the first frame).
    select_all: bool,
    /// A name that is already taken: the next Save with it replaces the file.
    replace: Option<PathBuf>,
}

/// Open it for the open design; false (and nothing opened) when there is no repository to save in,
/// so the caller falls back to the host's picker.
pub fn open(app: &mut SolveApp, json: bool) -> bool {
    let start = crate::designs_panel::save_folder(app).map(PathBuf::from);
    let Some(browser) = crate::designs_browse::Browser::new(app, start.as_deref()) else { return false };
    app.save_to_repo = Some(Dialog { browser, name: app.session.doc.name.clone(), json, select_all: true, replace: None });
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
        Some(self.browser.folder().join(format!("{stem}.{}", if self.json { "json" } else { solidcraft_engine::io::DESIGN_EXT })))
    }

    /// The name as it will be saved, for the "already there" question.
    fn file_name(&self) -> String {
        self.target().and_then(|t| t.file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_default()
    }

    /// The name typed is one already there: the dialog is asking before it replaces the file.
    pub fn asks_to_replace(&self) -> bool {
        self.replace.is_some() && self.replace.as_deref() == self.target().as_deref()
    }
}

/// The window. Called once per frame while it is open.
pub fn show(app: &mut SolveApp, ctx: &egui::Context) {
    let Some(mut dialog) = app.save_to_repo.take() else { return };
    dialog.browser.sync(ctx);
    let t = Tokens::get();
    let (mut save, mut cancel) = (false, false);
    let shown = dialog.browser.folder().to_path_buf();
    let (closed, _) = crate::ui_kit::Window::new(egui::Id::new("sc_designs_save"), "save", crate::i18n::tr("designs.save_title"))
        .cards(2.5)
        .fields()
        .show(ctx, true, |ui| {
            ui.set_min_width(460.0);
            // The repository, its folders and a new one if wanted: the same browser Move and
            // Copy show, so a save can also go to another repository.
            dialog.browser.show(app, ui);
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
    // Another folder (or repository) means the replace question was about another file.
    if dialog.browser.folder() != shown.as_path() {
        dialog.replace = None;
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
                    dialog.browser.fail(e.to_string());
                    dialog.replace = None;
                }
            }
        }
    }
    if closed {
        cancel = true;
    }
    if cancel {
        return;
    }
    app.save_to_repo = Some(dialog);
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

    /// Save As offers every repository: choosing another one saves there instead.
    #[test]
    fn save_as_can_write_into_another_repository() {
        let mut h = crate::scenario::Harness::new();
        let (first, second) = (h.dir.join("first"), h.dir.join("second"));
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        h.app.preferences.repositories = vec![
            crate::prefs::Repository { id: "a".into(), name: "A".into(), description: String::new(), path: first.to_string_lossy().into_owned() },
            crate::prefs::Repository { id: "b".into(), name: "B".into(), description: String::new(), path: second.to_string_lossy().into_owned() },
        ];
        h.app.preferences.active_repository = "a".into();
        assert!(open(&mut h.app, false));
        let dialog = h.app.save_to_repo.as_mut().expect("the Save As window did not open");
        assert_eq!(dialog.browser.folder(), first, "Save As did not start in the active repository");
        dialog.browser.choose("b");
        dialog.name = "Plate".into();
        let target = dialog.target().expect("no target in the chosen repository");
        assert_eq!(target, second.join(format!("Plate.{}", solidcraft_engine::io::DESIGN_EXT)));
    }

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
