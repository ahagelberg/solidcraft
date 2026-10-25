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
    assert!(matches!(h.app.viewport.hover, Some(Hit::Vertex { .. })), "the locked corner did not hover as a vertex: {:?}", h.app.viewport.hover);
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
