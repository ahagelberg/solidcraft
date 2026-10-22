//! The Designs panel: the designs of the active repository, in a folder tree, with project
//! metadata, context menus and drag-to-insert.
//!
//! File work is asynchronous: the listing and the thumbnails run on worker threads and arrive
//! through channels, so a slow or network folder never blocks the window. The panel refreshes
//! on open, navigation, after an action, when the window regains focus, and from its Refresh
//! button; it never polls.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use egui::{Color32, RichText, Sense, pos2, vec2};
use serde_json::{Value, json};

use crate::SolveApp;
use crate::theme::Tokens;

/// A design row being dragged out of the panel; the viewport or the Home page receives it.
#[derive(Clone)]
pub struct DesignDrag {
    pub path: String,
}

/// The panel's transient state (the settings it shows live in `Prefs` and `UiState`).
#[derive(Default)]
pub struct State {
    /// The repository the current listing belongs to.
    repository: String,
    /// The folder shown.
    current: Option<PathBuf>,
    entries: Option<Entries>,
    listing: Option<Receiver<Value>>,
    root_check: Option<(String, Receiver<Result<(), String>>)>,
    thumbnails: HashMap<String, Option<egui::TextureHandle>>,
    thumb_rx: Option<Receiver<(String, Option<egui::ColorImage>)>>,
    error: Option<String>,
    /// The folder the listing asked for (so a failure can be tied to it).
    listing_path: Option<String>,
    /// A folder that could not be made or listed: it is not retried until the user asks (or
    /// goes to another folder), so its message stays on screen instead of a spinner.
    failed: Option<String>,
    rename: Option<Rename>,
    confirm: Option<Confirm>,
    /// A repository root whose existence/create check was done (by path, so corrections are tried).
    created: Option<String>,
    /// The row picked last.
    pub(crate) selected: Option<String>,
    /// Where the panel was drawn last frame (`None` while it is hidden).
    panel_rect: Option<egui::Rect>,
    /// The panel was the last thing clicked in, so F2 renames its rows (not the model's selection).
    focused_panel: bool,
    /// The window was focused last frame (regaining focus refreshes the listing).
    focused: bool,
    /// An action asked for a new listing; the rows must not be put back.
    refresh: bool,
    /// An open Project Properties window.
    pub properties: Option<crate::designs_properties::Properties>,
    /// A version's own menu (right-clicked in the version list).
    version_menu: Option<VersionMenu>,
    /// The tree view's open folders (the repository root included) and their listings: a folder
    /// is read when it is opened, so the whole repository is never listed at once.
    tree: std::collections::BTreeSet<String>,
    tree_entries: HashMap<String, Entries>,
    tree_listing: Option<(String, Receiver<Value>)>,
    /// The tree row clicked last, and when: a second click on it opens or closes it, the way a
    /// file browser does.
    tree_click: Option<(String, f64)>,
}

/// A version's menu: which version, of how many, and where it was asked for.
#[derive(Clone)]
struct VersionMenu {
    path: String,
    version: u32,
    versions: u32,
    at: egui::Pos2,
    /// Opened this frame: the click that opened it is not a click outside it.
    fresh: bool,
}

struct Rename {
    path: String,
    name: String,
    /// The field still has to select the whole name (typing then replaces it).
    select_all: bool,
}

#[derive(Clone)]
struct Confirm {
    path: String,
    name: String,
    folder: bool,
    focus_delete: bool,
}

#[derive(Clone)]
struct Entries {
    path: String,
    folders: Vec<FolderRow>,
    designs: Vec<DesignRow>,
    truncated: bool,
}

#[derive(Clone)]
struct FolderRow {
    name: String,
    path: String,
    description: String,
    has_thumbnail: bool,
}

#[derive(Clone)]
struct DesignRow {
    name: String,
    path: String,
    modified_display: String,
    /// `design`, or what imports it: `step` (STEP, IGES, FreeCAD, Rhino) or `mesh` (3MF, STL,
    /// OBJ, glTF).
    kind: String,
    /// How many versions the file holds (the current one's number).
    versions: u32,
}

impl Entries {
    fn from_json(v: &Value) -> Option<Entries> {
        let path = v.get("path").and_then(Value::as_str)?.to_string();
        let folders = v
            .get("folders")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|f| {
                Some(FolderRow {
                    name: f.get("name").and_then(Value::as_str)?.to_string(),
                    path: f.get("path").and_then(Value::as_str)?.to_string(),
                    description: f.get("description").and_then(Value::as_str).unwrap_or_default().to_string(),
                    has_thumbnail: f.get("has_thumbnail").and_then(Value::as_bool).unwrap_or(false),
                })
            })
            .collect();
        let designs = v
            .get("designs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|d| {
                Some(DesignRow {
                    name: d.get("name").and_then(Value::as_str)?.to_string(),
                    path: d.get("path").and_then(Value::as_str)?.to_string(),
                    modified_display: crate::local_time::format(d.get("modified").and_then(Value::as_u64).unwrap_or(0)),
                    kind: d.get("kind").and_then(Value::as_str).unwrap_or("design").to_string(),
                    versions: d.get("versions").and_then(Value::as_u64).unwrap_or(1).clamp(1, 100_000) as u32,
                })
            })
            .collect();
        Some(Entries { path, folders, designs, truncated: v.get("truncated").and_then(Value::as_bool).unwrap_or(false) })
    }
}

enum Action {
    Open(String),
    /// Open an older version of a design (`version` of `versions`).
    OpenVersion {
        path: String,
        version: u32,
        versions: u32,
    },
    /// Open a version's own menu at `at` (right-clicked in the version list).
    VersionMenu {
        path: String,
        version: u32,
        versions: u32,
        at: egui::Pos2,
    },
    Insert(String),
    Rename {
        path: String,
        name: String,
    },
    Move(String),
    /// Copy a design into a folder the user picks (in any repository).
    CopyTo(String),
    /// A second copy beside the design.
    Duplicate(String),
    /// Make a folder inside this one (the New Folder window).
    NewFolder(String),
    Delete {
        path: String,
        name: String,
        folder: bool,
    },
    Properties {
        path: String,
        name: String,
    },
}

/// How narrow and how wide the panel can be dragged.
pub const WIDTH_MIN: f32 = 180.0;
pub const WIDTH_MAX: f32 = 520.0;

/// Draw the panel (and handle a design dropped from it, even while the panel is hidden).
pub fn show(app: &mut SolveApp, ui: &mut egui::Ui) {
    if !app.services.designs_panel || !app.preferences.repositories_panel {
        app.designs.panel_rect = None;
        app.designs.version_menu = None;
        app.designs.rename = None;
        return;
    }
    accept_drag(app, ui.ctx());
    if !app.ui.show_designs {
        app.designs.panel_rect = None;
        app.designs.rename = None;
        return;
    }
    pump(app, ui.ctx());
    sync(app, ui.ctx());
    let t = Tokens::get();
    let width = app.preferences.designs_width.clamp(WIDTH_MIN, WIDTH_MAX);
    let panel = egui::Panel::left("sc_designs")
        .default_size(width)
        .size_range(WIDTH_MIN..=WIDTH_MAX)
        .resizable(true)
        .frame(egui::Frame::NONE.fill(t.panel_header).inner_margin(egui::Margin::symmetric(6, 6)))
        .show(ui, |ui| {
            control_style(ui, &t);
            repository_header(app, ui, &t);
            ui.add_space(4.0);
            actions(app, ui);
            if app.preferences.designs_tree {
                ui.add_space(2.0);
                ui.separator();
                tree_rows(app, ui, &t);
            } else {
                ui.add_space(4.0);
                breadcrumb(app, ui);
                ui.separator();
                rows(app, ui, &t);
            }
        });
    let rect = panel.response.rect;
    crate::scenario::publish_rect("designs:panel", rect);
    app.designs.panel_rect = Some(rect);
    // The panel has the keyboard once it is clicked in (F2 renames its rows, not the model's).
    if ui.ctx().input(|i| i.pointer.any_pressed()) {
        app.designs.focused_panel = ui.ctx().input(|i| i.pointer.interact_pos()).is_some_and(|p| rect.contains(p));
    }
    let dragged = panel.response.rect.width();
    if dragged.is_finite() && (WIDTH_MIN..=WIDTH_MAX).contains(&dragged) {
        app.preferences.designs_width = dragged;
    }
    confirm_delete(app, ui.ctx());
    version_menu(app, ui.ctx());
    crate::designs_properties::show(app, ui.ctx());
}

