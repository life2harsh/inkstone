//! One undo history for the page: typing, block structure (splits, joins,
//! moves, inserts, deletes) and ink, undone in the order they happened.
//!
//! Text and structure are recorded as snapshots of the page's blocks before
//! and after each step. Undoing a step only touches the blocks that step
//! changed, so edits a collaborator made elsewhere on the page survive. Ink
//! steps defer to the ink engine's own history, which holds the same steps
//! in the same order.

use inkstone_core::TextTarget;
use inkstone_core::doc_ops::DocumentOp;
use inkstone_core::document::{BlockContent, DocumentBlock};
use inkstone_core::ids::{BlockId, DocId};
use std::collections::{HashMap, HashSet};

use crate::Driver;

/// Changes closer together than this are one step.
const TYPING_PAUSE_MS: f64 = 1000.0;
/// Changes made by one gesture or key press land within this.
const SAME_ACTION_MS: f64 = 25.0;
const MAX_STEPS: usize = 200;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ChangeKind {
    Typing(TextTarget),
    Structure,
}

#[derive(Clone)]
pub(crate) enum Step {
    /// The blocks took the page from `before` to `after` (`None` while the
    /// step is still growing: the page as it is now).
    Blocks {
        before: Vec<DocumentBlock>,
        after: Option<Vec<DocumentBlock>>,
    },
    Ink,
}

#[derive(Default, Clone)]
pub(crate) struct PageHistory {
    doc: Option<DocId>,
    undo: Vec<Step>,
    redo: Vec<Step>,
    last_kind: Option<ChangeKind>,
    last_ms: f64,
    /// The next change starts a new step whatever its timing.
    boundary: bool,
    /// Set while undo or redo issue their own changes.
    replaying: bool,
}

fn text_of(content: &BlockContent) -> Option<&str> {
    match content {
        BlockContent::Markdown { text } => Some(text),
        BlockContent::Math { source } => Some(source),
        _ => None,
    }
}

fn target_of(block: &DocumentBlock) -> Option<TextTarget> {
    match block.content {
        BlockContent::Markdown { .. } => Some(TextTarget::Markdown(block.id)),
        BlockContent::Math { .. } => Some(TextTarget::Math(block.id)),
        _ => None,
    }
}

fn same_kind(a: &BlockContent, b: &BlockContent) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b)
}

fn same_content(a: &BlockContent, b: &BlockContent) -> bool {
    same_kind(a, b)
        && match (a, b) {
            (BlockContent::Space { min_height: x }, BlockContent::Space { min_height: y }) => {
                x == y
            }
            (BlockContent::Divider, BlockContent::Divider) => true,
            _ => match (text_of(a), text_of(b)) {
                (Some(x), Some(y)) => x == y,
                _ => serde_json::to_string(a).ok() == serde_json::to_string(b).ok(),
            },
        }
}

/// UTF-16 offset just after the part of `new` that differs from `old`.
fn changed_end(old: &str, new: &str) -> u32 {
    let old = old.encode_utf16().collect::<Vec<_>>();
    let new = new.encode_utf16().collect::<Vec<_>>();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    (new.len() - suffix) as u32
}

/// The page ops and text edits that make the blocks `from → to` changed
/// look like `to` again in `current`.
pub(crate) struct Revert {
    pub ops: Vec<DocumentOp>,
    pub texts: Vec<(TextTarget, String, String)>,
    /// Where the caret goes afterwards.
    pub caret: Option<(BlockId, u32)>,
    /// Blocks brought back under new ids: `(old, new)`.
    pub renamed: Vec<(BlockId, BlockId)>,
}

fn rename(steps: &mut [Step], renamed: &[(BlockId, BlockId)]) {
    for step in steps {
        let Step::Blocks { before, after } = step else {
            continue;
        };
        for block in before.iter_mut().chain(after.iter_mut().flatten()) {
            if let Some((_, new)) = renamed.iter().find(|(old, _)| *old == block.id) {
                block.id = *new;
            }
        }
    }
}

