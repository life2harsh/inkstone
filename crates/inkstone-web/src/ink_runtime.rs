use std::collections::HashMap;

use inkstone_core::document::InkCollaborationState;
use inkstone_core::ids::{BlockId, DocId};
use stonepen_wasm::{StonepenHandle, mount_stonepen};
use uuid::Uuid;
use wasm_bindgen::JsValue;

pub struct InkUpdate {
    pub doc_id: DocId,
    pub block_id: BlockId,
    pub stream_epoch: Uuid,
    pub update: Vec<u8>,
    pub bootstrap_snapshot: Option<Vec<u8>>,
}

pub struct InkPreview {
    pub doc_id: DocId,
    pub block_id: BlockId,
    pub stream_epoch: Uuid,
    pub preview: Vec<u8>,
}

struct MountedInk {
    doc_id: DocId,
    block_id: BlockId,
    stream_epoch: Uuid,
    handle: StonepenHandle,
}

#[derive(Default)]
pub struct InkRuntimeRegistry {
    handles: HashMap<BlockId, MountedInk>,
}

impl InkRuntimeRegistry {
    #[allow(clippy::too_many_arguments)]
    pub fn mount(
        &mut self,
        doc_id: DocId,
        block_id: BlockId,
        canvas_id: &str,
        actor: [u8; 16],
        stored_json: &str,
        collaboration: Option<&InkCollaborationState>,
        tool: &str,
        brush_width: f32,
        brush_color: (u8, u8, u8),
        read_only: bool,
    ) -> Result<Option<InkUpdate>, JsValue> {
        if let Some(mut mounted) = self.handles.remove(&block_id) {
            mounted.handle.destroy();
        }

        let mut handle = mount_stonepen(canvas_id)?;
        handle.set_collaboration_actor(&actor)?;
        let (stream_epoch, bootstrap_snapshot) = if let Some(state) = collaboration {
            handle.load_collaboration_snapshot(&state.snapshot)?;
            (state.stream_epoch, None)
        } else {
            if !stored_json.is_empty() {
                handle.load_json(stored_json)?;
            }
            let snapshot = (!read_only)
                .then(|| handle.collaboration_snapshot())
                .transpose()?;
            (Uuid::new_v4(), snapshot)
        };
        handle.set_read_only(read_only);
        if !read_only {
            handle.set_tool(tool);
            handle.set_brush_width(brush_width);
            handle.set_brush_color(brush_color.0, brush_color.1, brush_color.2);
        }
        handle.resize();

        self.handles.insert(
            block_id,
            MountedInk {
                doc_id,
                block_id,
                stream_epoch,
                handle,
            },
        );

        Ok(bootstrap_snapshot.map(|snapshot| InkUpdate {
            doc_id,
            block_id,
            stream_epoch,
            update: Vec::new(),
            bootstrap_snapshot: Some(snapshot),
        }))
    }

    pub fn take_local_update(&mut self, block_id: BlockId) -> Result<Option<InkUpdate>, JsValue> {
        let Some(mounted) = self.handles.get_mut(&block_id) else {
            return Ok(None);
        };
        let update = mounted.handle.take_local_update()?;
        if update.is_empty() {
            return Ok(None);
        }
        Ok(Some(InkUpdate {
            doc_id: mounted.doc_id,
            block_id: mounted.block_id,
            stream_epoch: mounted.stream_epoch,
            update,
            bootstrap_snapshot: None,
        }))
    }

    pub fn take_all_local_updates(&mut self) -> Result<Vec<InkUpdate>, JsValue> {
        let block_ids = self.handles.keys().copied().collect::<Vec<_>>();
        let mut updates = Vec::new();
        for block_id in block_ids {
            if let Some(update) = self.take_local_update(block_id)? {
                updates.push(update);
            }
        }
        Ok(updates)
    }

    pub fn take_local_preview(&mut self, block_id: BlockId) -> Result<Option<InkPreview>, JsValue> {
        let Some(mounted) = self.handles.get_mut(&block_id) else {
            return Ok(None);
        };
        let preview = mounted.handle.take_local_preview()?;
        if preview.is_empty() {
            return Ok(None);
        }
        Ok(Some(InkPreview {
            doc_id: mounted.doc_id,
            block_id,
            stream_epoch: mounted.stream_epoch,
            preview,
        }))
    }