/// The panel's controls get the kit's field look. Its own fill is the header tone, which egui
/// also uses for an idle button, so a plain button here would read as a text label.
fn control_style(ui: &mut egui::Ui, t: &Tokens) {
    let v = ui.visuals_mut();
    v.widgets.inactive.weak_bg_fill = t.control_fill;
    v.widgets.inactive.bg_fill = t.control_fill;
    for state in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        state.bg_stroke = egui::Stroke::new(1.0, t.control_border);
        state.corner_radius = t.control_radius.into();
        state.expansion = 0.0;
    }
}

/// F2 over the panel: start renaming the selected row (a design or a folder), the way a file
/// browser does. True when it took the key, so F2 doesn't also rename the model's selection.
pub fn rename_selected(app: &mut SolveApp, ctx: &egui::Context) -> bool {
    if !app.services.designs_panel || !app.preferences.repositories_panel || !app.ui.show_designs {
        return false;
    }
    let hovered = app.designs.panel_rect.is_some_and(|r| ctx.pointer_hover_pos().is_some_and(|p| r.contains(p)));
    if !hovered && !app.designs.focused_panel {
        return false;
    }
    if app.designs.rename.is_some() {
        return true;
    }
    let Some(path) = app.designs.selected.clone() else { return true };
    let from_flat = app
        .designs
        .entries
        .as_ref()
        .map(|entries| entries.folders.iter().map(|f| (&f.path, &f.name)).chain(entries.designs.iter().map(|d| (&d.path, &d.name))));
    let from_tree = app
        .designs
        .tree_entries
        .values()
        .flat_map(|entries| entries.folders.iter().map(|f| (&f.path, &f.name)).chain(entries.designs.iter().map(|d| (&d.path, &d.name))));
    let name = from_flat.into_iter().flatten().chain(from_tree).find(|(p, _)| p.as_str() == path).map(|(_, name)| name.clone());
    if let Some(name) = name {
        app.designs.rename = Some(Rename { path, name, select_all: true });
    }
    true
}

/// The repository shown: the chosen one, else the first.
fn active_repository(app: &SolveApp) -> Option<&crate::prefs::Repository> {
    let id = &app.preferences.active_repository;
    app.preferences.repositories.iter().find(|r| r.id == *id).or_else(|| app.preferences.repositories.first())
}

fn folder_settings_target(app: &SolveApp) -> Option<(String, String)> {
    let current = app.designs.current.as_ref()?;
    let root = PathBuf::from(&active_repository(app)?.path);
    if current == &root || !current.starts_with(&root) {
        return None;
    }
    let name = current.file_name()?.to_string_lossy().into_owned();
    Some((current.to_string_lossy().into_owned(), name))
}

