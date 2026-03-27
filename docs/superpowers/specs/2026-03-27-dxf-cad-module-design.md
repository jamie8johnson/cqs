# DXF/DWG CAD Module — Design Spec

> **Status:** Skeleton with prompt notes. Not yet planned for implementation.
>
> **For agentic workers:** This is a design document, not an implementation plan. Read it to understand the vision, then use superpowers:writing-plans to create the implementation plan when ready.

## Goal

Semantic search, connectivity graph, and impact analysis for engineering CAD drawings (DXF/DWG). Apply cqs's code intelligence patterns — search by concept, trace dependencies, assess impact of changes — to engineering drawings.

## Why

Engineers grep DXF files with text editors or click through AutoCAD layer by layer. No semantic search, no "what connects to what" query tool, no impact analysis for detail changes. The market is completely unserved.

## Architecture Mapping

cqs already has all the infrastructure. CAD concepts map directly:

| Code concept | CAD equivalent | cqs command |
|-------------|---------------|-------------|
| Function definition | Block definition (BLOCK section) | `cqs explain PUMP_DETAIL` |
| Function call | Block INSERT reference | `cqs callers PUMP_DETAIL` |
| Module/file | Layer | `cqs context "Electrical"` |
| Call graph edge | Physical connection (shared endpoints) | `cqs trace pump_P101 tank_T201` |
| Type dependency | Attribute reference | `cqs deps "VALVE_TAG"` |
| Dead code | Unused block definitions | `cqs dead` |
| Impact analysis | "What uses this detail?" | `cqs impact VALVE_SYMBOL` |
| Gather | Spatial proximity + connectivity | `cqs gather "boiler room"` |

## DXF Format Overview

DXF (Drawing Exchange Format) is Autodesk's ASCII interchange format for CAD data.

### Structure
```
0           <- group code (always an integer)
SECTION     <- value (string, float, or integer depending on code)
2
HEADER
...
0
ENDSEC
0
SECTION
2
BLOCKS
...
```

### Key sections for cqs
- **BLOCKS**: Block definitions — the "functions" of CAD. Named, reusable groups of entities.
- **ENTITIES**: Top-level drawing entities (lines, circles, text, INSERTs).
- **TABLES**: Layer definitions, line types, dimension styles, text styles.
- **HEADER**: Drawing metadata (units, limits, version).

### Entity types to chunk
- **INSERT**: Block reference at a location. Has block name, position, rotation, scale, attributes.
- **TEXT/MTEXT**: Readable text. Tags, labels, notes, title block fields.
- **DIMENSION**: Dimension annotations with measurement values.
- **ATTRIB/ATTDEF**: Block attributes (key-value pairs on INSERT instances).
- **LINE/POLYLINE/LWPOLYLINE**: Geometry — primarily for connectivity graph, not chunking.
- **CIRCLE/ARC/ELLIPSE**: Geometry — skip for search, use for spatial queries.

## Phase 1: Parse & Index

### Parser

Use the `dxf` Rust crate (0.6.1) for reading. It handles both ASCII and binary DXF.

```rust
// Pseudocode — read before implementing
use dxf::Drawing;

let drawing = Drawing::load_file("plant.dxf")?;

// Extract block definitions as chunks
for block in &drawing.blocks {
    // ChunkType::Block
    // name = block.name
    // content = serialize block entities to readable form
    // signature = "BLOCK {name} ({entity_count} entities, layers: {layers})"
}

// Extract INSERT references as call edges
for entity in &drawing.entities {
    if let EntityType::Insert(insert) = &entity.specific {
        // Call edge: current context → insert.name
        // Position: insert.location
        // Attributes: insert.attributes
    }
}

// Extract TEXT/MTEXT as searchable chunks
for entity in &drawing.entities {
    match &entity.specific {
        EntityType::Text(t) => { /* ChunkType::Section, name = truncated text */ }
        EntityType::MText(t) => { /* ChunkType::Section, name = truncated text */ }
        _ => {}
    }
}
```

### Chunk types

```
ChunkType::Block       — block definition (like function)
ChunkType::Section     — text/mtext annotation (like doc comment)
ChunkType::Property    — dimension annotation (like constant)
ChunkType::Module      — layer definition (like module)
```

### NL generation

For each block:
```
"Block {name} containing {entity_count} entities on layer {layer}.
 Attributes: {attr1}={val1}, {attr2}={val2}.
 Contains text: {contained_text_entities}.
 Used {insert_count} times in the drawing."
```

For each text entity:
```
"Text annotation '{text}' on layer {layer} at position ({x}, {y}).
 Near block {nearest_block} (distance: {d})."
```

### Language definition