    pub fn take_all_local_previews(&mut self) -> Result<Vec<InkPreview>, JsValue> {
        let block_ids = self.handles.keys().copied().collect::<Vec<_>>();
        let mut previews = Vec::new();
        for block_id in block_ids {
            if let Some(preview) = self.take_local_preview(block_id)? {
                previews.push(preview);
            }
        }
        Ok(previews)
    }

    pub fn apply_remote_preview(
        &mut self,
        participant: Uuid,
        payload: &inkstone_core::InkPresencePayload,
        now_ms: f64,
    ) -> Result<bool, JsValue> {
        let block_id = BlockId(payload.block_id);
        let Some(mounted) = self.handles.get_mut(&block_id) else {
            return Ok(false);
        };
        if mounted.stream_epoch != payload.stream_epoch {
            return Ok(false);
        }
        mounted.handle.apply_remote_preview(
            participant.as_bytes(),
            payload.stream_epoch.as_bytes(),
            &payload.stonepen_preview,
            now_ms,
        )
    }

    pub fn remove_remote_participant(&mut self, participant: Uuid) {
        for mounted in self.handles.values_mut() {
            mounted
                .handle
                .remove_remote_preview(participant.as_bytes(), mounted.stream_epoch.as_bytes());
        }
    }

    pub fn expire_remote_previews(&mut self, now_ms: f64, ttl_ms: f64) {
        for mounted in self.handles.values_mut() {
            mounted.handle.expire_remote_previews(now_ms, ttl_ms);
        }
    }

    pub fn clear_remote_previews(&mut self) {
        for mounted in self.handles.values_mut() {
            mounted.handle.clear_remote_previews();
        }
    }

    pub fn destroy_all(&mut self) -> Result<Vec<InkUpdate>, JsValue> {
        let mut updates = Vec::new();
        for (_, mut mounted) in self.handles.drain() {
            let update = mounted.handle.take_local_update()?;
            if !update.is_empty() {
                updates.push(InkUpdate {
                    doc_id: mounted.doc_id,
                    block_id: mounted.block_id,
                    stream_epoch: mounted.stream_epoch,
                    update,
                    bootstrap_snapshot: None,
                });
            }
            mounted.handle.destroy();
        }
        Ok(updates)
    }

    pub fn set_tool(&mut self, block_id: BlockId, tool: &str) {
        if let Some(mounted) = self.handles.get_mut(&block_id) {
            mounted.handle.set_tool(tool);
        }
    }

    pub fn set_brush_width(&mut self, block_id: BlockId, width: f32) {
        if let Some(mounted) = self.handles.get_mut(&block_id) {
            mounted.handle.set_brush_width(width);
        }
    }

    pub fn set_brush_color(&mut self, block_id: BlockId, color: (u8, u8, u8)) {
        if let Some(mounted) = self.handles.get_mut(&block_id) {
            mounted.handle.set_brush_color(color.0, color.1, color.2);
        }
    }

    pub fn resize(&mut self, block_id: BlockId) {
        if let Some(mounted) = self.handles.get_mut(&block_id) {
            mounted.handle.resize();
        }
    }

    pub fn undo(&mut self, block_id: BlockId) -> Result<Option<InkUpdate>, JsValue> {
        if let Some(mounted) = self.handles.get_mut(&block_id) {
            mounted.handle.undo();
        }
        self.take_local_update(block_id)
    }

    pub fn redo(&mut self, block_id: BlockId) -> Result<Option<InkUpdate>, JsValue> {
        if let Some(mounted) = self.handles.get_mut(&block_id) {
            mounted.handle.redo();
        }
        self.take_local_update(block_id)
    }
}

impl Drop for InkRuntimeRegistry {
    fn drop(&mut self) {
        for (_, mut mounted) in self.handles.drain() {
            mounted.handle.destroy();
        }
    }
}
