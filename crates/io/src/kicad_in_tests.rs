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
    assert!(!mesh.positions.is_empty());
    assert!(mesh.triangles.iter().flatten().all(|index| (*index as usize) < mesh.positions.len()));
    let body = solidcraft_kernel::mesh_body(&mesh.positions, &mesh.triangles).unwrap();
    let measure = solidcraft_kernel::measure(&body).unwrap();
    assert!((measure.volume - 300.0 * 1.6).abs() < 1.0, "{measure:?}");
    assert!((measure.bbox.min.y + 20.0).abs() < 1e-9, "KiCad's Y is mapped to -Y: {measure:?}");
}

#[test]
fn curved_outlines_and_circular_cutouts_make_an_annular_board() {
    let source = r#"(kicad_pcb (general (thickness 1.6))
      (gr_circle (center 0 0) (end 10 0) (layer "Edge.Cuts"))
      (gr_circle (center 0 0) (end 3 0) (layer "Edge.Cuts")))"#;
    let board = parse_kicad_board(source.as_bytes(), "ring.kicad_pcb").unwrap();
    assert_eq!(board.outlines.len(), 2);
    let mesh = kicad_board_mesh(&board, 1.6).unwrap();
    let body = solidcraft_kernel::mesh_body(&mesh.positions, &mesh.triangles).unwrap();
    let measure = solidcraft_kernel::measure(&body).unwrap();
    let expected = std::f64::consts::PI * (100.0 - 9.0) * 1.6;
    assert!((measure.volume - expected).abs() < expected * 0.01, "{measure:?}");
}

#[test]
fn drilled_pads_cut_holes_in_the_substrate() {
    let source = r#"(kicad_pcb (general (thickness 1.6))
      (gr_rect (start 0 0) (end 20 10) (layer "Edge.Cuts"))
      (footprint "hole" (at 10 5) (layer "F.Cu")
        (pad "1" thru_hole circle (at 0 0) (size 2 2) (drill 1) (layers "*.Cu" "*.Mask"))))"#;
    let board = parse_kicad_board(source.as_bytes(), "holes.kicad_pcb").unwrap();
    assert_eq!(board.drills.len(), 1);
    assert_eq!(board.drills[0].center, [10.0, -5.0]);
    let mesh = kicad_board_mesh(&board, 1.6).unwrap();
    let body = solidcraft_kernel::mesh_body(&mesh.positions, &mesh.triangles).unwrap();
    let measure = solidcraft_kernel::measure(&body).unwrap();
    let expected = (200.0 - std::f64::consts::PI * 0.25) * 1.6;
    assert!((measure.volume - expected).abs() < expected * 0.01, "{measure:?}");
}

#[test]
fn missing_model_is_warned_and_the_board_is_kept() {
    let mut load = |_: &str| Err("model file was not found".to_string());
    let imported = read_kicad(BOARD.as_bytes(), "test.kicad_pcb", &KicadOptions::default(), &mut load).unwrap();
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

#[test]
fn import_options_build_copper_silk_and_a_colour_and_keep_the_plan_flat() {
    let source = r#"(kicad_pcb (general (thickness 1.6))
      (gr_rect (start 0 0) (end 20 10) (layer "Edge.Cuts"))
      (gr_line (start 1 1) (end 10 1) (layer "F.SilkS") (width 0.2))
      (segment (start 1 2) (end 10 2) (width 0.3) (layer "F.Cu") (net 1))
      (footprint "part" (at 5 5) (layer "F.Cu")
        (pad "1" smd rect (at 0 0) (size 1 2) (layers "F.Cu" "F.Paste"))))"#;
    let options = KicadOptions { copper: true, silkscreen: true, components: false, color: Some([0x11, 0x22, 0x33]), ..Default::default() };
    let mut load = |_: &str| Err("not used".to_string());
    let imported = read_kicad(source.as_bytes(), "board.kicad_pcb", &options, &mut load).unwrap();
    let solidcraft_doc::FeatureKind::MeshImport { meshes, .. } = imported.feature.kind else { panic!("expected a mesh import") };
    let names: Vec<&str> = meshes.iter().map(|m| m.name.as_str()).collect();
    assert!(names.contains(&"board"), "{names:?}");
    assert!(names.contains(&"Copper F.Cu"), "{names:?}");
    assert!(names.contains(&"Silkscreen F.SilkS"), "{names:?}");
    let board = meshes.iter().find(|m| m.name == "board").unwrap();
    assert_eq!(board.color, Some([0x11u8 as f32 / 255.0, 0x22u8 as f32 / 255.0, 0x33u8 as f32 / 255.0]));
    assert!(imported.plan.places.is_empty());
    assert!(imported.plan.slots.is_empty());
    assert!(imported.feature.warnings.is_empty(), "{:?}", imported.feature.warnings);
}

