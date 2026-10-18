//! Preferences > Designs: the folders the Designs panel browses (repositories), edited inline.

use egui::{RichText, vec2};

use crate::SolveApp;
use crate::prefs::Repository;
use crate::theme::Tokens;

/// The page. Edits go straight into `app.preferences`, so Cancel and Restore Defaults work like
/// every other page (the dialog snapshots the whole `Prefs`).
pub fn page(app: &mut SolveApp, ui: &mut egui::Ui) {
    let t = Tokens::get();
    let repositories_label = app.tr("designs.repositories").to_string();
    let note = app.tr("designs.repositories_note").to_string();
    let default_label = app.tr("designs.default_repository").to_string();
    let name_label = app.tr("designs.name").to_string();
    let description_label = app.tr("designs.description").to_string();
    let folder_label = app.tr("designs.folder").to_string();
    let browse_label = app.tr("designs.browse").to_string();
    let remove_label = app.tr("designs.remove").to_string();
    let add_label = app.tr("designs.add_repository").to_string();
    let empty_label = app.tr("designs.no_repositories").to_string();
    let duplicate_label = app.tr("designs.duplicate_name").to_string();

    ui.label(RichText::new(repositories_label).size(15.0).strong().color(t.text));
    ui.add_space(2.0);
    ui.label(RichText::new(note).size(11.0).color(t.text_dim));
    ui.add_space(8.0);

    // Take the list out while it is edited: the page needs `app` for labels, the picker and the
    // translations, so it must not hold a mutable borrow of it.
    let mut repositories = std::mem::take(&mut app.preferences.repositories);
    let mut active = std::mem::take(&mut app.preferences.active_repository);
    let mut remove = None;
    let mut browse = None;
    if repositories.is_empty() {
        ui.label(RichText::new(empty_label).color(t.text_dim));
        ui.add_space(6.0);
    }
    for (index, repository) in repositories.iter_mut().enumerate() {
        ui.group(|ui| {
            ui.horizontal(|ui| {
                let chosen = if active.is_empty() { index == 0 } else { active == repository.id };
                if ui.radio(chosen, default_label.as_str()).clicked() {
                    active = repository.id.clone();
                }
                ui.add(egui::TextEdit::singleline(&mut repository.name).desired_width(180.0).hint_text(name_label.as_str()));
                if ui.button(remove_label.as_str()).clicked() {
                    remove = Some(index);
                }
            });
            ui.add(egui::TextEdit::singleline(&mut repository.description).desired_width(f32::INFINITY).hint_text(description_label.as_str()));
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut repository.path).desired_width(360.0).hint_text(folder_label.as_str()));
                if ui.button(browse_label.as_str()).clicked() {
                    browse = Some(repository.id.clone());
                }
            });
            if repository.path.is_empty() {
                ui.label(RichText::new(app.tr("designs.choose_folder")).size(11.0).color(t.text_dim));
            }
        });
        ui.add_space(6.0);
    }
    if ui.add(egui::Button::new(add_label.as_str()).min_size(vec2(120.0, 24.0))).clicked() {
        let id = next_id(&repositories);
        repositories.push(Repository {
            id: id.clone(),
            name: format!("{} {}", app.tr("designs.repository"), repositories.len() + 1),
            description: String::new(),
            path: String::new(),
        });
        active = id;
    }
    ui.add_space(4.0);
    if has_duplicate_names(&repositories) {
        ui.label(RichText::new(duplicate_label.as_str()).size(11.0).color(t.warning));
    }

    if let Some(index) = remove {
        if let Some(gone) = repositories.get(index)
            && active == gone.id
        {
            active.clear();
        }
        if index < repositories.len() {
            repositories.remove(index);
        }
    }
    app.preferences.repositories = repositories;
    app.preferences.active_repository = active;
    if let Some(id) = browse {
        app.pick_folder(move |app, path| {
            let Some(path) = path else { return };
            if let Some(repository) = app.preferences.repositories.iter_mut().find(|r| r.id == id) {
                repository.path = path;
            }
        });
    }
}

/// A fresh `repo-N` that is not in use.
fn next_id(repositories: &[Repository]) -> String {
    for n in 1..=1024 {
        let id = format!("repo-{n}");
        if !repositories.iter().any(|r| r.id == id) {
            return id;
        }
    }
    "repo".into()
}

fn has_duplicate_names(repositories: &[Repository]) -> bool {
    let mut seen = std::collections::BTreeSet::new();
    repositories.iter().any(|r| {
        let name = r.name.trim().to_ascii_lowercase();
        !name.is_empty() && !seen.insert(name)
    })
}
