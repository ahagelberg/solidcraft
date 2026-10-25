//! The points of a face a joint snaps to: its corners, its edges' middles and its centre.
//!
//! With Ctrl held over a face while a snap input is active (a joint's Component 1 or 2, a joint
//! origin's Snap, a motion trace's Point), the pick is locked to that face: only these points can
//! be chosen, and each is marked in the viewport, so a joint point no longer depends on finding a
//! corner within a few pixels.

use egui::Pos2;
use solidcraft_engine::geom::Vec3;

use crate::viewport::{Hit, Proj};

/// How near the pointer (in points) a face point must be to be taken while the face is locked.
pub const REACH: f32 = 14.0;

/// The kind of point a joint can take on a face.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A corner: where two of the face's edges meet.
    Corner,
    /// The middle of one of the face's edges.
    EdgeMid,
    /// The middle of the face.
    Centre,
}

/// A point of the locked face.
#[derive(Clone, Copy, Debug)]
pub struct Point {
    pub at: Vec3,
    pub kind: Kind,
    /// The edge this point is the middle of (0 for a corner or the centre).
    pub edge: usize,
}

/// What holding Ctrl over a face locks the pick to.
#[derive(Clone, Debug)]
pub struct Lock {
    pub body: String,
    pub face: usize,
    pub points: Vec<Point>,
    /// The point under the pointer, when one is near enough; `None` takes the face itself, at the
    /// pointer, so Ctrl over a face always picks something of that face.
    pub chosen: Option<usize>,
}

impl Lock {
    /// The hit the pointer makes while the face is locked: the chosen point, or the face where the
    /// pointer is.
    pub fn hit(&self, at: Vec3) -> Hit {
        match self.chosen.and_then(|i| self.points.get(i)) {
            Some(p) => match p.kind {
                Kind::Corner => Hit::Vertex { body: self.body.clone(), point: p.at },
                Kind::EdgeMid => Hit::Edge { body: self.body.clone(), index: p.edge, mid: p.at },
                Kind::Centre => Hit::Face { body: self.body.clone(), index: self.face, point: p.at },
            },
            None => Hit::Face { body: self.body.clone(), index: self.face, point: at },
        }
    }
}

/// Whether a hit list holds a face the pick could be locked to, with `accept` being the active
/// input's kinds. Locking needs an input that takes corners (a snap input), so a face-only input
/// keeps its own picking.
pub fn takes_points(accept: crate::selection::Accept) -> bool {
    accept & crate::selection::VERTICES != 0
}

/// The face under the pointer, locked to its points with the one nearest the pointer chosen when
/// it is within [`REACH`].
pub fn lock(app: &crate::SolveApp, hits: &[Hit], proj: &Proj, pointer: Pos2) -> Option<Lock> {
    let face = hits.iter().find(|h| matches!(h, Hit::Face { .. }))?;
    let Hit::Face { body, index, .. } = face else { return None };
    let st = crate::open_meshing::shown_world(app);
    let b = st.body(body)?;
    let (mesh, place) = b.mesh_in_place();
    let moved = !solidcraft_engine::doc::is_identity(&place);
    let at = |p: Vec3| if moved { solidcraft_engine::doc::apply_point(&place, p) } else { p };
    let points: Vec<Point> = face_points(&mesh, *index)
        .into_iter()
        .map(|mut p| {
            p.at = at(p.at);
            p
        })
        .collect();
    if points.is_empty() {
        return None;
    }
    // The nearest point to the pointer, when it is close enough to be meant.
    let mut chosen = None;
    let mut best = REACH;
    for (i, p) in points.iter().enumerate() {
        if let Some(sp) = proj.to_screen(p.at) {
            let d = sp.distance(pointer);
            if d <= best {
                best = d;
                chosen = Some(i);
            }
        }
    }
    Some(Lock { body: body.clone(), face: *index, points, chosen })
}

/// The corners, edges' middles and centre of a mesh's face (in the mesh's own frame).
pub fn face_points(mesh: &solidcraft_engine::geom::Mesh, face: usize) -> Vec<Point> {
    let mut corners: Vec<Vec3> = Vec::new();
    let mut points: Vec<Point> = Vec::new();
    for (i, faces) in mesh.edge_faces.iter().enumerate() {
        if !faces.iter().any(|f| *f as usize == face) {
            continue;
        }
        let Some(line) = mesh.edges.get(i) else { continue };
        for end in [line.first(), line.last()].into_iter().flatten() {
            if !end.is_finite() {
                continue;
            }
            if !corners.iter().any(|c| c.dist(*end) <= 1e-9) {
                corners.push(*end);
            }
        }
        let mid = crate::selection::polyline_mid(line);
        if mid.is_finite() {
            points.push(Point { at: mid, kind: Kind::EdgeMid, edge: i });
        }
    }
    let mut all: Vec<Point> = corners.iter().map(|c| Point { at: *c, kind: Kind::Corner, edge: 0 }).collect();
    all.append(&mut points);
    if let Some(centre) = centre(&corners) {
        all.push(Point { at: centre, kind: Kind::Centre, edge: 0 });
    }
    all
}

/// The middle of a face, from its corners.
fn centre(corners: &[Vec3]) -> Option<Vec3> {
    if corners.is_empty() {
        return None;
    }
    let mut sum = Vec3::ZERO;
    for c in corners {
        sum += *c;
    }
    let c = sum * (1.0 / corners.len() as f64);
    c.is_finite().then_some(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A square face in the Z = 0 plane: four corners, four edges bounding face 0, and one
    /// triangle (so the mesh is a face as the viewport hands it to the pick).
    fn square() -> solidcraft_engine::geom::Mesh {
        let c = [Vec3::new(0.0, 0.0, 0.0), Vec3::new(10.0, 0.0, 0.0), Vec3::new(10.0, 10.0, 0.0), Vec3::new(0.0, 10.0, 0.0)];
        let edges: Vec<Vec<Vec3>> = (0..4).map(|i| vec![c[i], c[(i + 1) % 4]]).collect();
        solidcraft_engine::geom::Mesh {
            positions: c.to_vec(),
            normals: vec![Vec3::Z; 4],
            triangles: vec![[0, 1, 2], [0, 2, 3]],
            tri_face: vec![0, 0],
            edges,
            edge_faces: vec![vec![0]; 4],
            ..Default::default()
        }
    }

    #[test]
    fn a_faces_points_are_its_corners_its_edges_middles_and_its_centre() {
        let mesh = square();
        let points = face_points(&mesh, 0);
        let count = |k: Kind| points.iter().filter(|p| p.kind == k).count();
        assert_eq!((count(Kind::Corner), count(Kind::EdgeMid), count(Kind::Centre)), (4, 4, 1), "{points:?}");
        let centre = points.iter().find(|p| p.kind == Kind::Centre).unwrap().at;
        assert!(centre.dist(Vec3::new(5.0, 5.0, 0.0)) < 1e-9, "the centre is {centre:?}");
        // An edge's middle is halfway along it.
        let mids: Vec<Vec3> = points.iter().filter(|p| p.kind == Kind::EdgeMid).map(|p| p.at).collect();
        assert!(mids.iter().any(|m| m.dist(Vec3::new(5.0, 0.0, 0.0)) < 1e-9), "{mids:?}");
    }

    /// A face with no edges yet (a mesh without its boundary) offers nothing to lock to.
    #[test]
    fn a_face_without_edges_has_no_points() {
        let mesh = solidcraft_engine::geom::Mesh { triangles: vec![[0, 1, 2]], tri_face: vec![0], ..Default::default() };
        assert!(face_points(&mesh, 0).is_empty());
    }
}
