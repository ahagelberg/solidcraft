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
