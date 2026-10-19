use super::*;

const BOARD: &str = r#"(kicad_pcb
  (version 20250101)
  (general (thickness 1.6))
  (gr_line (start 0 0) (end 20 0) (layer "Edge.Cuts"))
  (gr_line (start 20 10) (end 0 10) (layer "Edge.Cuts"))
  (gr_line (start 20 0) (end 20 10) (layer "Edge.Cuts"))
  (gr_line (start 0 10) (end 0 0) (layer "Edge.Cuts"))
  (footprint "Device:R_0603" (layer "F.Cu") (at 10.25 20.5 90)
    (property "Reference" "R7")
    (property "Value" "10k")
    (model "${KICAD9_3DMODEL_DIR}/Resistor_SMD.3dshapes/R_0603.step"
      (at (xyz 0 0 0.1))
      (scale (xyz 1 1 1))
      (rotate (xyz 0 0 90))
    )
  )
)"#;

#[test]
fn parses_thickness_footprint_properties_and_model_transform() {
    let board = parse_kicad_board(BOARD.as_bytes(), "test.kicad_pcb").unwrap();
    assert_eq!(board.name, "test");
    assert_eq!(board.thickness, 1.6);
    let [footprint] = board.footprints.as_slice() else { panic!("expected one footprint") };
    assert_eq!(footprint.reference, "R7");
    assert_eq!(footprint.value, "10k");
    assert_eq!(footprint.layer, "F.Cu");
    assert_eq!(footprint.at, [10.25, 20.5, 90.0]);
    let [model] = footprint.models.as_slice() else { panic!("expected one model") };
    assert_eq!(model.path, "${KICAD9_3DMODEL_DIR}/Resistor_SMD.3dshapes/R_0603.step");
    assert_eq!(model.at, [0.0, 0.0, 0.1]);
    assert_eq!(model.scale, [1.0; 3]);
    assert_eq!(model.rotate, [0.0, 0.0, 90.0]);
    assert_eq!(board.outlines.len(), 1);
    assert_eq!(board.outlines[0].len(), 4);
}

#[test]
fn builds_a_closed_mesh_from_a_concave_outline() {
    let source = r#"(kicad_pcb (general (thickness 1.6))
      (gr_line (start 0 0) (end 20 0) (layer "Edge.Cuts"))
      (gr_line (start 20 0) (end 20 10) (layer "Edge.Cuts"))
      (gr_line (start 20 10) (end 10 10) (layer "Edge.Cuts"))
      (gr_line (start 10 10) (end 10 20) (layer "Edge.Cuts"))
      (gr_line (start 10 20) (end 0 20) (layer "Edge.Cuts"))
      (gr_line (start 0 20) (end 0 0) (layer "Edge.Cuts")))"#;
    let board = parse_kicad_board(source.as_bytes(), "board.kicad_pcb").unwrap();
    let mesh = kicad_board_mesh(&board, board.thickness).unwrap();
    assert_eq!(mesh.positions.len(), 12);
    assert_eq!(mesh.triangles.len(), 20);
    assert!(mesh.triangles.iter().flatten().all(|index| (*index as usize) < mesh.positions.len()));
}

#[test]
fn missing_model_is_warned_and_the_board_is_kept() {
    let mut load = |_: &str| Err("model file was not found".to_string());
    let imported = read_kicad(BOARD.as_bytes(), "test.kicad_pcb", None, &mut load).unwrap();
    let solidcraft_doc::FeatureKind::MeshImport { meshes, .. } = imported.feature.kind else { panic!("expected a mesh import") };
    assert_eq!(meshes.len(), 1, "only the board substrate should remain");
    assert!(imported.plan.places.is_empty());
    assert!(imported.feature.warnings.iter().any(|warning| warning.contains("model file was not found")));
}

#[test]
fn skips_footprints_with_invalid_models_and_keeps_other_parts() {
    let source = r#"(kicad_pcb (general (thickness 1.6))
      (footprint "bad" (at 1 2)
        (model "bad.step" (scale (xyz 0 1 1)))
        (model "good.step"))
      (footprint "good" (at 3 4 0) (property "Reference" "U1")))"#;
    let board = parse_kicad_board(source.as_bytes(), "board.kicad_pcb").unwrap();
    assert_eq!(board.footprints.len(), 2);
    assert_eq!(board.footprints[0].models.len(), 1);
    assert_eq!(board.footprints[0].models[0].path, "good.step");
    assert_eq!(board.footprints[1].reference, "U1");
    assert_eq!(board.warnings.len(), 1);
}

#[test]
fn rejects_malformed_and_wrong_root_documents() {
    assert!(parse_kicad_board(b"(kicad_pcb (general", "bad.kicad_pcb").is_err());
    assert!(parse_kicad_board(b"(kicad_pro)", "bad.kicad_pcb").is_err());
}

#[test]
fn handles_escaped_model_paths_and_kicad_extensions_case_insensitively() {
    let source = r#"(kicad_pcb
      (footprint "part" (at 0 0)
        (model "${KIPRJMOD}/models/a\\b\"c.step")))"#;
    let board = parse_kicad_board(source.as_bytes(), "board.kicad_pcb").unwrap();
    assert_eq!(board.footprints[0].models[0].path, "${KIPRJMOD}/models/a\\b\"c.step");
    assert!(is_kicad_path("demo.KICAD_PRO"));
    assert!(is_kicad_path("demo.KICAD_PCB"));
    assert!(!is_kicad_path("demo.step"));
}

#[test]
fn resolves_project_to_same_stem_board_and_keeps_direct_board() {
    assert_eq!(kicad_board_path(Path::new("folder/demo.kicad_pro")).unwrap(), Path::new("folder/demo.kicad_pcb"));
    assert_eq!(kicad_board_path(Path::new("folder/demo.kicad_pcb")).unwrap(), Path::new("folder/demo.kicad_pcb"));
    assert!(kicad_board_path(Path::new("folder/demo.step")).is_err());
}

#[test]
fn rejects_non_finite_footprint_placement() {
    let source = b"(kicad_pcb (footprint \"bad\" (at NaN 1)))";
    let board = parse_kicad_board(source, "board.kicad_pcb").unwrap();
    assert!(board.footprints.is_empty());
    assert_eq!(board.warnings.len(), 1);
}