fn open_folder_properties(app: &mut SolveApp, path: String, name: String) {
    let description = app
        .session
        .execute("designs.project_get", &json!({ "path": path }))
        .ok()
        .and_then(|v| v.get("description").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_default();
    app.designs.properties = Some(crate::designs_properties::Properties::new(path, name, description));
}

/// Follow the active repository: reset the listing when it changes, check/create its root once,
/// refresh when the window regains focus, and start a listing when one is due.
fn sync(app: &mut SolveApp, ctx: &egui::Context) {
    let Some(repository) = active_repository(app) else {
        app.designs.error = Some(crate::i18n::tr("designs.no_repositories").to_string());
        app.designs.entries = None;
        return;
    };
    let (id, path) = (repository.id.clone(), repository.path.clone());
    if app.designs.repository != id {
        app.designs.repository = id.clone();
        app.designs.current = None;
        app.designs.entries = None;
        app.designs.thumbnails.clear();
        app.designs.selected = None;
        app.designs.error = None;
        app.designs.failed = None;
        // The tree belongs to a repository: another one starts collapsed again.
        app.designs.tree.clear();
        app.designs.tree_entries.clear();
    }
    if path.is_empty() {
        app.designs.current = None;
        app.designs.entries = None;
        return;
    }
    let path = PathBuf::from(path);
    // The folder shown: the one navigated to, as long as it is inside this repository.
    if !app.designs.current.as_ref().is_some_and(|current| current.starts_with(&path)) {
        app.designs.current = Some(path.clone());
        app.designs.entries = None;
        app.designs.failed = None;
    }
    let root = path.to_string_lossy().into_owned();
    let focused = ctx.input(|i| i.focused);
    if focused && !app.designs.focused {
        app.designs.entries = None;
        // Back in the window: a folder that failed may have been fixed meanwhile.
        app.designs.failed = None;
        app.designs.created = None;
    }
    app.designs.focused = focused;
    // A repository folder that is not there is made, with its parents, the first time the panel
    // shows it (and again if the path is edited to another one).
    if app.designs.created.as_deref() != Some(root.as_str()) {
        app.designs.created = Some(root.clone());
        let (tx, rx) = mpsc::channel();
        let worker_root = root.clone();
        let spawned = std::thread::Builder::new().name("designs-root".into()).spawn(move || {
            let folder = PathBuf::from(&worker_root);
            let result = if folder.is_dir() {
                Ok(())
            } else {
                std::fs::create_dir_all(&folder).map_err(|e| format!("designs.folder_new: {worker_root}: {e}"))
            };
            let _ = tx.send(result);
        });
        match spawned {
            Ok(_) => app.designs.root_check = Some((root.clone(), rx)),
            Err(_) => {
                app.designs.error = Some(crate::i18n::tr("designs.list_failed").to_string());
                app.designs.failed = Some(root);
                app.designs.root_check = None;
                app.designs.entries = None;
                return;
            }
        }
    }
    if app.designs.root_check.as_ref().is_some_and(|(checking, _)| checking == &root) {
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
        return;
    }
    let shown = app.designs.current.clone().unwrap_or(path);
    let shown_text = shown.to_string_lossy().into_owned();
    // A folder that could not be read is not listed again on its own: its message stays until the
    // user tries again or goes elsewhere. The tree view lists the folders it opens instead.
    if !app.preferences.designs_tree
        && app.designs.entries.is_none()
        && app.designs.listing.is_none()
        && app.designs.failed.as_deref() != Some(shown_text.as_str())
    {
        start_listing(app, &shown, ctx);
    }
}

fn start_listing(app: &mut SolveApp, path: &Path, ctx: &egui::Context) {
    let dir = path.to_string_lossy().into_owned();
    let (tx, rx) = mpsc::channel();
    let worker_dir = dir.clone();
    let spawned = std::thread::Builder::new().name("designs-list".into()).spawn(move || {
        let result = std::panic::catch_unwind(|| solidcraft_engine::cmd::designs::list(&worker_dir));
        let value = match result {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => json!({ "error": e.to_string() }),
            Err(_) => json!({ "error": "the folder could not be listed" }),
        };
        let _ = tx.send(value);
    });
    if spawned.is_ok() {
        app.designs.listing = Some(rx);
        app.designs.listing_path = Some(dir);
        app.designs.error = None;
        app.designs.entries = None;
        app.designs.thumbnails.clear();
    } else {
        app.designs.error = Some(crate::i18n::tr("designs.list_failed").to_string());
        app.designs.failed = app.designs.listing_path.clone();
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(50));
}

/// Collect worker results.
fn pump(app: &mut SolveApp, ctx: &egui::Context) {
    let root_result = app.designs.root_check.as_ref().and_then(|(path, rx)| match rx.try_recv() {
        Ok(result) => Some((path.clone(), Ok(result))),
        Err(TryRecvError::Empty) => None,
        Err(TryRecvError::Disconnected) => Some((path.clone(), Err(()))),
    });
    if let Some((path, result)) = root_result {
        app.designs.root_check = None;
        if app.designs.created.as_deref() == Some(path.as_str()) {
            match result {
                Ok(Ok(())) => {
                    app.designs.entries = None;
                    app.designs.error = None;
                    app.designs.failed = None;
                }
                Ok(Err(error)) => {
                    app.designs.error = Some(error);
                    app.designs.failed = Some(path);
                    app.designs.entries = None;
                }
                Err(()) => {
                    app.designs.error = Some(crate::i18n::tr("designs.list_failed").to_string());
                    app.designs.failed = Some(path);
                    app.designs.entries = None;
                }
            }
        }
    }
    let received = app.designs.listing.as_ref().and_then(|rx| match rx.try_recv() {
        Ok(v) => Some(Ok(v)),
        Err(TryRecvError::Empty) => None,
        Err(TryRecvError::Disconnected) => Some(Err(())),
    });
    match received {
        Some(Ok(value)) => {
            app.designs.listing = None;
            if let Some(error) = value.get("error").and_then(Value::as_str) {
                app.designs.error = Some(error.to_string());
                app.designs.failed = app.designs.listing_path.clone();
                app.designs.entries = None;
            } else if let Some(entries) = Entries::from_json(&value) {
                app.designs.error = None;
                app.designs.failed = None;
                app.designs.current = Some(PathBuf::from(&entries.path));
                app.designs.thumb_rx =
                    if app.preferences.designs_thumbnails && !app.preferences.designs_tree { start_thumbnails(&entries) } else { None };
                app.designs.entries = Some(entries);
            }
        }
        Some(Err(())) => {
            app.designs.listing = None;
            app.designs.error = Some(crate::i18n::tr("designs.list_failed").to_string());
            app.designs.failed = app.designs.listing_path.clone();
        }
        None => {
            if app.designs.listing.is_some() {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
        }
    }
    if let Some(rx) = app.designs.thumb_rx.as_ref() {
        let mut batch = Vec::new();
        let mut disconnected = false;
        loop {
            match rx.try_recv() {
                Ok(item) => batch.push(item),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        for (key, image) in batch {
            let texture = image.map(|i| ctx.load_texture(format!("designs:{key}"), i, egui::TextureOptions::LINEAR));
            app.designs.thumbnails.insert(key, texture);
        }
        if disconnected {
            app.designs.thumb_rx = None;
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
    // Pictures are turned back on, or a listing arrived while they were off: fetch them now.
    let waiting =
        app.designs.thumb_rx.is_none() && app.designs.thumbnails.is_empty() && app.preferences.designs_thumbnails && !app.preferences.designs_tree;
    if waiting && let Some(entries) = app.designs.entries.clone() {
        app.designs.thumb_rx = start_thumbnails(&entries);
    }
    // A folder the tree opened: its rows are drawn as soon as it is read.
    let tree = app.designs.tree_listing.as_ref().and_then(|(path, rx)| match rx.try_recv() {
        Ok(value) => Some((path.clone(), Some(value))),
        Err(TryRecvError::Empty) => None,
        Err(TryRecvError::Disconnected) => Some((path.clone(), None)),
    });
    if let Some((path, value)) = tree {
        app.designs.tree_listing = None;
        let entries = value.and_then(|v| if v.get("error").is_some() { None } else { Entries::from_json(&v) });
        // A folder that cannot be read stays shut instead of being asked for again every frame.
        app.designs
            .tree_entries
            .insert(path.clone(), entries.unwrap_or(Entries { path, folders: Vec::new(), designs: Vec::new(), truncated: false }));
    } else if app.designs.tree_listing.is_some() {
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
    }
}

fn start_thumbnails(entries: &Entries) -> Option<Receiver<(String, Option<egui::ColorImage>)>> {
    let jobs: Vec<(String, PathBuf, bool)> = entries
        .folders
        .iter()
        .filter(|f| f.has_thumbnail)
        .map(|f| (f.path.clone(), PathBuf::from(&f.path), true))
        .chain(entries.designs.iter().map(|d| (d.path.clone(), PathBuf::from(&d.path), false)))
        .take(200)
        .collect();
    if jobs.is_empty() {
        return None;
    }
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new().name("designs-thumbs".into()).spawn(move || {
        for (key, path, folder) in jobs {
            let image = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| picture(&key, &path, folder))).ok().flatten();
            if tx.send((key, image)).is_err() {
                break;
            }
        }
    });
    spawned.ok().map(|_| rx)
}

/// A row's picture: a project's custom thumbnail, a design's cached or in-file thumbnail.
fn picture(key: &str, path: &Path, folder: bool) -> Option<egui::ColorImage> {
    if folder {
        let bytes = std::fs::read(solidcraft_engine::io::project::thumbnail_path(path)).ok()?;
        decode_picture(bytes, solidcraft_engine::io::project::THUMBNAIL_SIZE[0], solidcraft_engine::io::project::THUMBNAIL_SIZE[1])
    } else {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(image) = crate::home::cached(key) {
            return Some(image);
        }
        crate::home::decode_thumbnail(solidcraft_engine::io::read_design_thumbnail_file(key)?)
    }
}

/// Decode a PNG up to `max_w` x `max_h`, with the allocation limits the design thumbnails use.
pub(crate) fn decode_picture(bytes: Vec<u8>, max_w: u32, max_h: u32) -> Option<egui::ColorImage> {
    if bytes.len() > 1 << 20 {
        return None;
    }
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(max_w);
    limits.max_image_height = Some(max_h);
    limits.max_alloc = Some(u64::from(max_w) * u64::from(max_h) * 8);
    reader.limits(limits);
    let image = reader.decode().ok()?.to_rgba8();
    Some(egui::ColorImage::from_rgba_unmultiplied([image.width() as usize, image.height() as usize], image.as_raw()))
}

fn repository_header(app: &mut SolveApp, ui: &mut egui::Ui, t: &Tokens) {
    let active = active_repository(app).map(|r| (r.id.clone(), r.name.clone(), r.description.clone()));
    let title = active.as_ref().map(|(_, name, _)| name.clone()).unwrap_or_else(|| crate::i18n::tr("designs.no_repositories").to_string());
    egui::ComboBox::from_id_salt("designs_repository")
        .width(ui.available_width() - 4.0)
        .selected_text(RichText::new(title).size(13.0).strong())
        .show_ui(ui, |ui| {
            let repositories = app.preferences.repositories.clone();
            for repository in &repositories {
                let chosen = app.preferences.active_repository == repository.id;
                let response = ui.selectable_label(chosen, &repository.name);
                let response = if repository.description.is_empty() { response } else { response.on_hover_text(&repository.description) };
                if response.clicked() {
                    app.preferences.active_repository = repository.id.clone();
                    app.designs.repository.clear();
                    ui.close();
                }
            }
            ui.separator();
            if ui.button(crate::i18n::tr("designs.manage_repositories")).clicked() {
                crate::prefs::open_at(app, crate::prefs::PAGE_REPOSITORIES);
                ui.close();
            }
        });
    if let Some((_, _, description)) = active
        && !description.is_empty()
    {
        ui.label(RichText::new(description).size(11.0).color(t.text_dim));
    }
}

fn breadcrumb(app: &mut SolveApp, ui: &mut egui::Ui) {
    let Some(current) = app.designs.current.clone() else { return };
    let Some(repository) = active_repository(app) else { return };
    let root = PathBuf::from(&repository.path);
    let root_name = repository.name.clone();
    let relative: Vec<String> = current
        .strip_prefix(&root)
        .map(|p| {
            p.components()
                .filter_map(|c| match c {
                    Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let mut target = None;
    let settings_target = folder_settings_target(app);
    let mut open_settings = false;
    let t = Tokens::get();
    ui.horizontal_wrapped(|ui| {
        // A house, not the repository's name: the folder is the start of every path, and the
        // name is one hover away.
        let (home, response) = ui.allocate_exact_size(vec2(18.0, 18.0), egui::Sense::click());
        crate::scenario::publish_handle("designs:breadcrumb:root", home.center());
        let ink = if response.hovered() || response.has_focus() { t.accent } else { t.text };
        crate::icons::paint(
            ui.painter(),
            egui::Rect::from_center_size(home.center() + vec2(0.0, 1.0), vec2(17.0, 17.0)),
            "home",
            ink,
            t.icon_fill,
            t.accent,
        );
        let response = response.on_hover_text(&root_name).on_hover_cursor(egui::CursorIcon::PointingHand);
        if response.clicked() {
            target = Some(root.clone());
        }
        let mut path = root.clone();
        for part in &relative {
            ui.label(RichText::new(">").size(11.5).color(crate::theme::Tokens::get().text_dim));
            path.push(part);
            if ui.link(RichText::new(part).size(11.5)).clicked() {
                target = Some(path.clone());
            }
        }
        if settings_target.is_some() {
            let (rect, response) = ui.allocate_exact_size(vec2(24.0, 24.0), egui::Sense::click());
            if response.hovered() || response.has_focus() {
                ui.painter().rect_filled(rect, 4.0, t.hover);
            }
            crate::icons::paint_on(ui.painter(), rect.shrink(4.0), "settings", Some(crate::theme::is_dark()), 1.0);
            crate::scenario::publish_handle("designs:folder_settings", rect.center());
            if response.on_hover_text(crate::i18n::tr("designs.properties")).clicked() {
                open_settings = true;
            }
        }
    });
    if let Some(target) = target {
        navigate(app, target);
    }
    if open_settings && let Some((path, name)) = settings_target {
        open_folder_properties(app, path, name);
    }
}

fn navigate(app: &mut SolveApp, path: PathBuf) {
    app.designs.current = Some(path);
    app.designs.entries = None;
    app.designs.error = None;
    app.designs.failed = None;
    app.designs.selected = None;
    app.designs.refresh = true;
}

fn actions(app: &mut SolveApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        // The same New Folder window a folder's own menu opens, in the folder the panel shows.
        if ui.button(crate::i18n::tr("designs.new_folder")).clicked() {
            open_new_folder(app);
        }
        if ui.button(crate::i18n::tr("designs.refresh")).clicked() {
            app.designs.entries = None;
            app.designs.refresh = true;
            // Refresh means try again, even for a folder that could not be made or read.
            app.designs.failed = None;
            app.designs.created = None;
        }
        // Pictures or one compact line per row, toggled where the rows are; the tree is always
        // one line, so its own button takes the pictures button's place there.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let t = Tokens::get();
            let tree = app.preferences.designs_tree;
            // The icon shows the view in use, as the pictures button's does: the tree, or the
            // one folder the panel otherwise shows.
            let (icon, tip) = if tree { ("tree", "designs.tree_hide") } else { ("folder", "designs.tree") };
            if view_button(ui, &t, "designs:view:tree", icon, tip) {
                app.preferences.designs_tree = !tree;
                // Another view starts collapsed, as it does when the panel opens.
                app.designs.tree.clear();
                app.designs.tree_entries.clear();
            }
            if !tree {
                let pictures = app.preferences.designs_thumbnails;
                let (icon, tip) = if pictures { ("grid", "designs.thumbnails_hide") } else { ("list", "designs.thumbnails") };
                if view_button(ui, &t, "designs:view:pictures", icon, tip) {
                    app.preferences.designs_thumbnails = !pictures;
                }
            }
        });
    });
}

/// One of the panel's own view buttons: a small icon that toggles a way of showing the rows.
fn view_button(ui: &mut egui::Ui, t: &Tokens, key: &str, icon: &str, tip: &str) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(24.0, 24.0), egui::Sense::click());
    if response.hovered() || response.has_focus() {
        ui.painter().rect_filled(rect, 4.0, t.hover);
    }
    crate::icons::paint_on(ui.painter(), rect.shrink(4.0), icon, Some(crate::theme::is_dark()), 1.0);
    crate::scenario::publish_handle(key, rect.center());
    response.on_hover_text(crate::i18n::tr(tip)).clicked()
}

/// The New Folder window, in the folder the panel shows (the repository root with none).
fn open_new_folder(app: &mut SolveApp) {
    let Some(shown) = save_folder(app).map(PathBuf::from) else { return };
    crate::designs_new_folder::open(app, &shown.to_string_lossy());
}

/// A message without the command id the engine prefixes its errors with (`designs.folder_new: …`,
/// "invalid parameters for `designs.folder_new`: …"): what the panel shows under
/// `designs.unavailable`.
pub(crate) fn plain(error: &str) -> &str {
    let after = |head: &str, tail: &str| error.strip_prefix(head).and_then(|rest| rest.split_once(tail)).map(|(_, message)| message);
    after("designs.", ": ").or_else(|| after("invalid parameters for `", "`: ")).map_or(error, |message| message)
}

/// Read the panel's folders again: a file or folder was moved, copied or made (by the panel or
/// the Move/Copy window).
pub(crate) fn reload(app: &mut SolveApp) {
    app.designs.entries = None;
    app.designs.refresh = true;
    app.designs.tree_entries.clear();
}

fn rows(app: &mut SolveApp, ui: &mut egui::Ui, t: &Tokens) {
    let loading = app.designs.listing.is_some() || app.designs.root_check.is_some();
    crate::scenario::publish_count("designs:loading", if loading { 1.0 } else { 0.0 });
    let unavailable = app.designs.entries.is_none() && !loading && app.designs.error.is_some();
    crate::scenario::publish_count("designs:unavailable", if unavailable { 1.0 } else { 0.0 });
    let Some(entries) = app.designs.entries.take() else {
        if loading {
            ui.add(egui::Spinner::new().size(18.0));
        } else if let Some(error) = &app.designs.error {
            // The folder is not there and could not be made, or it cannot be read: say so, and
            // offer to try again instead of waiting.
            ui.label(RichText::new(crate::i18n::tr("designs.unavailable")).size(12.5).color(t.error));
            ui.add_space(2.0);
            ui.add(egui::Label::new(RichText::new(plain(error)).size(11.0).color(t.text_dim)).wrap());
            ui.add_space(6.0);
            if ui.button(crate::i18n::tr("designs.retry")).clicked() {
                app.designs.failed = None;
                app.designs.created = None;
                app.designs.entries = None;
            }
        }
        return;
    };
    app.designs.refresh = false;
    if let Some(error) = &app.designs.error {
        ui.label(RichText::new(plain(error)).color(t.error).size(12.0));
        ui.add_space(4.0);
    }
    let thumbnails = app.preferences.designs_thumbnails;
    crate::scenario::publish_count("designs:thumbnails_on", if thumbnails { 1.0 } else { 0.0 });
    let row_height = if thumbnails { ROW_PICTURE } else { ROW_COMPACT };
    let mut actions = Vec::new();
    crate::scenario::publish_count("designs:folders", entries.folders.len() as f64);
    crate::scenario::publish_count("designs:designs", entries.designs.len() as f64);
    let row_count = entries.folders.len() + entries.designs.len();
    let display_count = row_count + usize::from(entries.truncated || row_count == 0);
    egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_height, display_count, |ui, visible_rows| {
        for row in visible_rows {
            if let Some(folder) = entries.folders.get(row) {
                folder_row(app, ui, t, folder, &mut actions, thumbnails);
            } else if let Some(design) = entries.designs.get(row.saturating_sub(entries.folders.len()))
                && row < row_count
            {
                design_row(app, ui, t, design, &mut actions, thumbnails);
            } else if row_count == 0 {
                ui.label(RichText::new(crate::i18n::tr("designs.empty")).size(12.0).color(t.text_dim));
            } else if entries.truncated {
                ui.label(RichText::new(crate::i18n::tr("designs.truncated")).size(11.0).color(t.text_dim));
            }
        }
    });
    if !app.designs.refresh {
        app.designs.entries = Some(entries);
    }
    for action in actions {
        apply(app, action);
    }
}

/// How tall a row is: a picture with its two lines, or one compact line (thumbnails off).
const ROW_PICTURE: f32 = 54.0;
const ROW_COMPACT: f32 = 26.0;
/// A tree row: shorter than a list row, and drawn without a gap, so the tree reads as one column.
const ROW_TREE: f32 = 21.0;

/// The compact row's icon: one clear mark for a folder and for each kind of file, so a folder
/// never looks like a design and the importers stay apart. There is no dedicated mesh or board
/// artwork in the set yet: a grey body marks imported geometry, and a drawing sheet a board.
fn kind_icon(kind: &str) -> &'static str {
    match kind {
        "design" => "box",
        "step" => "import",
        "mesh" => "body",
        "kicad" => "dxf",
        _ => "folder",
    }
}

