# KiCad PCB import

SolidCraft reads KiCad printed-circuit boards (`.kicad_pcb`, or a `.kicad_pro` project, which resolves to its same-stem board). The reader in `crates/io/src/kicad_in.rs` is written from KiCad's documented s-expression format; it brings the board in as mesh bodies in a component named after the file, building the 3D models of its footprints as nested components laid out on the board.

- **Open**: File → Open (`doc.open`) accepts `.kicad_pcb`/`.kicad_pro` and starts a new design; `solidcraft board.kicad_pcb`; the MCP `open` tool.
- **Insert** into the current design: File → Insert KiCad PCB… or SOLID → INSERT (`file.insert_kicad {path}`), which first asks for the import settings.
- **Drag and drop** a board on the window: inserted when the design has features, opened otherwise.

The board's geometry is built once, at import: the design stores the resulting meshes, so it reopens without the board file or its 3D models. Changing the options means inserting the board again.

## What comes in

The board substrate always comes in:

- **Outline**: the closed contours on the `Edge.Cuts` layer at board level and inside footprints — lines, rectangles, arcs, circles, filled polygons, curves and Bézier curves (arcs, circles and curves are drawn as polylines within 0.02 mm). An open contour is dropped with a warning.
- **Cutouts**: a closed contour inside the outline is a hole through the board (even-odd nesting, so an island inside a cutout is board again).
- **Drilled holes**: pad drills and via drills, round or slotted, are removed from the substrate. If a drill that touches a cutout defeats the profile, the board is rebuilt without the drills rather than failing.
- **Thickness**: the board's own `general.thickness`, unless the `thickness` option overrides it. The substrate top is Z = 0, so components sit above it and back-side parts below.

The other layers are options:

- **3D models** (`components`, default on): the STEP and VRML models that footprints reference, placed by the footprint's position and rotation. A model is authored for the front; on a back-side footprint it gets a half turn about X, which also puts it under the board. The model's own `at`/`offset`, `scale` and `rotate` (X, then Y, then Z) are applied, and a mirroring scale is refused. STEP models are tessellated at 0.1 mm; VRML 2.0 models are read directly (see below). A model referenced by several footprints is read once and placed by each, and every footprint that places a model becomes a component (named after its reference, e.g. `R1`) under the import's component.
- **Copper** (`copper`, default off): the front and back copper foil, 0.035 mm thick, in a copper colour — tracks and track arcs, vias, filled zones, board-level copper graphics, and pads when `pads` is on (default on). Pads are the pad's copper shape; the drill passes through the substrate, not the copper.
- **Silkscreen** (`silkscreen`, default on): the lines and filled shapes of the front and back silk layers, 0.01 mm above (or below) the substrate, in a white colour.
- **Board colour** (`color`): the substrate colour, `"#rrggbb"` (or `"#rgb"`) or `[r, g, b]`. The default is solder-mask green.

```json
{"command": "file.insert_kicad", "params": {
  "path": "keyboard.kicad_pro",
  "components": true,
  "copper": true,
  "pads": true,
  "silkscreen": true,
  "color": "#103c2a"
}}
```

## Options

| option | default | meaning |
| --- | --- | --- |
| `path` | — | `.kicad_pcb` board or `.kicad_pro` project (any case) |
| `name` | the file's | the Import feature's and the component's name |
| `thickness` | the board's | substrate thickness in millimetres (finite, more than 0 and at most 1000) |
| `components` | `true` | load the footprint 3D models |
| `copper` | `false` | build the front and back copper |
| `pads` | `true` | include pad copper (only with `copper`) |
| `silkscreen` | `true` | build the front and back silkscreen |
| `color` | solder-mask green | board substrate colour, `"#rrggbb"`/`"#rgb"` or `[r, g, b]` |
| `representation` | `mesh` | `mesh` or `brep`; B-rep is not supported yet and is refused before the board is read |

`file.insert_kicad` validates every option before reading the board, so a bad colour or a B-rep request fails with a message and leaves the design untouched. The command's reply reports the feature, the component (`component`, `component_name`, `occurrence`), the body names and the warnings the reader collected.

## In the window

File → Insert KiCad PCB… (or SOLID → INSERT → Insert KiCad PCB) picks the board, then opens **Import KiCad PCB**: the 3D models, the copper layers (with pads), the silkscreen, the board's own thickness or a typed one, the board colour, and the representation (Mesh; B-rep is shown disabled). **Import** runs `file.insert_kicad` with those options, one undo step; **Cancel** leaves the design alone.

Opening a board, or dropping one on the window, uses the defaults (models, silkscreen, board thickness, no copper) and can be repeated with other options from the palette, a script or the control channel.

## VRML models

`.wrl` footprint models are read by `crates/io/src/vrml_in.rs`: VRML 2.0 with `Transform` (translation, rotation, scale), `Group`, `Switch` and the other grouping nodes, `Shape`/`Appearance` (one `diffuseColor` colours the model; several are reported and the first is used), `IndexedFaceSet`, `IndexedTriangleSet`, and the `Box`, `Sphere`, `Cylinder` and `Cone` primitives, with `DEF`/`USE` sharing. `PROTO`, `EXTERNPROTO` and `ROUTE` are skipped, VRML 1.0 is refused, and a file without geometry is refused. Textures, normal interpolation and lights are not used.

## Limits

The board file is untrusted input: its size (128 MB), token count, nesting, footprints, models, pads, tracks, vias, zones, graphics, vertices and triangles are capped, numbers must be finite and bounded, and the kernel calls that build the bodies are guarded. Passing a limit becomes a warning and skips the remaining items; a board that cannot be built fails the command.

Not modelled yet:

- **B-rep** boards (the mesh representation is the only one).
- **Text** on copper or silkscreen; it is reported as one warning.
- **Inner copper layers**: tracks and zones on layers other than `F.Cu` are drawn with the back copper, and pads that exist only on inner layers are skipped. There is no stackup, layer thickness or dielectric model.
- **Copper holes**: drills are cut from the substrate only.
- **Other footprint layers**: only `Edge.Cuts` graphics inside a footprint join the outline; courtyard, fab and other layers are ignored.
- **Nets, ratsnest, DRC, drawings and panelisation** (the board's own panel array is not built).
