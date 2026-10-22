//! Move or copy a design into a folder of a repository.
//!
//! The dialog walks the repositories and their own folders; the system's folder picker is never
//! used, so a design cannot be moved or copied somewhere no repository covers. It starts on the
//! repository the design is in (else the one the panel shows) and in the design's own folder.

use std::path::Path;

use egui::{RichText, vec2};
use serde_json::json;

use crate::SolveApp;
use crate::designs_browse::Browser;
use crate::theme::Tokens;

/// An open Move or Copy window.
pub struct Pick {
    /// The file to move or copy, its name, and which of the two this is.
    source: String,
    source_name: String,
    copy: bool,
    /// The repositories and the folder in one of them (the same browser Save As shows).
    browser: Browser,
}

/// Open the window for a design or an importable file.
pub fn open(app: &mut SolveApp, path: &str, copy: bool) {
    let source = Path::new(path);
    // The repository holding the file comes first: a copy stays where the design lives unless
    // the user says otherwise.
    let Some(browser) = Browser::new(app, source.parent()) else {
        app.set_status(crate::i18n::tr("designs.no_repositories"), true);
        return;
    };
    let source_name = source.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    app.designs_pick = Some(Pick { source: path.to_string(), source_name, copy, browser });
}

/// The window. Called once per frame while it is open.
pub fn show(app: &mut SolveApp, ctx: &egui::Context) {
    let Some(mut pick) = app.designs_pick.take() else { return };
    pick.browser.sync(ctx);
    let t = Tokens::get();
    let mut open = true;
    let (mut go, mut cancel) = (false, false);
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
            // The same repositories and folders Save As offers, so the three windows match.
            pick.browser.show(app, ui);
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
    if go {
        let destination = pick.browser.folder().to_path_buf();
        match perform(app, &pick.source, pick.copy, &destination) {
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
            Err(e) => pick.browser.fail(e),
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
        assert_eq!(pick.browser.repository(), "a", "the dialog did not start on the design's repository");
        assert_eq!(pick.browser.folder(), first.join("parts"), "the dialog did not start in the design's folder");

        // Choosing the other repository moves the walk to its root, and the copy lands there.
        {
            let browser = &mut h.app.designs_pick.as_mut().expect("the copy window did not open").browser;
            browser.choose("b");
            assert_eq!((browser.root(), browser.folder()), (second.as_path(), second.as_path()));
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