#[test]
fn a_filled_zone_adds_copper_next_to_tracks() {
    let source = r#"(kicad_pcb (general (thickness 1.6))
      (gr_rect (start -30 -20) (end 30 20) (layer "Edge.Cuts"))
      (segment (start -20 -12) (end 0 -12) (width 1.2) (layer "B.Cu") (net 1))
      (zone (net 1) (net_name "GND") (layer "B.Cu")
        (filled_polygon (layer "B.Cu") (pts (xy -24 16) (xy -14 16) (xy -14 18) (xy -24 18)))))"#;
    let options = KicadOptions { copper: true, components: false, silkscreen: false, ..Default::default() };
    let mut load = |_: &str| Err("not used".to_string());
    let imported = read_kicad(source.as_bytes(), "board.kicad_pcb", &options, &mut load).unwrap();
    let solidcraft_doc::FeatureKind::MeshImport { meshes, .. } = imported.feature.kind else { panic!("expected a mesh import") };
    let copper = meshes.iter().find(|mesh| mesh.name == "Copper B.Cu").expect("the back copper");
    // The track is at world y = +12; the zone is at world y = -18..-16.
    let ys: Vec<f64> = copper.positions.as_chunks::<3>().0.iter().map(|p| p[1]).collect();
    assert!(ys.iter().any(|y| *y < -15.0), "the zone is missing: {ys:?}");
    let vertices = copper.positions.len() / 3;
    assert!(copper.triangles.iter().all(|index| (*index as usize) < vertices), "the zone's triangles point at the wrong vertices");
}

#[test]
fn copper_and_silkscreen_are_off_by_default() {
    let source = r#"(kicad_pcb (general (thickness 1.6))
      (gr_rect (start 0 0) (end 20 10) (layer "Edge.Cuts"))
      (gr_line (start 1 1) (end 10 1) (layer "F.SilkS") (width 0.2))
      (segment (start 1 2) (end 10 2) (width 0.3) (layer "F.Cu") (net 1)))"#;
    let mut load = |_: &str| Err("not used".to_string());
    let options = KicadOptions { silkscreen: false, ..Default::default() };
    let imported = read_kicad(source.as_bytes(), "board.kicad_pcb", &options, &mut load).unwrap();
    let solidcraft_doc::FeatureKind::MeshImport { meshes, .. } = imported.feature.kind else { panic!("expected a mesh import") };
    assert_eq!(meshes.len(), 1);
    assert_eq!(meshes[0].name, "board");
}

#[test]
fn vrml_models_are_read_and_placed() {
    let source = r#"(kicad_pcb (general (thickness 1.6))
      (gr_rect (start 0 0) (end 20 10) (layer "Edge.Cuts"))
      (footprint "part" (at 5 5) (layer "F.Cu") (property "Reference" "U1")
        (model "part.wrl" (offset (xyz 0 0 0)) (scale (xyz 1 1 1)) (rotate (xyz 0 0 0)))))"#;
    let mut load = |_: &str| {
        Ok(br#"#VRML V2.0 utf8
      Shape { geometry Box { size 2 2 2 } }"#
            .to_vec())
    };
    let imported = read_kicad(source.as_bytes(), "board.kicad_pcb", &KicadOptions::default(), &mut load).unwrap();
    let solidcraft_doc::FeatureKind::MeshImport { meshes, .. } = imported.feature.kind else { panic!("expected a mesh import") };
    assert_eq!(meshes.len(), 2, "the board and the VRML model");
    assert_eq!(imported.plan.slots, vec!["U1".to_string()]);
    assert_eq!(imported.plan.places.len(), 1);
    assert!(imported.feature.warnings.is_empty(), "{:?}", imported.feature.warnings);
}

#[test]
fn b_rep_is_refused_with_a_clear_message() {
    let options = KicadOptions { representation: KicadRepresentation::Brep, ..Default::default() };
    let mut load = |_: &str| Err("not used".to_string());
    let error = read_kicad(BOARD.as_bytes(), "test.kicad_pcb", &options, &mut load).err().expect("B-rep must be refused");
    assert!(error.to_string().contains("B-rep"), "{error}");
}