```rust
// src/language/dxf.rs
//
// NOTE: DXF does not use tree-sitter. It uses a custom parser via the `dxf` crate.
// The LanguageDef is configured with grammar: None (like Markdown).
// The actual parsing happens in a dedicated DxfParser that implements
// the same Chunk/CallSite output interface as tree-sitter languages.
//
// Key design decisions:
// - Block definitions → ChunkType::Block (primary searchable unit)
// - Block INSERTs → call graph edges (caller=parent context, callee=block name)
// - TEXT/MTEXT → ChunkType::Section (searchable annotations)
// - Layers → used for filtering, not as separate chunks
// - Geometry (LINE/CIRCLE/ARC) → not chunked, but coordinates stored for Phase 4
// - Attributes on INSERTs → stored as chunk metadata, used in NL generation
```

### DWG support

DWG is proprietary binary. Two approaches:
1. **External conversion**: Shell out to ODA File Converter or LibreDWG's `dwg2dxf`
2. **Rust bindings**: `libredwg-sys` (if it exists) or FFI to LibreDWG C library

Recommend option 1 for MVP — same pattern as `cqs convert` which shells out to Python for PDF/CHM.

## Phase 2: Insert Graph

Block INSERT references create a call graph:

```
BLOCK "PUMP_ASSEMBLY"
  INSERT "VALVE_SYMBOL" at (10, 20)
  INSERT "PIPE_FLANGE" at (30, 20)
  INSERT "PIPE_FLANGE" at (50, 20)
ENDBLK
```

Graph edges:
- PUMP_ASSEMBLY → VALVE_SYMBOL (caller → callee)
- PUMP_ASSEMBLY → PIPE_FLANGE (caller → callee, 2 references)

This maps directly to cqs's `function_calls` table:
```sql
INSERT INTO function_calls (caller_name, callee_name, file)
VALUES ('PUMP_ASSEMBLY', 'VALVE_SYMBOL', 'plant.dxf');
```

All existing graph commands work immediately:
- `cqs callers VALVE_SYMBOL` → PUMP_ASSEMBLY, CONTROL_VALVE_DETAIL, ...
- `cqs impact PIPE_FLANGE` → every block that uses this detail
- `cqs dead` → block definitions never referenced by any INSERT

### Nested blocks

Blocks can contain INSERTs of other blocks (nested calls). The graph handles this naturally — BFS traversal through the insert graph gives transitive impact.

### Attributes as parameters

INSERT entities can carry ATTRIB values — like function parameters:
```
INSERT "INSTRUMENT_TAG"
  ATTRIB "TAG" = "FT-101"
  ATTRIB "SERVICE" = "Flow Transmitter"
  ATTRIB "RANGE" = "0-100 GPM"
```

Store attributes in chunk metadata. Include in NL: "Instrument tag FT-101, Flow Transmitter, range 0-100 GPM on layer Instrumentation."

## Phase 3: Connectivity Graph

Physical connections between entities based on shared endpoints.

### Endpoint extraction

For each LINE, POLYLINE, LWPOLYLINE, ARC:
- Extract start point and end point
- Round to precision tolerance (e.g., 0.001 units) to handle floating-point near-misses
- Build adjacency: point → set of entities touching that point

### Connection edges

Two entities sharing an endpoint = physical connection:
```sql
-- New table (or reuse function_calls with edge_type='connection')
INSERT INTO connections (entity_a, entity_b, point_x, point_y, layer)
VALUES ('LINE_42', 'LINE_43', 450.0, 230.0, 'Piping');
```

### Trace

`cqs trace pump_P101 tank_T201` = BFS through connectivity graph:
1. Find entities associated with pump_P101 (INSERT location + nearby entities)
2. BFS through connection edges
3. Find path to entities associated with tank_T201
4. Return path with every entity, connection point, and intervening components

### Layer-aware connections

Connections should respect layer boundaries:
- Piping layer entities connect to piping layer entities
- Electrical layer entities connect to electrical layer entities
- Cross-layer connections (e.g., instrument tapping a pipe) flagged specially

## Phase 4: Spatial Intelligence (future)

### Bounding box index
- Compute bounding box for each block INSERT instance
- R-tree spatial index for fast proximity queries
- "Everything within 5m of HX-201"

### Room/zone detection
- Closed polylines on architectural layers = room boundaries
- Point-in-polygon for entity containment
- `cqs gather "Boiler Room"` → all equipment in that zone

### Spatial NL enrichment
- "Located in the northwest corner of the mechanical room"
- "Adjacent to the main header"
- "On the second floor mezzanine"

## Phase 5: Visual/Multimodal (future)

