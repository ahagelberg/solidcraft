//! The Designs panel against a real folder: it lists a repository, makes a folder, walks into it,
//! and renames and deletes a design through the rows' context menus.
//!
//! The panel belongs to the desktop host (`Services::designs_panel`), so these tests turn it on
//! and point a repository at the scenario's scratch folder (`$DIR`).

use serde_json::json;
use solidcraft_ui_egui::prefs::Repository;
use solidcraft_ui_egui::scenario::Harness;

/// A harness whose Designs panel shows the scratch folder as its only repository.
fn panel() -> Harness {
    let mut h = Harness::new();
    h.app.services.designs_panel = true;
    h.app.preferences.repositories =
        vec![Repository { id: "test".into(), name: "Test".into(), description: String::new(), path: h.dir.to_string_lossy().into_owned() }];
    h.app.preferences.active_repository = "test".into();
    h.frames(4);
    h
}

/// The scratch folder's entries, for failure messages.
fn listing(h: &Harness) -> Vec<String> {
    std::fs::read_dir(&h.dir).map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default()
}

#[test]
fn a_repository_folder_that_cannot_be_made_is_reported_not_spun_on() {
    let mut h = Harness::new();
    h.app.services.designs_panel = true;
    // `blocker` is a file, so `blocker/child` can never be made.
    std::fs::write(h.dir.join("blocker"), b"not a folder").unwrap();
    let blocked = h.dir.join("blocker/child");
    h.app.preferences.repositories =
        vec![Repository { id: "test".into(), name: "Test".into(), description: String::new(), path: blocked.to_string_lossy().into_owned() }];
    h.app.preferences.active_repository = "test".into();
    h.frames(8);

    // The panel says the folder is not available, with the reason, and is not waiting on it.
    h.run(&json!([
        { "expect": { "count": { "name": "designs:unavailable", "value": 1 } } },
        { "expect": { "count": { "name": "designs:loading", "value": 0 } } }
    ]))
    .unwrap();
    assert!(!blocked.exists(), "{} was made after all", blocked.display());

    // Try Again once the way is clear makes the folder and lists it.
    std::fs::remove_file(h.dir.join("blocker")).unwrap();
    h.run(&json!([
        { "widget": "Try Again" },
        { "until": { "count": { "name": "designs:unavailable", "value": 0 } } },
        { "expect": { "count": { "name": "designs:folders", "value": 0 } } }
    ]))
    .unwrap();
    assert!(blocked.is_dir(), "Try Again did not make {}: {:?}", blocked.display(), listing(&h));
}

#[test]
fn the_switch_turns_the_button_the_panel_and_the_save_folder_off_and_on() {
    // Off (the preference): no button, no panel, and Save As starts where the picker was.
    let mut h = Harness::new();
    h.app.services.designs_panel = true;
    h.app.preferences.repositories =
        vec![Repository { id: "test".into(), name: "Test".into(), description: String::new(), path: h.dir.to_string_lossy().into_owned() }];
    h.app.preferences.active_repository = "test".into();
    h.app.preferences.repositories_panel = false;
    h.frames(4);
    assert!(solidcraft_ui_egui::scenario::handle_at("appbar:designs").is_none(), "the button is there with the switch off");
    // Nothing of the panel is drawn at all (no rows, no counts).
    h.run(&json!([{ "expect": { "count": { "name": "designs:folders", "value": null } } }])).unwrap();
    h.run(&json!([{ "click": { "handle": "appbar:folder" } }, { "widget": "Save As…" }])).unwrap();
    assert!(h.app.save_to_repo.is_none(), "the repository dialog opened with the switch off");
    // The switch off means the host's own picker, as before.
    h.run(&json!([{ "key": "Escape" }])).ok();

    // On: the button, the panel with the repository's rows, and Save As through the repository.
    let mut h = panel();
    assert!(solidcraft_ui_egui::scenario::handle_at("appbar:designs").is_some(), "no button with the switch on");
    h.run(&json!([{ "expect": { "count": { "name": "designs:folders", "value": 0 } } }])).unwrap();
    h.run(&json!([{ "click": { "handle": "appbar:folder" } }, { "widget": "Save As…" }])).unwrap();
    assert!(h.app.save_to_repo.is_some(), "Save As did not use the repository with the switch on");
}