pub(crate) fn plan_revert(
    from: &[DocumentBlock],
    to: &[DocumentBlock],
    current: &[DocumentBlock],
    fresh_id: &mut dyn FnMut() -> BlockId,
) -> Revert {
    let from_ids = from.iter().map(|block| block.id).collect::<HashSet<_>>();
    let to_ids = to.iter().map(|block| block.id).collect::<HashSet<_>>();
    let mut order = current.iter().map(|block| block.id).collect::<Vec<_>>();
    let current_by_id = current
        .iter()
        .map(|block| (block.id, block))
        .collect::<HashMap<_, _>>();
    let mut ops = Vec::new();
    let mut texts = Vec::new();
    let mut caret = None;

    // Blocks the step added go.
    for block in from {
        if !to_ids.contains(&block.id) && current_by_id.contains_key(&block.id) {
            ops.push(DocumentOp::DeleteBlock { block_id: block.id });
            order.retain(|id| *id != block.id);
        }
    }

    // Blocks the step removed come back after their old neighbour above.
    let mut placed = HashMap::new();
    for (index, block) in to.iter().enumerate() {
        if from_ids.contains(&block.id) {
            continue;
        }
        let id = if current_by_id.contains_key(&block.id) || order.contains(&block.id) {
            continue;
        } else {
            fresh_id()
        };
        let position = to[..index]
            .iter()
            .rev()
            .find_map(|above| {
                let above = placed.get(&above.id).copied().unwrap_or(above.id);
                order.iter().position(|id| *id == above)
            })
            .map(|position| position + 1)
            .unwrap_or(0);
        order.insert(position, id);
        placed.insert(block.id, id);
        ops.push(DocumentOp::InsertBlock {
            idx: position,
            block: DocumentBlock {
                id,
                ..block.clone()
            },
        });
        if caret.is_none() {
            if let Some(text) = text_of(&block.content) {
                caret = Some((id, text.encode_utf16().count() as u32));
            }
        }
    }

    // Blocks the step edited get their old content back.
    let from_by_id = from
        .iter()
        .map(|block| (block.id, block))
        .collect::<HashMap<_, _>>();
    for block in to {
        let Some(old) = from_by_id.get(&block.id) else {
            continue;
        };
        let Some(now) = current_by_id.get(&block.id) else {
            continue;
        };
        if same_content(&old.content, &block.content) {
            continue;
        }
        match (
            text_of(&now.content),
            text_of(&block.content),
            target_of(now),
        ) {
            (Some(now_text), Some(text), Some(target))
                if same_kind(&now.content, &block.content) =>
            {
                if now_text != text {
                    if caret.is_none() {
                        caret = Some((block.id, changed_end(now_text, text)));
                    }
                    texts.push((target, now_text.to_string(), text.to_string()));
                }
            }
            _ => ops.push(DocumentOp::ReplaceBlockContent {
                block_id: block.id,
                content: block.content.clone(),
            }),
        }
    }

    // Blocks the step moved go back to their old order.
    let common = |list: &[DocumentBlock]| {
        list.iter()
            .map(|block| block.id)
            .filter(|id| from_ids.contains(id) && to_ids.contains(id) && order.contains(id))
            .collect::<Vec<_>>()
    };
    let (moved_from, moved_to) = (common(from), common(to));
    if moved_from != moved_to {
        let slots = order
            .iter()
            .enumerate()
            .filter(|(_, id)| moved_to.contains(id))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let mut target = order.clone();
        for (slot, id) in slots.iter().zip(&moved_to) {
            target[*slot] = *id;
        }
        for (index, id) in target.iter().enumerate() {
            if order[index] != *id {
                ops.push(DocumentOp::MoveBlock {
                    block_id: *id,
                    new_idx: index,
                });
                order.retain(|other| other != id);
                order.insert(index, *id);
            }
        }
        if caret.is_none() {
            caret = moved_to.first().map(|id| (*id, 0));
        }
    }

    // After removing blocks, the caret goes where they joined.
    if caret.is_none() {
        let to_by_id = to
            .iter()
            .map(|block| (block.id, block))
            .collect::<HashMap<_, _>>();
        caret = from
            .iter()
            .position(|block| !to_ids.contains(&block.id))
            .and_then(|removed| {
                from[..removed].iter().rev().find_map(|above| {
                    let block = to_by_id.get(&above.id)?;
                    let text = text_of(&block.content)?;
                    Some((block.id, text.encode_utf16().count() as u32))
                })
            });
    }

    let renamed = placed.into_iter().collect();
    Revert {
        ops,
        texts,
        caret,
        renamed,
    }
}

impl Driver {
    fn current_blocks(&self) -> Vec<DocumentBlock> {
        let inner = self.0.borrow();
        Driver::effective_document(&inner).blocks
    }

