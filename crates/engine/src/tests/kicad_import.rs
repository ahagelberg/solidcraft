//! KiCad boards enter as persistent substrate meshes through both project and board paths.

use serde_json::json;
use solidcraft_geom::Vec3;

use super::run;
use crate::*;

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("solidcraft-kicad-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const BOARD: &str = r#"(kicad_pcb (general (thickness 1.6))
  (gr_line (start 0 0) (end 20 0) (layer "Edge.Cuts"))
  (gr_line (start 20 0) (end 20 10) (layer "Edge.Cuts"))
  (gr_line (start 20 10) (end 0 10) (layer "Edge.Cuts"))
  (gr_line (start 0 10) (end 0 0) (layer "Edge.Cuts")))"#;

fn board_box(s: &Session) -> [f64; 6] {
    body_box(s, 0)
}

fn body_box(s: &Session, index: usize) -> [f64; 6] {
    let world = s.world_state();
    let body = world.bodies.get(index).expect("imported body");
    let bbox = solidcraft_kernel::measure(&body.body).expect("body bounds").bbox;
    [bbox.min.x, bbox.min.y, bbox.min.z, bbox.max.x, bbox.max.y, bbox.max.z]
}

fn with_tolerance(actual: [f64; 6], expected: [f64; 6]) -> bool {
    actual.iter().zip(expected).all(|(a, b)| (a - b).abs() < 1e-6)
}

#[test]
fn project_open_resolves_board_and_applies_thickness() {
    let dir = temp_dir("project");
    let project = dir.join("controller.kicad_pro");
    let board = dir.join("controller.kicad_pcb");
    std::fs::write(&project, "{}").unwrap();
    std::fs::write(&board, BOARD).unwrap();

    let mut s = Session::default();
    let result = run(&mut s, "doc.open", json!({"path": project, "thickness": 2.4}));
    assert_eq!(result["board_path"], board.to_string_lossy().to_string());
    assert_eq!(board_box(&s), [0.0, -10.0, 0.0, 20.0, 0.0, 2.4]);
    assert_eq!(s.model.state().bodies.len(), 1);

    let design = dir.join("import.solidcraft");
    run(&mut s, "file.save_as", json!({"path": design}));
    let mut reopened = Session::default();
    run(&mut reopened, "doc.open", json!({"path": design}));
    assert_eq!(board_box(&reopened), [0.0, -10.0, 0.0, 20.0, 0.0, 2.4]);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn direct_board_opens_as_a_new_design() {
    let dir = temp_dir("open");
    let board = dir.join("sensor.kicad_pcb");
    std::fs::write(&board, BOARD).unwrap();

    let mut s = Session::default();
    let result = run(&mut s, "doc.open", json!({"path": board}));
    assert_eq!(result["component_name"], "sensor");
    assert_eq!(board_box(&s), [0.0, -10.0, 0.0, 20.0, 0.0, 1.6]);
    assert_eq!(s.doc.name, "sensor");
    let _ = std::fs::remove_dir_all(dir);
}

/// A 2 × 4 × 6 mm box written as STEP, so placements can be measured exactly.
fn step_box(dir: &std::path::Path) -> String {
    let body = solidcraft_kernel::box_solid(Vec3::new(0.0, 0.0, 0.0), Vec3::new(2.0, 4.0, 6.0)).expect("box");
    let step = solidcraft_kernel::step_export(&[&body], "SolidCraft KiCad test").expect("STEP export");
    let path = dir.join("box.step");
    std::fs::write(&path, step).unwrap();
    path.to_string_lossy().to_string()
}

fn board_with_model(model: &str, footprint: &str, rotate: &str) -> String {
    format!(
        r#"(kicad_pcb (general (thickness 1.6))
  (gr_rect (start -20 -20) (end 20 20) (layer "Edge.Cuts"))
  (footprint "test:Part" {footprint}
    (property "Reference" "U1")
    (model "{model}" (at (xyz 0 0 0)) (scale (xyz 1 1 1)) (rotate (xyz {rotate})))))"#
    )
}

#[test]
fn step_models_follow_the_footprint_rotation_and_back_side_flip() {
    let dir = temp_dir("place");
    let model = step_box(&dir);
    // Front, rotated 90°: the model's own 90° and the footprint's 90° make 180°.
    let board = build_board(&dir, &board_with_model(&model, "(layer \"F.Cu\") (at 10 20 90)", "0 0 90"));
    let mut s = Session::default();
    run(&mut s, "doc.open", json!({"path": board}));
    assert!(with_tolerance(body_box(&s, 1), [8.0, -24.0, 0.0, 10.0, -20.0, 6.0]), "{:?}", body_box(&s, 1));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn back_side_models_turn_under_the_board() {
    let dir = temp_dir("back");
    let model = step_box(&dir);
    let board = build_board(&dir, &board_with_model(&model, "(layer \"B.Cu\") (at 10 20 0)", "0 0 0"));
    let mut s = Session::default();
    run(&mut s, "doc.open", json!({"path": board}));
    // The model is authored above the board; the back-side half turn puts it below (negative Z).
    assert!(with_tolerance(body_box(&s, 1), [10.0, -24.0, -6.0, 12.0, -20.0, 0.0]), "{:?}", body_box(&s, 1));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn euler_rotations_are_applied_about_x_then_y_then_z() {
    let dir = temp_dir("euler");
    let model = step_box(&dir);
    let source = format!(
        r#"(kicad_pcb (general (thickness 1.6))
  (gr_rect (start -20 -20) (end 20 20) (layer "Edge.Cuts"))
  (footprint "test:Part" (layer "F.Cu") (at 0 0 0)
    (property "Reference" "U1")
    (model "{model}" (offset (xyz 0 0 0)) (scale (xyz 1 1 1)) (rotate (xyz 90 0 90)))))"#
    );
    let board = build_board(&dir, &source);
    let mut s = Session::default();
    run(&mut s, "doc.open", json!({"path": board}));
    // X first turns the 6 mm height into Y, then Z turns the 4 mm depth into X.
    assert!(with_tolerance(body_box(&s, 1), [0.0, 0.0, 0.0, 6.0, 2.0, 4.0]), "{:?}", body_box(&s, 1));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn copper_silk_colour_and_component_options_are_honoured_and_persist() {
    let dir = temp_dir("options");
    let board = dir.join("options.kicad_pcb");
    std::fs::write(
        &board,
        r#"(kicad_pcb (general (thickness 1.6))
  (gr_rect (start 0 0) (end 20 10) (layer "Edge.Cuts"))
  (gr_line (start 1 1) (end 10 1) (layer "F.SilkS") (width 0.2))
  (segment (start 1 2) (end 10 2) (width 0.3) (layer "F.Cu") (net 1))
  (footprint "test:Part" (layer "F.Cu") (at 5 5)
    (pad "1" smd rect (at 0 0) (size 1 2) (layers "F.Cu" "F.Paste"))))"#,
    )
    .unwrap();
    let mut s = Session::default();
    let result = run(&mut s, "doc.open", json!({"path": board, "components": false, "copper": true, "silkscreen": true, "color": "#ff8000"}));
    let names: Vec<String> = result["bodies"].as_array().unwrap().iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
    assert!(names.iter().any(|n| n == "Copper F.Cu"), "{names:?}");
    assert!(names.iter().any(|n| n == "Silkscreen F.SilkS"), "{names:?}");
    let board_body = &s.world_state().bodies[0];
    let color = board_body.body.color().expect("board colour");
    assert!((color[0] - 1.0).abs() < 1e-5 && (color[1] - 128.0 / 255.0).abs() < 1e-5 && color[2].abs() < 1e-5, "{color:?}");

    let design = dir.join("options.solidcraft");
    run(&mut s, "file.save_as", json!({"path": design}));
    let mut reopened = Session::default();
    run(&mut reopened, "doc.open", json!({"path": design}));
    assert!(reopened.world_state().bodies.iter().any(|b| b.name == "Copper F.Cu"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn b_rep_is_refused() {
    let mut s = Session::default();
    let board = std::env::temp_dir().join(format!("solidcraft-kicad-brep-{}.kicad_pcb", std::process::id()));
    std::fs::write(&board, BOARD).unwrap();
    let error = s.execute("doc.open", &json!({"path": board, "representation": "brep"})).expect_err("B-rep must be refused");
    assert!(error.to_string().contains("not supported yet"), "{error}");
    let _ = std::fs::remove_file(board);
}

/// Write a board source beside a test's files and return its path.
fn build_board(dir: &std::path::Path, source: &str) -> std::path::PathBuf {
    let path = dir.join("board.kicad_pcb");
    std::fs::write(&path, source).unwrap();
    path
}
