//! Bounded reader for the KiCad PCB s-expression format.
//!
//! Board coordinates are read as KiCad stores them (X right, Y down) and the board itself is
//! placed in SolidCraft's Z-up frame with Y negated: a board point `(x, y)` becomes `(x, -y)`.
//! Footprint children (pads, silkscreen shapes) are stored in the board file already flipped
//! when the footprint is on the back, so they are placed with the footprint's position and
//! rotation alone. 3D models are authored for the front and get a half turn about X on a back
//! footprint, which also puts them below the board.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use solidcraft_doc::Mat;
use solidcraft_geom::{Loop2, Plane, Region2, Vec2, Vec3};

use crate::{IoError, Result};

/// Largest KiCad board file read.
pub const MAX_KICAD_BYTES: usize = 128 << 20;
const MAX_TOKENS: usize = 2_000_000;
const MAX_DEPTH: usize = 128;
const MAX_FOOTPRINTS: usize = 100_000;
const MAX_MODELS: usize = 500_000;
const MAX_OUTLINE_SEGMENTS: usize = 500_000;
const MAX_OUTLINE_POINTS: usize = 500_000;
const MAX_MODEL_VERTICES: usize = 5_000_000;
const MAX_MODEL_TRIANGLES: usize = 5_000_000;
const MAX_NUMBER: f64 = 1.0e9;
const MAX_TRACKS: usize = 200_000;
const MAX_VIAS: usize = 100_000;
const MAX_ZONES: usize = 50_000;
const MAX_ZONE_POINTS: usize = 500_000;
const MAX_GRAPHICS: usize = 200_000;
const MAX_TEXTS: usize = 20_000;
/// Points one text's glyph outlines may have.
const MAX_TEXT_POINTS: usize = 50_000;
const MAX_PADS: usize = 500_000;
const MAX_COPPER_ITEMS: usize = 50_000;
const MAX_SILK_ITEMS: usize = 50_000;
/// How finely arcs and circles are drawn (mm).
const CURVE_TOLERANCE: f64 = 0.02;
const MAX_CURVE_SEGMENTS: usize = 4_096;
/// Copper foil thickness (1 oz) in millimetres.
const COPPER_THICKNESS: f64 = 0.035;
/// The copper overlay's thickness (mm): a thin film on the board surface.
const OVERLAY_THICKNESS: f64 = 0.001;
/// Silkscreen ink thickness in millimetres.
const SILK_THICKNESS: f64 = 0.01;

/// The board substrate's default colour (a solder-mask green).
pub const DEFAULT_BOARD_COLOR: [f32; 3] = [0.05, 0.33, 0.18];

/// How a KiCad board comes in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KicadRepresentation {
    /// Mesh bodies (the only representation implemented).
    Mesh,
    /// A B-rep board. Not supported yet.
    Brep,
}

impl KicadRepresentation {
    /// `mesh` or `brep` (any case); `None` for anything else.
    pub fn parse(s: &str) -> Option<KicadRepresentation> {
        match s.trim().to_ascii_lowercase().as_str() {
            "mesh" => Some(KicadRepresentation::Mesh),
            "brep" | "b-rep" => Some(KicadRepresentation::Brep),
            _ => None,
        }
    }
}

/// How the board's copper (tracks, vias, zones, pads and copper text) comes in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KicadCopper {
    /// Leave the copper out.
    #[default]
    None,
    /// A flat copper surface on the board, without thickness: an overlay for the board's look.
    Overlay,
    /// Copper with its real thickness, as mesh bodies.
    Mesh,
    /// Exact B-rep copper: solids with the copper's real thickness, built as a STEP import in
    /// the board's component (copper on each layer is one body of separate lumps).
    Brep,
}

impl KicadCopper {
    /// `none`, `overlay`, `mesh`/`modelled`/`modeled` or `brep` (any case).
    pub fn parse(s: &str) -> Option<KicadCopper> {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" | "off" => Some(KicadCopper::None),
            "overlay" => Some(KicadCopper::Overlay),
            "mesh" | "modelled" | "modeled" => Some(KicadCopper::Mesh),
            "brep" | "b-rep" => Some(KicadCopper::Brep),
            _ => None,
        }
    }

    /// The option's parameter value.
    pub fn as_str(self) -> &'static str {
        match self {
            KicadCopper::None => "none",
            KicadCopper::Overlay => "overlay",
            KicadCopper::Mesh => "mesh",
            KicadCopper::Brep => "brep",
        }
    }
}

/// What the importer brings in besides the board substrate.
#[derive(Clone, Debug, PartialEq)]
pub struct KicadOptions {
    /// Board thickness override in millimetres (`None`: the board's own).
    pub thickness: Option<f64>,
    /// Load the referenced 3D models (STEP and VRML).
    pub components: bool,
    /// The front and back copper: left out, an overlay, or modelled with thickness.
    pub copper: KicadCopper,
    /// Include pads in the copper (when it is not left out).
    pub pads: bool,
    /// Build the front and back silkscreen.
    pub silkscreen: bool,
    /// Board substrate colour (`None`: [`DEFAULT_BOARD_COLOR`]).
    pub color: Option<[u8; 3]>,
    /// Mesh or B-rep (B-rep is refused).
    pub representation: KicadRepresentation,
}