fn draw_kind_icon(ui: &egui::Ui, rect: egui::Rect, kind: &str) {
    let t = Tokens::get();
    let mark = egui::Rect::from_center_size(pos2(rect.left() + 18.0, rect.center().y), vec2(16.0, 16.0));
    crate::icons::paint(ui.painter(), mark, kind_icon(kind), t.icon, t.icon_fill, t.icon);
}

/// A row of the tree view: a folder, a design or an importable file, at some depth.
#[derive(Clone)]
struct TreeRow {
    path: String,
    name: String,
    folder: bool,
    kind: String,
    depth: usize,
}

/// How deep the tree goes before it stops: a hostile or looping folder structure cannot make
/// the panel recurse without end.
const TREE_DEPTH_MAX: usize = 32;

/// The tree view: the repository root and, under it, the folders that were opened, one line
/// each, indented by depth. A folder is read when it is opened, never the whole repository at
/// once; everything starts collapsed.
fn tree_rows(app: &mut SolveApp, ui: &mut egui::Ui, t: &Tokens) {
    let Some(repository) = active_repository(app) else {
        ui.label(RichText::new(crate::i18n::tr("designs.no_repositories")).size(12.0).color(t.text_dim));
        return;
    };
    if repository.path.is_empty() {
        ui.label(RichText::new(crate::i18n::tr("designs.choose_folder")).size(12.0).color(t.text_dim));
        return;
    }
    let (root, root_name) = (repository.path.clone(), repository.name.clone());
    // A root that could not be made or read says so here too, not as an empty tree.
    if let Some(error) = &app.designs.error
        && app.designs.root_check.is_none()
        && app.designs.tree_entries.is_empty()
    {
        ui.label(RichText::new(crate::i18n::tr("designs.unavailable")).size(12.5).color(t.error));
        ui.label(RichText::new(plain(error)).size(11.0).color(t.text_dim));
        ui.add_space(4.0);
    }
    // An action asked for a fresh tree (a new folder, a rename, Refresh): read the open folders
    // again, keeping which ones are open.
    if std::mem::take(&mut app.designs.refresh) {
        app.designs.tree_entries.clear();
    }
    tree_request(app, ui.ctx());
    let rows = tree_layout(&app.designs.tree, &app.designs.tree_entries, &root, &root_name);
    crate::scenario::publish_count("designs:tree_rows", rows.len() as f64);
    crate::scenario::publish_count("designs:tree_loaded", app.designs.tree_entries.len() as f64);
    let mut actions = Vec::new();
    let mut toggle = None;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for row in &rows {
            let (rect, response) = row_response(app, ui, &row.path, ROW_TREE, true);
            crate::scenario::publish_handle(&format!("designs:row:{}", row.name), rect.center());
            crate::scenario::publish_rect(&format!("designs:row:{}", row.name), rect);
            let indent = rect.left() + 10.0 + 14.0 * row.depth as f32;
            if row.folder {
                let open = app.designs.tree.contains(&row.path);
                let ink = if response.hovered() { t.text } else { t.text_dim };
                crate::ui_kit::disclosure(ui.painter(), pos2(indent, rect.center().y), open, ink);
                // The disclosure itself opens and closes, as in a file browser.
                let hit = egui::Rect::from_center_size(pos2(indent, rect.center().y), vec2(16.0, 16.0));
                crate::scenario::publish_handle(&format!("designs:tree-toggle:{}", row.name), hit.center());
                if ui.interact(hit, ui.id().with(("designs-tree", row.path.as_str())), Sense::click()).clicked() {
                    toggle = Some(row.path.clone());
                }
            }
            let mark = egui::Rect::from_center_size(pos2(indent + 16.0, rect.center().y), vec2(16.0, 16.0));
            crate::icons::paint(ui.painter(), mark, if row.folder { "folder" } else { kind_icon(&row.kind) }, t.icon, t.icon_fill, t.icon);
            let text = egui::Rect::from_min_max(pos2(indent, rect.top()), rect.max);
            row_text(app, ui, text, &row.path, &row.name, None);
            if response.clicked() {
                // The second click on a row opens or closes it, as in a file browser (a folder
                // elsewhere in the panel is entered on a double click).
                let now = ui.input(|i| i.time);
                let again = app.designs.tree_click.as_ref().is_some_and(|(p, t)| p == &row.path && now - t <= 0.45);
                app.designs.tree_click = (!again).then(|| (row.path.clone(), now));
                if row.folder {
                    // The folder picked is where a new one goes.
                    app.designs.current = Some(PathBuf::from(&row.path));
                }
                if again {
                    if row.folder {
                        toggle = Some(row.path.clone());
                    } else {
                        actions.push(Action::Open(row.path.clone()));
                    }
                }
            }
            let (path, name) = (row.path.clone(), row.name.clone());
            if row.folder {
                response.context_menu(|ui| folder_menu(app, ui, &path, &name, &mut actions));
            } else {
                response.context_menu(|ui| design_menu(app, ui, &path, &name, &mut actions));
            }
        }
    });
    if let Some(path) = toggle
        && !app.designs.tree.remove(&path)
    {
        app.designs.tree.insert(path);
    }
    for action in actions {
        apply(app, action);
    }
}