    /// The history for the open note, emptied when another note opens.
    fn with_history<R>(&self, f: impl FnOnce(&mut PageHistory) -> R) -> R {
        let mut inner = self.0.borrow_mut();
        let doc = inner.selected_doc_id;
        let history = &mut inner.page_history;
        if history.doc != Some(doc) {
            *history = PageHistory {
                doc: Some(doc),
                ..Default::default()
            };
        }
        f(history)
    }

    /// Ends the current step, so the next change starts its own.
    pub(crate) fn history_boundary(&self) {
        self.with_history(|history| history.boundary = true);
    }

    pub(crate) fn clear_history(&self) {
        self.with_history(|history| {
            history.undo.clear();
            history.redo.clear();
            history.last_kind = None;
        });
    }

    /// Called before a local text or structure change is applied.
    pub(crate) fn record_change(&self, kind: ChangeKind) {
        if self.0.borrow().page_history.replaying {
            return;
        }
        let now = js_sys::Date::now();
        let continues = self.with_history(|history| {
            let gap = now - history.last_ms;
            let continues = matches!(history.undo.last(), Some(Step::Blocks { after: None, .. }))
                && !history.boundary
                && ((gap < SAME_ACTION_MS && kind == ChangeKind::Structure)
                    || (history.last_kind == Some(kind)
                        && matches!(kind, ChangeKind::Typing(_))
                        && gap < TYPING_PAUSE_MS));
            history.last_ms = now;
            history.boundary = false;
            if !continues || kind == ChangeKind::Structure {
                history.last_kind = Some(kind);
            }
            continues
        });
        if continues {
            return;
        }
        let blocks = self.current_blocks();
        self.with_history(|history| {
            if let Some(Step::Blocks {
                after: after @ None,
                ..
            }) = history.undo.last_mut()
            {
                *after = Some(blocks.clone());
            }
            history.undo.push(Step::Blocks {
                before: blocks,
                after: None,
            });
            history.redo.clear();
            if history.undo.len() > MAX_STEPS {
                history.undo.remove(0);
            }
        });
    }

    /// Called after an ink change the ink engine recorded.
    pub(crate) fn record_ink_step(&self) {
        if self.0.borrow().page_history.replaying {
            return;
        }
        let blocks = self.current_blocks();
        self.with_history(|history| {
            if let Some(Step::Blocks {
                after: after @ None,
                ..
            }) = history.undo.last_mut()
            {
                *after = Some(blocks);
            }
            history.undo.push(Step::Ink);
            history.redo.clear();
            history.last_kind = None;
        });
    }

    pub(crate) fn undo_page(&self) -> bool {
        self.step_history(true)
    }

    pub(crate) fn redo_page(&self) -> bool {
        self.step_history(false)
    }

