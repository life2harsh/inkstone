//! Reordering blocks: drag a block by its grip, or move the block being
//! typed in with Alt+Shift+↑/↓. Ink anchored to a block's text moves with
//! the block.

use inkstone_client::ClientEvent;
use inkstone_core::doc_ops::DocumentOp;
use inkstone_core::ids::{BlockId, UpdateId};
use wasm_bindgen::JsCast;

use crate::Driver;

/// Pointer travel before a press on the grip becomes a drag.
const DRAG_SLOP: f64 = 4.0;

#[derive(Clone)]
pub(crate) struct BlockDrag {
    pointer_id: i32,
    block_id: BlockId,
    start_y: f64,
    moved: bool,
    /// Index the block would be inserted before, among the page's blocks.
    gap: Option<usize>,
}

fn page_surface() -> Option<web_sys::Element> {
    web_sys::window()?
        .document()?
        .query_selector(".page-surface")
        .ok()
        .flatten()
}

/// The page's block elements with their ids, top to bottom.
fn block_elements(surface: &web_sys::Element) -> Vec<(BlockId, web_sys::Element)> {
    let Ok(list) = surface.query_selector_all(":scope > .page-block") else {
        return Vec::new();
    };
    (0..list.length())
        .filter_map(|index| list.item(index)?.dyn_into::<web_sys::Element>().ok())
        .filter_map(|element| {
            let id = element
                .get_attribute("data-page-block-id")
                .and_then(|value| uuid::Uuid::parse_str(&value).ok())
                .map(BlockId)?;
            Some((id, element))
        })
        .collect()
}

/// The new index for a block at `from` dropped into `gap` (an insertion
/// point between blocks), or `None` when it would not move.
pub(crate) fn drop_index(from: usize, gap: usize) -> Option<usize> {
    let to = if gap > from { gap - 1 } else { gap };
    (to != from).then_some(to)
}

impl Driver {
    /// A press on a block's grip. Returns whether it was taken.
    pub(crate) fn drag_pointer_down(&self, event: &web_sys::PointerEvent) -> bool {
        if event.button() != 0 || !self.page_editable() {
            return false;
        }
        let Some(grip) = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
            .and_then(|target| target.closest(".page-grip").ok().flatten())
        else {
            return false;
        };
        let Some(block_id) = grip
            .get_attribute("data-block-id")
            .and_then(|value| uuid::Uuid::parse_str(&value).ok())
            .map(BlockId)
        else {
            return false;
        };
        event.prevent_default();
        let _ = grip.set_pointer_capture(event.pointer_id());
        self.0.borrow_mut().block_drag = Some(BlockDrag {
            pointer_id: event.pointer_id(),
            block_id,
            start_y: f64::from(event.client_y()),
            moved: false,
            gap: None,
        });
        true
    }

    pub(crate) fn drag_pointer_move(&self, event: &web_sys::PointerEvent) -> bool {
        let Some(mut drag) = self.0.borrow().block_drag.clone() else {
            return false;
        };
        if drag.pointer_id != event.pointer_id() {
            return false;
        }
        event.prevent_default();
        let y = f64::from(event.client_y());
        if !drag.moved && (y - drag.start_y).abs() < DRAG_SLOP {
            return true;
        }
        let Some(surface) = page_surface() else {
            return true;
        };
        let blocks = block_elements(&surface);
        if !drag.moved {
            drag.moved = true;
            let _ = surface.class_list().add_1("block-dragging");
            if let Some((_, element)) = blocks.iter().find(|(id, _)| *id == drag.block_id) {
                let _ = element.class_list().add_1("drag-source");
            }
        }

        // The gap whose line is nearest the pointer.
        let gap = blocks
            .iter()
            .position(|(_, element)| {
                let rect = element.get_bounding_client_rect();
                y < rect.top() + rect.height() / 2.0
            })
            .unwrap_or(blocks.len());
        drag.gap = Some(gap);

        let surface_rect = surface.get_bounding_client_rect();
        let line_y = match blocks.get(gap) {
            Some((_, element)) if gap > 0 => {
                let above = blocks[gap - 1].1.get_bounding_client_rect().bottom();
                (above + element.get_bounding_client_rect().top()) / 2.0
            }
            Some((_, element)) => element.get_bounding_client_rect().top() - 2.0,
            None => blocks
                .last()
                .map(|(_, element)| element.get_bounding_client_rect().bottom() + 2.0)
                .unwrap_or(surface_rect.top()),
        };
        if let Some(document) = surface.owner_document() {
            let line = match surface.query_selector(".block-drop-line").ok().flatten() {
                Some(line) => line,
                None => {
                    let line = document.create_element("div").unwrap();
                    line.set_class_name("block-drop-line");
                    let _ = surface.append_child(&line);
                    line
                }
            };
            if let Ok(line) = line.dyn_into::<web_sys::HtmlElement>() {
                let _ = line
                    .style()
                    .set_property("top", &format!("{}px", line_y - surface_rect.top()));
            }
        }
        self.0.borrow_mut().block_drag = Some(drag);
        true
    }

