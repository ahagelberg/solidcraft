//! Project Properties: a project folder's name, description and custom thumbnail.

use egui::{Color32, RichText, Sense, pos2, vec2};
use serde_json::json;

use crate::SolveApp;
use crate::theme::Tokens;

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
    let mut open = true;
    let (mut ok, mut cancel, mut choose, mut remove) = (false, false, false, false);
    crate::frame::window(ctx, RichText::new(crate::i18n::tr("designs.project_properties")).strong().size(13.0), crate::frame::Width::Normal)
        .id(egui::Id::new("sc_designs_properties"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.set_min_width(420.0);
            egui::Grid::new("sc_designs_properties_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label(crate::i18n::tr("designs.name"));
                ui.add(egui::TextEdit::singleline(&mut properties.name).desired_width(280.0));
                ui.end_row();
                ui.label(crate::i18n::tr("designs.description"));
                ui.add(egui::TextEdit::multiline(&mut properties.description).desired_rows(3).desired_width(280.0));
                ui.end_row();
                ui.label(crate::i18n::tr("designs.thumbnail"));
                ui.vertical(|ui| {
                    let (rect, _) = ui.allocate_exact_size(vec2(160.0, 100.0), Sense::hover());
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
                    ui.horizontal(|ui| {
                        if ui.button(crate::i18n::tr("designs.choose_image")).clicked() {
                            choose = true;
                        }
                        if ui.button(crate::i18n::tr("designs.remove_image")).clicked() {
                            remove = true;
                        }
                    });
                });
                ui.end_row();
            });
            if let Some(error) = &properties.error {
                ui.add_space(4.0);
                ui.label(RichText::new(error).color(t.error).size(11.5));
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(crate::i18n::tr("designs.ok")).clicked() {
                    ok = true;
                }
                if ui.button(crate::i18n::tr("designs.cancel")).clicked() {
                    cancel = true;
                }
            });
        });
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
