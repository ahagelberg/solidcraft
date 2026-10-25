//! The joint being edited shows its origin with translate-style arrows in the view: drag one to
//! move the joint across or along its own axes (Offset X, Y and Z follow in the dialog). The whole
//! drag is one undo step.

use egui::{Pos2, Sense, Stroke};
use serde_json::json;
use solidcraft_engine::geom::Vec3;

use crate::SolveApp;
use crate::theme::Tokens;
use crate::viewport::Proj;

/// How near an arrow (in points) a press takes it.
const GRIP: f32 = 9.0;
/// The arrows' length on screen, in points.
const LENGTH: f32 = 82.0;

/// A drag along one of the joint's axes.
#[derive(Clone, Debug)]
pub struct Drag {
    axis: usize,
    /// Where along the arrow the drag began, 0 at the origin and 1 at its tip.
    from: f64,
    /// The three offsets when it began (mm).
    base: [f64; 3],
    /// The drag's undo step is on top of the history (it went in with the first frame that moved).
    pushed: bool,
}

/// The joint the dialog is editing, its world frame and its offsets: (x, y across the frame), z
/// along it.
fn editing(app: &SolveApp) -> Option<(u64, [Vec3; 3], Vec3, [f64; 3])> {
    let d = app.dialog.as_ref()?;
    let crate::dialogs::Kind::Assembly(crate::dialogs_assembly::Asm::Joint(f)) = &d.kind else { return None };
    let id = f.editing?;
    let j = app.session.doc.assembly.joints.iter().find(|j| j.id == id)?;
    let st = app.session.model.state();
    let m = app.session.doc.origin_world(&st, &j.a).ok()?;
    let axis = |i: usize| Vec3::new(m[i][0], m[i][1], m[i][2]);
    let origin = Vec3::new(m[3][0], m[3][1], m[3][2]);
    let mm = |e: &str| crate::length_input::eval(&app.session.doc, e, solidcraft_engine::doc::expr::Kind::Length).unwrap_or(0.0);
    let offsets = [mm(&f.offset[0]), mm(&f.offset[1]), mm(&f.offset[2])];
    Some((id, [axis(0), axis(1), axis(2)], origin, offsets))
}

/// Draw the arrows (and handle a drag along one). Called once a frame with the viewport's painter.
pub fn show(app: &mut SolveApp, ui: &mut egui::Ui, proj: &Proj) {
    let Some((id, axes, origin, offsets)) = editing(app) else {
        app.viewport.joint_gizmo = None;
        return;
    };
    let t = Tokens::get();
    let (Some(o), Some(ox), Some(oy), Some(oz)) =
        (proj.to_screen(origin), proj.to_screen(origin + axes[0]), proj.to_screen(origin + axes[1]), proj.to_screen(origin + axes[2]))
    else {
        return;
    };
    let dirs = [ox - o, oy - o, oz - o];
    let cols = [t.axis_x, t.axis_y, t.axis_z];
    let painter = ui.painter().clone();
    painter.circle(o, 4.0, t.sel_vertex, Stroke::new(1.0, t.hover_edge));
    for (i, d) in dirs.iter().enumerate() {
        let tip = o + d.normalized() * LENGTH;
        painter.line_segment([o, tip], Stroke::new(2.0, cols[i]));
        painter.circle_filled(tip, 4.0, cols[i]);
    }
    // Each arrow is its own grip: a press near one starts a drag along it.
    for (i, d) in dirs.iter().enumerate() {
        let tip = o + d.normalized() * LENGTH;
        let hit = egui::Rect::from_two_pos(o, tip).expand(GRIP);
        let r = ui.interact(hit, ui.id().with(("joint_gizmo_axis", i)), Sense::click_and_drag());
        if r.drag_started() {
            let start = along(o, tip, r.interact_pointer_pos().unwrap_or(o));
            app.viewport.joint_gizmo = Some(Drag { axis: i, from: start, base: offsets, pushed: false });
        }
        if r.hovered() || r.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
    }
    let Some(drag) = app.viewport.joint_gizmo.clone() else { return };
    let tip = o + dirs[drag.axis].normalized() * LENGTH;
    let Some(pointer) = ui.ctx().pointer_interact_pos() else { return };
    if !ui.input(|i| i.pointer.primary_down()) {
        app.viewport.joint_gizmo = None;
        return;
    }
    // How far the pointer has gone along the arrow, in mm (the arrow is `LENGTH` points long and
    // stands for one arrow-length of the axis in the view).
    let moved = (along(o, tip, pointer) - drag.from) * arrow_mm(app);
    let mut value = drag.base;
    value[drag.axis] = drag.base[drag.axis] + moved;
    // The dialog shows the numbers the drag is setting.
    if let Some(d) = app.dialog.as_mut()
        && let crate::dialogs::Kind::Assembly(crate::dialogs_assembly::Asm::Joint(f)) = &mut d.kind
    {
        for (i, v) in value.iter().enumerate() {
            f.offset[i] = crate::length_input::model(&app.session.doc, *v);
        }
    }
    let params = json!({
        "joint": id,
        "offset_x": value[0],
        "offset_y": value[1],
        "offset": value[2],
    });
    // One undo step for the whole drag: the first frame's, kept on top while the others run.
    let own = if drag.pushed { app.session.undo.pop() } else { None };
    let before = app.session.doc.clone();
    let pushed = app.run("joint.edit", params).is_ok() && !std::sync::Arc::ptr_eq(&before, &app.session.doc);
    match own {
        Some(step) => {
            if pushed {
                app.session.undo.pop();
            }
            app.session.undo.push(step);
        }
        None if pushed => {
            if let Some(d) = app.viewport.joint_gizmo.as_mut() {
                d.pushed = true;
            }
        }
        None => {}
    }
}

/// How far along `a`→`b` the point `p` is: 0 at `a`, 1 at `b`, clamped to the segment.
fn along(a: Pos2, b: Pos2, p: Pos2) -> f64 {
    let (d, q) = (b - a, p - a);
    let len2 = d.length_sq() as f64;
    if !(len2 > 1e-9) {
        return 0.0;
    }
    ((d.x as f64 * q.x as f64 + d.y as f64 * q.y as f64) / len2).clamp(0.0, 1.0)
}

/// The world length one arrow stands for: a fixed share of the view, so a drag reads the same at
/// any zoom.
fn arrow_mm(app: &SolveApp) -> f64 {
    (app.cam.half_height() * 0.25).max(1e-6)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_point_along_an_arrow_is_measured_from_its_origin() {
        let (a, b) = (egui::pos2(10.0, 10.0), egui::pos2(110.0, 10.0));
        assert!((along(a, b, egui::pos2(60.0, 10.0)) - 0.5).abs() < 1e-9);
        assert_eq!(along(a, b, egui::pos2(-40.0, 10.0)), 0.0, "behind the origin is clamped");
        assert_eq!(along(a, b, egui::pos2(400.0, 10.0)), 1.0, "past the tip is clamped");
        // Sideways of the line makes no difference: the arrow's own line is what counts.
        assert!((along(a, b, egui::pos2(60.0, 90.0)) - 0.5).abs() < 1e-9);
    }
}
