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
    // The listing is a background job: wait for it rather than guess a frame count.
    h.run(&json!([{ "until": { "count": { "name": "designs:folders", "value": 0 } } }])).unwrap();
    h
}

#[test]
fn designs_panel_spans_from_the_titlebar_and_shifts_the_toolbar() {
    let _h = panel();
    let browser = solidcraft_ui_egui::scenario::rect_of("designs:panel").expect("the Designs panel was drawn");
    let toolbar = solidcraft_ui_egui::scenario::rect_of("toolbar").expect("the toolbar was drawn");
    assert!((browser.top() - toolbar.top()).abs() < 1.0, "browser starts at {}, toolbar at {}", browser.top(), toolbar.top());
    assert!(browser.right() <= toolbar.left() + 1.0, "toolbar starts at {}, browser ends at {}", toolbar.left(), browser.right());
    assert!(browser.height() > toolbar.height(), "browser height {}, toolbar height {}", browser.height(), toolbar.height());
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
        { "until": { "count": { "name": "designs:unavailable", "value": 1 } } },
        { "expect": { "count": { "name": "designs:loading", "value": 0 } } }
    ]))
    .unwrap();
    assert!(!blocked.exists(), "{} was made after all", blocked.display());

    // Try Again once the way is clear makes the folder and lists it. The list is the thing to
    // wait for: while a listing runs the rows are not drawn at all (no `designs:folders`), so
    // "the error is gone" alone could be seen mid-listing.
    std::fs::remove_file(h.dir.join("blocker")).unwrap();
    h.run(&json!([
        { "widget": "Try Again" },
        { "until": { "count": { "name": "designs:folders", "value": 0 } } },
        { "expect": { "count": { "name": "designs:unavailable", "value": 0 } } }
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
    assert!(solidcraft_ui_egui::scenario::handle_at("designs:folder_settings").is_none(), "repository root has folder settings");

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
        { "until": { "shown_handles": ["designs:row:Brackets"] } },
        { "click": { "handle": "designs:row:Brackets" }, "double": true },
        { "until": { "count": { "name": "designs:designs", "value": 1 } } },
        { "expect": { "count": { "name": "designs:folders", "value": 0 } } },
        { "expect": { "shown_handles": ["designs:folder_settings"] } },
        { "click": { "handle": "designs:folder_settings" } },
        { "widget": "Project Properties" },
        { "widget": "Cancel" },
        { "click": { "handle": "designs:breadcrumb:root" } },
        { "until": { "count": { "name": "designs:folders", "value": 1 } } },
        { "expect": { "count": { "name": "designs:designs", "value": 0 } } }
    ]))
    .unwrap();
}

#[test]
fn thumbnails_off_gives_one_compact_line_per_row() {
    // The optional compact list: one line per folder or file, with an icon for its kind, and
    // neither dates nor version selectors (those belong to the picture rows).
    let mut h = panel();
    h.run(&json!([
        { "call": "engine.execute", "params": { "command": "designs.folder_new", "params": { "path": "$DIR/Brackets" } } },
        { "call": "engine.execute", "params": { "command": "file.save_as", "params": { "path": "$DIR/plate.solidcraft" } } },
        { "until": { "count": { "name": "designs:folders", "value": 1 } } },
        { "until": { "count": { "name": "designs:designs", "value": 1 } } }
    ]))
    .unwrap();
    h.frames(2);
    assert!(solidcraft_ui_egui::scenario::handle_at("designs:versions:plate").is_some(), "the picture row lost its version selector");

    h.app.preferences.designs_thumbnails = false;
    h.frames(2);
    h.run(&json!([{ "expect": { "count": { "name": "designs:thumbnails_on", "value": 0 } } }])).unwrap();
    assert!(solidcraft_ui_egui::scenario::handle_at("designs:versions:plate").is_none(), "the compact row kept its version selector");
    // One line means one line: the row is short, and only the name is written in it (no date,
    // no "Folder" tag).
    for (name, expected) in [("plate", "plate"), ("Brackets", "Brackets")] {
        let row = solidcraft_ui_egui::scenario::rect_of(&format!("designs:row:{name}")).expect("the row was drawn");
        assert!(row.height() <= 28.0, "{name} is {:.0} pt tall in the compact list", row.height());
        let texts: Vec<String> = h
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) if row.contains(text.pos) => Some(text.galley.job.text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec![expected.to_string()], "{name}: {texts:?}");
    }
}

