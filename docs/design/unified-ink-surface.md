# Unified Ink Surface (design draft)

Status: proposal, not implemented.
Replaces: `BlockContent::Ink` as a container for handwriting.

## Problem

Today a document is `Vec<DocumentBlock>` and ink can only exist inside a
`BlockContent::Ink` rectangle (`crates/inkstone-core/src/document.rs`). The web
editor reinforces this: Edit mode is a list of boxed blocks, each with a type
header ("MARKDOWN", "INK"), Up/Down/Delete buttons and a raw `<textarea>`;
Preview mode is a separate rendering. You cannot circle a word, annotate a
paragraph in the margin, draw an arrow from a sentence to a sketch, or write
between two lines without first creating an ink box. That is the Obsidian +
Excalidraw split, moved into one file.

## Goal

One page. Text is the reflowable spine; ink is a single layer over the whole
page that can go anywhere, and stays attached to the text it was drawn next to
when that text moves.

Non-goals: handwriting recognition/OCR (Stonepen explicitly does not do it),
free-form board documents (separate doc type, later), changing the server
(still opaque encrypted ops).

## Core idea

```
 ┌──────────────── page column (fixed design width) ───────────────┐
 │  # Heading                                          ┌─margin─┐  │
 │  Some paragraph with an (important) word ←── ink    │ note ✎ │  │
 │       ╰── circle anchored to "important" ──╯        └────────┘  │
 │  ┌ Space block: height derived from ink inside it ┐             │
 │  │   ✎ sketch anchored to this Space              │             │
 │  └────────────────────────────────────────────────┘             │
 │  Next paragraph …                                               │
 └─────────────────────────────────────────────────────────────────┘
       ink layer = ONE Stonepen document for the whole page,
       strokes grouped; each group has one anchor.
```

- **Ink group**: a set of strokes that move together. Created automatically
  from strokes drawn close together in time and space; editable via lasso.
- **Anchor**: where a group is pinned: a stable text position (yrs
  `StickyIndex`, already used via `RelativePosition` in `text_crdt.rs`) or a
  block, plus an offset in page units.
- **Space block**: replaces the Ink block. It holds no strokes; it only
  reserves vertical room. Its height is *derived* from the groups anchored to
  it, so two people writing in the same space converge without a height
  conflict.
- **Layout**: the web layer resolves every anchor to page coordinates and
  hands Stonepen a per-group view offset. Reflow never mutates stroke data,
  so it never generates collaboration ops and is identical on every replica.

## Decisions

1. **Fixed design width for the page column** (e.g. 760 CSS px). Narrow
   viewports scale the whole column instead of reflowing it, the way
   GoodNotes/OneNote do. Only *text edits* move ink; window resizes never do.
   This removes the hardest class of bugs (ink drifting off its word when the
   window changes).
2. **One editing surface.** Drop the Edit/Preview split and the boxed-block
   chrome. Blocks render as formatted Markdown; the block holding the caret
   shows source (Typora/Obsidian live preview). Ink is always visible and
   always editable.
3. **Rendered layout is the coordinate frame for ink.** While a block is in
   source mode, groups anchored inside it follow via the source map
   (best-effort) and settle on blur.
4. **Anchors are canonical state; positions are derived.** Anchors live in
   the document and sync as ops. Pixel positions are recomputed from layout
   and never stored, in line with the "derived state is reproducible" rule.

## Data model (inkstone-core)

