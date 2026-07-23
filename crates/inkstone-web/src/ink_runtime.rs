use std::collections::HashMap;

use inkstone_core::ids::{BlockId, DocId};
use stonepen_wasm::{StonepenHandle, mount_stonepen};
use wasm_bindgen::JsValue;

pub struct InkSnapshot {
    pub doc_id: DocId,
    pub block_id: BlockId,
    pub stonepen_json: String,
}

struct MountedInk {
    doc_id: DocId,
    block_id: BlockId,
    handle: StonepenHandle,
    last_exported_json: String,
}

#[derive(Default)]
pub struct InkRuntimeRegistry {
    handles: HashMap<BlockId, MountedInk>,
}

impl InkRuntimeRegistry {
    pub fn mount(
        &mut self,
        doc_id: DocId,
        block_id: BlockId,
        canvas_id: &str,
        stored_json: &str,
        tool: &str,
        brush_width: f32,
        brush_color: (u8, u8, u8),
        read_only: bool,
    ) -> Result<Option<InkSnapshot>, JsValue> {
        if let Some(mut mounted) = self.handles.remove(&block_id) {
            mounted.handle.destroy();
        }

        let mut handle = mount_stonepen(canvas_id)?;
        if !stored_json.is_empty() {
            handle.load_json(stored_json)?;
        }
        handle.set_read_only(read_only);
        if !read_only {
            handle.set_tool(tool);
            handle.set_brush_width(brush_width);
            handle.set_brush_color(brush_color.0, brush_color.1, brush_color.2);
        }
        handle.resize();

        let exported = handle.export_json()?;
        let initial_snapshot = (!read_only && stored_json.is_empty()).then(|| InkSnapshot {
            doc_id,
            block_id,
            stonepen_json: exported.clone(),
        });
        let last_exported_json = if stored_json.is_empty() {
            exported
        } else {
            stored_json.to_string()
        };

        self.handles.insert(
            block_id,
            MountedInk {
                doc_id,
                block_id,
                handle,
                last_exported_json,
            },
        );

        Ok(initial_snapshot)
    }

    pub fn export_changed(&mut self, block_id: BlockId) -> Result<Option<InkSnapshot>, JsValue> {
        let Some(mounted) = self.handles.get_mut(&block_id) else {
            return Ok(None);
        };
        if !mounted.handle.is_dirty() {
            return Ok(None);
        }

        let stonepen_json = mounted.handle.export_json()?;
        if stonepen_json == mounted.last_exported_json {
            return Ok(None);
        }
        mounted.last_exported_json = stonepen_json.clone();
        Ok(Some(InkSnapshot {
            doc_id: mounted.doc_id,
            block_id: mounted.block_id,
            stonepen_json,
        }))
    }

    pub fn export_all_changed(&mut self) -> Result<Vec<InkSnapshot>, JsValue> {
        let block_ids = self.handles.keys().copied().collect::<Vec<_>>();
        let mut snapshots = Vec::new();
        for block_id in block_ids {
            if let Some(snapshot) = self.export_changed(block_id)? {
                snapshots.push(snapshot);
            }
        }
        Ok(snapshots)
    }

    pub fn destroy_all(&mut self) -> Result<Vec<InkSnapshot>, JsValue> {
        let mut snapshots = Vec::new();
        for (_, mut mounted) in self.handles.drain() {
            if mounted.handle.is_dirty() {
                let stonepen_json = mounted.handle.export_json()?;
                if stonepen_json != mounted.last_exported_json {
                    snapshots.push(InkSnapshot {
                        doc_id: mounted.doc_id,
                        block_id: mounted.block_id,
                        stonepen_json,
                    });
                }
            }
            mounted.handle.destroy();
        }
        Ok(snapshots)
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

    pub fn undo(&mut self, block_id: BlockId) -> Result<Option<InkSnapshot>, JsValue> {
        if let Some(mounted) = self.handles.get_mut(&block_id) {
            mounted.handle.undo();
        }
        self.export_changed(block_id)
    }

    pub fn redo(&mut self, block_id: BlockId) -> Result<Option<InkSnapshot>, JsValue> {
        if let Some(mounted) = self.handles.get_mut(&block_id) {
            mounted.handle.redo();
        }
        self.export_changed(block_id)
    }
}

impl Drop for InkRuntimeRegistry {
    fn drop(&mut self) {
        for (_, mut mounted) in self.handles.drain() {
            mounted.handle.destroy();
        }
    }
}