#[test]
fn the_panel_actions_are_buttons_and_not_text_labels() {
    // The panel's own fill is the header tone, which egui uses for an idle button: a plain
    // `ui.button` here would paint invisible chrome and read as a label. The actions must show
    // the kit's field face (control fill, control border) like every other button.
    let mut h = panel();
    h.frames(2);
    let t = solidcraft_ui_egui::theme::Tokens::get();
    for label in ["New Folder", "Refresh"] {
        let [x, y] = h.widget(label).unwrap_or_else(|| panic!("no {label} button in the actions row"));
        let face = h.shapes.iter().any(|clipped| match &clipped.shape {
            egui::Shape::Rect(rect) => {
                rect.fill == t.control_fill
                    && rect.stroke.width == 1.0
                    && rect.stroke.color == t.control_border
                    && rect.rect.contains(egui::pos2(x as f32, y as f32))
            }
            _ => false,
        });
        assert!(face, "{label} has no button face: it is drawn as a plain label");
    }
    assert_ne!(t.control_fill, t.panel_header, "the fix relies on buttons not sharing the panel's fill");
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
fn clicking_elsewhere_cancels_new_folder_and_rename_edits() {
    let mut h = panel();
    h.run(&json!([
        { "widget": "New Folder" },
        { "text": "abandoned" },
        { "widget": "Refresh" },
        { "widget": "New Folder" },
        { "text": "Kept" },
        { "key": "Enter" },
        { "until": { "count": { "name": "designs:folders", "value": 1 } } }
    ]))
    .unwrap();
    assert!(!h.dir.join("abandoned").exists(), "clicking Refresh created the unfinished folder");
    assert!(h.dir.join("Kept").is_dir(), "the canceled editor blocked starting a new folder");

    h.run(&json!([
        { "call": "engine.execute", "params": { "command": "file.save_as", "params": { "path": "$DIR/plate.solidcraft" } } },
        { "call": "engine.execute", "params": { "command": "file.save_as", "params": { "path": "$DIR/dish.solidcraft" } } },
        { "until": { "count": { "name": "designs:designs", "value": 2 } } },
        { "click": { "handle": "designs:row:plate" }, "button": "right" },
        { "widget": "Rename" },
        { "click": { "handle": "designs:row:dish" } },
        { "key": "F2" },
        { "text": "cup" },
        { "key": "Enter" },
        { "until": { "count": { "name": "designs:designs", "value": 2 } } }
    ]))
    .unwrap();
    assert!(h.dir.join("plate.solidcraft").is_file(), "the unfinished rename was committed");
    assert!(h.dir.join("cup.solidcraft").is_file(), "the outside click did not allow renaming the selected row");
    assert!(!h.dir.join("dish.solidcraft").exists());
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
fn the_row_lists_the_versions_and_an_old_one_asks_before_it_opens_and_saves() {
    let mut h = panel();
    // Two saves: the file now holds version 1 and version 2.
    h.run(&json!([
        { "call": "engine.execute", "params": { "command": "file.save_as", "params": { "path": "$DIR/plate.solidcraft" } } },
        { "call": "engine.execute", "params": { "command": "file.save", "params": {} } },
        { "until": { "count": { "name": "designs:designs", "value": 1 } } }
    ]))
    .unwrap();
    assert!(solidcraft_ui_egui::scenario::handle_at("designs:versions:plate").is_some(), "no version selector on the row");

    // The selector lists both; picking version 1 asks first.
    h.run(&json!([{ "click": { "handle": "designs:versions:plate" } }, { "widget": "Version 1" }])).unwrap();
    assert!(
        matches!(h.app.version_prompt, Some(solidcraft_ui_egui::designs_versions::Prompt::Open { version: 1, versions: 2, .. })),
        "opening version 1 did not ask: {:?}",
        h.app.version_prompt
    );
    h.run(&json!([{ "widget": "Open It" }])).unwrap();
    assert_eq!(h.app.session.version, Some(1), "version 1 did not open");

    // Saving it would delete version 2: the save asks, and only the answer saves.
    h.run(&json!([{ "key": "s", "cmd": true }])).unwrap();
    assert!(
        matches!(h.app.version_prompt, Some(solidcraft_ui_egui::designs_versions::Prompt::SaveDeletesLater { version: 1, versions: 2 })),
        "saving version 1 did not ask: {:?}",
        h.app.version_prompt
    );
    h.run(&json!([{ "widget": "Save" }])).unwrap();
    assert_eq!(h.app.session.version, None, "the save did not go through");
    assert_eq!(h.app.session.versions, 2, "the edited version 1 should be the file's version 2");
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
        { "key": "Enter" },
        { "until": { "count": { "name": "designs:designs", "value": 1 } } }
    ]))
    .unwrap();
    assert!(!h.dir.join("dish.solidcraft").exists(), "delete left {:?}", listing(&h));

    h.run(&json!([
        { "widget": "New Folder" },
        { "text": "empty" },
        { "key": "Enter" },
        { "until": { "count": { "name": "designs:folders", "value": 1 } } },
        { "click": { "handle": "designs:row:empty" }, "button": "right" },
        { "widget": "Delete" },
        { "key": "Space" },
        { "until": { "count": { "name": "designs:folders", "value": 0 } } }
    ]))
    .unwrap();
    assert!(!h.dir.join("empty").exists(), "Space did not activate the focused Delete button");
}