/// The visible rows: the root, then each open folder's folders and designs in turn.
fn tree_layout(tree: &std::collections::BTreeSet<String>, entries: &HashMap<String, Entries>, root: &str, root_name: &str) -> Vec<TreeRow> {
    let mut rows = vec![TreeRow { path: root.to_string(), name: root_name.to_string(), folder: true, kind: "folder".into(), depth: 0 }];
    if tree.contains(root) {
        tree_children(&mut rows, tree, entries, root, 1);
    }
    rows
}

fn tree_children(rows: &mut Vec<TreeRow>, tree: &std::collections::BTreeSet<String>, entries: &HashMap<String, Entries>, parent: &str, depth: usize) {
    if depth > TREE_DEPTH_MAX {
        return;
    }
    let Some(listing) = entries.get(parent) else { return };
    for folder in &listing.folders {
        rows.push(TreeRow { path: folder.path.clone(), name: folder.name.clone(), folder: true, kind: "folder".into(), depth });
        if tree.contains(&folder.path) {
            tree_children(rows, tree, entries, &folder.path, depth + 1);
        }
    }
    for design in &listing.designs {
        rows.push(TreeRow { path: design.path.clone(), name: design.name.clone(), folder: false, kind: design.kind.clone(), depth });
    }
}

/// Ask for the contents of one open folder that is not listed yet, off the window thread: one
/// folder at a time, so a slow or network folder never holds the window.
fn tree_request(app: &mut SolveApp, ctx: &egui::Context) {
    if app.designs.tree_listing.is_some() {
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
        return;
    }
    let Some(path) = app.designs.tree.iter().find(|p| !app.designs.tree_entries.contains_key(*p)).cloned() else { return };
    let (tx, rx) = mpsc::channel();
    let worker_path = path.clone();
    let spawned = std::thread::Builder::new().name("designs-tree".into()).spawn(move || {
        let result = std::panic::catch_unwind(|| solidcraft_engine::cmd::designs::list(&worker_path));
        let value = match result {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => json!({ "error": e.to_string() }),
            Err(_) => json!({ "error": "the folder could not be listed" }),
        };
        let _ = tx.send(value);
    });
    match spawned {
        Ok(_) => app.designs.tree_listing = Some((path, rx)),
        Err(_) => {
            // Nothing to retry with: the row stays shut, and the panel says why.
            app.set_status(crate::i18n::tr("designs.list_failed"), true);
            app.designs.tree_entries.insert(path, Entries { path: String::new(), folders: Vec::new(), designs: Vec::new(), truncated: false });
        }
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(50));
}

