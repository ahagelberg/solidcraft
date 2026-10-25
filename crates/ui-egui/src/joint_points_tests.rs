//! Holding Ctrl over a face while a snap input is active locks the pick to that face's points: its
//! corners, its edges' middles and its centre. A joint can then snap to any of them instead of only
//! to what happens to be within a few pixels of the pointer.

use serde_json::json;

use crate::joint_points::Kind;
use crate::scenario::Harness;
use crate::viewport::Hit;

/// A box of 20 mm, with the Joint dialog open (its inputs take snaps), framed in the view.
fn joint_on_a_box() -> Harness {
    let mut h = Harness::new();
    h.app.run("solid.box", json!({"corner": [0.0, 0.0, 0.0], "length": 20.0, "width": 20.0, "height": 20.0})).unwrap();
    h.app.start("joint.create");
    h.app.fit_view();
    h.frames(3);
    h
}

#[test]
fn ctrl_over_a_face_locks_the_pick_to_its_points() {
    let mut h = joint_on_a_box();
    let centre = h.at(&json!({"world": [10.0, 10.0, 20.0]})).unwrap();
    h.step(&json!({ "move": centre })).unwrap();
    assert!(matches!(h.app.viewport.hover, Some(Hit::Face { .. })), "no face under the middle of the top: {:?}", h.app.viewport.hover);
    assert!(h.app.viewport.face_lock.is_none(), "a face locked without Ctrl held");

    h.step(&json!({ "move": centre, "ctrl": true })).unwrap();
    let lock = h.app.viewport.face_lock.clone().expect("Ctrl over the face did not lock it");
    // Four corners, the four edges' middles and the centre.
    assert_eq!(lock.points.len(), 9, "{:?}", lock.points);
    assert_eq!(lock.chosen.map(|i| lock.points[i].kind), Some(Kind::Centre), "the middle of the face was not taken");
    assert!(matches!(h.app.viewport.hover, Some(Hit::Face { .. })), "the centre did not hover as the face's point: {:?}", h.app.viewport.hover);
    // The face's own normal: the direction a joint on it takes.
    assert!(lock.normal.dist(solidcraft_engine::geom::Vec3::Z) < 1e-6, "{:?}", lock.normal);
}

#[test]
fn ctrl_near_a_corner_takes_the_corner() {
    let mut h = joint_on_a_box();
    let corner = h.at(&json!({"world": [0.0, 0.0, 20.0]})).unwrap();
    let centre = h.at(&json!({"world": [10.0, 10.0, 20.0]})).unwrap();
    // A couple of pixels inside the face from the corner: the corner is still the nearest point.
    let (dx, dy) = (centre[0] - corner[0], centre[1] - corner[1]);
    let len = (dx * dx + dy * dy).sqrt();
    h.step(&json!({ "move": [corner[0] + dx / len * 2.0, corner[1] + dy / len * 2.0], "ctrl": true })).unwrap();
    let lock = h.app.viewport.face_lock.clone().expect("Ctrl near the corner did not lock a face");
    assert_eq!(lock.chosen.map(|i| lock.points[i].kind), Some(Kind::Corner), "{:?}", lock.points);
    // The vertex itself is the pick (not the face's centre), and the face still gives the
    // direction, which the dialog adds to it.
    let corner_at = lock.points[lock.chosen.unwrap()].at;
    match &h.app.viewport.hover {
        Some(Hit::Vertex { point, .. }) => assert!(point.dist(corner_at) < 1e-6, "{point:?} vs {corner_at:?}"),
        other => panic!("the locked corner did not hover as its vertex: {other:?}"),
    }
    assert!(lock.normal.dist(solidcraft_engine::geom::Vec3::Z) < 1e-6, "{:?}", lock.normal);
}

/// A few pixels along an edge from its end takes the edge, as Fusion does; the corner itself takes
/// the vertex.
#[test]
fn a_pointer_along_an_edge_takes_the_edge_while_the_corner_takes_the_vertex() {
    let mut h = joint_on_a_box();
    let corner = h.at(&json!({"world": [0.0, 0.0, 20.0]})).unwrap();
    let along = h.at(&json!({"world": [5.0, 0.0, 20.0]})).unwrap();
    // Four pixels from the corner toward the middle of the edge, in screen space.
    let (dx, dy) = (along[0] - corner[0], along[1] - corner[1]);
    let len = (dx * dx + dy * dy).sqrt();
    assert!(len > 8.0, "the edge is {len:.1} px on screen: too short to tell apart");
    h.step(&json!({ "move": [corner[0] + dx / len * 4.0, corner[1] + dy / len * 4.0] })).unwrap();
    assert!(
        matches!(h.app.viewport.hover, Some(Hit::Edge { .. })),
        "4 px along the edge from its corner did not take the edge: {:?}",
        h.app.viewport.hover
    );
    h.step(&json!({ "move": corner })).unwrap();
    assert!(matches!(h.app.viewport.hover, Some(Hit::Vertex { .. })), "the corner itself did not take the vertex: {:?}", h.app.viewport.hover);
}