### Render and describe
- Render each block to SVG/PNG (using `dxf_to_svg` crate or custom renderer)
- Pass through vision model (Claude) for natural language description
- "Butterfly valve with flanged connections and handwheel operator"
- Embed the description for visual similarity search

### Drawing diff
- Load two DXF revisions
- Compare entity-by-entity (position, attributes, layer)
- `cqs diff rev1.dxf rev2.dxf` → spatial change map
- Highlight added/removed/modified entities

## Dependencies

```toml
# Phase 1
dxf = "0.6.1"  # DXF/DXB reading and writing

# Phase 3 (optional, for spatial index)
# rstar = "0.12"  # R-tree spatial index

# DWG support (optional)
# Shell out to dwg2dxf (LibreDWG) — no Rust dependency needed
```

## File structure

```
src/language/dxf.rs          — LanguageDef + DXF-specific chunk/call extraction
src/parser/dxf_parser.rs     — Custom parser (no tree-sitter) using dxf crate
src/connectivity.rs          — Phase 3: endpoint matching, connection graph
```

## Open questions

1. **Coordinate precision**: What tolerance for endpoint matching? DXF files from different CAD tools have different floating-point precision.
2. **Block nesting depth**: Should we limit INSERT graph traversal depth? Real P&IDs can have 5-6 levels of nested blocks.
3. **Multi-file drawings**: Engineering projects span hundreds of DXF files. Reference/project infrastructure handles this, but the connectivity graph would need cross-file connections via XREF.
4. **Units**: DXF files can use any units (inches, mm, feet). Spatial queries need unit awareness.
5. **3D**: Some DXF files are 3D. Phase 1-3 work in 2D projection. Phase 4 spatial queries would need Z-axis handling.

## Success criteria

- `cqs "pressure relief valve" --json` finds PRV blocks in a P&ID
- `cqs callers PUMP_DETAIL` lists every drawing using that pump symbol
- `cqs impact VALVE_SYMBOL` shows all affected locations
- `cqs dead` finds unused block definitions
- `cqs trace P101 T201` finds the pipe route (Phase 3)

## Agent Prompts

Use these when dispatching agents to implement each phase. Each prompt is self-contained — the agent doesn't need to read this full spec.

### Phase 1 Agent Prompt

```
Implement DXF file support for cqs at /mnt/c/Projects/cqs.

cqs is a semantic code search tool that parses source files into chunks,
embeds them, and provides search + call graph navigation. You're adding
DXF (CAD drawing) support as a new language module.

EXISTING PATTERNS TO FOLLOW:
- Read src/language/bash.rs for the simplest language module example
- Read src/language/mod.rs for LanguageDef struct and registration
- Read src/parser/mod.rs for how parse_file/parse_file_all work
- Read CONTRIBUTING.md "Adding a New Language" section

DXF IS NOT A PROGRAMMING LANGUAGE. It's a CAD drawing format.
- No tree-sitter grammar. Set grammar: None (like Markdown).
- Use the `dxf` crate (0.6.1) for parsing. Add to Cargo.toml.
- Custom parser logic goes in src/parser/dxf_parser.rs

WHAT TO CHUNK:
- Block definitions (BLOCK section) → ChunkType::Block
  name = block name, content = serialized entity list
- TEXT/MTEXT entities → ChunkType::Section
  name = first 60 chars of text, content = full text + position + layer
- DIMENSION entities → ChunkType::Property
  name = measurement value, content = dimension text + position
- Layer definitions → ChunkType::Module
  name = layer name, content = layer properties (color, line type)

CALL GRAPH (INSERT references):
- Each INSERT entity = a call edge: parent context → block name
- Return these as CallSite entries from the parser
- Nested blocks (block containing INSERT) = nested calls
- This enables cqs callers/callees/impact for block references

NL GENERATION:
- Block: "Block {name} with {n} entities on layer {layer}. Attributes: {attrs}. Contains: {text_entities}."
- Text: "Text '{content}' on layer {layer} at ({x}, {y})."
- Dimension: "Dimension {value} on layer {layer}."

FILE EXTENSIONS: .dxf
Also register .dxb (binary DXF) if the dxf crate supports it.

DWG: Not in this phase. Add a note in the code that DWG needs
external conversion (dwg2dxf from LibreDWG).

TEST: Create a minimal test DXF file in tests/fixtures/ with:
- 2 block definitions (one referencing the other via INSERT)
- 3 text entities on different layers
- 5 INSERT references
Verify: parse produces correct chunks, call graph has INSERT edges,
search finds blocks by description.

BUILD: cargo build --features gpu-index
TEST: cargo test --features gpu-index dxf
```

### Phase 2 Agent Prompt