fn row_response(app: &mut SolveApp, ui: &mut egui::Ui, path: &str, height: f32, plain: bool) -> (egui::Rect, egui::Response) {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click_and_drag());
    let selected = app.designs.selected.as_deref() == Some(path);
    // The flat list's rows are cards on the panel; a tree row is bare, so indentation reads.
    if !plain {
        ui.painter().rect_filled(rect, 3.0, crate::theme::Tokens::get().field);
    }
    if selected {
        ui.painter().rect_filled(rect, 3.0, crate::theme::Tokens::get().accent_soft);
    } else if response.hovered() {
        ui.painter().rect_filled(rect, 3.0, crate::theme::Tokens::get().hover);
    }
    if response.clicked() {
        app.designs.selected = Some(path.to_string());
    }
    (rect, response)
}

fn draw_picture(app: &SolveApp, ui: &egui::Ui, rect: egui::Rect, path: &str, icon: &str) {
    let t = crate::theme::Tokens::get();
    let picture = egui::Rect::from_min_size(rect.min + vec2(4.0, 5.0), vec2(64.0, 44.0));
    match app.designs.thumbnails.get(path).and_then(Option::as_ref) {
        Some(texture) => {
            ui.painter().rect_filled(picture, 3.0, t.field);
            ui.painter().image(texture.id(), picture, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
        }
        None => {
            ui.painter().rect_filled(picture, 3.0, t.panel_header);
            crate::icons::paint(ui.painter(), egui::Rect::from_center_size(picture.center(), vec2(26.0, 26.0)), icon, t.icon, t.icon_fill, t.icon);
        }
    }
}

fn row_text(app: &mut SolveApp, ui: &mut egui::Ui, rect: egui::Rect, path: &str, name: &str, sub: Option<&str>) {
    let t = crate::theme::Tokens::get();
    let x = rect.left() + if sub.is_some() { 76.0 } else { 32.0 };
    let name_rect = if sub.is_some() {
        egui::Rect::from_min_size(pos2(x, rect.top() + 8.0), vec2(rect.right() - x - 6.0, 18.0))
    } else {
        egui::Rect::from_min_max(pos2(x, rect.top() + 2.0), pos2(rect.right() - 6.0, rect.bottom() - 2.0))
    };
    if app.designs.rename.as_ref().is_some_and(|r| r.path == path) {
        let id = egui::Id::new(("designs-rename", path));
        // Read the keys before the text field consumes them (it is focused from the previous
        // frame).
        let focused = ui.memory(|m| m.has_focus(id));
        let entered = focused && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let escaped = focused && ui.input(|i| i.key_pressed(egui::Key::Escape));
        let mut commit = None;
        let mut cancel = false;
        if let Some(rename) = app.designs.rename.as_mut() {
            let response = ui.put(name_rect, egui::TextEdit::singleline(&mut rename.name).id(id).desired_width(name_rect.width()));
            if rename.select_all {
                response.request_focus();
                // The name starts selected, so typing replaces it (as a file browser does).
                let mut state = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
                let all = egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(rename.name.chars().count()));
                state.cursor.set_char_range(Some(all));
                state.store(ui.ctx(), id);
                rename.select_all = false;
            }
            if entered {
                commit = Some(rename.name.trim().to_string());
            } else if escaped || response.lost_focus() {
                cancel = true;
            }
        }
        if let Some(name) = commit {
            app.designs.rename = None;
            let path = path.to_string();
            match app.run("designs.rename", json!({ "path": path, "name": name })) {
                Ok(v) => {
                    app.designs.entries = None;
                    app.designs.refresh = true;
                    if let (Some(old), Some(new)) = (v.get("old_path").and_then(Value::as_str), v.get("path").and_then(Value::as_str)) {
                        crate::documents::retarget(app, old, new);
                    }
                }
                Err(e) => app.set_status(e, true),
            }
        } else if cancel {
            app.designs.rename = None;
        }
        return;
    }
    let p = ui.painter().with_clip_rect(egui::Rect::from_min_max(pos2(x, rect.top()), pos2(rect.right() - 4.0, rect.bottom())));
    match sub {
        Some(sub) => {
            p.text(pos2(x, rect.top() + 8.0), egui::Align2::LEFT_TOP, name, egui::FontId::proportional(13.0), t.text);
            p.text(pos2(x, rect.top() + 28.0), egui::Align2::LEFT_TOP, sub, egui::FontId::proportional(11.0), t.text_dim);
        }
        // One line: the name, centred on the row (no dates, no folder tag).
        None => {
            p.text(pos2(x, rect.center().y), egui::Align2::LEFT_CENTER, name, egui::FontId::proportional(12.5), t.text);
        }
    }
}

fn folder_row(app: &mut SolveApp, ui: &mut egui::Ui, _t: &Tokens, folder: &FolderRow, actions: &mut Vec<Action>, thumbnails: bool) {
    let height = if thumbnails { ROW_PICTURE } else { ROW_COMPACT };
    let (rect, response) = row_response(app, ui, &folder.path, height, false);
    crate::scenario::publish_handle(&format!("designs:row:{}", folder.name), rect.center());
    crate::scenario::publish_rect(&format!("designs:row:{}", folder.name), rect);
    let sub = if folder.description.is_empty() { crate::i18n::tr("designs.folder_label").to_string() } else { folder.description.clone() };
    if thumbnails {
        draw_picture(app, ui, rect, &folder.path, "folder");
        row_text(app, ui, rect, &folder.path, &folder.name, Some(&sub));
    } else {
        draw_kind_icon(ui, rect, "folder");
        row_text(app, ui, rect, &folder.path, &folder.name, None);
    }
    if response.double_clicked() {
        navigate(app, PathBuf::from(&folder.path));
    }
    let path = folder.path.clone();
    let name = folder.name.clone();
    response.context_menu(|ui| folder_menu(app, ui, &path, &name, actions));
}

fn design_row(app: &mut SolveApp, ui: &mut egui::Ui, _t: &Tokens, design: &DesignRow, actions: &mut Vec<Action>, thumbnails: bool) {
    let height = if thumbnails { ROW_PICTURE } else { ROW_COMPACT };
    let (rect, response) = row_response(app, ui, &design.path, height, false);
    crate::scenario::publish_handle(&format!("designs:row:{}", design.name), rect.center());
    crate::scenario::publish_rect(&format!("designs:row:{}", design.name), rect);
    if response.drag_started() {
        egui::DragAndDrop::set_payload(ui.ctx(), DesignDrag { path: design.path.clone() });
    }
    if thumbnails {
        // A design shows its own thumbnail; a file SolidCraft imports has none, so it shows the
        // importer's icon.
        draw_picture(app, ui, rect, &design.path, kind_icon(&design.kind));
        row_text(app, ui, rect, &design.path, &design.name, Some(&design.modified_display));
        if design.kind == "design" {
            version_button(ui, rect, design, actions);
        }
    } else {
        // One compact line: the kind's icon and the name. No date, no version selector.
        draw_kind_icon(ui, rect, &design.kind);
        row_text(app, ui, rect, &design.path, &design.name, None);
    }
    if response.double_clicked() {
        actions.push(Action::Open(design.path.clone()));
    }
    let path = design.path.clone();
    let name = design.name.clone();
    response.context_menu(|ui| design_menu(app, ui, &path, &name, actions));
}