#[test]
fn the_panel_lists_a_repository_and_makes_a_folder() {
    let mut h = panel();
    // The panel makes the repository folder when it opens, and lists it empty.
    assert!(h.dir.is_dir(), "{} is not a folder", h.dir.display());
    h.run(&json!([{ "expect": { "count": { "name": "designs:folders", "value": 0 } } }])).unwrap();

    h.run(&json!([
        { "widget": "New Folder" },
        { "text": "Brackets" },
        { "key": "Enter" },
        { "until": { "count": { "name": "designs:folders", "value": 1 } } }
    ]))
    .unwrap();
    assert!(h.dir.join("Brackets").is_dir(), "New Folder made no folder: {:?}", listing(&h));

    // A folder opens on a double click, and the breadcrumb walks back out of it.
    h.run(&json!([
        { "call": "engine.execute", "params": { "command": "file.save_as", "params": { "path": "$DIR/Brackets/inner.solidcraft" } } },
        { "click": { "handle": "designs:row:Brackets" }, "double": true },
        { "until": { "count": { "name": "designs:designs", "value": 1 } } },
        { "expect": { "count": { "name": "designs:folders", "value": 0 } } },
        { "click": { "handle": "designs:breadcrumb:root" } },
        { "until": { "count": { "name": "designs:folders", "value": 1 } } },
        { "expect": { "count": { "name": "designs:designs", "value": 0 } } }
    ]))
    .unwrap();
}

#[test]
fn f2_renames_the_picked_row_like_a_file_browser() {
    let mut h = panel();
    h.run(&json!([
        { "call": "engine.execute", "params": { "command": "file.save_as", "params": { "path": "$DIR/plate.solidcraft" } } },
        { "until": { "count": { "name": "designs:designs", "value": 1 } } }
    ]))
    .unwrap();

    // Pick the row (as a click does) and press F2: the name is in an editor, the whole of it
    // selected, so typing replaces it without clearing it first.
    h.run(&json!([
        { "click": { "handle": "designs:row:plate" } },
        { "key": "F2" },
        { "text": "dish" },
        { "key": "Enter" },
        { "until": { "count": { "name": "designs:designs", "value": 1 } } }
    ]))
    .unwrap();
    assert!(h.dir.join("dish.solidcraft").is_file(), "F2 did not rename: {:?}", listing(&h));
    assert!(!h.dir.join("plate.solidcraft").exists(), "the old file stayed: {:?}", listing(&h));
}

#[test]
fn the_panel_can_be_dragged_wider_and_remembers_it() {
    let mut h = panel();
    let before = solidcraft_ui_egui::scenario::rect_of("designs:panel").expect("the panel was drawn");
    let (from, to) = ((before.right() - 1.0, before.center().y), (before.right() + 79.0, before.center().y));
    h.run(&json!([{ "drag": [[from.0, from.1], [to.0, to.1]], "steps": 8 }])).unwrap();
    let after = solidcraft_ui_egui::scenario::rect_of("designs:panel").expect("the panel was drawn");
    assert!(after.width() > before.width() + 50.0, "the panel is {:.0} px wide, was {:.0}", after.width(), before.width());
    let width = h.app.preferences.designs_width;
    assert!((width - after.width()).abs() < 1.0, "the preference is {width}, the panel {}", after.width());

    // A fresh app reads the width back.
    let mut other = Harness::new();
    other.app.services.designs_panel = true;
    other.app.load_prefs(&h.app.prefs());
    other.frames(2);
    let shown = solidcraft_ui_egui::scenario::rect_of("designs:panel").map(|r| r.width()).unwrap_or(0.0);
    assert!((shown - width).abs() < 1.0, "a new app shows the panel {shown} px wide, want {width}");
}