```rust
pub struct InkstoneDocument {
    pub id: DocId,
    pub schema_version: u32,          // bump to 2
    pub title: String,
    pub blocks: Vec<DocumentBlock>,
    pub metadata: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ink: Option<InkLayer>,        // one per document
}

pub struct InkLayer {
    /// Reuses the existing struct: version, stream_epoch, opaque snapshot.
    pub collaboration: InkCollaborationState,
    /// Stonepen-owned projection, still opaque to Inkstone.
    pub stonepen_json: String,
    pub anchors: BTreeMap<InkGroupId, InkAnchor>,
}

pub struct InkAnchor {
    pub target: AnchorTarget,
    /// Page units (CSS px at 100% zoom) from the resolved anchor point.
    pub offset: [f32; 2],
    pub lane: InkLane,
}

pub enum AnchorTarget {
    /// A character position inside a Markdown block. Survives concurrent
    /// edits; resolves to the nearest surviving character if deleted.
    Text(RelativePosition),
    /// Top-left of a block. Used for Space, Math, Divider, and as the
    /// fallback when a text anchor's block is deleted.
    Block(BlockId),
}

pub enum InkLane {
    /// Over the text column (circles, underlines, arrows, gap sketches).
    Overlay,
    /// Outside the column; x is measured from the column edge.
    Margin(Side),
}

pub enum BlockContent {
    Markdown { text: String },
    Math { source: String },
    Divider,
    /// Reserved room for ink. Height = max(min_height, extent of groups
    /// anchored here + padding). Derived, never stored.
    Space { min_height: f32 },
    // `Ink { .. }` stays deserializable for migration only.
}
```

`InkGroupId` is a typed UUID in `ids.rs`, the same as the other ids.

## Operations

New `DocumentOp` variants (v2 envelope):

| Op | Meaning | Sequenced replay |
|---|---|---|
| `ApplyInkLayerUpdate { stream_epoch, update, bootstrap_snapshot }` | Stonepen opaque update for the page layer (same shape as `CollaborativeInkUpdate` minus `block_id`) | Same rules as today's ink updates; stale epoch → `NoOp` |
| `SetInkAnchor { group, anchor }` | Create or move a group's anchor | Last server-sequenced write wins |
| `RemoveInkAnchor { group }` | Drop an anchor (group deleted) | Missing → `NoOp` |
| `MigrateInkBlocks { snapshot, anchors, conversions }` | One-shot v1→v2 conversion (see Migration) | First wins; later ones → `NoOp` |

**Block deletion rule.** This is deterministic, so it lives in core and runs
in both `apply_update` and `sequenced_replay`. When `DeleteBlock` removes block
B, every anchor whose target is in B is rewritten to `Block(successor of B)`
(or the predecessor if B was last), keeping `offset.x` and setting
`offset.y = 0`. The UI asks "delete the ink attached here too?" before a local
delete. This rule only covers the concurrent case.

**Text deletion** needs no rule: a `StickyIndex` on a deleted character
already resolves to its neighbour.

**Block moves** need no rule either, because anchors reference block content,
not indices.

## Stroke grouping (inkstone-client, platform-independent)

When the pen goes down at page point P:

1. If P is within `GROUP_JOIN_PX` (≈ 24px) of a group's bbox, and that group
   received a stroke within `GROUP_JOIN_MS` (≈ 1500 ms) or is currently
   selected, add the stroke to that group.
2. Otherwise, create a new group. Choose its anchor from what is under P:
   - over a text glyph → `Text(position of that glyph)`, lane `Overlay`
   - over a Space/Math/Divider block → `Block(id)`, `Overlay`
   - outside the column → nearest block's text position on that line,
     `Margin(side)`
   - in the gap between blocks → insert a `Space` block there and anchor to it
3. The offset is P minus the resolved anchor point.

Lasso + "group" / "ungroup" / "re-anchor here" give manual control. The
geometry inputs (group bboxes, glyph hit results) are passed in, so this logic
has no DOM dependency and is unit-testable in `inkstone-client`.

## Layout and anchor resolution (inkstone-web)

On each layout pass:

1. Render blocks. Space heights come from the previous pass's group extents
   and converge in one extra pass.
2. For every group near the viewport, resolve its anchor:
   `RelativePosition → UTF-16 offset (yrs) → DOM point → client rect →
   page coords`.
   The rendered Markdown needs a source map. comrak's `sourcepos` option gives
   `data-sourcepos` per node; within a text node, offsets map by character
   count (`Range.getClientRects`).
3. Compute each group's view offset as page coords + `anchor.offset`, then
   send them to Stonepen in one call.