/// The row's version selector (`V3`), like a file browser's: it lists the versions, newest first,
/// and opens one (the current one opens at once; an older one asks first).
fn version_button(ui: &mut egui::Ui, row: egui::Rect, design: &DesignRow, actions: &mut Vec<Action>) {
    let rect = egui::Rect::from_center_size(pos2(row.right() - 27.0, row.bottom() - 10.0), vec2(46.0, 18.0));
    let button = ui.put(rect, egui::Button::new(RichText::new(format!("V{} ▾", design.versions)).size(11.0)).small());
    crate::scenario::publish_handle(&format!("designs:versions:{}", design.name), button.rect.center());
    crate::scenario::publish_count(&format!("designs:version:{}", design.name), f64::from(design.versions));
    let path = design.path.clone();
    let versions = design.versions;
    let mut chosen = None;
    // Where the version's own menu goes: beside the list, so the list stays readable.
    let mut menu = None;
    let mut menu_at = None;
    // The popup is offered every frame (it keeps its own open state), not only on the click frame.
    // A click on one of the versions does not close it by itself: choosing one closes it, while a
    // right click keeps it open beside the version's own menu (below).
    egui::Popup::menu(&button).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        for version in (1..=versions).rev() {
            let current = version == versions;
            let label = if current {
                crate::i18n::format("designs.version_current", &[("version", &version.to_string())])
            } else {
                crate::i18n::format("designs.version", &[("version", &version.to_string())])
            };
            let item = ui.button(label);
            if item.clicked() {
                chosen = Some(version);
                ui.close();
            }
            // A version's own menu (right click): make it the latest, or drop the ones before it.
            // The list stays open, so the menu never floats alone.
            if item.secondary_clicked() {
                menu = Some(version);
                menu_at = Some(item.rect.right_top() + vec2(4.0, 0.0));
            }
        }
    });
    if let Some(version) = menu {
        let at = menu_at.or_else(|| ui.ctx().pointer_interact_pos()).unwrap_or(rect.left_bottom());
        actions.push(Action::VersionMenu { path, version, versions, at });
    } else {
        match chosen {
            Some(version) if version == versions => actions.push(Action::Open(path)),
            Some(version) => actions.push(Action::OpenVersion { path, version, versions }),
            None => {}
        }
    }
}

/// A version's own menu, drawn where it was right-clicked in the version list: promote it (the
/// later versions go) or purge the ones before it. Both ask before they delete anything.
///
/// The list closes when one of its versions is right-clicked (egui closes the popups it opened),
/// so the menu is kept here and drawn by the panel instead.
fn version_menu(app: &mut SolveApp, ctx: &egui::Context) {
    let Some(menu) = app.designs.version_menu.clone() else { return };
    let mut ask = None;
    let t = Tokens::get();
    // Keep it on screen: a row near the bottom or the right edge would push it out.
    let screen = ctx.content_rect();
    let at = pos2((menu.at.x).min(screen.right() - 200.0).max(screen.left()), (menu.at.y).min(screen.bottom() - 70.0).max(screen.top()));
    let area = egui::Area::new(egui::Id::new("designs-version-menu")).order(egui::Order::Foreground).fixed_pos(at).show(ctx, |ui| {
        egui::Frame::popup(ui.style()).show(ui, |ui| {
            ui.set_min_width(190.0);
            ui.label(RichText::new(crate::i18n::format("designs.version", &[("version", &menu.version.to_string())])).size(10.5).color(t.text_dim));
            ui.add_space(2.0);
            let width = ui.available_width();
            let up = ui.add_enabled(
                menu.version < menu.versions,
                egui::Button::new(RichText::new(crate::i18n::tr("designs.promote")).size(12.0)).min_size(vec2(width, 20.0)),
            );
            if up.on_disabled_hover_text(crate::i18n::tr("designs.promote_current")).clicked() {
                ask = Some(true);
            }
            let older = ui.add_enabled(
                menu.version > 1,
                egui::Button::new(RichText::new(crate::i18n::tr("designs.purge_older")).size(12.0)).min_size(vec2(width, 20.0)),
            );
            if older.on_disabled_hover_text(crate::i18n::tr("designs.purge_first")).clicked() {
                ask = Some(false);
            }
        });
    });
    let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
    let clicked_outside = !menu.fresh && ctx.input(|i| i.pointer.any_click()) && !area.response.contains_pointer();
    let question = match ask {
        Some(true) => Some(crate::designs_versions::Prompt::Promote { path: menu.path.clone(), version: menu.version, versions: menu.versions }),
        Some(false) => Some(crate::designs_versions::Prompt::PurgeOlder { path: menu.path.clone(), version: menu.version, versions: menu.versions }),
        None => None,
    };
    let asked = question.is_some();
    if let Some(question) = question {
        app.version_prompt = Some(question);
    }
    if asked || escape || clicked_outside {
        app.designs.version_menu = None;
    } else if let Some(open) = app.designs.version_menu.as_mut() {
        open.fresh = false;
    }
}

fn folder_menu(_app: &mut SolveApp, ui: &mut egui::Ui, path: &str, name: &str, actions: &mut Vec<Action>) {
    if ui.button(crate::i18n::tr("designs.new_folder_inside")).clicked() {
        actions.push(Action::NewFolder(path.to_string()));
        ui.close();
    }
    if ui.button(crate::i18n::tr("designs.open")).clicked() {
        actions.push(Action::Open(path.to_string()));
        ui.close();
    }
    if ui.button(crate::i18n::tr("designs.rename")).clicked() {
        actions.push(Action::Rename { path: path.to_string(), name: name.to_string() });
        ui.close();
    }
    if ui.button(crate::i18n::tr("designs.move")).clicked() {
        actions.push(Action::Move(path.to_string()));
        ui.close();
    }
    let delete = ui.button(crate::i18n::tr("designs.delete"));
    if delete.on_hover_text(crate::i18n::tr("designs.folder_delete_hint")).clicked() {
        actions.push(Action::Delete { path: path.to_string(), name: name.to_string(), folder: true });
        ui.close();
    }
    ui.separator();
    if ui.button(crate::i18n::tr("designs.properties")).clicked() {
        actions.push(Action::Properties { path: path.to_string(), name: name.to_string() });
        ui.close();
    }
}

fn design_menu(app: &mut SolveApp, ui: &mut egui::Ui, path: &str, name: &str, actions: &mut Vec<Action>) {
    if ui.button(crate::i18n::tr("designs.open")).clicked() {
        actions.push(Action::Open(path.to_string()));
        ui.close();
    }
    let can_insert = !app.home.open;
    let insert = ui.add_enabled(can_insert, egui::Button::new(crate::i18n::tr("designs.insert")));
    if insert.on_disabled_hover_text(crate::i18n::tr("designs.needs_design")).clicked() {
        actions.push(Action::Insert(path.to_string()));
        ui.close();
    }
    if ui.button(crate::i18n::tr("designs.rename")).clicked() {
        actions.push(Action::Rename { path: path.to_string(), name: name.to_string() });
        ui.close();
    }
    if ui.button(crate::i18n::tr("designs.duplicate")).clicked() {
        actions.push(Action::Duplicate(path.to_string()));
        ui.close();
    }
    if ui.button(crate::i18n::tr("designs.move")).clicked() {
        actions.push(Action::Move(path.to_string()));
        ui.close();
    }
    if ui.button(crate::i18n::tr("designs.copy_to")).clicked() {
        actions.push(Action::CopyTo(path.to_string()));
        ui.close();
    }
    let open = app.docs.is_open(app, path);
    let delete = ui.add_enabled(!open, egui::Button::new(crate::i18n::tr("designs.delete")));
    let delete = if open { delete.on_disabled_hover_text(crate::i18n::tr("designs.delete_open")) } else { delete };
    if delete.clicked() {
        actions.push(Action::Delete { path: path.to_string(), name: name.to_string(), folder: false });
        ui.close();
    }
    if ui.button(crate::i18n::tr("designs.show_in_folder")).clicked() {
        if let Some(show) = &app.services.show_in_folder {
            show(path);
        }
        ui.close();
    }
}