#[test]
fn a_version_is_promoted_to_latest_or_the_older_ones_purged_from_its_menu() {
    let mut h = panel();
    // Three saves: the file holds versions 1, 2 and 3.
    h.run(&json!([
        { "call": "engine.execute", "params": { "command": "file.save_as", "params": { "path": "$DIR/plate.solidcraft" } } },
        { "call": "engine.execute", "params": { "command": "file.save", "params": {} } },
        { "call": "engine.execute", "params": { "command": "file.save", "params": {} } },
        { "until": { "count": { "name": "designs:version:plate", "value": 3.0 } } }
    ]))
    .unwrap();

    // Right-click version 2 in the selector: promote it (version 3 goes) after a question.
    h.run(&json!([
        { "click": { "handle": "designs:versions:plate" } },
        { "widget": "Version 2", "button": "right" },
        { "widget": "Promote to Latest" }
    ]))
    .unwrap();
    assert!(
        matches!(h.app.version_prompt, Some(solidcraft_ui_egui::designs_versions::Prompt::Promote { version: 2, versions: 3, .. })),
        "promoting version 2 did not ask: {:?}",
        h.app.version_prompt
    );
    // Cancelling changes nothing.
    h.run(&json!([{ "click": { "handle": "designs:version_cancel" } }])).unwrap();
    assert_eq!(solidcraft_ui_egui::scenario::count_of("designs:version:plate"), Some(3.0), "cancel changed the file");
    // Agreeing drops version 3 and lists the file again: two versions are left.
    h.run(&json!([
        { "click": { "handle": "designs:versions:plate" } },
        { "widget": "Version 2", "button": "right" },
        { "widget": "Promote to Latest" },
        { "click": { "handle": "designs:version_go" } },
        { "until": { "count": { "name": "designs:version:plate", "value": 2.0 } } }
    ]))
    .unwrap();
    assert_eq!(h.app.session.versions, 2, "the open design still thinks the file holds three versions");

    // Purging older ones from version 2's menu deletes version 1 and renumbers the rest.
    h.run(&json!([
        { "click": { "handle": "designs:versions:plate" } },
        { "widget": "Version 2", "button": "right" },
        { "widget": "Purge Older Versions" },
        { "click": { "handle": "designs:version_go" } },
        { "until": { "count": { "name": "designs:version:plate", "value": 1.0 } } }
    ]))
    .unwrap();
    assert!(h.app.version_prompt.is_none(), "the purge left a question open: {:?}", h.app.version_prompt);
    // One version is left, and the file still opens.
    h.run(&json!([{ "call": "engine.execute", "params": { "command": "doc.open", "params": { "path": "$DIR/plate.solidcraft" } } }])).unwrap();
    assert_eq!(h.app.session.versions, 1, "the purged file does not hold one version");
}

#[test]
fn the_row_reports_the_version_the_file_holds() {
    let mut h = panel();
    h.run(&json!([
        { "call": "engine.execute", "params": { "command": "file.save_as", "params": { "path": "$DIR/plate.solidcraft" } } },
        { "call": "engine.execute", "params": { "command": "file.save", "params": {} } },
        { "until": { "count": { "name": "designs:version:plate", "value": 2.0 } } }
    ]))
    .unwrap();
}