4. Re-run when text changes, a block is inserted or removed, fonts load, or
   the column scale changes. Cache results per block (keyed by a block content
   hash) so typing in one paragraph re-resolves only the groups in and below
   it.

Input routing on the page container:
- `pointerType == "pen"` → ink, always (palm rejection comes for free)
- `"touch"` → scroll or select text
- `"mouse"` → text, unless an ink tool is toggled on

## What Stonepen needs (requires explicit approval)

The current API assumes one handle that owns one canvas and its own pointer
listeners. A whole-page layer with reflow needs the following. Every item is
geometry or view state only, so Inkstone still never reads stroke internals.

1. **Groups on items**: strokes carry `group: Option<GroupId>`, set at
   creation, plus a `RegroupItems` collaboration op.
2. **Host-supplied per-group view offsets**: `set_group_view_offsets(&[(GroupId, dx, dy)])`.
   These are applied at render and hit-test time and are never persisted or
   synced.
3. **Host-driven camera**: the canvas covers the viewport, and the host sets
   scroll and scale (`set_camera`). Stonepen's own pan/zoom gestures are off in
   this mode.
4. **Host-driven input**: `pointer_down/move/up(...)` forwarded by Inkstone,
   plus `set_active_group(GroupId)` before a stroke starts.
5. **Group geometry queries**: `group_bboxes() -> Vec<(GroupId, BBox)>`.
6. **Group export**: `export_group_svg(snapshot, group)` in `stonepen-core`.
7. **Merge for migration**: import a legacy snapshot into a layer under a
   given group, without needing to read its strokes.

Fallback if Stonepen must stay untouched: one Stonepen document and canvas per
group, positioned by Inkstone. This works for gap sketches, but it can't start
a stroke in empty space without a pre-mounted canvas, can't lasso across
groups, and costs one canvas per group. I don't recommend it beyond a
prototype.

## Migration (schema v1 → v2)

For each `BlockContent::Ink` block:
- Replace it in place with `Space { min_height: <old canvas height> }`, using
  the same `BlockId`.
- Merge its Stonepen snapshot into the page layer under a new group G.
- `SetInkAnchor(G, Block(same id), offset [0,0], Overlay)`.

Nothing moves visually. This is emitted as a single `MigrateInkBlocks` op so
concurrent clients can't double-migrate (first sequenced op wins). v1 clients
fail with `UnsupportedDocumentSchemaVersion`, so this must ship with a
minimum-client-version gate.

## Export (derived, reproducible)

- Markdown text exports unchanged.
- `Space` → `![](<doc>.ink/<group>.svg)` for each group anchored to it.
- Overlay and margin groups have no faithful Markdown form. They are exported
  in a sidecar `.inkstone/ink/<doc-id>.json` (anchors + Stonepen snapshot) so
  re-import is lossless, plus an optional full-page SVG/PDF render.
- This defines the Stonepen sidecar deferred by `workspace-format-v1.md`
  rule 9, as a format v2 proposal.
- Graph, tag and wikilink indexes are unaffected (they derive from text).

## Phases

1. **Foundation**: Stonepen API (items 1–5), core types + ops + migration,
   tests in `inkstone-core` for anchor rewrite and replay determinism.
2. **Unified surface, block-level anchors**: single live-preview editor,
   fixed column, page ink layer, Space blocks, `Block` anchors only. This is
   already the big UX win: ink anywhere, across blocks, following block moves.
3. **Text-precise anchors**: source map, `Text` anchors, margin lane,
   auto-grouping.
4. **Inline ink and polish**: handwriting inline in a sentence
   (`![](ink:<group>)` token rendered as an inline box), unified text+ink undo,
   export sidecar.
5. **Later**: board document type (free 2D surface with Markdown cards).

## Open questions

- Approve the Stonepen API additions above?
- OK to replace the Edit/Preview split and block chrome? This contradicts the
  "no UI redesign" completion criterion in SKILLS.md, so it needs a deliberate
  exception.
- Fixed column width: what value? Should it be per document?
- When anchored text is deleted locally: move the ink to the neighbour
  (proposed) or delete it with the text?
