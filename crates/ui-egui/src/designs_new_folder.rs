//! The New Folder window: a name for a new folder in a repository folder.
//!
//! The panel's New Folder button and a folder's own menu both come here, so a folder is made
//! the same way wherever it is asked for (the menu makes it inside the folder it was opened on).

use std::path::PathBuf;

use serde_json::json;

use crate::SolveApp;

/// An open New Folder window.
pub struct Dialog {
    /// The folder the new one goes in.
    pub folder: PathBuf,
    /// Its name in the panel, for the title.
    pub folder_name: String,
    pub name: String,
    /// The name field still has to take the keyboard.
    focus: bool,
    error: Option<String>,
}

/// Open it for a folder (the repository's root, the folder the panel shows, or a folder's row).
pub fn open(app: &mut SolveApp, folder: &str) {
    let folder = PathBuf::from(folder);
    let folder_name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    app.new_folder_dialog = Some(Dialog { folder, folder_name, name: String::new(), focus: true, error: None });
}

/// The window. Called once per frame while it is open.
pub fn show(app: &mut SolveApp, ctx: &egui::Context) {
    let Some(mut dialog) = app.new_folder_dialog.take() else { return };
    let (mut create, mut cancel) = (false, false);
    let title = crate::i18n::format("designs.new_folder_in", &[("name", &dialog.folder_name)]);
    let ready = !dialog.name.trim().is_empty();
    let (closed, action) = crate::ui_kit::Window::new(egui::Id::new("sc_designs_new_folder"), "folder", &title)
        .cards(2.0)
        .actions(crate::i18n::tr("designs.create"), crate::i18n::tr("designs.cancel"), None)
        .show(ctx, ready, |ui| {
            let id = egui::Id::new("designs-new-folder-name");
            let response = ui.add(
                egui::TextEdit::singleline(&mut dialog.name)
                    .id(id)
                    .desired_width(f32::INFINITY)
                    .hint_text(crate::i18n::tr("designs.name")),
            );
            crate::scenario::publish_handle("designs:new_folder:name", response.rect.center());
            if dialog.focus {
                response.request_focus();
                dialog.focus = false;
            }
            if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                create = true;
            }
            if let Some(error) = &dialog.error {
                crate::ui_kit::status_line(ui, error, crate::ui_kit::StatusTone::Error);
            }
        });
    match action {
        crate::ui_kit::Action::Ok => create = true,
        crate::ui_kit::Action::Cancel => cancel = true,
        crate::ui_kit::Action::Apply | crate::ui_kit::Action::None => {}
    }
    let open = !closed;
    if create {
        let path = dialog.folder.join(dialog.name.trim());
        match app.run("designs.folder_new", json!({ "path": path.to_string_lossy() })) {
            Ok(_) => {
                crate::designs_panel::reload(app);
                cancel = true;
            }
            Err(e) => dialog.error = Some(crate::designs_panel::plain(&e.to_string()).to_string()),
        }
    }
    if !open || cancel {
        return;
    }
    app.new_folder_dialog = Some(dialog);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window makes its folder in the folder it was opened on, and says why not when it
    /// cannot (the name is already there).
    #[test]
    fn the_window_makes_the_folder_in_the_folder_it_was_opened_on() {
        let mut h = crate::scenario::Harness::new();
        let parts = h.dir.join("parts");
        std::fs::create_dir_all(&parts).unwrap();
        open(&mut h.app, &parts.to_string_lossy());
        h.frames(1);
        h.step(&json!({ "text": "Brackets" })).unwrap();
        h.step(&json!({ "key": "Enter" })).unwrap();
        h.frames(1);
        assert!(parts.join("Brackets").is_dir(), "the window made no folder");
        assert!(h.app.new_folder_dialog.is_none(), "the window stayed open");

        // The same name again is refused, with the reason, and the window stays open.
        open(&mut h.app, &parts.to_string_lossy());
        h.frames(1);
        h.step(&json!({ "text": "Brackets" })).unwrap();
        h.step(&json!({ "key": "Enter" })).unwrap();
        h.frames(1);
        let dialog = h.app.new_folder_dialog.as_ref().expect("the window closed on a refusal");
        assert!(dialog.error.is_some(), "the refusal is not shown");
    }
}
