//! Page ink layer helpers: migrating legacy Ink blocks into the layer.

use crate::doc_ops::{DocumentOp, InkBlockConversion};
use crate::document::{BlockContent, DEFAULT_SPACE_HEIGHT, InkstoneDocument, PAGE_WIDTH};
use crate::ids::InkGroupId;

/// Vertical room left below migrated ink so it does not touch the next block.
const MIGRATED_SPACE_PADDING: f32 = 24.0;

/// Builds the op that folds every legacy `BlockContent::Ink` block into a new
/// page ink layer, one group per block, each anchored to the `Space` block
/// that replaces it. Returns `None` when there is nothing to migrate or the
/// document already has a page layer.
pub fn plan_ink_block_migration(doc: &InkstoneDocument) -> Result<Option<DocumentOp>, String> {
    if doc.ink.is_some() {
        return Ok(None);
    }
    let mut parts = Vec::new();
    let mut targets = Vec::new();
    for block in &doc.blocks {
        let BlockContent::Ink {
            stonepen_json,
            collaboration,
        } = &block.content
        else {
            continue;
        };
        let snapshot = match collaboration {
            Some(state) => state.snapshot.bytes()?.to_vec(),
            None if stonepen_json.trim().is_empty() => {
                stonepen_core::new_opaque_document(PAGE_WIDTH, PAGE_WIDTH)
                    .map_err(|error| error.to_string())?
                    .0
            }
            None => stonepen_core::snapshot_from_legacy_json(stonepen_json)
                .map_err(|error| error.to_string())?,
        };
        let group = InkGroupId::new_v4();
        parts.push((group.0, snapshot));
        targets.push((block.id, group));
    }
    if parts.is_empty() {
        return Ok(None);
    }

    let refs = parts
        .iter()
        .map(|(frame, snapshot)| (*frame, snapshot.as_slice()))
        .collect::<Vec<_>>();
    let (snapshot, _) = stonepen_core::compose_framed_document(PAGE_WIDTH, PAGE_WIDTH, &refs)
        .map_err(|error| error.to_string())?;
    let composed = stonepen_core::CollaborationDoc::from_snapshot(
        stonepen_core::ActorId::from_bytes([0; 16]),
        &snapshot,
    )
    .map_err(|error| error.to_string())?;
    let bounds = composed.document().frame_bounds();

    let conversions = targets
        .into_iter()
        .map(|(block_id, group)| {
            let bottom = bounds
                .iter()
                .find(|(frame, _)| frame.0 == group.0)
                .map(|(_, bbox)| bbox.max_y + MIGRATED_SPACE_PADDING)
                .unwrap_or(0.0);
            InkBlockConversion {
                block_id,
                group,
                min_height: bottom.max(DEFAULT_SPACE_HEIGHT),
            }
        })
        .collect();

    Ok(Some(DocumentOp::MigrateInkBlocks {
        stream_epoch: uuid::Uuid::new_v4(),
        snapshot,
        conversions,
    }))
}