    pub(crate) fn drag_pointer_end(&self, event: &web_sys::PointerEvent, cancelled: bool) -> bool {
        let Some(drag) = self.0.borrow().block_drag.clone() else {
            return false;
        };
        if drag.pointer_id != event.pointer_id() {
            return false;
        }
        self.0.borrow_mut().block_drag = None;
        if let Some(surface) = page_surface() {
            let _ = surface.class_list().remove_1("block-dragging");
            if let Ok(Some(line)) = surface.query_selector(".block-drop-line") {
                line.remove();
            }
            if let Ok(Some(source)) = surface.query_selector(".drag-source") {
                let _ = source.class_list().remove_1("drag-source");
            }
        }
        if cancelled || !drag.moved {
            return true;
        }
        let from = {
            let inner = self.0.borrow();
            Driver::effective_document(&inner)
                .blocks
                .iter()
                .position(|block| block.id == drag.block_id)
        };
        if let Some(to) = from
            .zip(drag.gap)
            .and_then(|(from, gap)| drop_index(from, gap))
        {
            self.move_block(drag.block_id, to);
        }
        true
    }

    fn move_block(&self, block_id: BlockId, new_idx: usize) {
        self.flush_ink_before_transition();
        crate::page_driver::blur_page_source();
        self.dispatch(ClientEvent::LocalIntent {
            op: DocumentOp::MoveBlock { block_id, new_idx },
            update_id: UpdateId::new_v4(),
            applied_at_ms: js_sys::Date::now() as i64,
        });
        self.render();
    }

    /// Alt+Shift+↑/↓ while typing: moves the block one place, keeping the
    /// caret where it was.
    pub(crate) fn move_editing_block(
        &self,
        textarea: &web_sys::HtmlTextAreaElement,
        block_id: BlockId,
        up: bool,
    ) -> bool {
        let (index, count) = {
            let inner = self.0.borrow();
            let doc = Driver::effective_document(&inner);
            let Some(index) = doc.blocks.iter().position(|block| block.id == block_id) else {
                return false;
            };
            (index, doc.blocks.len())
        };
        let to = if up {
            index.checked_sub(1)
        } else {
            (index + 1 < count).then_some(index + 1)
        };
        let Some(to) = to else {
            return true;
        };
        let caret = textarea.selection_start().ok().flatten().unwrap_or(0);
        {
            let mut inner = self.0.borrow_mut();
            inner.editing_block = Some(block_id);
            inner.pending_block_caret = Some((block_id, caret));
        }
        self.move_block(block_id, to);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::drop_index;

    #[test]
    fn dropping_into_a_gap_accounts_for_the_block_leaving() {
        // Blocks 0..4; block 1 dragged.
        assert_eq!(drop_index(1, 0), Some(0));
        assert_eq!(drop_index(1, 1), None);
        assert_eq!(drop_index(1, 2), None);
        assert_eq!(drop_index(1, 3), Some(2));
        assert_eq!(drop_index(1, 4), Some(3));
    }
}
