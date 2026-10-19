//! Bounded reader for the KiCad PCB s-expression format.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use solidcraft_geom::Vec3;

use crate::{IoError, Result};

/// Largest KiCad board file read.
pub const MAX_KICAD_BYTES: usize = 128 << 20;
const MAX_TOKENS: usize = 2_000_000;
const MAX_DEPTH: usize = 128;
const MAX_FOOTPRINTS: usize = 100_000;
const MAX_MODELS: usize = 500_000;
const MAX_OUTLINE_SEGMENTS: usize = 100_000;
const MAX_OUTLINE_POINTS: usize = 100_000;
const MAX_MODEL_VERTICES: usize = 5_000_000;
const MAX_MODEL_TRIANGLES: usize = 5_000_000;
const MAX_NUMBER: f64 = 1.0e9;

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
    /// Closed Edge.Cuts contours in board millimetres.
    pub outlines: Vec<Vec<[f64; 2]>>,
    pub footprints: Vec<KicadFootprint>,
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
}

#[derive(Clone, Debug, PartialEq)]
pub struct KicadModel {
    pub path: String,
    /// Model-local translation in millimetres.
    pub at: [f64; 3],
    pub scale: [f64; 3],
    /// Model-local Euler rotation in degrees.
    pub rotate: [f64; 3],
}

/// A KiCad board and the reusable component placements it references.
pub struct KicadImport {
    pub feature: crate::MeshFeature,
    pub plan: crate::MeshPlan,
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

    let mut board = KicadBoard {
        name: Path::new(file).file_stem().and_then(|n| n.to_str()).filter(|n| !n.trim().is_empty()).unwrap_or("PCB").to_string(),
        thickness,
        outlines: Vec::new(),
        footprints: Vec::new(),
        warnings: Vec::new(),
    };
    let mut segments = Vec::new();
    for item in children(&root).iter().filter(|e| matches!(head(e), Some("gr_line" | "gr_rect" | "gr_arc"))) {
        if field(item, "layer").and_then(|e| children(e).get(1)).and_then(atom) != Some("Edge.Cuts") {
            continue;
        }
        match head(item) {
            Some("gr_line") => match (field(item, "start").map(|e| numbers(e, 2)), field(item, "end").map(|e| numbers(e, 2))) {
                (Some(Ok(start)), Some(Ok(end))) => segments.push((point2(&start), point2(&end))),
                _ => board.warnings.push("skipped an invalid Edge.Cuts line".into()),
            },
            Some("gr_rect") => match (field(item, "start").map(|e| numbers(e, 2)), field(item, "end").map(|e| numbers(e, 2))) {
                (Some(Ok(start)), Some(Ok(end))) => {
                    let (a, b) = (point2(&start), point2(&end));
                    let c = [b[0], a[1]];
                    let d = [a[0], b[1]];
                    segments.extend([(a, c), (c, b), (b, d), (d, a)]);
                }
                _ => board.warnings.push("skipped an invalid Edge.Cuts rectangle".into()),
            },
            Some("gr_arc") => board.warnings.push("curved Edge.Cuts items are not supported yet".into()),
            _ => {}
        }
    }
    if segments.len() > MAX_OUTLINE_SEGMENTS {
        return Err(IoError::Invalid(format!("KiCad board has more than {MAX_OUTLINE_SEGMENTS} Edge.Cuts segments")));
    }
    board.outlines = join_outline_segments(segments, &mut board.warnings);
    if board.outlines.len() > 1 {
        board.warnings.push("additional Edge.Cuts contours (board cutouts) are not modeled yet".into());
    }
    let mut model_count = 0usize;
    for item in children(&root).iter().filter(|e| head(e) == Some("footprint")) {
        if board.footprints.len() >= MAX_FOOTPRINTS {
            return Err(IoError::Invalid(format!("KiCad PCB has more than {MAX_FOOTPRINTS} footprints")));
        }
        match parse_footprint(item, &mut model_count, &mut board.warnings) {
            Ok(footprint) => board.footprints.push(footprint),
            Err(reason) => board.warnings.push(format!("skipped a footprint: {reason}")),
        }
    }
    Ok(board)
}