```
Enhance DXF support in cqs at /mnt/c/Projects/cqs with INSERT graph
features (block reference tracking).

PREREQUISITE: Phase 1 is complete. DXF parsing produces chunks and
basic call edges from INSERT entities.

READ FIRST:
- src/language/dxf.rs (current DXF language module)
- src/parser/dxf_parser.rs (current DXF parser)
- src/store/calls/ (how call graph edges are stored and queried)

WHAT TO ADD:

1. ATTRIBUTE EXTRACTION from INSERT entities:
   - INSERT entities can carry ATTRIB values (key=value pairs)
   - Store as chunk metadata on the INSERT chunk
   - Include in NL: "Instrument FT-101, Flow Transmitter, 0-100 GPM"
   - Attributes make INSERTs searchable by their instance data

2. NESTED BLOCK RESOLUTION:
   - Blocks can contain INSERTs of other blocks (nesting)
   - Verify the call graph captures transitive relationships
   - cqs impact on a deeply nested block should show all ancestors

3. BLOCK USAGE STATISTICS in NL:
   - Count how many times each block is INSERTed
   - Add to block NL: "Used 47 times across 12 layers"
   - Unused blocks should appear in cqs dead

4. XREF SUPPORT (external references):
   - DXF files can reference other DXF files via XREF
   - Map to cqs's reference/project infrastructure
   - cqs ref add should work with XREF paths

TEST with a real-world-like P&ID fixture:
- 10+ block definitions with 2-3 levels of nesting
- Attributes on instrument tags (TAG, SERVICE, RANGE)
- At least one XREF to another DXF
Verify: cqs callers, cqs impact, cqs dead all work correctly.
```

### Phase 3 Agent Prompt

```
Add physical connectivity graph to DXF support in cqs at /mnt/c/Projects/cqs.

This is the novel feature — trace physical connections through piping,
electrical, or structural elements in engineering drawings.

PREREQUISITE: Phase 1+2 complete. DXF parsing and INSERT graph working.

READ FIRST:
- src/parser/dxf_parser.rs (DXF parser)
- src/store/calls/ (call graph storage — reuse or extend for connections)
- src/impact/bfs.rs (BFS traversal — same algorithm applies)

CONCEPT: Two entities sharing an endpoint = physical connection.
LINE (0,0)→(10,0) and LINE (10,0)→(10,10) share point (10,0) = connected.

WHAT TO BUILD:

1. ENDPOINT EXTRACTION (src/connectivity.rs):
   - For LINE: start_point, end_point
   - For POLYLINE/LWPOLYLINE: all vertex points
   - For ARC: start_angle_point, end_angle_point
   - For INSERT: insertion point (connects to whatever touches it)
   - Round coordinates to configurable precision (default: 0.001 units)

2. CONNECTION GRAPH:
   - Build adjacency: point → set of entity IDs touching that point
   - Two entities sharing a point = connection edge
   - Store in function_calls table with edge_type='connection'
     OR create a new connections table
   - Layer-aware: only connect entities on same layer (configurable)

3. TRACE COMMAND:
   - cqs trace entity_A entity_B = BFS through connection graph
   - Returns path: [entity_A, connection_point_1, entity_B, ...]
   - Include intervening components (valves, tees, reducers)
   - Output format matches existing trace command JSON

4. PRECISION TOLERANCE:
   - Endpoint matching needs configurable tolerance
   - Different CAD tools export with different floating-point precision
   - Default 0.001, configurable via .cqs.toml [dxf] section

TEST with a piping fixture:
- Pipe route: pump → valve → tee → two branches → tanks
- Endpoints match at connection points
- cqs trace pump tank_A returns correct path through valve and tee
- cqs trace pump tank_B returns the other branch
```

### Phase 4 Agent Prompt (future — spatial)

```
Add spatial intelligence to DXF support in cqs. This extends
search and gather with physical proximity awareness.

READ: docs/superpowers/specs/2026-03-27-dxf-cad-module-design.md Phase 4 section.

Key features:
- R-tree bounding box index for fast proximity queries
- Room/zone detection from closed polylines
- Spatial NL enrichment ("located near the main header")
- cqs gather with spatial radius parameter

Use rstar crate for R-tree. Store spatial index alongside HNSW.
```

### Phase 5 Agent Prompt (future — visual)

```
Add visual/multimodal capabilities to DXF support in cqs.

READ: docs/superpowers/specs/2026-03-27-dxf-cad-module-design.md Phase 5 section.

Key features:
- Render blocks to SVG using dxf_to_svg crate
- Pass rendered images through vision model for descriptions
- Embed descriptions for visual similarity search
- Drawing diff: spatial change detection between revisions

This requires Claude vision API access for block description generation.
Use the existing LLM batch infrastructure (BatchProvider trait).
```
