//! Project Properties: a project folder's name, description and custom thumbnail.

use egui::{Color32, Sense, pos2, vec2};
use serde_json::json;

use crate::SolveApp;
use crate::theme::Tokens;

/// The picture's box in the dialog; the row it sits in is as tall as it.
const THUMBNAIL: [f32; 2] = [160.0, 100.0];

/// The open Properties window.
pub struct Properties {
    pub path: String,
    pub name: String,
    opened_name: String,
    pub description: String,
    pub source: Option<String>,
    pub remove_thumbnail: bool,
    pub error: Option<String>,
    preview: Option<egui::TextureHandle>,
    loaded: bool,
}

impl Properties {
    pub fn new(path: String, name: String, description: String) -> Properties {
        Properties {
            opened_name: name.clone(),
            path,
            name,
            description,
            source: None,
            remove_thumbnail: false,
            error: None,
            preview: None,
            loaded: false,
        }
    }

    /// The picture to show: the chosen file, or the project's own thumbnail.
    fn load_preview(&mut self, app: &SolveApp, ctx: &egui::Context) {
        self.preview = None;
        if self.remove_thumbnail {
            return;
        }
        let image = match &self.source {
            Some(source) => decode_source(source),
            None => {
                let bytes = std::fs::read(solidcraft_engine::io::project::thumbnail_path(std::path::Path::new(&self.path))).ok();
                bytes.and_then(|b| {
                    crate::designs_panel::decode_picture(
                        b,
                        solidcraft_engine::io::project::THUMBNAIL_SIZE[0],
                        solidcraft_engine::io::project::THUMBNAIL_SIZE[1],
                    )
                })
            }
        };
        let _ = app;
        if let Some(image) = image {
            self.preview = Some(ctx.load_texture(format!("designs-project:{}", self.path), image, egui::TextureOptions::LINEAR));
        }
    }
}

/// The window. Called once per frame when it is open.
pub fn show(app: &mut SolveApp, ctx: &egui::Context) {
    let Some(mut properties) = app.designs.properties.take() else { return };
    if !properties.loaded {
        properties.load_preview(app, ctx);
        properties.loaded = true;
    }
    let t = Tokens::get();
    let (mut ok, mut cancel, mut choose, mut remove) = (false, false, false, false);
    let ready = !properties.name.trim().is_empty();
    let (closed, action) = crate::ui_kit::Window::new(egui::Id::new("sc_designs_properties"), "box", crate::i18n::tr("designs.project_properties"))
        .cards(2.5)
        .actions(crate::i18n::tr("designs.ok"), crate::i18n::tr("designs.cancel"), None)
        .show(ctx, ready, |ui| {
            // The kit's own rows: the label column is the form's, so nothing is elided and the
            // fields fill the card.
            let (_, enter) = crate::ui_kit::text_row(ui, crate::i18n::tr("designs.name"), &mut properties.name, "");
            if enter {
                ok = true;
            }
            crate::ui_kit::multiline_text_field(
                ui,
                crate::i18n::tr("designs.description"),
                egui::Id::new("sc_designs_properties_description"),
                &mut properties.description,
            );
            // The picture with its two actions beside it, at the top of the row.
            crate::ui_kit::sized_row(ui, crate::i18n::tr("designs.thumbnail"), THUMBNAIL[1], |ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(THUMBNAIL[0], THUMBNAIL[1]), Sense::hover());
                match &properties.preview {
                    Some(texture) => {
                        ui.painter().image(texture.id(), rect, egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                    }
                    None => {
                        ui.painter().rect_filled(rect, 4.0, t.field);
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            crate::i18n::tr("designs.no_thumbnail"),
                            egui::FontId::proportional(11.0),
                            t.text_dim,
                        );
                    }
                }
                ui.add_space(t.space_8);
                ui.vertical(|ui| {
                    if crate::ui_kit::button(ui, crate::i18n::tr("designs.choose_image"), crate::ui_kit::Button::Normal).clicked() {
                        choose = true;
                    }
                    if crate::ui_kit::button(ui, crate::i18n::tr("designs.remove_image"), crate::ui_kit::Button::Normal).clicked() {
                        remove = true;
                    }
                });
            });
            if let Some(error) = &properties.error {
                crate::ui_kit::status_line(ui, error, crate::ui_kit::StatusTone::Error);
            }
        });
    match action {
        crate::ui_kit::Action::Ok => ok = true,
        crate::ui_kit::Action::Cancel => cancel = true,
        crate::ui_kit::Action::Apply | crate::ui_kit::Action::None => {}
    }
    let open = !closed;
    if choose {
        app.pick_open_for(&["png", "jpg", "jpeg", "webp"], |app, path| {
            let Some(path) = path else { return };
            if let Some(properties) = app.designs.properties.as_mut() {
                properties.source = Some(path);
                properties.remove_thumbnail = false;
                properties.preview = None;
                properties.loaded = false;
            }
        });
    }
    if remove {
        properties.remove_thumbnail = true;
        properties.source = None;
        properties.preview = None;
    }
    let mut done = false;
    if ok {
        let mut path = properties.path.clone();
        if properties.name.trim() != properties.opened_name.trim() {
            match app.run("designs.rename", json!({ "path": path, "name": properties.name.trim() })) {
                Ok(v) => {
                    if let Some(new) = v.get("path").and_then(serde_json::Value::as_str) {
                        path = new.to_string();
                    }
                }
                Err(e) => properties.error = Some(e),
            }
        }
        if properties.error.is_none() {
            let mut params = json!({ "path": path, "description": properties.description });
            if properties.remove_thumbnail {
                params["remove_thumbnail"] = json!(true);
            }
            if let Some(source) = &properties.source {
                params["thumbnail_source"] = json!(source);
            }
            match app.run("designs.project_set", params) {
                Ok(_) => {
                    app.designs.refresh_after_change(&properties.path);
                    done = true;
                }
                Err(e) => properties.error = Some(e),
            }
        }
    }
    if cancel || !open {
        done = true;
    }
    if !done {
        app.designs.properties = Some(properties);
    }
}

/// A chosen picture, decoded with the same limits the thumbnail writer uses.
fn decode_source(source: &str) -> Option<egui::ColorImage> {
    let meta = std::fs::metadata(source).ok()?;
    if !meta.is_file() || meta.len() > solidcraft_engine::io::project::MAX_SOURCE_BYTES {
        return None;
    }
    let bytes = std::fs::read(source).ok()?;
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(solidcraft_engine::io::project::MAX_SOURCE_SIDE);
    limits.max_image_height = Some(solidcraft_engine::io::project::MAX_SOURCE_SIDE);
    limits.max_alloc =
        Some(u64::from(solidcraft_engine::io::project::MAX_SOURCE_SIDE) * u64::from(solidcraft_engine::io::project::MAX_SOURCE_SIDE) * 4);
    reader.limits(limits);
    let image = reader.decode().ok()?.to_rgba8();
    Some(egui::ColorImage::from_rgba_unmultiplied([image.width() as usize, image.height() as usize], image.as_raw()))
}
