//! What a design's versions ask before they act: opening an older version, and saving one (which
//! deletes the versions after it).
//!
//! The questions are asked before anything happens, so a version that is not the current one is
//! never opened or replaced by accident.

use egui::RichText;
use serde_json::json;

use crate::SolveApp;
use crate::theme::Tokens;

/// A question waiting for its answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Prompt {
    /// Open `version` of a design whose file holds `versions`.
    Open { path: String, version: u32, versions: u32 },
    /// Make `version` the latest, deleting the versions after it.
    Promote { path: String, version: u32, versions: u32 },
    /// Delete the versions before `version` (the ones kept renumber from 1).
    PurgeOlder { path: String, version: u32, versions: u32 },
    /// Save the design opened from `version`: the versions after it go.
    SaveDeletesLater { version: u32, versions: u32 },
}

/// The question a Save has to ask first, if any: the design was opened from an older version and
/// saving it deletes the later ones.
pub fn save_prompt(app: &SolveApp) -> Option<Prompt> {
    if app.save_discards_later {
        return None;
    }
    let version = app.session.version?;
    let versions = app.session.versions;
    (versions > version).then_some(Prompt::SaveDeletesLater { version, versions })
}

/// Promote or purge versions of `path` (the user agreed): the panel lists the file again, and an
/// open design updates what it knows of the file it came from.
fn rewrite(app: &mut SolveApp, command: &str, path: &str, version: u32) {
    match app.run(command, json!({ "path": path, "version": version })) {
        Ok(reply) => {
            let versions = reply.get("versions").and_then(serde_json::Value::as_u64).unwrap_or(1).clamp(1, u64::from(u32::MAX)) as u32;
            if app.session.path.as_deref() == Some(path) {
                app.session.versions = versions;
                // An open version keeps its number unless the ones before it went (a purge
                // renumbers from 1); one the operation deleted is no longer a version of the file,
                // so saving what is open now would replace the latest one: say so.
                let promote = command == "designs.version_promote";
                let open = app.session.version;
                app.session.version = open.and_then(|v| match (promote, v.cmp(&version)) {
                    (_, std::cmp::Ordering::Equal) => Some(version.min(versions)),
                    (true, std::cmp::Ordering::Less) => Some(v),
                    (true, std::cmp::Ordering::Greater) => None,
                    (false, std::cmp::Ordering::Greater) => Some(v - version + 1),
                    (false, std::cmp::Ordering::Less) => None,
                });
                if open.is_some() && app.session.version.is_none() {
                    app.set_status(crate::i18n::format("designs.version_gone", &[("name", &app.session.doc.name)]), false);
                }
            }
            app.designs.refresh_after_change(path);
        }
        Err(e) => app.set_status(e, true),
    }
}

/// The window. Called once per frame while a question is open.
pub fn show(app: &mut SolveApp, ctx: &egui::Context) {
    let Some(prompt) = app.version_prompt.clone() else { return };
    let t = Tokens::get();
    let (title, text, go) = match &prompt {
        Prompt::Open { version, versions, .. } => (
            crate::i18n::tr("designs.open_version_title").to_string(),
            crate::i18n::format("designs.open_version_text", &[("version", &version.to_string()), ("versions", &versions.to_string())]),
            crate::i18n::tr("designs.open_it").to_string(),
        ),
        Prompt::Promote { version, versions, .. } => {
            let (from, to) = (version.saturating_add(1), *versions);
            // One version going is said in the singular ("Version 3 is deleted").
            let text = if from == to {
                crate::i18n::format("designs.promote_text_one", &[("version", &version.to_string()), ("from", &from.to_string())])
            } else {
                crate::i18n::format(
                    "designs.promote_text",
                    &[("version", &version.to_string()), ("from", &from.to_string()), ("to", &to.to_string())],
                )
            };
            (crate::i18n::format("designs.promote_title", &[("version", &version.to_string())]), text, crate::i18n::tr("designs.promote").to_string())
        }
        Prompt::PurgeOlder { version, .. } => {
            let to = version.saturating_sub(1);
            // One version going is said in the singular ("Version 1 is deleted").
            let text = if to == 1 {
                crate::i18n::format("designs.purge_text_one", &[("to", &to.to_string()), ("keep", &version.to_string())])
            } else {
                crate::i18n::format("designs.purge_text", &[("from", "1"), ("to", &to.to_string()), ("keep", &version.to_string())])
            };
            (crate::i18n::tr("designs.purge_title").to_string(), text, crate::i18n::tr("designs.delete").to_string())
        }
        Prompt::SaveDeletesLater { version, versions } => {
            let (from, to) = (version.saturating_add(1), *versions);
            let text = if from == to {
                crate::i18n::format("designs.save_version_text_one", &[("from", &from.to_string())])
            } else {
                crate::i18n::format("designs.save_version_text", &[("from", &from.to_string()), ("to", &to.to_string())])
            };
            (crate::i18n::tr("designs.save_version_title").to_string(), text, crate::i18n::tr("ui.save").to_string())
        }
    };
    let mut answer = None;
    let (closed, _) = crate::ui_kit::Window::new(egui::Id::new("sc_designs_versions"), "box", &title).cards(2.0).show(ctx, true, |ui| {
        ui.set_max_width(380.0);
        ui.label(RichText::new(text).size(12.0).color(t.text));
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let go_button = ui.button(go.as_str());
            crate::scenario::publish_handle("designs:version_go", go_button.rect.center());
            if go_button.clicked() {
                answer = Some(true);
            }
            let cancel_button = ui.button(crate::i18n::tr("designs.cancel"));
            crate::scenario::publish_handle("designs:version_cancel", cancel_button.rect.center());
            if cancel_button.clicked() {
                answer = Some(false);
            }
        });
    });
    let open = !closed;
    match answer {
        Some(true) => match prompt {
            Prompt::Open { path, version, .. } => app.open_version_async(&path, version),
            Prompt::Promote { path, version, .. } => rewrite(app, "designs.version_promote", &path, version),
            Prompt::PurgeOlder { path, version, .. } => rewrite(app, "designs.version_purge", &path, version),
            Prompt::SaveDeletesLater { .. } => {
                app.save_discards_later = true;
                crate::toolbar::save(app);
            }
        },
        Some(false) => {}
        None if open => {
            app.version_prompt = Some(prompt);
            return;
        }
        None => {}
    }
    app.version_prompt = None;
}