fn point2(values: &[f64]) -> [f64; 2] {
    [values.first().copied().unwrap_or(0.0), values.get(1).copied().unwrap_or(0.0)]
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

/// Make a mesh body for the largest closed board outline. `thickness` is in millimetres.
pub fn kicad_board_mesh(board: &KicadBoard, thickness: f64) -> Result<crate::MeshObject> {
    if !thickness.is_finite() || thickness <= 0.0 || thickness > 1000.0 {
        return Err(IoError::Invalid("KiCad board thickness must be finite and between 0 and 1000 mm".into()));
    }
    let outline = board
        .outlines
        .iter()
        .max_by(|a, b| polygon_area(a).abs().total_cmp(&polygon_area(b).abs()))
        .ok_or_else(|| IoError::Invalid("KiCad board has no closed Edge.Cuts outline".into()))?;
    if outline.len() < 3 || outline.len() > MAX_OUTLINE_POINTS || outline.iter().flatten().any(|v| !v.is_finite() || v.abs() > MAX_NUMBER) {
        return Err(IoError::Invalid("KiCad board outline has an invalid point count or coordinate".into()));
    }
    let triangles = triangulate_outline(outline)?;
    let count = u32::try_from(outline.len()).map_err(|_| IoError::Invalid("KiCad board outline is too large".into()))?;
    let mut positions = Vec::with_capacity(outline.len() * 2);
    positions.extend(outline.iter().map(|p| Vec3::new(p[0], p[1], 0.0)));
    positions.extend(outline.iter().map(|p| Vec3::new(p[0], p[1], thickness)));
    let mut faces = Vec::with_capacity(triangles.len() * 2 + outline.len() * 2);
    for [a, b, c] in triangles {
        faces.push([a + count, b + count, c + count]);
        faces.push([c, b, a]);
    }
    let ccw = polygon_area(outline) > 0.0;
    for index in 0..count {
        let next = (index + 1) % count;
        let (a, b) = if ccw { (index, next) } else { (next, index) };
        faces.extend([[a, b, b + count], [a, b + count, a + count]]);
    }
    Ok(crate::MeshObject { name: board.name.clone(), positions, triangles: faces, ..Default::default() })
}

/// Read a board and its supported STEP model references into one mesh feature and placement plan.
pub fn read_kicad(bytes: &[u8], file: &str, thickness: Option<f64>, load: KicadLoader<'_>) -> Result<KicadImport> {
    let board = parse_kicad_board(bytes, file)?;
    let board_mesh = kicad_board_mesh(&board, thickness.unwrap_or(board.thickness))?;
    solidcraft_kernel::mesh_body(&board_mesh.positions, &board_mesh.triangles).map_err(|e| IoError::Invalid(format!("KiCad board: {e}")))?;
    let mut meshes = vec![mesh_data(board_mesh.name, board_mesh.positions, board_mesh.triangles, None)];
    let mut plan = crate::MeshPlan { bodies: vec![None], ..Default::default() };
    let mut warnings = board.warnings;
    let mut slots = std::collections::HashMap::<(String, [u64; 6]), usize>::new();
    let mut totals = (0usize, 0usize);
    for footprint in &board.footprints {
        for model in &footprint.models {
            let ext = Path::new(&model.path).extension().and_then(|e| e.to_str()).unwrap_or("");
            if !ext.eq_ignore_ascii_case("step") && !ext.eq_ignore_ascii_case("stp") {
                warnings.push(format!("{}: unsupported 3D model format", model.path));
                continue;
            }
            if model.scale.iter().any(|v| *v < 0.0) {
                warnings.push(format!("{}: mirrored 3D model scale is unsupported", model.path));
                continue;
            }
            let key = (model.path.clone(), model_transform_key(model));
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
                if model_bytes.len() > crate::MAX_STEP_BYTES {
                    warnings.push(format!("{}: model file exceeds the STEP size limit", model.path));
                    continue;
                }
                let text = String::from_utf8_lossy(&model_bytes);
                let imported = match solidcraft_kernel::step_import_shared(&text) {
                    Ok(imported) => imported,
                    Err(error) => {
                        warnings.push(format!("{}: {error}", model.path));
                        continue;
                    }
                };
                let slot = plan.slots.len();
                let label = footprint.reference.trim();
                plan.slots.push(if label.is_empty() { model.path.clone() } else { label.to_string() });
                let mut imported_meshes = Vec::new();
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
                    let positions: Vec<Vec3> = mesh.positions.into_iter().map(|p| transform_model_point(p, model)).collect();
                    let mut triangles = mesh.triangles;
                    if model.scale.iter().product::<f64>() < 0.0 {
                        for triangle in &mut triangles {
                            triangle.swap(1, 2);
                        }
                    }
                    if let Err(error) = solidcraft_kernel::mesh_body(&positions, &triangles) {
                        failed = Some(error.to_string());
                        break;
                    }
                    imported_meshes.push(mesh_data(body.name.clone(), positions, triangles, body.color));
                }
                if let Some(reason) = failed {
                    warnings.push(format!("{}: {reason}", model.path));
                    continue;
                }
                if imported_meshes.is_empty() {
                    warnings.push(format!("{}: no solid bodies found", model.path));
                    continue;
                }
                for mesh in imported_meshes {
                    meshes.push(mesh);
                    plan.bodies.push(Some(slot));
                }
                slots.insert(key, slot);
                slot
            };
            let angle = footprint.at[2].to_radians();
            let footprint_transform = solidcraft_doc::rigid(Vec3::new(footprint.at[0], footprint.at[1], 0.0), Vec3::ZERO, Vec3::Z, angle);
            let local_transform = solidcraft_doc::rigid(Vec3::new(model.at[0], model.at[1], model.at[2]), Vec3::ZERO, Vec3::Z, 0.0);
            let mut transform = solidcraft_doc::mat_mul(&footprint_transform, &local_transform);
            if footprint.layer.starts_with("B.") {
                let flip = solidcraft_doc::rigid(Vec3::ZERO, Vec3::ZERO, Vec3::X, std::f64::consts::PI);
                transform = solidcraft_doc::mat_mul(&transform, &flip);
            }
            plan.places.push(crate::PlanPlace { slot, parent: None, transform });
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
    Ok(KicadImport { feature, plan })
}

fn mesh_data(name: String, positions: Vec<Vec3>, triangles: Vec<[u32; 3]>, color: Option<[f32; 3]>) -> solidcraft_doc::MeshData {
    solidcraft_doc::MeshData {
        name,
        positions: positions.into_iter().flat_map(|p| [p.x, p.y, p.z]).collect(),
        triangles: triangles.into_iter().flatten().collect(),
        color,
    }
}

fn model_transform_key(model: &KicadModel) -> [u64; 6] {
    [
        model.scale[0].to_bits(),
        model.scale[1].to_bits(),
        model.scale[2].to_bits(),
        model.rotate[0].to_bits(),
        model.rotate[1].to_bits(),
        model.rotate[2].to_bits(),
    ]
}

fn transform_model_point(point: Vec3, model: &KicadModel) -> Vec3 {
    let scaled = Vec3::new(point.x * model.scale[0], point.y * model.scale[1], point.z * model.scale[2]);
    let (sx, cx) = model.rotate[0].to_radians().sin_cos();
    let (sy, cy) = model.rotate[1].to_radians().sin_cos();
    let (sz, cz) = model.rotate[2].to_radians().sin_cos();
    let x = scaled.x;
    let y = cx * scaled.y - sx * scaled.z;
    let z = sx * scaled.y + cx * scaled.z;
    let rotated_x = cy * x + sy * z;
    let z = -sy * x + cy * z;
    Vec3::new(cz * rotated_x - sz * y, sz * rotated_x + cz * y, z)
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

fn parse_footprint(expr: &Expr<'_>, model_count: &mut usize, warnings: &mut Vec<String>) -> std::result::Result<KicadFootprint, String> {
    let at = field(expr, "at").ok_or_else(|| "no placement".to_string()).and_then(|e| numbers(e, 2))?;
    let mut position = [
        at.first().copied().ok_or_else(|| "no X position".to_string())?,
        at.get(1).copied().ok_or_else(|| "no Y position".to_string())?,
        at.get(2).copied().unwrap_or(0.0),
    ];
    if position.iter().any(|v| !v.is_finite() || v.abs() > MAX_NUMBER) {
        return Err("invalid placement".into());
    }
    let layer = field(expr, "layer").and_then(|e| children(e).get(1)).and_then(atom).unwrap_or("").to_string();
    let reference = property(expr, "Reference").or_else(|| fp_text(expr, "reference")).unwrap_or_default();
    let value = property(expr, "Value").or_else(|| fp_text(expr, "value")).unwrap_or_default();
    let mut models = Vec::new();
    for model in children(expr).iter().filter(|e| head(e) == Some("model")) {
        *model_count = model_count.saturating_add(1);
        if *model_count > MAX_MODELS {
            return Err(format!("board has more than {MAX_MODELS} 3D models"));
        }
        match parse_model(model) {
            Ok(model) => models.push(model),
            Err(reason) => warnings.push(format!("skipped a 3D model: {reason}")),
        }
    }
    position[2] = position[2].rem_euclid(360.0);
    Ok(KicadFootprint { reference, value, layer, at: position, models })
}

fn parse_model(expr: &Expr<'_>) -> std::result::Result<KicadModel, String> {
    let path = children(expr).get(1).and_then(atom).filter(|p| !p.trim().is_empty()).ok_or_else(|| "empty model path".to_string())?.to_string();
    let at = model_xyz(expr, "at", [0.0; 3])?;
    let scale = model_xyz(expr, "scale", [1.0; 3])?;
    let rotate = model_xyz(expr, "rotate", [0.0; 3])?;
    if scale.contains(&0.0) {
        return Err("zero model scale".into());
    }
    Ok(KicadModel { path, at, scale, rotate })
}

fn model_xyz(expr: &Expr<'_>, name: &str, default: [f64; 3]) -> std::result::Result<[f64; 3], String> {
    if field(expr, name).is_none() {
        return Ok(default);
    }
    let xyz = field(expr, name)
        .and_then(|e| children(e).get(1))
        .filter(|e| head(e) == Some("xyz"))
        .ok_or_else(|| format!("`{name}` must contain an xyz vector"))?;
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
