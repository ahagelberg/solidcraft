//! KiCad boards enter as persistent substrate meshes through both project and board paths.

use serde_json::json;

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
    let world = s.world_state();
    let body = world.bodies.first().expect("imported board body");
    let bbox = solidcraft_kernel::measure(&body.body).expect("board bounds").bbox;
    [bbox.min.x, bbox.min.y, bbox.min.z, bbox.max.x, bbox.max.y, bbox.max.z]
}

#[test]
fn project_insert_resolves_board_and_applies_thickness() {
    let dir = temp_dir("project");
    let project = dir.join("controller.kicad_pro");
    let board = dir.join("controller.kicad_pcb");
    std::fs::write(&project, "{}").unwrap();
    std::fs::write(&board, BOARD).unwrap();

    let mut s = Session::default();
    let result = run(&mut s, "file.insert_kicad", json!({"path": project, "thickness": 2.4}));
    assert_eq!(result["board_path"], board.to_string_lossy().to_string());
    assert_eq!(board_box(&s), [0.0, 0.0, 0.0, 20.0, 10.0, 2.4]);
    assert_eq!(s.model.state().bodies.len(), 1);

    let design = dir.join("import.solidcraft");
    run(&mut s, "file.save_as", json!({"path": design}));
    let mut reopened = Session::default();
    run(&mut reopened, "doc.open", json!({"path": design}));
    assert_eq!(board_box(&reopened), [0.0, 0.0, 0.0, 20.0, 10.0, 2.4]);

    run(&mut s, "edit.undo", json!({}));
    assert!(s.model.state().bodies.is_empty());
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
    assert_eq!(board_box(&s), [0.0, 0.0, 0.0, 20.0, 10.0, 1.6]);
    assert_eq!(s.doc.name, "sensor");
    let _ = std::fs::remove_dir_all(dir);
}