/// Ctrl locks one face: crossing its edge (the pointer now over the face next door) keeps the
/// locked face, its points and its direction, so no other face highlights or steers the joint.
#[test]
fn the_locked_face_stays_while_the_pointer_crosses_its_edge() {
    let mut h = joint_on_a_box();
    let centre = h.at(&json!({"world": [10.0, 10.0, 20.0]})).unwrap();
    h.step(&json!({ "move": centre, "ctrl": true })).unwrap();
    let top = h.app.viewport.face_lock.clone().expect("Ctrl over the top face did not lock it");
    assert!(top.normal.dist(solidcraft_engine::geom::Vec3::Z) < 1e-6, "{:?}", top.normal);

    // Ten pixels past the near edge: the pointer is over a side face, near the top's own points.
    let edge = h.at(&json!({"world": [10.0, 0.0, 20.0]})).unwrap();
    let (dx, dy) = (edge[0] - centre[0], edge[1] - centre[1]);
    let len = (dx * dx + dy * dy).sqrt();
    assert!(len > 20.0, "the face is only {len:.1} px across on screen");
    h.step(&json!({ "move": [edge[0] + dx / len * 10.0, edge[1] + dy / len * 10.0], "ctrl": true })).unwrap();
    let lock = h.app.viewport.face_lock.clone().expect("the face lock was dropped at its edge");
    assert_eq!((lock.body.as_str(), lock.face), (top.body.as_str(), top.face), "the lock jumped to the face under the pointer");
    assert!(lock.normal.dist(solidcraft_engine::geom::Vec3::Z) < 1e-6, "the direction came from another face: {:?}", lock.normal);
}

/// The whole way through: a corner clicked while Ctrl locks the top face is the joint's point, and
/// the joint is made facing the way the face does (not along world Z, not toward another face).
#[test]
fn a_corner_clicked_on_a_locked_face_takes_that_faces_direction() {
    let mut h = joint_on_a_box();
    let corner = h.at(&json!({"world": [0.0, 0.0, 20.0]})).unwrap();
    let centre = h.at(&json!({"world": [10.0, 10.0, 20.0]})).unwrap();
    let (dx, dy) = (centre[0] - corner[0], centre[1] - corner[1]);
    let len = (dx * dx + dy * dy).sqrt();
    let at = [corner[0] + dx / len * 2.0, corner[1] + dy / len * 2.0];
    h.step(&json!({ "move": at, "ctrl": true })).unwrap();
    h.step(&json!({ "click": at, "ctrl": true })).unwrap();
    let d = h.app.dialog.clone().expect("the Joint dialog stayed open");
    let crate::dialogs::Kind::Assembly(crate::dialogs_assembly::Asm::Joint(f)) = &d.kind else { panic!("not the joint form") };
    assert_eq!(f.dirs.len(), 1, "the pick kept no direction: {:?}", f.dirs);
    for z in f.dirs.values() {
        assert!(z.dist(solidcraft_engine::geom::Vec3::Z) < 1e-6, "the direction is not the face's: {z:?}");
    }
}

/// The arrow's direction is the very direction the joint takes: both come from the dialog's face
/// snap, so a corner pick cannot show one axis and joint along another.
#[test]
fn the_locked_faces_direction_is_the_direction_the_joint_takes() {
    let mut h = joint_on_a_box();
    let centre = h.at(&json!({"world": [10.0, 10.0, 20.0]})).unwrap();
    h.step(&json!({ "move": centre, "ctrl": true })).unwrap();
    let lock = h.app.viewport.face_lock.clone().expect("Ctrl over the face did not lock it");
    let pick = solidcraft_engine::Sel::Face { body: lock.body.clone(), index: lock.face, point: lock.points[0].at };
    let joint_dir = crate::dialogs_assembly::snap_of(&h.app.session, &pick).expect("the face's snap").z;
    assert!(lock.normal.dist(joint_dir) < 1e-9, "the arrow shows {:?}, the joint would take {joint_dir:?}", lock.normal);
}