#[test]
fn save_as_stays_in_the_repository_and_starts_where_the_panel_shows() {
    let mut h = panel();
    // A subfolder, opened in the panel.
    h.run(&json!([
        { "widget": "New Folder" },
        { "text": "Parts" },
        { "key": "Enter" },
        { "until": { "count": { "name": "designs:folders", "value": 1 } } },
        { "click": { "handle": "designs:row:Parts" }, "double": true },
        { "until": { "count": { "name": "designs:folders", "value": 0 } } }
    ]))
    .unwrap();

    // Save As: the repository's dialog, not the host's picker.
    h.run(&json!([{ "click": { "handle": "appbar:folder" } }, { "widget": "Save As…" }])).unwrap();
    assert!(h.app.save_to_repo.is_some(), "the system picker was used instead of the repository's dialog");

    // A name with a folder in it cannot leave the folder shown: nothing saves.
    h.run(&json!([
        { "click": { "handle": "designs:save:name" } },
        { "key": "a", "cmd": true },
        { "text": "../escape" },
        { "click": { "handle": "designs:save:save" } }
    ]))
    .unwrap();
    assert!(h.app.save_to_repo.is_some(), "a name with a folder in it closed the dialog");
    assert!(!h.dir.join("escape.solidcraft").exists() && !h.dir.join("..").join("escape.solidcraft").exists());

    // A plain name saves in the folder the panel showed, and the dialog closes.
    h.run(&json!([
        { "click": { "handle": "designs:save:name" } },
        { "key": "a", "cmd": true },
        { "text": "plate" },
        { "key": "Enter" },
        { "until": { "count": { "name": "designs:designs", "value": 1 } } }
    ]))
    .unwrap();
    assert!(h.dir.join("Parts/plate.solidcraft").is_file(), "saved in the wrong place: {:?}", listing(&h));
    assert!(h.app.save_to_repo.is_none(), "the dialog stayed open after saving");

    // The same name again asks first: one click is the question, the second replaces it.
    h.run(&json!([{ "click": { "handle": "appbar:folder" } }, { "widget": "Save As…" }])).unwrap();
    h.run(&json!([
        { "click": { "handle": "designs:save:name" } },
        { "key": "a", "cmd": true },
        { "text": "plate" },
        { "click": { "handle": "designs:save:save" } }
    ]))
    .unwrap();
    assert!(h.app.save_to_repo.as_ref().is_some_and(|d| d.asks_to_replace()), "the first click replaced it");
    h.run(&json!([{ "click": { "handle": "designs:save:save" } }])).unwrap();
    assert!(h.app.save_to_repo.is_none(), "the second click did not replace");
}

#[test]
fn importable_files_are_listed_and_the_panel_shows_on_the_start_page() {
    let mut h = panel();
    // A STEP file next to a design: both rows, the STEP one from the importer.
    std::fs::write(h.dir.join("bracket.step"), b"ISO-10303-21;\n").unwrap();
    h.run(&json!([
        { "call": "engine.execute", "params": { "command": "file.save_as", "params": { "path": "$DIR/plate.solidcraft" } } },
        { "until": { "count": { "name": "designs:designs", "value": 2 } } }
    ]))
    .unwrap();
    let rows = solidcraft_ui_egui::scenario::handles("designs:row:");
    let names: Vec<&str> = rows.iter().map(|(n, _)| n.trim_start_matches("designs:row:")).collect();
    assert!(names.contains(&"bracket") && names.contains(&"plate"), "{names:?}");

    // The panel is on the start page too (the button hides it there like everywhere else).
    h.run(&json!([{ "call": "ui.home", "params": { "open": true } }])).unwrap();
    h.frames(3);
    assert!(solidcraft_ui_egui::scenario::rect_of("designs:panel").is_some(), "no panel on the start page");
    assert!(solidcraft_ui_egui::scenario::handle_at("designs:row:bracket").is_some(), "no STEP row on the start page");
    h.run(&json!([{ "click": { "handle": "appbar:designs" } }])).unwrap();
    h.frames(2);
    assert!(solidcraft_ui_egui::scenario::rect_of("designs:panel").is_none(), "the button did not hide the start page's panel");
}

#[test]
fn a_row_renames_and_deletes_its_design() {
    let mut h = panel();
    h.run(&json!([
        { "call": "engine.execute", "params": { "command": "file.save_as", "params": { "path": "$DIR/plate.solidcraft" } } },
        { "until": { "count": { "name": "designs:designs", "value": 1 } } }
    ]))
    .unwrap();
    assert!(h.dir.join("plate.solidcraft").is_file(), "not saved: {:?}", listing(&h));

    // Rename from the row's context menu: the file follows the name.
    h.run(&json!([
        { "click": { "handle": "designs:row:plate" }, "button": "right" },
        { "widget": "Rename" },
        { "key": "a", "cmd": true },
        { "text": "dish" },
        { "key": "Enter" },
        { "until": { "count": { "name": "designs:designs", "value": 1 } } }
    ]))
    .unwrap();
    assert!(h.dir.join("dish.solidcraft").is_file(), "rename left {:?}", listing(&h));

    // Delete asks first, then removes the file. A design that is open is kept (its row's Delete
    // is disabled), so the document is saved elsewhere first.
    h.run(&json!([
        { "call": "engine.execute", "params": { "command": "file.save_as", "params": { "path": "$DIR/other.solidcraft" } } },
        { "until": { "count": { "name": "designs:designs", "value": 2 } } },
        { "click": { "handle": "designs:row:dish" }, "button": "right" },
        { "widget": "Delete" },
        { "click": { "handle": "designs:confirm:delete" } },
        { "until": { "count": { "name": "designs:designs", "value": 1 } } }
    ]))
    .unwrap();
    assert!(!h.dir.join("dish.solidcraft").exists(), "delete left {:?}", listing(&h));
}