    fn step_history(&self, undo: bool) -> bool {
        // Text typed but not yet sent belongs to the step being undone.
        if let Some(textarea) = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.active_element())
            .filter(|active| active.class_list().contains("page-source"))
            .and_then(|active| {
                wasm_bindgen::JsCast::dyn_into::<web_sys::HtmlTextAreaElement>(active).ok()
            })
        {
            if let Some(block_id) = textarea
                .get_attribute("data-block-id")
                .and_then(|value| uuid::Uuid::parse_str(&value).ok())
                .map(BlockId)
            {
                let target = if textarea.get_attribute("data-block-kind").as_deref() == Some("math")
                {
                    TextTarget::Math(block_id)
                } else {
                    TextTarget::Markdown(block_id)
                };
                self.commit_source(&textarea, target);
            }
        }
        let current = self.current_blocks();
        let step = self.with_history(|history| {
            let stack = if undo {
                &mut history.undo
            } else {
                &mut history.redo
            };
            let step = stack.pop()?;
            history.boundary = true;
            Some(match step {
                Step::Blocks { before, after } => Step::Blocks {
                    before,
                    after: Some(after.unwrap_or_else(|| current.clone())),
                },
                Step::Ink => Step::Ink,
            })
        });
        let Some(step) = step else {
            return false;
        };
        self.0.borrow_mut().page_history.replaying = true;
        let inverse = match step {
            Step::Ink => {
                self.page_ink_history(undo);
                Step::Ink
            }
            Step::Blocks { before, after } => {
                let after = after.unwrap_or_default();
                let mut plan = plan_revert(&after, &before, &current, &mut BlockId::new_v4);
                let renamed = std::mem::take(&mut plan.renamed);
                self.with_history(|history| {
                    rename(&mut history.undo, &renamed);
                    rename(&mut history.redo, &renamed);
                });
                for op in plan.ops {
                    self.dispatch_page_op(op);
                }
                for (target, now, text) in &plan.texts {
                    self.dispatch_text_edit(*target, now, text);
                }
                let reached = self.current_blocks();
                match plan.caret {
                    Some((block_id, caret))
                        if reached.iter().any(|block| {
                            block.id == block_id && text_of(&block.content).is_some()
                        }) =>
                    {
                        self.reopen_at(block_id, caret);
                    }
                    _ => {
                        self.0.borrow_mut().editing_block = None;
                        crate::page_driver::blur_page_source();
                        self.render();
                    }
                }
                Step::Blocks {
                    before: current,
                    after: Some(reached),
                }
            }
        };
        self.0.borrow_mut().page_history.replaying = false;
        self.with_history(|history| {
            let stack = if undo {
                &mut history.redo
            } else {
                &mut history.undo
            };
            stack.push(inverse);
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn markdown(id: u128, text: &str) -> DocumentBlock {
        DocumentBlock::new(
            BlockId(uuid::Uuid::from_u128(id)),
            BlockContent::Markdown { text: text.into() },
        )
    }

    fn fresh() -> impl FnMut() -> BlockId {
        let mut next = 100;
        move || {
            next += 1;
            BlockId(uuid::Uuid::from_u128(next))
        }
    }

    #[test]
    fn undoing_a_split_deletes_the_new_block_and_restores_the_text() {
        let before = vec![markdown(1, "Hello world")];
        let after = vec![markdown(1, "Hello"), markdown(2, " world")];
        let plan = plan_revert(&after, &before, &after, &mut fresh());
        assert!(matches!(
            plan.ops.as_slice(),
            [DocumentOp::DeleteBlock { block_id }] if *block_id == after[1].id
        ));
        assert_eq!(
            plan.texts,
            vec![(
                TextTarget::Markdown(before[0].id),
                "Hello".into(),
                "Hello world".into()
            )]
        );
        assert_eq!(plan.caret, Some((before[0].id, 11)));

        // Undoing Enter at the end of a line puts the caret back there.
        let before = vec![markdown(1, "Hello")];
        let after = vec![markdown(1, "Hello"), markdown(2, "")];
        let plan = plan_revert(&after, &before, &after, &mut fresh());
        assert_eq!(plan.caret, Some((before[0].id, 5)));
    }

    #[test]
    fn undoing_a_delete_brings_the_block_back_in_place() {
        let before = vec![markdown(1, "a"), markdown(2, "b"), markdown(3, "c")];
        let after = vec![markdown(1, "a"), markdown(3, "c")];
        let plan = plan_revert(&after, &before, &after, &mut fresh());
        match plan.ops.as_slice() {
            [DocumentOp::InsertBlock { idx: 1, block }] => {
                assert_eq!(text_of(&block.content), Some("b"));
            }
            ops => panic!("unexpected {ops:?}"),
        }
    }

    #[test]
    fn undo_leaves_other_blocks_a_collaborator_changed() {
        let before = vec![markdown(1, "mine"), markdown(2, "theirs")];
        let after = vec![markdown(1, "mine!"), markdown(2, "theirs")];
        let current = vec![markdown(1, "mine!"), markdown(2, "theirs, edited")];
        let plan = plan_revert(&after, &before, &current, &mut fresh());
        assert!(plan.ops.is_empty());
        assert_eq!(plan.texts.len(), 1);
        assert_eq!(plan.texts[0].2, "mine");
    }

    #[test]
    fn undoing_a_move_restores_the_order() {
        let before = vec![markdown(1, "a"), markdown(2, "b"), markdown(3, "c")];
        let after = vec![markdown(3, "c"), markdown(1, "a"), markdown(2, "b")];
        let plan = plan_revert(&after, &before, &after, &mut fresh());
        let mut order = after.iter().map(|block| block.id).collect::<Vec<_>>();
        for op in &plan.ops {
            let DocumentOp::MoveBlock { block_id, new_idx } = op else {
                panic!("unexpected {op:?}");
            };
            order.retain(|id| id != block_id);
            order.insert(*new_idx, *block_id);
        }
        assert_eq!(
            order,
            before.iter().map(|block| block.id).collect::<Vec<_>>()
        );
    }
}