fn apply(app: &mut SolveApp, action: Action) {
    match action {
        // A board opens with its import settings, so every Open goes through the same door as
        // File > Open (and a recent design on Home). A folder opens in place in the tree, and
        // is walked into one folder at a time otherwise.
        Action::Open(path) => {
            if Path::new(&path).is_dir() {
                if app.preferences.designs_tree {
                    app.designs.tree.insert(path);
                } else {
                    navigate(app, PathBuf::from(path));
                }
            } else {
                app.open_chosen_file(&path);
            }
        }
        Action::OpenVersion { path, version, versions } => {
            // Opening an old version asks first: it is history, not the design as it stands.
            app.version_prompt = Some(crate::designs_versions::Prompt::Open { path, version, versions });
        }
        Action::VersionMenu { path, version, versions, at } => {
            app.designs.version_menu = Some(VersionMenu { path, version, versions, at, fresh: true });
        }
        Action::Insert(path) => insert_design(app, &path),
        Action::Rename { path, name } => app.designs.rename = Some(Rename { path, name, select_all: true }),
        Action::Move(path) => crate::designs_folder::open(app, &path, false),
        Action::NewFolder(path) => crate::designs_new_folder::open(app, &path),
        Action::CopyTo(path) => crate::designs_folder::open(app, &path, true),
        Action::Duplicate(path) => match app.run("designs.duplicate", json!({ "path": path })) {
            Ok(v) => {
                reload(app);
                if let Some(new) = v.get("path").and_then(Value::as_str) {
                    app.designs.selected = Some(new.to_string());
                }
            }
            Err(e) => app.set_status(e, true),
        },
        Action::Delete { path, name, folder } => app.designs.confirm = Some(Confirm { path, name, folder, focus_delete: true }),
        Action::Properties { path, name } => {
            let description = app
                .session
                .execute("designs.project_get", &json!({ "path": path }))
                .ok()
                .and_then(|v| v.get("description").and_then(Value::as_str).map(str::to_string))
                .unwrap_or_default();
            app.designs.properties = Some(crate::designs_properties::Properties::new(path, name, description));
        }
    }
}

fn confirm_delete(app: &mut SolveApp, ctx: &egui::Context) {
    let Some(confirm) = app.designs.confirm.clone() else { return };
    let mut answer = None;
    crate::frame::window(ctx, crate::i18n::tr_label("Delete"), crate::frame::Width::Normal)
        .id(egui::Id::new("sc_designs_delete"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, -60.0))
        .show(ctx, |ui| {
            ui.set_max_width(360.0);
            let text = if confirm.folder {
                crate::i18n::format("designs.delete_folder_confirm", &[("name", &confirm.name)])
            } else {
                crate::i18n::format("designs.delete_confirm", &[("name", &confirm.name)])
            };
            ui.label(text);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let delete = ui.button(crate::i18n::tr("designs.delete"));
                if confirm.focus_delete {
                    delete.request_focus();
                }
                crate::scenario::publish_handle("designs:confirm:delete", delete.rect.center());
                if delete.clicked() {
                    answer = Some(true);
                }
                let cancel = ui.button(crate::i18n::tr("designs.cancel"));
                crate::scenario::publish_handle("designs:confirm:cancel", cancel.rect.center());
                if cancel.clicked() {
                    answer = Some(false);
                }
            });
        });
    if let Some(confirm) = app.designs.confirm.as_mut() {
        confirm.focus_delete = false;
    }
    if let Some(accept) = answer {
        app.designs.confirm = None;
        if accept {
            match app.run("designs.delete", json!({ "path": confirm.path })) {
                Ok(_) => {
                    app.designs.entries = None;
                    app.designs.refresh = true;
                    app.designs.thumbnails.remove(&confirm.path);
                }
                Err(e) => app.set_status(e, true),
            }
        }
    }
}

/// The app bar's toggle: a 3x3 grid at `x`, left of the File menu.
pub fn app_bar_button(app: &mut SolveApp, ui: &mut egui::Ui, bar: egui::Rect, x: f32, idle: bool) -> bool {
    let rect = egui::Rect::from_center_size(pos2(x, bar.center().y), vec2(26.0, 26.0));
    let response = ui.interact(rect, ui.id().with(("ab", "designs")), if idle { Sense::click() } else { Sense::hover() });
    crate::scenario::publish_handle("appbar:designs", rect.center());
    crate::scenario::publish_rect("appbar:designs", rect);
    if idle && response.hovered() {
        ui.painter().rect_filled(rect, 4.0, crate::theme::Tokens::get().hover);
    }
    let opacity = if !idle {
        0.35
    } else if app.ui.show_designs {
        1.0
    } else {
        0.7
    };
    crate::icons::paint_on(ui.painter(), rect.shrink(5.0), "grid", Some(true), opacity);
    idle && response.on_hover_text(crate::i18n::tr("designs.title")).clicked()
}

/// The active repository's name, for the save dialog's breadcrumb.
pub fn active_repository_name(app: &SolveApp) -> Option<String> {
    Some(active_repository(app)?.name.clone())
}

/// The active repository's folder, when the repositories are on and it is there. Saves go inside
/// it and nowhere else.
pub fn repository_root(app: &SolveApp) -> Option<PathBuf> {
    if !app.preferences.repositories_panel || !app.services.designs_panel {
        return None;
    }
    let root = PathBuf::from(&active_repository(app)?.path);
    root.is_dir().then_some(root)
}

/// Where Save As starts: the folder the panel shows, when it is inside the active repository (and
/// still there), else the repository itself. `None`: repositories are off (or the host has no
/// panel), so the picker starts wherever the host left it.
pub fn save_folder(app: &SolveApp) -> Option<String> {
    let root = repository_root(app)?;
    let shown = app.designs.current.as_ref().filter(|shown| shown.starts_with(&root) && shown.is_dir());
    Some(shown.unwrap_or(&root).to_string_lossy().into_owned())
}

/// A file or folder dropped on the window: a design becomes a component of the open design.
pub fn drop_path(app: &mut SolveApp, path: &str) {
    if app.home.open {
        app.open_chosen_file(path);
        return;
    }
    if Path::new(path).is_dir() {
        app.set_status(crate::i18n::tr("designs.drop_folder").to_string(), true);
        return;
    }
    insert_design(app, path);
}

fn insert_design(app: &mut SolveApp, path: &str) {
    if app.home.open {
        app.open_chosen_file(path);
        return;
    }
    if Path::new(path).is_dir() {
        app.set_status(crate::i18n::tr("designs.drop_folder").to_string(), true);
        return;
    }
    if solidcraft_engine::io::is_design_path(path) {
        if same_path(app.session.path.as_deref(), path) {
            app.set_status(crate::i18n::tr("designs.insert_self").to_string(), true);
            return;
        }
        match app.run("component.insert_design", json!({ "path": path })) {
            Ok(_) => app.fit_view(),
            Err(e) => app.set_status(e, true),
        }
    } else if solidcraft_engine::io::is_kicad_path(path) {
        // A board is a whole design: it opens with its import settings, not as a component.
        crate::kicad_import_dialog::open(app, path);
    } else if solidcraft_engine::io::is_step_path(path) || solidcraft_engine::io::is_mesh_path(path) {
        app.insert_path_async(path);
    } else {
        app.set_status(crate::i18n::tr("designs.drop_unknown").to_string(), true);
    }
}

pub(crate) fn same_path(a: Option<&str>, b: &str) -> bool {
    let Some(a) = a else { return false };
    let full = |p: &str| std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p));
    full(a) == full(b)
}

/// A row released after a drag: insert it into the design under the pointer.
fn accept_drag(app: &mut SolveApp, ctx: &egui::Context) {
    if !ctx.input(|i| i.pointer.any_released()) {
        return;
    }
    if let Some(payload) = egui::DragAndDrop::take_payload::<DesignDrag>(ctx) {
        insert_design(app, &payload.path);
    }
}

/// A save changed a design's picture: show the new one next time.
pub fn invalidate(app: &mut SolveApp, path: &str) {
    app.designs.thumbnails.remove(path);
    app.designs.relist();
}

impl State {
    /// List the folder again. A listing already running asked before whatever changed (a save in
    /// place, a menu action), so it is dropped: its answer would put the old folder back.
    pub(crate) fn relist(&mut self) {
        self.entries = None;
        self.listing = None;
        self.failed = None;
    }

    /// A project's metadata or name changed: list again and drop its picture.
    pub(crate) fn refresh_after_change(&mut self, path: &str) {
        self.relist();
        self.refresh = true;
        self.thumbnails.remove(path);
    }
}