impl Default for KicadOptions {
    fn default() -> Self {
        KicadOptions {
            thickness: None,
            components: true,
            copper: KicadCopper::None,
            pads: true,
            silkscreen: true,
            color: None,
            representation: KicadRepresentation::Mesh,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Expr<'a> {
    List(Vec<Expr<'a>>),
    Atom(Cow<'a, str>),
}

#[derive(Clone, Debug, PartialEq)]
enum Token<'a> {
    Open,
    Close,
    Atom(&'a str),
    String(Cow<'a, str>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct KicadBoard {
    pub name: String,
    pub thickness: f64,
    /// Closed Edge.Cuts contours in the board frame (X right, Y up; KiCad's Y is negated).
    pub outlines: Vec<Vec<[f64; 2]>>,
    pub footprints: Vec<KicadFootprint>,
    /// Front/back copper tracks and arcs. Their points are in the board frame.
    pub tracks: Vec<KicadStroke>,
    pub vias: Vec<KicadVia>,
    /// Filled copper zone polygons per layer.
    pub zones: Vec<KicadZone>,
    /// Board-level graphics on silk or copper layers.
    pub graphics: Vec<KicadStroke>,
    /// Board-level text on silk or copper layers, in the board frame.
    pub texts: Vec<KicadText>,
    /// Drilled holes (pads and vias) in the board frame.
    pub drills: Vec<KicadDrill>,
    pub color: [f32; 3],
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KicadFootprint {
    pub reference: String,
    pub value: String,
    pub layer: String,
    /// Board X/Y in millimetres and footprint rotation in degrees.
    pub at: [f64; 3],
    pub models: Vec<KicadModel>,
    /// Pads in the footprint frame, as the board file stores them.
    pub pads: Vec<KicadPad>,
    /// Graphics on silk, copper or Edge.Cuts in the footprint frame, as the board file stores them.
    pub graphics: Vec<KicadStroke>,
    /// Text on silk or copper in the footprint frame, as the board file stores it.
    pub texts: Vec<KicadText>,
}

/// Text on a copper or silkscreen layer, in its containing frame.
#[derive(Clone, Debug, PartialEq)]
pub struct KicadText {
    pub text: String,
    /// X/Y in millimetres and rotation in degrees, in the containing frame.
    pub at: [f64; 3],
    pub layer: String,
    /// Cap height and glyph width in millimetres.
    pub height: f64,
    pub width: f64,
    /// Stroke width in millimetres (the outlines are filled, so it only records the file's value).
    pub thickness: f64,
    pub bold: bool,
    pub italic: bool,
    /// The text reads from the other side of the board.
    pub mirror: bool,
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KicadModel {
    pub path: String,
    /// Model-local translation in millimetres.
    pub at: [f64; 3],
    pub scale: [f64; 3],
    /// Model-local Euler rotation in degrees, applied about X, then Y, then Z.
    pub rotate: [f64; 3],
}

/// A pad in the footprint frame. `at[2]` is its rotation in degrees.
#[derive(Clone, Debug, PartialEq)]
pub struct KicadPad {
    pub at: [f64; 3],
    pub size: [f64; 2],
    /// `circle`, `rect`, `oval`, `roundrect`, `trapezoid` or `custom`.
    pub shape: String,
    pub layers: Vec<String>,
    /// `(width, height)` of the drill, if any.
    pub drill: Option<[f64; 2]>,
    pub roundrect_ratio: f64,
}

/// A polyline of a schematic or copper object in its containing frame.
#[derive(Clone, Debug, PartialEq)]
pub struct KicadStroke {
    pub layer: String,
    pub points: Vec<[f64; 2]>,
    pub closed: bool,
    pub filled: bool,
    pub width: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KicadVia {
    /// Board frame.
    pub at: [f64; 2],
    pub size: f64,
    pub drill: f64,
    pub layers: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KicadZone {
    pub layer: String,
    /// Each filled polygon is a closed contour in the board frame; later ones may be holes.
    pub contours: Vec<Vec<[f64; 2]>>,
}

/// A hole through the board, in the board frame.
#[derive(Clone, Debug, PartialEq)]
pub struct KicadDrill {
    pub center: [f64; 2],
    /// Round holes have equal sides; a slot is a stadium of this width and height.
    pub size: [f64; 2],
}

/// A board's copper as B-rep solids: STEP text for a second import in the board's component.
#[derive(Clone, Debug, Default)]
pub struct KicadCopperBrep {
    /// The copper as a STEP file (ISO 10303-21), a body per copper layer.
    pub step: String,
    /// How many board items the copper has (for the reply's description).
    pub items: usize,
}

/// A KiCad board and the reusable component placements it references.
pub struct KicadImport {
    pub feature: crate::MeshFeature,
    pub plan: crate::MeshPlan,
    /// B-rep copper, when the options ask for it: the engine adds it as a STEP import in the
    /// board's component (the board and its models are the mesh feature).
    pub copper: Option<KicadCopperBrep>,
}

/// Load a KiCad model reference. The engine resolves project and environment paths.
pub type KicadLoader<'a> = &'a mut dyn FnMut(&str) -> std::result::Result<Vec<u8>, String>;

/// Is this a KiCad project (`.kicad_pro`) or PCB (`.kicad_pcb`) path, any case?
pub fn is_kicad_path(path: &str) -> bool {
    Path::new(path).extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("kicad_pro") || e.eq_ignore_ascii_case("kicad_pcb"))
}

/// Resolve a selected KiCad project to its same-stem board, or keep a directly selected board.
pub fn kicad_board_path(path: &Path) -> Result<PathBuf> {
    match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("kicad_pro") => Ok(path.with_extension("kicad_pcb")),
        Some("kicad_pcb") => Ok(path.to_path_buf()),
        _ => Err(IoError::Format(path.extension().and_then(|e| e.to_str()).unwrap_or("unknown").to_string())),
    }
}

/// Parse a KiCad PCB board. A project file is not board content; callers resolve it to its
/// same-stem `.kicad_pcb` before calling this function.
pub fn parse_kicad_board(bytes: &[u8], file: &str) -> Result<KicadBoard> {
    parse_kicad_board_with(bytes, file, KicadOptions::default())
}

/// Parse a KiCad PCB board with the importer's options (only the colour affects parsing).
pub fn parse_kicad_board_with(bytes: &[u8], file: &str, options: KicadOptions) -> Result<KicadBoard> {
    if bytes.len() > MAX_KICAD_BYTES {
        return Err(IoError::Invalid(format!("KiCad PCB file too large (limit {} MB)", MAX_KICAD_BYTES >> 20)));
    }
    let source = std::str::from_utf8(bytes).map_err(|_| IoError::Invalid("KiCad PCB file is not UTF-8".into()))?;
    let tokens = tokenize(source)?;
    let mut parser = Parser { tokens, next: 0, nodes: 0 };
    let root = parser.expression(0)?;
    if parser.next != parser.tokens.len() {
        return Err(IoError::Invalid("KiCad PCB has trailing s-expression data".into()));
    }
    if head(&root) != Some("kicad_pcb") {
        return Err(IoError::Invalid("not a KiCad PCB board file".into()));
    }

    let general = field(&root, "general");
    let thickness = general
        .and_then(|g| field(g, "thickness"))
        .and_then(|e| numbers(e, 1).ok())
        .and_then(|n| n.first().copied())
        .filter(|v| *v > 0.0 && *v <= 1000.0)
        .unwrap_or(1.6);

    let color = options.color.map(|c| [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0]).unwrap_or(DEFAULT_BOARD_COLOR);
    let mut board = KicadBoard {
        name: Path::new(file).file_stem().and_then(|n| n.to_str()).filter(|n| !n.trim().is_empty()).unwrap_or("PCB").to_string(),
        thickness,
        outlines: Vec::new(),
        footprints: Vec::new(),
        tracks: Vec::new(),
        vias: Vec::new(),
        zones: Vec::new(),
        graphics: Vec::new(),
        texts: Vec::new(),
        drills: Vec::new(),
        color,
        warnings: Vec::new(),
    };
    let mut segments = Vec::new();
    let mut model_count = 0usize;
    let mut pad_count = 0usize;
    let mut text_count = 0usize;
    let mut extra_text_seen = false;
    for item in children(&root) {
        match head(item) {
            Some("footprint" | "module") => {
                if board.footprints.len() >= MAX_FOOTPRINTS {
                    return Err(IoError::Invalid(format!("KiCad PCB has more than {MAX_FOOTPRINTS} footprints")));
                }
                match parse_footprint(item, &mut model_count, &mut pad_count, &mut text_count, &mut board.warnings, &mut extra_text_seen) {
                    Ok(footprint) => {
                        // Footprint graphics on Edge.Cuts are board edges, in board coordinates.
                        let transform = footprint_world(&footprint);
                        for stroke in &footprint.graphics {
                            if stroke.layer == "Edge.Cuts" {
                                push_stroke_segments(stroke, &transform, &mut segments);
                            }
                        }
                        board.footprints.push(footprint);
                    }
                    Err(reason) => board.warnings.push(format!("skipped a footprint: {reason}")),
                }
            }
            Some(kind @ ("gr_line" | "gr_rect" | "gr_arc" | "gr_circle" | "gr_poly" | "gr_curve" | "bezier")) => {
                let Some(layer) = layer_of(item) else { continue };
                let stroke = match parse_graphic(kind, item, &mut board.warnings) {
                    Some(stroke) => stroke,
                    None => continue,
                };
                if layer == "Edge.Cuts" {
                    push_stroke_segments(&stroke, &board_to_world(), &mut segments);
                } else if is_silk(&layer) || is_outer_copper(&layer) {
                    if board.graphics.len() >= MAX_GRAPHICS {
                        board.warnings.push("additional board graphics were skipped".into());
                    } else {
                        board.graphics.push(KicadStroke { layer, ..stroke });
                    }
                }
            }
            Some("gr_text") => {
                if let Some(text) = parse_text(item, 1) {
                    if board.texts.len() >= MAX_TEXTS {
                        board.warnings.push("additional board text was skipped".into());
                    } else {
                        board.texts.push(text);
                    }
                }
            }
            Some("gr_text_box" | "dimension" | "table") => extra_text_seen = true,
            Some("segment") => {
                if let Some(track) = parse_track(item, &mut board.warnings) {
                    if board.tracks.len() >= MAX_TRACKS {
                        board.warnings.push("additional tracks were skipped".into());
                    } else {
                        board.tracks.push(track);
                    }
                }
            }
            Some("arc") => {
                if let Some(track) = parse_track_arc(item, &mut board.warnings) {
                    if board.tracks.len() >= MAX_TRACKS {
                        board.warnings.push("additional tracks were skipped".into());
                    } else {
                        board.tracks.push(track);
                    }
                }
            }
            Some("via") => {
                if let Some(via) = parse_via(item, &mut board.warnings) {
                    if board.vias.len() >= MAX_VIAS {
                        board.warnings.push("additional vias were skipped".into());
                    } else {
                        board.vias.push(via);
                    }
                }
            }
            Some("zone") => {
                if let Some(zone) = parse_zone(item, &mut board.warnings) {
                    if board.zones.len() >= MAX_ZONES {
                        board.warnings.push("additional zones were skipped".into());
                    } else {
                        board.zones.push(zone);
                    }
                }
            }
            _ => {}
        }
    }
    if segments.len() > MAX_OUTLINE_SEGMENTS {
        return Err(IoError::Invalid(format!("KiCad board has more than {MAX_OUTLINE_SEGMENTS} Edge.Cuts segments")));
    }
    board.outlines = join_outline_segments(segments, &mut board.warnings);
    // Drilled holes are cut from the substrate.
    for footprint in &board.footprints {
        let transform = footprint_world(footprint);
        for pad in &footprint.pads {
            let Some(drill) = pad.drill else { continue };
            let center = apply2(&transform, pad.at[0], pad.at[1]);
            board.drills.push(KicadDrill { center, size: [drill[0].abs(), drill[1].abs()] });
        }
    }
    for via in &board.vias {
        board.drills.push(KicadDrill { center: [via.at[0], -via.at[1]], size: [via.drill.abs(), via.drill.abs()] });
    }
    if extra_text_seen {
        board.warnings.push("text boxes, dimensions and tables are not modeled yet".into());
    }
    Ok(board)
}

/// The matrix that maps a board point (KiCad's Y down) to SolidCraft's Y up.
fn board_to_world() -> Mat {
    let mut m = solidcraft_doc::IDENTITY;
    m[1][1] = -1.0;
    m
}

/// The world transform of a footprint's own frame: KiCad's board position (Y negated), its
/// rotation, and the Y-down to Y-up map for footprint-local points.
fn footprint_world(footprint: &KicadFootprint) -> Mat {
    let position = solidcraft_doc::rigid(Vec3::new(footprint.at[0], -footprint.at[1], 0.0), Vec3::ZERO, Vec3::Z, footprint.at[2].to_radians());
    solidcraft_doc::mat_mul(&position, &board_to_world())
}

/// The matrix that puts a footprint-local point (board file convention) into the world.
fn apply2(m: &Mat, x: f64, y: f64) -> [f64; 2] {
    let p = solidcraft_doc::apply_point(m, Vec3::new(x, y, 0.0));
    [p.x, p.y]
}

fn layer_of(expr: &Expr<'_>) -> Option<String> {
    field(expr, "layer").and_then(|e| children(e).get(1)).and_then(atom).map(str::to_string)
}

fn is_silk(layer: &str) -> bool {
    layer == "F.SilkS" || layer == "B.SilkS"
}

fn is_outer_copper(layer: &str) -> bool {
    layer == "F.Cu" || layer == "B.Cu"
}

/// Turn a stroke into line segments in its containing frame, applying `transform`.
fn push_stroke_segments(stroke: &KicadStroke, transform: &Mat, out: &mut Vec<([f64; 2], [f64; 2])>) {
    let points: Vec<[f64; 2]> = stroke.points.iter().map(|p| apply2(transform, p[0], p[1])).collect();
    if points.len() < 2 {
        return;
    }
    for pair in points.windows(2) {
        if let [a, b] = pair {
            out.push((*a, *b));
        }
    }
    if stroke.closed
        && let (Some(a), Some(b)) = (points.last(), points.first())
    {
        out.push((*a, *b));
    }
}

/// A graphical item as a stroke. Points are in the object's frame (no mapping applied).
fn parse_graphic(kind: &str, expr: &Expr<'_>, warnings: &mut Vec<String>) -> Option<KicadStroke> {
    let layer = layer_of(expr).unwrap_or_default();
    let width =
        field(expr, "width").and_then(|e| numbers(e, 1).ok()).and_then(|n| n.first().copied()).filter(|w| w.is_finite() && *w >= 0.0).unwrap_or(0.15);
    let filled = field(expr, "fill").and_then(|e| children(e).get(1)).and_then(atom) == Some("yes");
    let points = match kind {
        "gr_line" => match (point_of(expr, "start"), point_of(expr, "end")) {
            (Some(a), Some(b)) => vec![a, b],
            _ => return None,
        },
        "gr_rect" => match (point_of(expr, "start"), point_of(expr, "end")) {
            (Some(a), Some(b)) => vec![a, [b[0], a[1]], b, [a[0], b[1]]],
            _ => return None,
        },
        "gr_arc" => match (point_of(expr, "start"), point_of(expr, "mid"), point_of(expr, "end")) {
            (Some(a), Some(m), Some(b)) => arc_points(a, m, b),
            _ => return None,
        },
        "gr_circle" => match (point_of(expr, "center"), point_of(expr, "end")) {
            (Some(c), Some(e)) => circle_points(c, distance2(c, e)),
            _ => return None,
        },
        "gr_poly" => poly_points(expr)?,
        "gr_curve" | "bezier" => match poly_points(expr) {
            Some(points) if points.len() == 4 => bezier_points(points[0], points[1], points[2], points[3]),
            _ => return None,
        },
        _ => return None,
    };
    if points.len() < 2 {
        warnings.push("skipped a degenerate graphic item".into());
        return None;
    }
    let closed = matches!(kind, "gr_circle" | "gr_poly" | "gr_rect" | "bezier") || filled;
    Some(KicadStroke { layer, points, closed, filled, width })
}

fn point_of(expr: &Expr<'_>, name: &str) -> Option<[f64; 2]> {
    let values = numbers(field(expr, name)?, 2).ok()?;
    Some([*values.first()?, *values.get(1)?])
}

fn poly_points(expr: &Expr<'_>) -> Option<Vec<[f64; 2]>> {
    let pts = field(expr, "pts")?;
    let mut out = Vec::new();
    for item in children(pts).iter().filter(|e| head(e) == Some("xy")) {
        let n = numbers(item, 2).ok()?;
        out.push([*n.first()?, *n.get(1)?]);
    }
    (!out.is_empty()).then_some(out)
}

fn distance2(a: [f64; 2], b: [f64; 2]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

fn circle_points(center: [f64; 2], radius: f64) -> Vec<[f64; 2]> {
    if !(radius.is_finite() && radius > 1e-6) {
        return Vec::new();
    }
    let n = ((std::f64::consts::TAU * radius / CURVE_TOLERANCE).ceil() as usize).clamp(8, MAX_CURVE_SEGMENTS);
    (0..n)
        .map(|i| {
            let a = std::f64::consts::TAU * i as f64 / n as f64;
            [center[0] + radius * a.cos(), center[1] + radius * a.sin()]
        })
        .collect()
}

/// A circular arc through `start`, `mid` and `end`.
fn arc_points(start: [f64; 2], mid: [f64; 2], end: [f64; 2]) -> Vec<[f64; 2]> {
    let (a, b, c) = (start, mid, end);
    let d = 2.0 * (a[0] * (b[1] - c[1]) + b[0] * (c[1] - a[1]) + c[0] * (a[1] - b[1]));
    if d.abs() < 1e-12 {
        return vec![a, c];
    }
    let ux =
        ((a[0] * a[0] + a[1] * a[1]) * (b[1] - c[1]) + (b[0] * b[0] + b[1] * b[1]) * (c[1] - a[1]) + (c[0] * c[0] + c[1] * c[1]) * (a[1] - b[1])) / d;
    let uy =
        ((a[0] * a[0] + a[1] * a[1]) * (c[0] - b[0]) + (b[0] * b[0] + b[1] * b[1]) * (a[0] - c[0]) + (c[0] * c[0] + c[1] * c[1]) * (b[0] - a[0])) / d;
    let center = [ux, uy];
    let radius = distance2(center, a);
    if !(radius.is_finite() && radius > 1e-9) {
        return vec![a, c];
    }
    let angle = |p: [f64; 2]| (p[1] - center[1]).atan2(p[0] - center[0]);
    let (a0, am, a1) = (angle(a), angle(b), angle(c));
    let norm = |x: f64| x.rem_euclid(std::f64::consts::TAU);
    let ccw = norm(am - a0) <= norm(a1 - a0);
    let sweep = if ccw { norm(a1 - a0) } else { -norm(a0 - a1) };
    let n = ((sweep.abs() * radius / CURVE_TOLERANCE).ceil() as usize).clamp(2, MAX_CURVE_SEGMENTS);
    (0..=n)
        .map(|i| {
            let t = a0 + sweep * i as f64 / n as f64;
            [center[0] + radius * t.cos(), center[1] + radius * t.sin()]
        })
        .collect()
}

fn bezier_points(p0: [f64; 2], p1: [f64; 2], p2: [f64; 2], p3: [f64; 2]) -> Vec<[f64; 2]> {
    let rough = distance2(p0, p1) + distance2(p1, p2) + distance2(p2, p3);
    let n = ((rough / CURVE_TOLERANCE).ceil() as usize).clamp(4, MAX_CURVE_SEGMENTS);
    (0..=n)
        .map(|i| {
            let t = i as f64 / n as f64;
            let u = 1.0 - t;
            [
                u * u * u * p0[0] + 3.0 * u * u * t * p1[0] + 3.0 * u * t * t * p2[0] + t * t * t * p3[0],
                u * u * u * p0[1] + 3.0 * u * u * t * p1[1] + 3.0 * u * t * t * p2[1] + t * t * t * p3[1],
            ]
        })
        .collect()
}

fn parse_track(expr: &Expr<'_>, warnings: &mut Vec<String>) -> Option<KicadStroke> {
    let layer = layer_of(expr)?;
    if !is_outer_copper(&layer) {
        return None;
    }
    let a = point_of(expr, "start")?;
    let b = point_of(expr, "end")?;
    let width = field(expr, "width").and_then(|e| numbers(e, 1).ok()).and_then(|n| n.first().copied()).unwrap_or(0.25);
    if !(width.is_finite() && width > 0.0) {
        warnings.push("skipped a track with an invalid width".into());
        return None;
    }
    Some(KicadStroke { layer, points: vec![a, b], closed: false, filled: true, width })
}

fn parse_track_arc(expr: &Expr<'_>, warnings: &mut Vec<String>) -> Option<KicadStroke> {
    let layer = layer_of(expr)?;
    if !is_outer_copper(&layer) {
        return None;
    }
    let a = point_of(expr, "start")?;
    let m = point_of(expr, "mid")?;
    let b = point_of(expr, "end")?;
    let width = field(expr, "width").and_then(|e| numbers(e, 1).ok()).and_then(|n| n.first().copied()).unwrap_or(0.25);
    if !(width.is_finite() && width > 0.0) {
        warnings.push("skipped a track arc with an invalid width".into());
        return None;
    }
    Some(KicadStroke { layer, points: arc_points(a, m, b), closed: false, filled: true, width })
}

fn parse_via(expr: &Expr<'_>, warnings: &mut Vec<String>) -> Option<KicadVia> {
    let at = point_of(expr, "at")?;
    let size = field(expr, "size").and_then(|e| numbers(e, 1).ok()).and_then(|n| n.first().copied()).unwrap_or(0.6);
    let drill = field(expr, "drill").and_then(|e| numbers(e, 1).ok()).and_then(|n| n.first().copied()).unwrap_or(0.3);
    if !(size.is_finite() && size > 0.0 && drill.is_finite() && drill > 0.0) {
        warnings.push("skipped a via with an invalid size".into());
        return None;
    }
    let layers = field(expr, "layers").map(|e| children(e).iter().skip(1).filter_map(atom).map(str::to_string).collect()).unwrap_or_default();
    Some(KicadVia { at, size, drill, layers })
}

fn parse_zone(expr: &Expr<'_>, warnings: &mut Vec<String>) -> Option<KicadZone> {
    let layer = layer_of(expr)?;
    if !is_outer_copper(&layer) {
        return None;
    }
    let mut contours = Vec::new();
    let mut total = 0usize;
    for fill in children(expr).iter().filter(|e| head(e) == Some("filled_polygon")) {
        if let Some(points) = poly_points(fill) {
            total = total.saturating_add(points.len());
            if total > MAX_ZONE_POINTS {
                warnings.push("a copper zone was too large and was skipped".into());
                return None;
            }
            contours.push(points);
        }
    }
    if contours.is_empty() {
        return None;
    }
    Some(KicadZone { layer, contours })
}

fn join_outline_segments(segments: Vec<([f64; 2], [f64; 2])>, warnings: &mut Vec<String>) -> Vec<Vec<[f64; 2]>> {
    let mut adjacent = std::collections::HashMap::<(i64, i64), Vec<usize>>::new();
    for (index, (a, b)) in segments.iter().enumerate() {
        adjacent.entry(point_key(*a)).or_default().push(index);
        adjacent.entry(point_key(*b)).or_default().push(index);
    }
    let mut contours = Vec::new();
    let mut used = vec![false; segments.len()];
    for index in 0..segments.len() {
        if used[index] {
            continue;
        }
        used[index] = true;
        let (start, end) = segments[index];
        let mut contour = vec![start, end];
        let mut closed = near2(start, end);
        while !closed {
            let last = contour.last().copied().unwrap_or(start);
            let next_index = adjacent.get(&point_key(last)).and_then(|indices| {
                indices.iter().copied().find(|index| !used[*index] && segments.get(*index).is_some_and(|(a, b)| near2(*a, last) || near2(*b, last)))
            });
            let Some(next_index) = next_index else { break };
            used[next_index] = true;
            let (a, b) = segments[next_index];
            let next = if near2(a, last) { b } else { a };
            contour.push(next);
            if contour.len() > MAX_OUTLINE_POINTS {
                contour.clear();
                break;
            }
            closed = near2(next, start);
        }
        if closed && contour.len() >= 4 {
            contour.pop();
            contours.push(contour);
        } else if !contour.is_empty() {
            warnings.push("skipped an open Edge.Cuts contour".into());
        }
    }
    contours
}

fn point_key(point: [f64; 2]) -> (i64, i64) {
    ((point[0] * 1e6).round() as i64, (point[1] * 1e6).round() as i64)
}

fn near2(a: [f64; 2], b: [f64; 2]) -> bool {
    (a[0] - b[0]).abs() <= 1e-6 && (a[1] - b[1]).abs() <= 1e-6
}

/// Make a mesh body for the board: its largest (or several) closed outlines, with board cutouts
/// and drilled holes removed. `thickness` is in millimetres.
pub fn kicad_board_mesh(board: &KicadBoard, thickness: f64) -> Result<crate::MeshObject> {
    if !thickness.is_finite() || thickness <= 0.0 || thickness > 1000.0 {
        return Err(IoError::Invalid("KiCad board thickness must be finite and between 0 and 1000 mm".into()));
    }
    let loops = outline_loops(board)?;
    let regions = regions_from_loops(loops.clone(), Some(&board.drills));
    let mesh = extrude_regions(&regions, 0.0, thickness).or_else(|_| {
        // A drill that touches a cutout can defeat the profile: keep the board and lose the holes.
        let regions = regions_from_loops(loops, None);
        extrude_regions(&regions, 0.0, thickness)
    })?;
    Ok(crate::MeshObject {
        name: board.name.clone(),
        positions: mesh.positions,
        triangles: mesh.triangles,
        color: Some(board.color),
        ..Default::default()
    })
}

/// Read a board and its referenced STEP/VRML models into one mesh feature and placement plan.
pub fn read_kicad(bytes: &[u8], file: &str, options: &KicadOptions, load: KicadLoader<'_>) -> Result<KicadImport> {
    if options.representation == KicadRepresentation::Brep {
        return Err(IoError::Invalid("B-rep KiCad boards are not supported yet; choose mesh".into()));
    }
    let board = parse_kicad_board_with(bytes, file, options.clone())?;
    let thickness = options.thickness.unwrap_or(board.thickness);
    let board_mesh = kicad_board_mesh(&board, thickness)?;
    let mut meshes = vec![mesh_data(board_mesh.name, board_mesh.positions, board_mesh.triangles, board_mesh.color)];
    let mut plan = crate::MeshPlan { bodies: vec![None], ..Default::default() };
    let mut warnings = board.warnings.clone();
    let mut slots = std::collections::HashMap::<(String, [u64; 10]), usize>::new();
    let mut totals = (0usize, 0usize);
    if options.components {
        for footprint in &board.footprints {
            let back = footprint.layer.starts_with("B.");
            for model in &footprint.models {
                let ext = Path::new(&model.path).extension().and_then(|e| e.to_str()).unwrap_or("");
                if !ext.eq_ignore_ascii_case("step") && !ext.eq_ignore_ascii_case("stp") && !ext.eq_ignore_ascii_case("wrl") {
                    warnings.push(format!("{}: unsupported 3D model format", model.path));
                    continue;
                }
                if model.scale.iter().any(|v| !v.is_finite() || *v <= 0.0) {
                    warnings.push(format!("{}: invalid or mirrored 3D model scale is unsupported", model.path));
                    continue;
                }
                let key = (model.path.clone(), model_transform_key(model, back));
                let slot = if let Some(slot) = slots.get(&key).copied() {
                    slot
                } else {
                    let model_bytes = match load(&model.path) {
                        Ok(bytes) => bytes,
                        Err(reason) => {
                            warnings.push(format!("{}: {reason}", model.path));
                            continue;
                        }
                    };
                    let (imported_meshes, reason) = if ext.eq_ignore_ascii_case("wrl") {
                        if model_bytes.len() > crate::MAX_VRML_BYTES {
                            (Vec::new(), Some("model file exceeds the VRML size limit".to_string()))
                        } else {
                            match crate::vrml_in::read_vrml(&model_bytes, &model.path) {
                                Ok(m) => {
                                    warnings.extend(m.warnings.iter().map(|w| format!("{}: {w}", model.path)));
                                    match solidcraft_kernel::mesh_body(&m.positions, &m.triangles) {
                                        Ok(_) => (vec![mesh_data(component_name(footprint, &model.path), m.positions, m.triangles, m.color)], None),
                                        Err(e) => (Vec::new(), Some(e.to_string())),
                                    }
                                }
                                Err(e) => (Vec::new(), Some(e.to_string())),
                            }
                        }
                    } else {
                        if model_bytes.len() > crate::MAX_STEP_BYTES {
                            (Vec::new(), Some("model file exceeds the STEP size limit".to_string()))
                        } else {
                            let text = String::from_utf8_lossy(&model_bytes);
                            match solidcraft_kernel::step_import_shared(&text) {
                                Ok(imported) => {
                                    let mut out = Vec::new();
                                    let mut failed = None;
                                    for body in &imported.bodies {
                                        let mesh = match body.body.tessellate(0.1) {
                                            Ok(mesh) => mesh,
                                            Err(error) => {
                                                failed = Some(error.to_string());
                                                break;
                                            }
                                        };
                                        totals.0 = totals.0.saturating_add(mesh.positions.len());
                                        totals.1 = totals.1.saturating_add(mesh.triangles.len());
                                        if totals.0 > MAX_MODEL_VERTICES || totals.1 > MAX_MODEL_TRIANGLES {
                                            failed = Some("KiCad model geometry exceeds the import limits".into());
                                            break;
                                        }
                                        out.push(mesh_data(body.name.clone(), mesh.positions, mesh.triangles, body.color));
                                    }
                                    (out, failed)
                                }
                                Err(error) => (Vec::new(), Some(error.to_string())),
                            }
                        }
                    };
                    if let Some(reason) = reason {
                        warnings.push(format!("{}: {reason}", model.path));
                        continue;
                    }
                    if imported_meshes.is_empty() {
                        warnings.push(format!("{}: no solid bodies found", model.path));
                        continue;
                    }
                    let slot = plan.slots.len();
                    plan.slots.push(component_name(footprint, &model.path));
                    let transform = model_local_matrix(model, back);
                    let mut failed = None;
                    let mut placed = Vec::with_capacity(imported_meshes.len());
                    for mesh in imported_meshes {
                        let positions: Vec<Vec3> = mesh
                            .positions
                            .as_chunks::<3>()
                            .0
                            .iter()
                            .map(|p| solidcraft_doc::apply_point(&transform, Vec3::new(p[0], p[1], p[2])))
                            .collect();
                        let triangles: Vec<[u32; 3]> = mesh.triangles.as_chunks::<3>().0.to_vec();
                        if let Err(error) = solidcraft_kernel::mesh_body(&positions, &triangles) {
                            failed = Some(error.to_string());
                            break;
                        }
                        placed.push(mesh_data(mesh.name, positions, triangles, mesh.color));
                    }
                    if let Some(reason) = failed {
                        warnings.push(format!("{}: {reason}", model.path));
                        continue;
                    }
                    for mesh in placed {
                        meshes.push(mesh);
                        plan.bodies.push(Some(slot));
                    }
                    slots.insert(key, slot);
                    slot
                };
                plan.places.push(crate::PlanPlace { slot, parent: None, transform: footprint_place(footprint) });
            }
        }
    }
    let mut copper = None;
    if options.copper == KicadCopper::Brep {
        copper = Some(copper_brep(&board, options.pads, thickness, &mut warnings)?);
    } else if options.copper != KicadCopper::None {
        for (mesh, layer) in copper_meshes(&board, options.pads, thickness, options.copper, &mut warnings) {
            meshes.push(mesh_data(format!("Copper {layer}"), mesh.positions, mesh.triangles, Some(COPPER_COLOR)));
            plan.bodies.push(None);
        }
    }
    if options.silkscreen {
        for (mesh, layer) in silkscreen_meshes(&board, thickness, &mut warnings) {
            meshes.push(mesh_data(format!("Silkscreen {layer}"), mesh.positions, mesh.triangles, Some([0.92, 0.92, 0.90])));
            plan.bodies.push(None);
        }
    }
    let body_names: Vec<String> = meshes.iter().map(|mesh| mesh.name.clone()).collect();
    let file_name = Path::new(file).file_name().map(|name| name.to_string_lossy().to_string()).unwrap_or_else(|| file.to_string());
    let feature = crate::MeshFeature {
        name: board.name.clone(),
        kind: solidcraft_doc::FeatureKind::MeshImport { file: file_name, meshes },
        body_names,
        warnings,
    };
    Ok(KicadImport { feature, plan, copper })
}

fn component_name(footprint: &KicadFootprint, path: &str) -> String {
    let label = footprint.reference.trim();
    if label.is_empty() { path.to_string() } else { label.to_string() }
}

/// A footprint's placement in the world (its own rotation; back-side models are baked).
fn footprint_place(footprint: &KicadFootprint) -> Mat {
    solidcraft_doc::rigid(Vec3::new(footprint.at[0], -footprint.at[1], 0.0), Vec3::ZERO, Vec3::Z, footprint.at[2].to_radians())
}

/// A model's transform in the footprint's 3D frame: translation, a half turn about X on the
/// back, the Euler rotation (X, then Y, then Z) and the scale.
fn model_local_matrix(model: &KicadModel, back: bool) -> Mat {
    let mut m = solidcraft_doc::rigid(Vec3::new(model.at[0], model.at[1], model.at[2]), Vec3::ZERO, Vec3::Z, 0.0);
    if back {
        m = solidcraft_doc::mat_mul(&m, &solidcraft_doc::rigid(Vec3::ZERO, Vec3::ZERO, Vec3::X, std::f64::consts::PI));
    }
    let rx = solidcraft_doc::rigid(Vec3::ZERO, Vec3::ZERO, Vec3::X, model.rotate[0].to_radians());
    let ry = solidcraft_doc::rigid(Vec3::ZERO, Vec3::ZERO, Vec3::Y, model.rotate[1].to_radians());
    let rz = solidcraft_doc::rigid(Vec3::ZERO, Vec3::ZERO, Vec3::Z, model.rotate[2].to_radians());
    m = solidcraft_doc::mat_mul(&m, &solidcraft_doc::mat_mul(&rz, &solidcraft_doc::mat_mul(&ry, &rx)));
    let mut scale = solidcraft_doc::IDENTITY;
    scale[0][0] = model.scale[0];
    scale[1][1] = model.scale[1];
    scale[2][2] = model.scale[2];
    solidcraft_doc::mat_mul(&m, &scale)
}

fn mesh_data(name: String, positions: Vec<Vec3>, triangles: Vec<[u32; 3]>, color: Option<[f32; 3]>) -> solidcraft_doc::MeshData {
    solidcraft_doc::MeshData {
        name,
        positions: positions.into_iter().flat_map(|p| [p.x, p.y, p.z]).collect(),
        triangles: triangles.into_iter().flatten().collect(),
        color,
    }
}

fn model_transform_key(model: &KicadModel, back: bool) -> [u64; 10] {
    [
        model.at[0].to_bits(),
        model.at[1].to_bits(),
        model.at[2].to_bits(),
        model.scale[0].to_bits(),
        model.scale[1].to_bits(),
        model.scale[2].to_bits(),
        model.rotate[0].to_bits(),
        model.rotate[1].to_bits(),
        model.rotate[2].to_bits(),
        u64::from(back),
    ]
}

/// The closed contours of the board's Edge.Cuts, in the world frame.
fn outline_loops(board: &KicadBoard) -> Result<Vec<Loop2>> {
    let mut loops = Vec::new();
    for outline in &board.outlines {
        if let Some(l) = clean_loop(outline) {
            loops.push(l);
        }
    }
    if loops.is_empty() {
        return Err(IoError::Invalid("KiCad board has no closed Edge.Cuts outline".into()));
    }
    Ok(loops)
}

fn clean_loop(points: &[[f64; 2]]) -> Option<Loop2> {
    let mut pts = Vec::with_capacity(points.len());
    for p in points {
        if !(p[0].is_finite() && p[1].is_finite()) || p[0].abs() > MAX_NUMBER || p[1].abs() > MAX_NUMBER {
            return None;
        }
        if pts.last().is_none_or(|q: &Vec2| (q.x - p[0]).hypot(q.y - p[1]) > 1e-9) {
            pts.push(Vec2::new(p[0], p[1]));
        }
    }
    if pts.len() > 3 && (pts[0] - *pts.last()?).len() < 1e-9 {
        pts.pop();
    }
    (pts.len() >= 3).then(|| Loop2::polygon(&pts))
}

/// Classify closed contours by containment (even depth is solid, odd is a cutout) and cut the
/// given drills from the region that holds them.
fn regions_from_loops(loops: Vec<Loop2>, drills: Option<&[KicadDrill]>) -> Vec<Region2> {
    let areas: Vec<f64> = loops.iter().map(|l| l.signed_area().abs()).collect();
    let samples: Vec<Vec2> = loops.iter().map(loop_sample).collect();
    let mut parents: Vec<Option<usize>> = vec![None; loops.len()];
    for i in 0..loops.len() {
        let mut best: Option<usize> = None;
        for j in 0..loops.len() {
            if i == j || areas[j] <= areas[i] + 1e-9 {
                continue;
            }
            if loops[j].contains(samples[i]) && best.is_none_or(|b| areas[j] < areas[b]) {
                best = Some(j);
            }
        }
        parents[i] = best;
    }
    let depth = |mut i: usize| {
        let mut d = 0;
        while let Some(p) = parents[i] {
            d += 1;
            i = p;
        }
        d
    };
    let mut regions: Vec<(Vec2, Region2)> = Vec::new();
    let mut region_of: HashMap<usize, usize> = HashMap::new();
    for i in 0..loops.len() {
        if depth(i) % 2 != 0 {
            continue;
        }
        let holes: Vec<Loop2> = (0..loops.len()).filter(|j| parents[*j] == Some(i)).filter_map(|j| loops.get(j).cloned()).collect();
        let region = Region2 { outer: loops[i].ccw(), holes: holes.into_iter().map(|h| h.reversed()).collect() };
        region_of.insert(i, regions.len());
        regions.push((samples[i], region));
    }
    if let Some(drills) = drills {
        for drill in drills {
            let center = Vec2::new(drill.center[0], drill.center[1]);
            let mut best: Option<usize> = None;
            for (i, (_, region)) in regions.iter().enumerate() {
                if region.outer.contains(center)
                    && !region.holes.iter().any(|h| h.contains(center))
                    && best.is_none_or(|b| region.outer.signed_area().abs() < regions[b].1.outer.signed_area().abs())
                {
                    best = Some(i);
                }
            }
            let Some(i) = best else { continue };
            let size = [drill.size[0].abs(), drill.size[1].abs()];
            let r = (size[0].min(size[1]) * 0.5).max(1e-4);
            let hole = if (size[0] - size[1]).abs() <= 1e-6 { Loop2::circle_from(center, r, 0.0) } else { slot_loop(center, size[0], size[1]) };
            if let Some((_, region)) = regions.get_mut(i) {
                region.holes.push(hole.reversed());
            }
        }
    }
    regions.into_iter().map(|(_, r)| r).collect()
}

fn slot_loop(center: Vec2, width: f64, height: f64) -> Loop2 {
    let (long, short, vertical) = if width >= height { (width, height, false) } else { (height, width, true) };
    let r = short * 0.5;
    let half = (long * 0.5 - r).max(0.0);
    let n = 16;
    let mut pts = Vec::new();
    for i in 0..=n {
        let a = -std::f64::consts::FRAC_PI_2 + std::f64::consts::PI * i as f64 / n as f64;
        let (dx, dy) = (r * a.cos(), r * a.sin());
        pts.push(if vertical { Vec2::new(center.x + dy, center.y + half + dx) } else { Vec2::new(center.x + half + dx, center.y + dy) });
    }
    for i in 0..=n {
        let a = std::f64::consts::FRAC_PI_2 + std::f64::consts::PI * i as f64 / n as f64;
        let (dx, dy) = (r * a.cos(), r * a.sin());
        pts.push(if vertical { Vec2::new(center.x + dy, center.y - half + dx) } else { Vec2::new(center.x - half + dx, center.y + dy) });
    }
    Loop2::polygon(&pts)
}

fn loop_sample(loop_: &Loop2) -> Vec2 {
    let c = loop_.centroid();
    if loop_.contains(c) {
        return c;
    }
    let poly = loop_.polyline(1e-3);
    for i in 0..poly.len() {
        let (Some(a), Some(b)) = (poly.get(i), poly.get((i + 1) % poly.len())) else { continue };
        let m = (*a + *b) * 0.5;
        let p = m + (c - m) * 1e-3;
        if loop_.contains(p) {
            return p;
        }
    }
    poly.first().copied().unwrap_or_default()
}

/// Extrude regions between `z0` and `z1` into B-rep bodies (one per region).
fn extrude_regions_bodies(regions: &[Region2], z0: f64, z1: f64) -> Result<Vec<solidcraft_kernel::Body>> {
    if regions.is_empty() {
        return Err(IoError::Invalid("nothing to extrude".into()));
    }
    if !(z0.is_finite() && z1.is_finite() && (z1 - z0).abs() >= 1e-6) {
        return Err(IoError::Invalid("KiCad copper has a zero or invalid thickness".into()));
    }
    let plane = Plane { origin: Vec3::new(0.0, 0.0, z0), ..Plane::XY };
    solidcraft_kernel::guard("KiCad profile", || solidcraft_kernel::extrude(&plane, regions, 0.0, (z1 - z0).abs()))
        .map_err(|e| IoError::Invalid(format!("KiCad profile: {e}")))
}

/// Extrude regions between `z0` and `z1` and merge them into one mesh.
fn extrude_regions(regions: &[Region2], z0: f64, z1: f64) -> Result<solidcraft_geom::Mesh> {
    let bodies = extrude_regions_bodies(regions, z0, z1)?;
    let mut out = solidcraft_geom::Mesh::default();
    for body in &bodies {
        let mesh = body.tessellate(0.05).map_err(|e| IoError::Invalid(format!("KiCad mesh: {e}")))?;
        out.append(&mesh, 0);
    }
    Ok(out)
}

/// A filled region with no holes from a closed polygon outline.
fn points_region(points: &[[f64; 2]]) -> Option<Region2> {
    clean_loop(points).map(|outer| Region2 { outer, holes: Vec::new() })
}

/// What a stroke of `width` along `points` fills: its outline as one region, with mitered corners
/// (bevelled where a corner is too sharp for a miter). A `closed` stroke is a ring — the region
/// inside its outer edge with the region inside its inner edge as a hole. `None` when the points
/// do not make a stroke.
fn stroke_region(points: &[[f64; 2]], width: f64, closed: bool) -> Option<Region2> {
    if !(width.is_finite() && width > 0.0) {
        return None;
    }
    let mut pts: Vec<[f64; 2]> = Vec::with_capacity(points.len());
    for &p in points {
        if !(p[0].is_finite() && p[1].is_finite()) || p[0].abs() > MAX_NUMBER || p[1].abs() > MAX_NUMBER {
            return None;
        }
        if pts.last().is_none_or(|q: &[f64; 2]| (q[0] - p[0]).hypot(q[1] - p[1]) > 1e-9) {
            pts.push(p);
        }
    }
    if pts.len() < 2 {
        return None;
    }
    let half = width * 0.5;
    // A stroke that comes back to its start is one closed ring: its seam is a corner too.
    let repeats = pts.len() > 2 && pts.first().zip(pts.last()).is_some_and(|(f, l)| (l[0] - f[0]).hypot(l[1] - f[1]) <= 1e-9);
    let closed = closed || repeats;
    if repeats {
        pts.pop();
    }
    if pts.len() < 2 || (closed && pts.len() < 3) {
        return None;
    }
    let mut dirs: Vec<[f64; 2]> = Vec::with_capacity(pts.len());
    let segments = if closed { pts.len() } else { pts.len() - 1 };
    for i in 0..segments {
        let (Some(a), Some(b)) = (pts.get(i), pts.get((i + 1) % pts.len())) else { return None };
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len = (dx * dx + dy * dy).sqrt();
        if !(len.is_finite() && len > 0.0) {
            return None;
        }
        dirs.push([dx / len, dy / len]);
    }
    let normal = |d: [f64; 2]| [-d[1], d[0]];
    let mut left: Vec<Vec2> = Vec::with_capacity(pts.len() + 4);
    let mut right: Vec<Vec2> = Vec::with_capacity(pts.len() + 4);
    for (k, &p) in pts.iter().enumerate() {
        let incoming = if k == 0 { if closed { dirs.last().copied() } else { None } } else { dirs.get(k - 1).copied() };
        let outgoing = dirs.get(k).copied();
        match (incoming, outgoing) {
            (Some(d), None) | (None, Some(d)) => {
                let n = normal(d);
                left.push(Vec2::new(p[0] + n[0] * half, p[1] + n[1] * half));
                right.push(Vec2::new(p[0] - n[0] * half, p[1] - n[1] * half));
            }
            (Some(d0), Some(d1)) => {
                let (n0, n1) = (normal(d0), normal(d1));
                let (mx, my) = (n0[0] + n1[0], n0[1] + n1[1]);
                let miter = (mx * mx + my * my).sqrt();
                // The miter reaches half/cos(turn/2); past a 157° turn it runs away, so bevel.
                let cos = if miter > 1e-9 { (mx / miter) * n0[0] + (my / miter) * n0[1] } else { 0.0 };
                if miter > 1e-9 && cos > 0.2 {
                    let scale = half / cos;
                    left.push(Vec2::new(p[0] + mx / miter * scale, p[1] + my / miter * scale));
                    right.push(Vec2::new(p[0] - mx / miter * scale, p[1] - my / miter * scale));
                } else {
                    left.push(Vec2::new(p[0] + n0[0] * half, p[1] + n0[1] * half));
                    left.push(Vec2::new(p[0] + n1[0] * half, p[1] + n1[1] * half));
                    right.push(Vec2::new(p[0] - n0[0] * half, p[1] - n0[1] * half));
                    right.push(Vec2::new(p[0] - n1[0] * half, p[1] - n1[1] * half));
                }
            }
            (None, None) => return None,
        }
    }
    if closed {
        // The outer edge with the inner edge as its hole: no seam to leave a gap at.
        let inner = Loop2::polygon(&left);
        if inner.signed_area().abs() < 1e-9 {
            return None;
        }
        return Some(Region2 { outer: Loop2::polygon(&right), holes: vec![inner] });
    }
    right.reverse();
    left.extend(right);
    (left.len() >= 3).then(|| Region2 { outer: Loop2::polygon(&left), holes: Vec::new() })
}

/// Points closer than this (mm) are the same point when tracks are joined.
const TRACK_JOIN_TOLERANCE: f64 = 1e-6;

/// A track end, rounded to [`TRACK_JOIN_TOLERANCE`] so tracks that meet are found.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct TrackEnd(i64, i64);

fn track_end(p: [f64; 2]) -> TrackEnd {
    TrackEnd((p[0] / TRACK_JOIN_TOLERANCE).round() as i64, (p[1] / TRACK_JOIN_TOLERANCE).round() as i64)
}

/// The side and width a trace is on: only tracks that share them join.
type TraceKey = (bool, u64, TrackEnd);

/// The board's tracks and track arcs as continuous traces: KiCad stores a routed trace as one
/// `segment` (or `arc`) per leg, so legs that meet end to end on the same layer and width are
/// joined into one polyline. Points are in the board frame.
fn track_polylines(tracks: &[KicadStroke]) -> Vec<(bool, f64, Vec<[f64; 2]>)> {
    let mut ends: HashMap<TraceKey, Vec<usize>> = HashMap::new();
    for (i, track) in tracks.iter().enumerate() {
        let (Some(first), Some(last)) = (track.points.first(), track.points.last()) else { continue };
        let key = (track.layer == "F.Cu", track.width.to_bits());
        ends.entry((key.0, key.1, track_end(*first))).or_default().push(i);
        if track_end(*first) != track_end(*last) {
            ends.entry((key.0, key.1, track_end(*last))).or_default().push(i);
        }
    }
    let mut used = vec![false; tracks.len()];
    let mut out = Vec::new();
    for start in 0..tracks.len() {
        let track = &tracks[start];
        if used[start] || track.points.len() < 2 {
            used[start] = true;
            continue;
        }
        used[start] = true;
        let front = track.layer == "F.Cu";
        let width = track.width.to_bits();
        let mut chain = track.points.clone();
        extend_trace(&mut chain, tracks, &ends, &mut used, front, width, true);
        extend_trace(&mut chain, tracks, &ends, &mut used, front, width, false);
        if chain.len() >= 2 {
            out.push((front, track.width, chain.iter().map(|p| [p[0], -p[1]]).collect()));
        }
    }
    out
}

/// Follow the tracks that continue `chain` at one of its ends (exactly one unused leg of the
/// same layer and width, else a junction or a loose end stops the trace).
fn extend_trace(
    chain: &mut Vec<[f64; 2]>,
    tracks: &[KicadStroke],
    ends: &HashMap<TraceKey, Vec<usize>>,
    used: &mut [bool],
    front: bool,
    width: u64,
    at_end: bool,
) {
    loop {
        let Some(anchor) = (if at_end { chain.last() } else { chain.first() }).copied() else { return };
        let Some(candidates) = ends.get(&(front, width, track_end(anchor))) else { return };
        let mut free = candidates.iter().copied().filter(|i| !used.get(*i).copied().unwrap_or(true));
        let (Some(next), None) = (free.next(), free.next()) else { return };
        let Some(points) = tracks.get(next).map(|t| t.points.as_slice()) else { return };
        let (Some(first), Some(last)) = (points.first().copied(), points.last().copied()) else { return };
        let mut grow: Vec<[f64; 2]> = Vec::with_capacity(points.len());
        if track_end(first) == track_end(anchor) {
            grow.extend(points.iter().skip(1).copied());
        } else if track_end(last) == track_end(anchor) {
            grow.extend(points.iter().rev().skip(1).copied());
        } else {
            return;
        }
        used[next] = true;
        if at_end {
            chain.extend(grow);
        } else {
            // The leg runs away from the chain's front: its points go in reversed.
            grow.reverse();
            let tail = std::mem::take(chain);
            chain.extend(grow);
            chain.extend(tail);
        }
    }
}

/// How finely a text outline curve is drawn (mm).
const TEXT_CURVE_STEP: f64 = 0.05;

/// Segments for a control polygon, so short glyph curves stay cheap.
fn glyph_segments(points: &[Vec2]) -> usize {
    let mut length = 0.0;
    for pair in points.windows(2) {
        if let [a, b] = pair {
            length += a.dist(*b);
        }
    }
    ((length / TEXT_CURVE_STEP).ceil() as usize).clamp(4, 64)
}

/// A point on an open Bézier curve (de Casteljau).
fn glyph_bezier(points: &[Vec2], t: f64) -> Vec2 {
    let mut tmp = points.to_vec();
    for r in 1..tmp.len() {
        for i in 0..tmp.len() - r {
            let a = tmp[i];
            let b = tmp.get(i + 1).copied().unwrap_or(a);
            tmp[i] = Vec2::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t);
        }
    }
    tmp.first().copied().unwrap_or_default()
}

/// A glyph outline element as a polyline (the text engine emits lines, conics and control
/// splines; circles and arcs are handled for completeness).
fn glyph_polyline(link: &solidcraft_sketch::LinkGeom) -> Vec<Vec2> {
    use solidcraft_sketch::LinkGeom;
    match link {
        LinkGeom::Line(a, b) => vec![*a, *b],
        LinkGeom::Circle(c, r) => {
            let r = r.abs();
            let n = ((std::f64::consts::TAU * r / TEXT_CURVE_STEP).ceil() as usize).clamp(8, 256);
            (0..=n)
                .map(|i| {
                    let a = std::f64::consts::TAU * i as f64 / n as f64;
                    Vec2::new(c.x + r * a.cos(), c.y + r * a.sin())
                })
                .collect()
        }
        LinkGeom::Arc { c, a, b } => {
            let r = c.dist(*a);
            if !(r.is_finite() && r > 1e-9) {
                return Vec::new();
            }
            let start = (a.y - c.y).atan2(a.x - c.x);
            let mut end = (b.y - c.y).atan2(b.x - c.x);
            while end <= start {
                end += std::f64::consts::TAU;
            }
            let n = ((r * (end - start) / TEXT_CURVE_STEP).ceil() as usize).clamp(4, 256);
            (0..=n)
                .map(|i| {
                    let t = start + (end - start) * i as f64 / n as f64;
                    Vec2::new(c.x + r * t.cos(), c.y + r * t.sin())
                })
                .collect()
        }
        LinkGeom::Conic { a, apex, b, rho } => {
            // A rational quadratic; rho 0.5 is a plain quadratic Bézier.
            let w = (rho / (1.0 - rho)).max(1e-9);
            let n = glyph_segments(&[*a, *apex, *b]);
            (0..=n)
                .map(|i| {
                    let t = i as f64 / n as f64;
                    let (u, v) = (1.0 - t, t);
                    let (aa, bb, cc) = (u * u, 2.0 * u * v * w, v * v);
                    let d = aa + bb + cc;
                    Vec2::new((aa * a.x + bb * apex.x + cc * b.x) / d, (aa * a.y + bb * apex.y + cc * b.y) / d)
                })
                .collect()
        }
        LinkGeom::Spline { pts, control, periodic, degree: _ } => {
            if !*control || *periodic || pts.len() < 2 {
                return Vec::new();
            }
            let n = glyph_segments(pts);
            (0..=n).map(|i| glyph_bezier(pts, i as f64 / n as f64)).collect()
        }
        LinkGeom::Ellipse { .. } | LinkGeom::Point(_) => Vec::new(),
    }
}

/// The closed contours of a text's glyph outlines, in glyph coordinates.
fn glyph_loops(geometry: &[solidcraft_sketch::LinkGeom]) -> Vec<Vec<Vec2>> {
    let mut loops = Vec::new();
    let mut current: Vec<Vec2> = Vec::new();
    let mut points = 0usize;
    for link in geometry {
        let polyline = glyph_polyline(link);
        let Some(first) = polyline.first() else { continue };
        points = points.saturating_add(polyline.len());
        if points > MAX_TEXT_POINTS {
            return Vec::new();
        }
        if current.is_empty() {
            current = polyline;
            continue;
        }
        if current.last().is_some_and(|end| end.dist(*first) <= 1e-6) {
            current.extend_from_slice(polyline.get(1..).unwrap_or_default());
        } else {
            if current.len() >= 3 {
                loops.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
            current = polyline;
        }
    }
    if current.len() >= 3 {
        loops.push(current);
    }
    loops
        .into_iter()
        .filter_map(|mut contour| {
            if contour.len() >= 2 && contour.first().is_some_and(|first| contour.last().is_some_and(|last| first.dist(*last) <= 1e-6)) {
                contour.pop();
            }
            (contour.len() >= 3).then_some(contour)
        })
        .collect()
}

/// The filled glyph outlines of a text item, in world coordinates.
fn text_regions(text: &KicadText, origin: Vec2, angle: f64) -> Vec<Region2> {
    let style = solidcraft_sketch::TextStyle { bold: text.bold, italic: text.italic, ..Default::default() };
    let Ok(geometry) = solidcraft_sketch::text_geometry_styled(None, &text.text, Vec2::new(0.0, 0.0), text.height, 0.0, &style, None) else {
        return Vec::new();
    };
    let contours = glyph_loops(&geometry);
    if contours.is_empty() {
        return Vec::new();
    }
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for point in contours.iter().flatten() {
        min_x = min_x.min(point.x);
        min_y = min_y.min(point.y);
        max_x = max_x.max(point.x);
        max_y = max_y.max(point.y);
    }
    // KiCad's position is the text's centre unless it says how it is justified.
    let offset_x = if text.left {
        -min_x
    } else if text.right {
        -max_x
    } else {
        -(min_x + max_x) * 0.5
    };
    let offset_y = if text.top {
        -max_y
    } else if text.bottom {
        -min_y
    } else {
        -(min_y + max_y) * 0.5
    };
    // `size` gives the height and the (possibly different) glyph width.
    let wider = (text.width / text.height).max(1e-9);
    let mirror = if text.mirror { -1.0 } else { 1.0 };
    let (sin, cos) = angle.sin_cos();
    let mut loops = Vec::with_capacity(contours.len());
    for contour in &contours {
        let mut points = Vec::with_capacity(contour.len());
        for point in contour {
            let x = (point.x + offset_x) * wider * mirror;
            let y = point.y + offset_y;
            points.push(Vec2::new(origin.x + cos * x - sin * y, origin.y + sin * x + cos * y));
        }
        loops.push(Loop2::polygon(&points));
    }
    regions_from_loops(loops, None)
}

/// Add a text's filled glyphs to a plate; false when its outlines could not be built.
fn add_text_plate(plate: &mut Plate, text: &KicadText, origin: Vec2, angle: f64, z0: f64, z1: f64) -> bool {
    let regions = text_regions(text, origin, angle);
    if regions.is_empty() {
        return false;
    }
    match extrude_regions(&regions, z0, z1) {
        Ok(mesh) => {
            plate.add_mesh(mesh.positions, mesh.triangles);
            true
        }
        Err(_) => false,
    }
}

/// Accumulate simple polygonal plates into one mesh.
#[derive(Default)]
struct Plate {
    positions: Vec<Vec3>,
    triangles: Vec<[u32; 3]>,
}

impl Plate {
    /// Append a mesh whose triangle indices start from zero.
    fn add_mesh(&mut self, positions: Vec<Vec3>, triangles: Vec<[u32; 3]>) {
        let Ok(base) = u32::try_from(self.positions.len()) else { return };
        self.positions.extend(positions);
        for [a, b, c] in triangles {
            let (Some(a), Some(b), Some(c)) = (a.checked_add(base), b.checked_add(base), c.checked_add(base)) else { continue };
            self.triangles.push([a, b, c]);
        }
    }

    fn add_loop(&mut self, points: &[[f64; 2]], z0: f64, z1: f64) {
        let Some(loop_) = clean_loop(points) else { return };
        let pts: Vec<Vec2> = loop_.polyline(1e-3).to_vec();
        let pts: Vec<[f64; 2]> = pts.into_iter().map(|p| [p.x, p.y]).collect();
        if pts.len() < 3 {
            return;
        }
        let Ok(triangles) = triangulate_outline(&pts) else { return };
        let count = match u32::try_from(pts.len()) {
            Ok(c) => c,
            Err(_) => return,
        };
        let base = match u32::try_from(self.positions.len()) {
            Ok(b) => b,
            Err(_) => return,
        };
        self.positions.extend(pts.iter().map(|p| Vec3::new(p[0], p[1], z0)));
        self.positions.extend(pts.iter().map(|p| Vec3::new(p[0], p[1], z1)));
        for [a, b, c] in triangles {
            self.triangles.push([a + base + count, b + base + count, c + base + count]);
            self.triangles.push([c + base, b + base, a + base]);
        }
        let ccw = polygon_area(&pts) > 0.0;
        for index in 0..count {
            let next = (index + 1) % count;
            let (a, b) = if ccw { (index, next) } else { (next, index) };
            self.triangles.push([a + base, b + base, b + base + count]);
            self.triangles.push([a + base, b + base + count, a + base + count]);
        }
    }

    fn add_track(&mut self, points: &[[f64; 2]], width: f64, closed: bool, z0: f64, z1: f64) {
        if let Some(region) = stroke_region(points, width, closed) {
            self.add_shape(&region, z0, z1);
        }
    }

    /// A stroke's shape: a filled outline, or a ring with its hole.
    fn add_shape(&mut self, region: &Region2, z0: f64, z1: f64) {
        if !region.holes.is_empty() {
            self.add_region(region, z0, z1);
            return;
        }
        let points: Vec<[f64; 2]> = region.outer.polyline(1e-3).to_vec().into_iter().map(|p| [p.x, p.y]).collect();
        self.add_loop(&points, z0, z1);
    }

    fn add_circle(&mut self, center: [f64; 2], radius: f64, z0: f64, z1: f64) {
        if !(radius.is_finite() && radius > 1e-9) {
            return;
        }
        let points = circle_points(center, radius);
        self.add_loop(&points, z0, z1);
    }

    /// A disc of copper with a drilled hole through it (a via's ring).
    fn add_ring(&mut self, center: [f64; 2], radius: f64, hole: f64, z0: f64, z1: f64) {
        if !(hole.is_finite() && hole > 1e-9 && hole < radius) {
            self.add_circle(center, radius, z0, z1);
            return;
        }
        let Some(region) = via_region(center, radius, hole) else { return };
        self.add_region(&region, z0, z1);
    }

    /// A pad's copper: its outline, with the holes its drills make.
    fn add_pad(&mut self, points: &[[f64; 2]], holes: &[Loop2], z0: f64, z1: f64) {
        if holes.is_empty() {
            self.add_loop(points, z0, z1);
            return;
        }
        let Some(outer) = clean_loop(points) else { return };
        self.add_region(&Region2 { outer, holes: holes.to_vec() }, z0, z1);
    }

    /// A region (a filled loop, maybe with holes) as a closed mesh.
    fn add_region(&mut self, region: &Region2, z0: f64, z1: f64) {
        if let Ok(mesh) = extrude_regions(std::slice::from_ref(region), z0, z1) {
            self.add_mesh(mesh.positions, mesh.triangles);
        }
    }

    fn is_empty(&self) -> bool {
        self.triangles.is_empty()
    }
}

/// Every drilled hole of the board as a convex outline in the board frame: no copper may cover
/// one, whichever side it is on (a drill goes through the board).
fn drill_outlines(board: &KicadBoard) -> Vec<Vec<[f64; 2]>> {
    let mut out = Vec::new();
    for via in &board.vias {
        let center = [via.at[0], -via.at[1]];
        let points = circle_points(center, (via.drill * 0.5).abs());
        if points.len() >= 3 {
            out.push(points);
        }
    }
    for footprint in &board.footprints {
        let transform = footprint_world(footprint);
        for pad in &footprint.pads {
            let Some(points) = pad_drill_points(pad) else { continue };
            let moved: Vec<[f64; 2]> = points.iter().map(|p| apply2(&transform, p[0], p[1])).collect();
            if moved.len() >= 3 {
                out.push(moved);
            }
        }
    }
    out
}

/// The part of a segment `a`→`b` inside a convex outline, as a range of `t` along it.
fn inside_span(a: [f64; 2], b: [f64; 2], outline: &[[f64; 2]]) -> Option<(f64, f64)> {
    if outline.len() < 3 {
        return None;
    }
    let mut twice_area = 0.0;
    for i in 0..outline.len() {
        let p = outline[i];
        let q = outline[(i + 1) % outline.len()];
        twice_area += p[0] * q[1] - q[0] * p[1];
    }
    if !twice_area.is_finite() || twice_area.abs() < 1e-12 {
        return None;
    }
    let sign = if twice_area > 0.0 { 1.0 } else { -1.0 };
    let d = [b[0] - a[0], b[1] - a[1]];
    let (mut t0, mut t1) = (0.0, 1.0);
    for i in 0..outline.len() {
        let p = outline[i];
        let q = outline[(i + 1) % outline.len()];
        let e = [q[0] - p[0], q[1] - p[1]];
        let rel = [a[0] - p[0], a[1] - p[1]];
        // Inside is sign·(e × (a + t·d − p)) ≥ 0.
        let num = sign * (e[0] * rel[1] - e[1] * rel[0]);
        let denom = sign * (e[0] * d[1] - e[1] * d[0]);
        if denom.abs() < 1e-12 {
            if num < 0.0 {
                return None;
            }
            continue;
        }
        let t = -num / denom;
        if denom > 0.0 {
            if t > t0 {
                t0 = t;
            }
        } else if t < t1 {
            t1 = t;
        }
        if t0 > t1 {
            return None;
        }
    }
    (t1 - t0 > 1e-9).then_some((t0, t1))
}

/// A convex drill outline grown by `margin` all round: radially from its own centre, which is
/// exact for a round drill and close enough for a slot.
fn grow_outline(outline: &[[f64; 2]], margin: f64) -> Vec<[f64; 2]> {
    if !(margin > 0.0) || outline.len() < 3 {
        return outline.to_vec();
    }
    let count = outline.len() as f64;
    let cx = outline.iter().map(|p| p[0]).sum::<f64>() / count;
    let cy = outline.iter().map(|p| p[1]).sum::<f64>() / count;
    outline
        .iter()
        .map(|p| {
            let (dx, dy) = (p[0] - cx, p[1] - cy);
            let d = (dx * dx + dy * dy).sqrt();
            if d < 1e-12 { *p } else { [p[0] + dx / d * margin, p[1] + dy / d * margin] }
        })
        .collect()
}

/// The runs of a polyline that no drilled hole covers: copper stops at a hole, and a trace that
/// runs into one comes back as the pieces on either side of it. Each hole is grown by half the
/// stroke's width, so the stroke's own corners never reach into it.
fn trim_around_drills(points: &[[f64; 2]], width: f64, drills: &[Vec<[f64; 2]>]) -> Vec<Vec<[f64; 2]>> {
    let half = if width.is_finite() && width > 0.0 { width * 0.5 } else { 0.0 };
    let grown: Vec<Vec<[f64; 2]>> = drills.iter().map(|outline| grow_outline(outline, half)).collect();
    let at = |a: [f64; 2], b: [f64; 2], t: f64| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
    let mut pieces: Vec<([f64; 2], [f64; 2])> = Vec::new();
    for pair in points.windows(2) {
        let (Some(a), Some(b)) = (pair.first().copied(), pair.get(1).copied()) else { continue };
        let mut spans: Vec<(f64, f64)> = grown.iter().filter_map(|outline| inside_span(a, b, outline)).collect();
        spans.sort_by(|x, y| x.0.total_cmp(&y.0));
        let mut t = 0.0;
        for (s0, s1) in spans {
            let (s0, s1) = (s0.max(0.0), s1.min(1.0));
            if s1 <= t {
                continue;
            }
            if s0 > t {
                pieces.push((at(a, b, t), at(a, b, s0)));
            }
            t = t.max(s1);
            if t >= 1.0 {
                break;
            }
        }
        if t < 1.0 {
            pieces.push((at(a, b, t), b));
        }
    }
    let mut runs: Vec<Vec<[f64; 2]>> = Vec::new();
    for (p, q) in pieces {
        if (q[0] - p[0]).hypot(q[1] - p[1]) <= 1e-12 {
            continue;
        }
        if let Some(run) = runs.last_mut()
            && run.last().is_some_and(|last| (last[0] - p[0]).hypot(last[1] - p[1]) <= 1e-9)
        {
            run.push(q);
            continue;
        }
        runs.push(vec![p, q]);
    }
    runs
}

/// Front and back copper plates: tracks, vias, pads and zones, modelled with their thickness or
/// as a thin film overlaid on the board's surfaces.
fn copper_meshes(board: &KicadBoard, pads: bool, thickness: f64, mode: KicadCopper, warnings: &mut Vec<String>) -> Vec<(Plate, &'static str)> {
    let mut front = Plate::default();
    let mut back = Plate::default();
    // Modelled copper has its real thickness; an overlay is a thin film on the board's
    // surfaces (below the silkscreen).
    let (top, bottom, under, under_top) = if mode == KicadCopper::Overlay {
        (thickness, thickness + OVERLAY_THICKNESS, -OVERLAY_THICKNESS, 0.0)
    } else {
        (thickness, thickness + COPPER_THICKNESS, -COPPER_THICKNESS, 0.0)
    };
    let mut items = 0usize;
    let mut too_many = false;
    let mut add = |plate: &mut Plate, f: &dyn Fn(&mut Plate)| {
        if items >= MAX_COPPER_ITEMS {
            too_many = true;
            return;
        }
        items += 1;
        f(plate);
    };
    // A routed trace is one item, however many legs KiCad stores it in; it stops at every drill.
    let drills = drill_outlines(board);
    for (front_side, width, points) in track_polylines(&board.tracks) {
        let (z0, z1) = if front_side { (top, bottom) } else { (under, under_top) };
        let plate = if front_side { &mut front } else { &mut back };
        for run in trim_around_drills(&points, width, &drills) {
            add(plate, &|p| p.add_track(&run, width, false, z0, z1));
        }
    }
    for via in &board.vias {
        let center = [via.at[0], -via.at[1]];
        let radius = via.size * 0.5;
        let hole = via.drill * 0.5;
        let two_sided = via.layers.iter().any(|l| l == "F.Cu" || l == "*.Cu" || l == "F&B.Cu");
        if two_sided || via.layers.iter().any(|l| l == "F.Cu") {
            add(&mut front, &|p| p.add_ring(center, radius, hole, top, bottom));
        }
        if two_sided || via.layers.iter().any(|l| l == "B.Cu") {
            add(&mut back, &|p| p.add_ring(center, radius, hole, under, under_top));
        }
    }
    for zone in &board.zones {
        let contours: Vec<Loop2> = zone
            .contours
            .iter()
            .filter_map(|c| {
                let points: Vec<Vec2> = c.iter().map(|p| Vec2::new(p[0], -p[1])).collect();
                (points.len() >= 3).then(|| Loop2::polygon(&points))
            })
            .collect();
        if contours.is_empty() {
            continue;
        }
        let regions = regions_from_loops(contours, None);
        let front_side = zone.layer == "F.Cu";
        let (z0, z1) = if front_side { (top, bottom) } else { (under, under_top) };
        let plate = if front_side { &mut front } else { &mut back };
        match extrude_regions(&regions, z0, z1) {
            Ok(mesh) => plate.add_mesh(mesh.positions, mesh.triangles),
            Err(_) => warnings.push(format!("a copper zone on {} could not be built", zone.layer)),
        }
    }
    if pads {
        for footprint in &board.footprints {
            let transform = footprint_world(footprint);
            for pad in &footprint.pads {
                let on_front = pad.layers.iter().any(|l| l == "F.Cu" || l == "*.Cu" || l == "F&B.Cu");
                let on_back = pad.layers.iter().any(|l| l == "B.Cu" || l == "*.Cu" || l == "F&B.Cu");
                if !on_front && !on_back {
                    continue;
                }
                let loops = pad_loops(pad);
                // A through-hole pad's drill is a hole through its copper too.
                let holes = pad_drill_holes(pad, &transform);
                for loop_ in loops {
                    let points: Vec<[f64; 2]> = loop_.iter().map(|p| apply2(&transform, p[0], p[1])).collect();
                    if on_front {
                        add(&mut front, &|p| p.add_pad(&points, &holes, top, bottom));
                    }
                    if on_back {
                        add(&mut back, &|p| p.add_pad(&points, &holes, under, under_top));
                    }
                }
            }
        }
    }
    for graphic in &board.graphics {
        let layer = graphic.layer.as_str();
        if !is_outer_copper(layer) {
            continue;
        }
        let front_side = layer == "F.Cu";
        let points: Vec<[f64; 2]> = graphic.points.iter().map(|p| [p[0], -p[1]]).collect();
        let (z0, z1) = if front_side { (top, bottom) } else { (under, under_top) };
        let plate = if front_side { &mut front } else { &mut back };
        if graphic.filled && points.len() >= 3 {
            add(plate, &|p| p.add_loop(&points, z0, z1));
        } else {
            add(plate, &|p| p.add_track(&points, graphic.width, graphic.closed, z0, z1));
        }
    }
    let mut text_failed = false;
    {
        let mut one = |text: &KicadText, origin: Vec2, angle: f64| {
            if !is_outer_copper(&text.layer) {
                return;
            }
            let front_side = text.layer == "F.Cu";
            let plate = if front_side { &mut front } else { &mut back };
            let failed = std::cell::Cell::new(false);
            let (z0, z1) = if front_side { (top, bottom) } else { (under, under_top) };
            add(plate, &|p| failed.set(failed.get() || !add_text_plate(p, text, origin, angle, z0, z1)));
            if failed.get() {
                text_failed = true;
            }
        };
        let board_frame = board_to_world();
        for text in &board.texts {
            let origin = apply2(&board_frame, text.at[0], text.at[1]);
            one(text, Vec2::new(origin[0], origin[1]), text.at[2].to_radians());
        }
        for footprint in &board.footprints {
            let frame = footprint_world(footprint);
            for text in &footprint.texts {
                let origin = apply2(&frame, text.at[0], text.at[1]);
                one(text, Vec2::new(origin[0], origin[1]), (footprint.at[2] + text.at[2]).to_radians());
            }
        }
    }
    if text_failed {
        warnings.push("some copper text could not be built".into());
    }
    if too_many {
        warnings.push("additional copper items were skipped".into());
    }
    let mut out = Vec::new();
    if !front.is_empty() {
        out.push((front, "F.Cu"));
    }
    if !back.is_empty() {
        out.push((back, "B.Cu"));
    }
    out
}

/// Most copper items one B-rep layer may have: each is a solid to build, export and keep.
const MAX_BREP_COPPER_ITEMS: usize = 20_000;
/// Most pieces in one compound copper body (the kernel tells a body's pieces apart up to a
/// thousand shells; STEP export splits the compound into a solid per piece).
const MAX_COMPOUND_SHELLS: usize = 512;
/// Copper's colour on the board (a soldered copper tone).
const COPPER_COLOR: [f32; 3] = [0.72, 0.45, 0.20];

/// Front and back copper as B-rep solids with the copper's real thickness: one body per layer,
/// whose pieces (tracks, vias, zones, pads, copper graphics and text) are its lumps. The copper
/// comes back as STEP text, added as an import in the board's component.
fn copper_brep(board: &KicadBoard, pads: bool, thickness: f64, warnings: &mut Vec<String>) -> Result<KicadCopperBrep> {
    let (top, bottom, under, under_top) = (thickness, thickness + COPPER_THICKNESS, -COPPER_THICKNESS, 0.0);
    let mut front: Vec<Region2> = Vec::new();
    let mut back: Vec<Region2> = Vec::new();
    let mut items = 0usize;
    let mut too_many = false;
    let mut push = |target: &mut Vec<Region2>, mut regions: Vec<Region2>| {
        if regions.is_empty() {
            return;
        }
        if items >= MAX_BREP_COPPER_ITEMS {
            too_many = true;
            return;
        }
        items += 1;
        target.append(&mut regions);
    };
    // A routed trace is one item, however many legs KiCad stores it in; it stops at every drill.
    let drills = drill_outlines(board);
    for (front_side, width, points) in track_polylines(&board.tracks) {
        for run in trim_around_drills(&points, width, &drills) {
            let regions: Vec<Region2> = stroke_region(&run, width, false).into_iter().collect();
            if front_side {
                push(&mut front, regions);
            } else {
                push(&mut back, regions);
            }
        }
    }
    for via in &board.vias {
        let center = [via.at[0], -via.at[1]];
        let radius = via.size * 0.5;
        let hole = via.drill * 0.5;
        let two_sided = via.layers.iter().any(|l| l == "F.Cu" || l == "*.Cu" || l == "F&B.Cu");
        if two_sided || via.layers.iter().any(|l| l == "F.Cu") {
            push(&mut front, via_region(center, radius, hole).into_iter().collect());
        }
        if two_sided || via.layers.iter().any(|l| l == "B.Cu") {
            push(&mut back, via_region(center, radius, hole).into_iter().collect());
        }
    }
    for zone in &board.zones {
        let contours: Vec<Loop2> = zone
            .contours
            .iter()
            .filter_map(|c| {
                let points: Vec<Vec2> = c.iter().map(|p| Vec2::new(p[0], -p[1])).collect();
                (points.len() >= 3).then(|| Loop2::polygon(&points))
            })
            .collect();
        if contours.is_empty() {
            continue;
        }
        let regions = regions_from_loops(contours, None);
        push(if zone.layer == "F.Cu" { &mut front } else { &mut back }, regions);
    }
    if pads {
        for footprint in &board.footprints {
            let transform = footprint_world(footprint);
            for pad in &footprint.pads {
                let on_front = pad.layers.iter().any(|l| l == "F.Cu" || l == "*.Cu" || l == "F&B.Cu");
                let on_back = pad.layers.iter().any(|l| l == "B.Cu" || l == "*.Cu" || l == "F&B.Cu");
                if !on_front && !on_back {
                    continue;
                }
                // A through-hole pad's drill is a hole through its copper too.
                let holes = pad_drill_holes(pad, &transform);
                let regions: Vec<Region2> = pad_loops(pad)
                    .into_iter()
                    .filter_map(|loop_| {
                        let points: Vec<[f64; 2]> = loop_.iter().map(|p| apply2(&transform, p[0], p[1])).collect();
                        let outer = clean_loop(&points)?;
                        Some(Region2 { outer, holes: holes.clone() })
                    })
                    .collect();
                if on_front {
                    push(&mut front, regions.clone());
                }
                if on_back {
                    push(&mut back, regions);
                }
            }
        }
    }
    for graphic in &board.graphics {
        let layer = graphic.layer.as_str();
        if !is_outer_copper(layer) {
            continue;
        }
        let points: Vec<[f64; 2]> = graphic.points.iter().map(|p| [p[0], -p[1]]).collect();
        let regions: Vec<Region2> = if graphic.filled && points.len() >= 3 {
            points_region(&points).into_iter().collect()
        } else {
            stroke_region(&points, graphic.width, graphic.closed).into_iter().collect()
        };
        push(if layer == "F.Cu" { &mut front } else { &mut back }, regions);
    }
    let mut text_failed = false;
    {
        let mut one = |text: &KicadText, origin: Vec2, angle: f64| {
            if !is_outer_copper(&text.layer) {
                return;
            }
            let regions = text_regions(text, origin, angle);
            if regions.is_empty() {
                text_failed = true;
                return;
            }
            push(if text.layer == "F.Cu" { &mut front } else { &mut back }, regions);
        };
        let board_frame = board_to_world();
        for text in &board.texts {
            let origin = apply2(&board_frame, text.at[0], text.at[1]);
            one(text, Vec2::new(origin[0], origin[1]), text.at[2].to_radians());
        }
        for footprint in &board.footprints {
            let frame = footprint_world(footprint);
            for text in &footprint.texts {
                let origin = apply2(&frame, text.at[0], text.at[1]);
                one(text, Vec2::new(origin[0], origin[1]), (footprint.at[2] + text.at[2]).to_radians());
            }
        }
    }
    if text_failed {
        warnings.push("some copper text could not be built".into());
    }
    if too_many {
        warnings.push(format!("the board has more copper items than B-rep copper builds ({}); the rest were left out", MAX_BREP_COPPER_ITEMS));
    }
    let mut out: Vec<(String, solidcraft_kernel::Body)> = Vec::new();
    for (regions, layer, z0, z1) in [(front, "F.Cu", top, bottom), (back, "B.Cu", under, under_top)] {
        if regions.is_empty() {
            continue;
        }
        let bodies = extrude_regions_bodies(&regions, z0, z1).or_else(|error| {
            // One bad item must not lose the layer: build them one by one.
            let mut one = Vec::new();
            for region in &regions {
                match extrude_regions_bodies(std::slice::from_ref(region), z0, z1) {
                    Ok(mut bodies) => one.append(&mut bodies),
                    Err(_) => warnings.push(format!("a copper item on {layer} could not be built")),
                }
            }
            if one.is_empty() { Err(error) } else { Ok(one) }
        })?;
        for (n, chunk) in bodies.chunks(MAX_COMPOUND_SHELLS).enumerate() {
            let pieces: Vec<&solidcraft_kernel::Body> = chunk.iter().collect();
            let body = solidcraft_kernel::compound(&pieces).map_err(|e| IoError::Invalid(format!("KiCad copper: {e}")))?;
            let name = if n == 0 { format!("Copper {layer}") } else { format!("Copper {layer} {}", n + 1) };
            out.push((name, body));
        }
    }
    let exports: Vec<solidcraft_kernel::ExportBody> =
        out.iter().map(|(name, body)| solidcraft_kernel::ExportBody { name: name.clone(), body, color: Some(COPPER_COLOR) }).collect();
    let header = solidcraft_kernel::StepHeader { file_name: "copper.step".into(), organization: "SolidCraft".into(), ..Default::default() };
    let step = solidcraft_kernel::step_export_bodies(&exports, &header).map_err(|e| IoError::Invalid(format!("KiCad copper STEP: {e}")))?;
    if step.len() > crate::MAX_STEP_BYTES {
        return Err(IoError::Invalid(format!(
            "this board's B-rep copper exceeds the STEP size limit ({} MB); choose modelled mesh",
            crate::MAX_STEP_BYTES >> 20
        )));
    }
    Ok(KicadCopperBrep { step, items })
}

/// Front and back silkscreen plates.
fn silkscreen_meshes(board: &KicadBoard, thickness: f64, warnings: &mut Vec<String>) -> Vec<(Plate, &'static str)> {
    let mut front = Plate::default();
    let mut back = Plate::default();
    let (top, top_end) = (thickness + 0.001, thickness + 0.001 + SILK_THICKNESS);
    let (bottom, bottom_end) = (-0.001 - SILK_THICKNESS, -0.001);
    let mut items = 0usize;
    let mut too_many = false;
    let mut add = |plate: &mut Plate, f: &dyn Fn(&mut Plate)| {
        if items >= MAX_SILK_ITEMS {
            too_many = true;
            return;
        }
        items += 1;
        f(plate);
    };
    let mut one = |stroke: &KicadStroke, transform: &Mat| {
        let points: Vec<[f64; 2]> = stroke.points.iter().map(|p| apply2(transform, p[0], p[1])).collect();
        let front_side = stroke.layer == "F.SilkS";
        let (z0, z1) = if front_side { (top, top_end) } else { (bottom, bottom_end) };
        let plate = if front_side { &mut front } else { &mut back };
        if stroke.filled && points.len() >= 3 {
            add(plate, &|p| p.add_loop(&points, z0, z1));
        } else {
            let width = if stroke.width > 0.0 { stroke.width } else { 0.15 };
            add(plate, &|p| p.add_track(&points, width, stroke.closed, z0, z1));
        }
    };
    for graphic in &board.graphics {
        if is_silk(&graphic.layer) {
            one(graphic, &board_to_world());
        }
    }
    for footprint in &board.footprints {
        let transform = footprint_world(footprint);
        for graphic in &footprint.graphics {
            if is_silk(&graphic.layer) {
                one(graphic, &transform);
            }
        }
    }
    let mut text_failed = false;
    {
        let mut text_one = |text: &KicadText, origin: Vec2, angle: f64| {
            if !is_silk(&text.layer) {
                return;
            }
            let front_side = text.layer == "F.SilkS";
            let (z0, z1) = if front_side { (top, top_end) } else { (bottom, bottom_end) };
            let plate = if front_side { &mut front } else { &mut back };
            let failed = std::cell::Cell::new(false);
            add(plate, &|p| failed.set(failed.get() || !add_text_plate(p, text, origin, angle, z0, z1)));
            if failed.get() {
                text_failed = true;
            }
        };
        let board_frame = board_to_world();
        for text in &board.texts {
            let origin = apply2(&board_frame, text.at[0], text.at[1]);
            text_one(text, Vec2::new(origin[0], origin[1]), text.at[2].to_radians());
        }
        for footprint in &board.footprints {
            let frame = footprint_world(footprint);
            for text in &footprint.texts {
                let origin = apply2(&frame, text.at[0], text.at[1]);
                text_one(text, Vec2::new(origin[0], origin[1]), (footprint.at[2] + text.at[2]).to_radians());
            }
        }
    }
    if text_failed {
        warnings.push("some silkscreen text could not be built".into());
    }
    if too_many {
        warnings.push("additional silkscreen items were skipped".into());
    }
    let mut out = Vec::new();
    if !front.is_empty() {
        out.push((front, "F.SilkS"));
    }
    if !back.is_empty() {
        out.push((back, "B.SilkS"));
    }
    out
}

/// The outline(s) of a pad, in its footprint frame.
fn pad_loops(pad: &KicadPad) -> Vec<Vec<[f64; 2]>> {
    let sx = pad.size[0].abs();
    let sy = pad.size[1].abs();
    if !(sx.is_finite() && sy.is_finite() && sx > 0.0 && sy > 0.0) {
        return Vec::new();
    }
    let shape = pad.shape.to_ascii_lowercase();
    let mut points: Vec<[f64; 2]> = match shape.as_str() {
        "circle" => {
            let r = sx * 0.5;
            let n = ((std::f64::consts::TAU * r / CURVE_TOLERANCE).ceil() as usize).clamp(8, 128);
            (0..n)
                .map(|i| {
                    let a = std::f64::consts::TAU * i as f64 / n as f64;
                    [r * a.cos(), r * a.sin()]
                })
                .collect()
        }
        "oval" => oval_points(sx, sy),
        "roundrect" => rounded_rect_points(sx, sy, (pad.roundrect_ratio.clamp(0.0, 0.5)) * sx.min(sy)),
        _ => vec![[-sx * 0.5, -sy * 0.5], [sx * 0.5, -sy * 0.5], [sx * 0.5, sy * 0.5], [-sx * 0.5, sy * 0.5]],
    };
    let angle = pad.at[2].to_radians();
    let (s, c) = angle.sin_cos();
    for p in &mut points {
        let (x, y) = (p[0], p[1]);
        p[0] = c * x + s * y + pad.at[0];
        p[1] = -s * x + c * y + pad.at[1];
    }
    vec![points]
}

/// A via's copper in the board frame: a disc, or a ring when the via is drilled.
fn via_region(center: [f64; 2], radius: f64, hole: f64) -> Option<Region2> {
    if !(radius.is_finite() && radius > 1e-9) {
        return None;
    }
    let outer = clean_loop(&circle_points(center, radius))?;
    let holes = if hole.is_finite() && hole > 1e-9 && hole < radius {
        clean_loop(&circle_points(center, hole)).map(|l| l.reversed()).into_iter().collect()
    } else {
        Vec::new()
    };
    Some(Region2 { outer, holes })
}

/// The outline of a pad's drill in the footprint frame (a circle, or a slot for an oblong drill).
fn pad_drill_points(pad: &KicadPad) -> Option<Vec<[f64; 2]>> {
    let drill = pad.drill?;
    let size = [drill[0].abs(), drill[1].abs()];
    if !(size[0].is_finite() && size[1].is_finite()) || size[0].min(size[1]) <= 1e-6 || size[0].max(size[1]) > MAX_NUMBER {
        return None;
    }
    let center = Vec2::new(pad.at[0], pad.at[1]);
    let hole = if (size[0] - size[1]).abs() <= 1e-6 { Loop2::circle_from(center, size[0] * 0.5, 0.0) } else { slot_loop(center, size[0], size[1]) };
    Some(hole.polyline(1e-3).to_vec().into_iter().map(|p| [p.x, p.y]).collect())
}

/// The holes a pad's drill makes, in the board frame and wound as region holes.
fn pad_drill_holes(pad: &KicadPad, transform: &Mat) -> Vec<Loop2> {
    pad_drill_points(pad)
        .and_then(|points| {
            let moved: Vec<[f64; 2]> = points.iter().map(|p| apply2(transform, p[0], p[1])).collect();
            clean_loop(&moved)
        })
        .map(|l| l.reversed())
        .into_iter()
        .collect()
}

fn oval_points(sx: f64, sy: f64) -> Vec<[f64; 2]> {
    let n = 16;
    let (long, short, vertical) = if sx >= sy { (sx, sy, false) } else { (sy, sx, true) };
    let r = short * 0.5;
    let half = (long * 0.5 - r).max(0.0);
    let mut points = Vec::new();
    for i in 0..=n {
        let a = -std::f64::consts::FRAC_PI_2 + std::f64::consts::PI * i as f64 / n as f64;
        let (dx, dy) = (r * a.cos(), r * a.sin());
        points.push(if vertical { [dy, half + dx] } else { [half + dx, dy] });
    }
    for i in 0..=n {
        let a = std::f64::consts::FRAC_PI_2 + std::f64::consts::PI * i as f64 / n as f64;
        let (dx, dy) = (r * a.cos(), r * a.sin());
        points.push(if vertical { [dy, -half + dx] } else { [-half + dx, dy] });
    }
    points
}

fn rounded_rect_points(sx: f64, sy: f64, radius: f64) -> Vec<[f64; 2]> {
    let r = radius.clamp(0.0, sx.min(sy) * 0.5);
    if r <= 1e-9 {
        return vec![[-sx * 0.5, -sy * 0.5], [sx * 0.5, -sy * 0.5], [sx * 0.5, sy * 0.5], [-sx * 0.5, sy * 0.5]];
    }
    let n = 6;
    let mut points = Vec::new();
    for (cx, cy, start) in [
        (sx * 0.5 - r, -sy * 0.5 + r, -std::f64::consts::FRAC_PI_2),
        (sx * 0.5 - r, sy * 0.5 - r, 0.0),
        (-sx * 0.5 + r, sy * 0.5 - r, std::f64::consts::FRAC_PI_2),
        (-sx * 0.5 + r, -sy * 0.5 + r, std::f64::consts::PI),
    ] {
        for i in 0..=n {
            let a = start + std::f64::consts::FRAC_PI_2 * i as f64 / n as f64;
            points.push([cx + r * a.cos(), cy + r * a.sin()]);
        }
    }
    points
}

fn polygon_area(points: &[[f64; 2]]) -> f64 {
    (0..points.len())
        .map(|i| {
            let a = points[i];
            let b = points[(i + 1) % points.len()];
            a[0] * b[1] - b[0] * a[1]
        })
        .sum::<f64>()
        * 0.5
}

fn triangulate_outline(points: &[[f64; 2]]) -> Result<Vec<[u32; 3]>> {
    let area = polygon_area(points);
    if !area.is_finite() || area.abs() <= 1e-12 {
        return Err(IoError::Invalid("KiCad board outline has zero area".into()));
    }
    let ccw = area > 0.0;
    let mut remaining: Vec<usize> = (0..points.len()).collect();
    let mut triangles = Vec::with_capacity(points.len() - 2);
    while remaining.len() > 3 {
        let mut ear = None;
        for i in 0..remaining.len() {
            let prev = remaining[(i + remaining.len() - 1) % remaining.len()];
            let curr = remaining[i];
            let next = remaining[(i + 1) % remaining.len()];
            if turn(points[prev], points[curr], points[next]) * if ccw { 1.0 } else { -1.0 } <= 1e-12 {
                continue;
            }
            if remaining.iter().copied().any(|other| {
                other != prev && other != curr && other != next && point_in_triangle(points[other], points[prev], points[curr], points[next], ccw)
            }) {
                continue;
            }
            ear = Some((i, [prev, curr, next]));
            break;
        }
        let Some((index, mut triangle)) = ear else {
            return Err(IoError::Invalid("KiCad board outline is self-intersecting or cannot be triangulated".into()));
        };
        if !ccw {
            triangle.swap(1, 2);
        }
        triangles.push(triangle.map(|v| v as u32));
        remaining.remove(index);
    }
    let mut triangle = [remaining[0], remaining[1], remaining[2]];
    if !ccw {
        triangle.swap(1, 2);
    }
    triangles.push(triangle.map(|v| v as u32));
    Ok(triangles)
}

fn turn(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

fn point_in_triangle(p: [f64; 2], a: [f64; 2], b: [f64; 2], c: [f64; 2], ccw: bool) -> bool {
    let sign = if ccw { 1.0 } else { -1.0 };
    turn(a, b, p) * sign >= -1e-12 && turn(b, c, p) * sign >= -1e-12 && turn(c, a, p) * sign >= -1e-12
}

fn parse_footprint(
    expr: &Expr<'_>,
    model_count: &mut usize,
    pad_count: &mut usize,
    text_count: &mut usize,
    warnings: &mut Vec<String>,
    extra_text_seen: &mut bool,
) -> std::result::Result<KicadFootprint, String> {
    let at = field(expr, "at").ok_or_else(|| "no placement".to_string()).and_then(|e| numbers(e, 2))?;
    let mut position = [
        at.first().copied().ok_or_else(|| "no X position".to_string())?,
        at.get(1).copied().ok_or_else(|| "no Y position".to_string())?,
        at.get(2).copied().unwrap_or(0.0),
    ];
    if position.iter().any(|v| !v.is_finite() || v.abs() > MAX_NUMBER) {
        return Err("invalid placement".into());
    }
    let layer = layer_of(expr).unwrap_or_default();
    let reference = property(expr, "Reference").or_else(|| fp_text(expr, "reference")).unwrap_or_default();
    let value = property(expr, "Value").or_else(|| fp_text(expr, "value")).unwrap_or_default();
    let mut models = Vec::new();
    let mut pads = Vec::new();
    let mut graphics = Vec::new();
    let mut texts = Vec::new();
    for child in children(expr) {
        match head(child) {
            Some("model") => {
                *model_count = model_count.saturating_add(1);
                if *model_count > MAX_MODELS {
                    return Err(format!("board has more than {MAX_MODELS} 3D models"));
                }
                match parse_model(child) {
                    Ok(model) => models.push(model),
                    Err(reason) => warnings.push(format!("skipped a 3D model: {reason}")),
                }
            }
            Some("pad") => {
                *pad_count = pad_count.saturating_add(1);
                if *pad_count > MAX_PADS {
                    return Err(format!("board has more than {MAX_PADS} pads"));
                }
                match parse_pad(child) {
                    Ok(pad) => pads.push(pad),
                    Err(reason) => warnings.push(format!("skipped a pad: {reason}")),
                }
            }
            Some(kind @ ("fp_line" | "fp_rect" | "fp_arc" | "fp_circle" | "fp_poly" | "fp_curve")) => {
                let base = kind.trim_start_matches("fp_");
                let gr = match base {
                    "line" => "gr_line",
                    "rect" => "gr_rect",
                    "arc" => "gr_arc",
                    "circle" => "gr_circle",
                    "poly" => "gr_poly",
                    "curve" => "gr_curve",
                    _ => continue,
                };
                match parse_graphic(gr, child, warnings) {
                    Some(mut stroke) => {
                        stroke.layer = layer_of(child).unwrap_or_else(|| layer.clone());
                        graphics.push(stroke);
                    }
                    None => warnings.push(format!("skipped an invalid {kind}")),
                }
            }
            Some("fp_text") => {
                *text_count = text_count.saturating_add(1);
                if *text_count > MAX_TEXTS {
                    return Err(format!("board has more than {MAX_TEXTS} text items"));
                }
                if let Some(text) = parse_text(child, 2) {
                    texts.push(text);
                }
            }
            Some("property") => {
                // KiCad 8 and later write visible footprint fields as properties with a
                // placement and effects (older files use `fp_text`).
                if let Some(text) = parse_text(child, 2)
                    && !texts.iter().any(|t: &KicadText| t.text == text.text && t.layer == text.layer && t.at == text.at)
                {
                    *text_count = text_count.saturating_add(1);
                    if *text_count > MAX_TEXTS {
                        return Err(format!("board has more than {MAX_TEXTS} text items"));
                    }
                    texts.push(text);
                }
            }
            Some("fp_text_box") => *extra_text_seen = true,
            _ => {}
        }
    }
    position[2] = position[2].rem_euclid(360.0);
    Ok(KicadFootprint { reference, value, layer, at: position, models, pads, graphics, texts })
}

fn parse_pad(expr: &Expr<'_>) -> std::result::Result<KicadPad, String> {
    let at = field(expr, "at").ok_or_else(|| "no position".to_string()).and_then(|e| numbers(e, 2))?;
    let x = at.first().copied().ok_or_else(|| "no X position".to_string())?;
    let y = at.get(1).copied().ok_or_else(|| "no Y position".to_string())?;
    let angle = at.get(2).copied().unwrap_or(0.0);
    if [x, y, angle].iter().any(|v| !v.is_finite() || v.abs() > MAX_NUMBER) {
        return Err("invalid position".into());
    }
    let size = field(expr, "size").ok_or_else(|| "no size".to_string()).and_then(|e| numbers(e, 2))?;
    let sx = *size.first().ok_or_else(|| "no size".to_string())?;
    let sy = *size.get(1).ok_or_else(|| "no size".to_string())?;
    if !(sx.is_finite() && sy.is_finite() && sx > 0.0 && sy > 0.0 && sx <= MAX_NUMBER && sy <= MAX_NUMBER) {
        return Err("invalid size".into());
    }
    let shape = children(expr).get(3).and_then(atom).unwrap_or("rect").to_string();
    let layers = field(expr, "layers").map(|e| children(e).iter().skip(1).filter_map(atom).map(str::to_string).collect()).unwrap_or_default();
    let drill = field(expr, "drill").and_then(numeric_atoms).and_then(|n| {
        let first = *n.first()?;
        if !(first.is_finite() && first > 0.0) {
            return None;
        }
        let second = n.get(1).copied().unwrap_or(first);
        Some([first, if second.is_finite() && second > 0.0 { second } else { first }])
    });
    let roundrect_ratio = field(expr, "roundrect_rratio")
        .and_then(|e| numbers(e, 1).ok())
        .and_then(|n| n.first().copied())
        .filter(|v| v.is_finite() && *v >= 0.0 && *v <= 0.5)
        .unwrap_or(0.0);
    Ok(KicadPad { at: [x, y, angle], size: [sx, sy], shape, layers, drill, roundrect_ratio })
}

fn numeric_atoms(expr: &Expr<'_>) -> Option<Vec<f64>> {
    let values: Vec<f64> = children(expr)
        .iter()
        .skip(1)
        .filter_map(atom)
        .filter_map(|a| a.parse::<f64>().ok())
        .filter(|v| v.is_finite() && v.abs() <= MAX_NUMBER)
        .collect();
    (!values.is_empty()).then_some(values)
}

/// Is this text or effects container hidden (`hide`, or `(hide yes)`)? `(hide no)` is not.
fn is_hidden(expr: &Expr<'_>) -> bool {
    children(expr).iter().any(|child| match child {
        Expr::Atom(value) => value == "hide",
        Expr::List(_) => head(child) == Some("hide") && children(child).get(1).and_then(atom) != Some("no"),
    })
}

/// Text on a copper or silkscreen layer: `gr_text`'s string is the first attribute, `fp_text`'s
/// the second (after its type).
fn parse_text(expr: &Expr<'_>, text_index: usize) -> Option<KicadText> {
    let text = children(expr).get(text_index).and_then(atom)?.to_string();
    if text.trim().is_empty() {
        return None;
    }
    let layer = layer_of(expr)?;
    if !is_silk(&layer) && !is_outer_copper(&layer) {
        return None;
    }
    let effects = field(expr, "effects");
    if is_hidden(expr) || effects.is_some_and(is_hidden) {
        return None;
    }
    let at = field(expr, "at").and_then(|e| numbers(e, 2).ok()).unwrap_or_default();
    let x = at.first().copied().unwrap_or(0.0);
    let y = at.get(1).copied().unwrap_or(0.0);
    let angle = at.get(2).copied().unwrap_or(0.0);
    if [x, y, angle].iter().any(|v| !v.is_finite() || v.abs() > MAX_NUMBER) {
        return None;
    }
    let (mut height, mut width, mut thickness) = (1.0, 1.0, 0.15);
    let (mut bold, mut italic) = (false, false);
    if let Some(font) = effects.and_then(|e| field(e, "font")) {
        if let Some(size) = field(font, "size").and_then(|e| numbers(e, 2).ok()) {
            height = size.first().copied().unwrap_or(1.0);
            width = size.get(1).copied().unwrap_or(height);
        }
        if let Some(t) = field(font, "thickness").and_then(|e| numbers(e, 1).ok()).and_then(|v| v.first().copied()) {
            thickness = t;
        }
        bold = children(font).iter().any(|e| atom(e) == Some("bold"));
        italic = children(font).iter().any(|e| atom(e) == Some("italic"));
    }
    if !(height.is_finite() && height > 1e-6 && height < 1e6 && width.is_finite() && width > 1e-6 && width < 1e6) {
        return None;
    }
    let (mut left, mut right, mut top, mut bottom, mut mirror) = (false, false, false, false, false);
    if let Some(justify) = effects.and_then(|e| field(e, "justify")) {
        for word in children(justify).iter().filter_map(atom) {
            match word {
                "left" => left = true,
                "right" => right = true,
                "top" => top = true,
                "bottom" => bottom = true,
                "mirror" => mirror = true,
                _ => {}
            }
        }
    }
    Some(KicadText { text, at: [x, y, angle], layer, height, width, thickness, bold, italic, mirror, left, right, top, bottom })
}

fn parse_model(expr: &Expr<'_>) -> std::result::Result<KicadModel, String> {
    let path = children(expr).get(1).and_then(atom).filter(|p| !p.trim().is_empty()).ok_or_else(|| "empty model path".to_string())?.to_string();
    let at = model_xyz(expr, "at", [0.0; 3])?;
    let scale = model_xyz(expr, "scale", [1.0; 3])?;
    let rotate = model_xyz(expr, "rotate", [0.0; 3])?;
    if scale.iter().any(|v| !v.is_finite() || *v <= 0.0) {
        return Err("invalid or mirrored model scale".into());
    }
    Ok(KicadModel { path, at, scale, rotate })
}

fn model_xyz(expr: &Expr<'_>, name: &str, default: [f64; 3]) -> std::result::Result<[f64; 3], String> {
    // KiCad writes `(offset (xyz ...))` in some versions and `(at (xyz ...))` in others.
    let vector = field(expr, name).or_else(|| if name == "at" { field(expr, "offset") } else { None });
    let Some(vector) = vector else { return Ok(default) };
    let xyz = children(vector).get(1).filter(|e| head(e) == Some("xyz")).ok_or_else(|| format!("`{name}` must contain an xyz vector"))?;
    let values = numbers(xyz, 3)?;
    let result = [
        values.first().copied().ok_or_else(|| format!("missing X in `{name}`"))?,
        values.get(1).copied().ok_or_else(|| format!("missing Y in `{name}`"))?,
        values.get(2).copied().ok_or_else(|| format!("missing Z in `{name}`"))?,
    ];
    if result.iter().any(|v| !v.is_finite() || v.abs() > MAX_NUMBER) {
        return Err(format!("invalid `{name}` vector"));
    }
    Ok(result)
}

fn property(expr: &Expr<'_>, key: &str) -> Option<String> {
    children(expr)
        .iter()
        .filter(|e| head(e) == Some("property"))
        .find(|e| children(e).get(1).and_then(atom) == Some(key))
        .and_then(|e| children(e).get(2))
        .and_then(atom)
        .map(str::to_string)
}

fn fp_text(expr: &Expr<'_>, kind: &str) -> Option<String> {
    children(expr)
        .iter()
        .filter(|e| head(e) == Some("fp_text"))
        .find(|e| children(e).get(1).and_then(atom) == Some(kind))
        .and_then(|e| children(e).get(2))
        .and_then(atom)
        .map(str::to_string)
}

fn field<'e, 'a>(expr: &'e Expr<'a>, name: &str) -> Option<&'e Expr<'a>> {
    children(expr).iter().find(|e| head(e) == Some(name))
}

fn head<'e, 'a>(expr: &'e Expr<'a>) -> Option<&'e str> {
    match expr {
        Expr::List(items) => items.first().and_then(atom),
        Expr::Atom(_) => None,
    }
}

fn children<'e, 'a>(expr: &'e Expr<'a>) -> &'e [Expr<'a>] {
    match expr {
        Expr::List(items) => items,
        Expr::Atom(_) => &[],
    }
}

fn atom<'e, 'a>(expr: &'e Expr<'a>) -> Option<&'e str> {
    match expr {
        Expr::Atom(value) => Some(value),
        Expr::List(_) => None,
    }
}

fn numbers(expr: &Expr<'_>, minimum: usize) -> std::result::Result<Vec<f64>, String> {
    let mut values = Vec::new();
    for item in children(expr).iter().skip(1) {
        if let Some(value) = atom(item) {
            let number = value.parse::<f64>().map_err(|_| format!("invalid number `{value}`"))?;
            if !number.is_finite() || number.abs() > MAX_NUMBER {
                return Err(format!("number `{value}` is outside the supported range"));
            }
            values.push(number);
        }
    }
    if values.len() < minimum {
        return Err("not enough numeric values".into());
    }
    Ok(values)
}

fn tokenize(source: &str) -> Result<Vec<Token<'_>>> {
    let mut tokens = Vec::new();
    let mut index = 0usize;
    while index < source.len() {
        let byte = source.as_bytes().get(index).copied().unwrap_or_default();
        match byte {
            b'(' => {
                tokens.push(Token::Open);
                index += 1;
            }
            b')' => {
                tokens.push(Token::Close);
                index += 1;
            }
            b'"' => {
                let (value, next) = quoted(source, index + 1)?;
                tokens.push(Token::String(value));
                index = next;
            }
            b';' => {
                while index < source.len() && source.as_bytes().get(index) != Some(&b'\n') {
                    index += 1;
                }
            }
            b if b.is_ascii_whitespace() => index += 1,
            _ => {
                let start = index;
                while index < source.len()
                    && !source.as_bytes().get(index).is_some_and(|b| b.is_ascii_whitespace() || matches!(b, b'(' | b')' | b';'))
                {
                    index += 1;
                }
                let word = source.get(start..index).ok_or_else(|| IoError::Invalid("invalid UTF-8 boundary in KiCad PCB".into()))?;
                tokens.push(Token::Atom(word));
            }
        }
        if tokens.len() > MAX_TOKENS {
            return Err(IoError::Invalid(format!("KiCad PCB has more than {MAX_TOKENS} s-expression tokens")));
        }
    }
    Ok(tokens)
}

fn quoted(source: &str, start: usize) -> Result<(Cow<'_, str>, usize)> {
    let mut index = start;
    let mut segment = start;
    let mut decoded = None::<String>;
    while index < source.len() {
        match source.as_bytes().get(index).copied() {
            Some(b'"') => {
                let value = if let Some(mut out) = decoded {
                    out.push_str(source.get(segment..index).ok_or_else(|| IoError::Invalid("invalid quoted string".into()))?);
                    Cow::Owned(out)
                } else {
                    Cow::Borrowed(source.get(start..index).ok_or_else(|| IoError::Invalid("invalid quoted string".into()))?)
                };
                return Ok((value, index + 1));
            }
            Some(b'\\') => {
                let out = decoded.get_or_insert_with(String::new);
                out.push_str(source.get(segment..index).ok_or_else(|| IoError::Invalid("invalid quoted string".into()))?);
                index += 1;
                let rest = source.get(index..).ok_or_else(|| IoError::Invalid("invalid escape in KiCad PCB".into()))?;
                let mut chars = rest.chars();
                let escaped = chars.next().ok_or_else(|| IoError::Invalid("trailing escape in KiCad PCB string".into()))?;
                out.push(match escaped {
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    other => other,
                });
                index += escaped.len_utf8();
                segment = index;
            }
            Some(_) => index += 1,
            None => break,
        }
    }
    Err(IoError::Invalid("unterminated string in KiCad PCB".into()))
}

struct Parser<'a> {
    tokens: Vec<Token<'a>>,
    next: usize,
    nodes: usize,
}

impl<'a> Parser<'a> {
    fn expression(&mut self, depth: usize) -> Result<Expr<'a>> {
        if depth > MAX_DEPTH {
            return Err(IoError::Invalid(format!("KiCad PCB nesting exceeds {MAX_DEPTH}")));
        }
        self.nodes = self.nodes.saturating_add(1);
        if self.nodes > MAX_TOKENS {
            return Err(IoError::Invalid("KiCad PCB has too many s-expression nodes".into()));
        }
        match self.tokens.get(self.next).cloned() {
            Some(Token::Open) => {
                self.next += 1;
                let mut items = Vec::new();
                loop {
                    match self.tokens.get(self.next) {
                        Some(Token::Close) => {
                            self.next += 1;
                            return Ok(Expr::List(items));
                        }
                        None => return Err(IoError::Invalid("unterminated list in KiCad PCB".into())),
                        _ => items.push(self.expression(depth + 1)?),
                    }
                }
            }
            Some(Token::Close) => Err(IoError::Invalid("unexpected close parenthesis in KiCad PCB".into())),
            Some(Token::Atom(value)) => {
                self.next += 1;
                Ok(Expr::Atom(Cow::Borrowed(value)))
            }
            Some(Token::String(value)) => {
                self.next += 1;
                Ok(Expr::Atom(value))
            }
            None => Err(IoError::Invalid("empty KiCad PCB file".into())),
        }
    }
}

#[cfg(test)]
#[path = "kicad_in_tests.rs"]
pub(crate) mod tests;
