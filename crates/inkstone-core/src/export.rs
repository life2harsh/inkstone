use std::collections::{BTreeMap, HashSet};

use crate::document::{AnchorTarget, BlockContent, InkstoneDocument};
use crate::ids::BlockId;

const INK_PADDING: f32 = 6.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InkImage {
    pub file_name: String,
    pub svg: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortableNote {
    pub markdown: String,
    pub images: Vec<InkImage>,
}

pub fn page_ink_svg(doc: &InkstoneDocument) -> Option<String> {
    let layer = doc.ink.as_ref()?;
    let ink = stonepen_core::export_json::deserialize_doc(
        layer.collaboration.snapshot.projection().ok()?,
    )
    .ok()?;
    let groups = stonepen_core::export_svg::export_group_svgs(&ink, 0.0);
    let bounds = groups
        .iter()
        .map(|group| group.bounds)
        .reduce(|a, b| a.union(b))?;
    Some(stonepen_core::export_svg::export_svg_view(
        &ink,
        bounds.expand_by(INK_PADDING),
    ))
}

/// Markdown any other editor can open: handwriting becomes SVG images placed
/// right after the paragraph it was written beside.
pub fn portable_markdown(doc: &InkstoneDocument, image_dir: &str, stem: &str) -> PortableNote {
    let mut images = Vec::new();
    let mut by_block: BTreeMap<BlockId, Vec<(f32, String)>> = BTreeMap::new();
    let mut top = Vec::new();
    let mut loose = Vec::new();
    let block_ids: HashSet<BlockId> = doc.blocks.iter().map(|block| block.id).collect();

    if let Some(layer) = doc.ink.as_ref() {
        let ink = layer
            .collaboration
            .snapshot
            .projection()
            .ok()
            .and_then(|json| stonepen_core::export_json::deserialize_doc(json).ok());
        if let Some(ink) = ink {
            for group in stonepen_core::export_svg::export_group_svgs(&ink, INK_PADDING) {
                let file_name = format!("{stem}-ink-{}.svg", images.len() + 1);
                let link = image_link(image_dir, &file_name);
                images.push(InkImage {
                    file_name,
                    svg: group.svg,
                });
                let anchor = group
                    .group
                    .and_then(|id| layer.anchors.get(&crate::ids::InkGroupId(id.0)));
                match anchor {
                    Some(anchor) => match anchor.target.block_id() {
                        Some(block) if block_ids.contains(&block) => {
                            by_block.entry(block).or_default().push((anchor.dy, link));
                        }
                        Some(_) => loose.push((anchor.dy, link)),
                        None if matches!(anchor.target, AnchorTarget::Page) => {
                            top.push((anchor.dy, link))
                        }
                        None => top.push((anchor.dy, link)),
                    },
                    None => loose.push((group.bounds.min_y, link)),
                }
            }
        }
    }

    let mut output = String::new();
    if !doc.title.is_empty() {
        output.push_str("# ");
        output.push_str(&doc.title);
        output.push_str("\n\n");
    }
    push_images(&mut output, top);
    for block in &doc.blocks {
        match &block.content {
            BlockContent::Markdown { text } => push_paragraph(&mut output, text),
            BlockContent::Math { source } => {
                push_paragraph(&mut output, &format!("$$\n{}\n$$", source.trim()))
            }
            BlockContent::Ink { stonepen_json, .. } => {
                let svg = stonepen_core::export_json::deserialize_doc(stonepen_json)
                    .ok()
                    .and_then(|ink| {
                        let groups = stonepen_core::export_svg::export_group_svgs(&ink, 0.0);
                        let bounds = groups
                            .iter()
                            .map(|group| group.bounds)
                            .reduce(|a, b| a.union(b))?;
                        Some(stonepen_core::export_svg::export_svg_view(
                            &ink,
                            bounds.expand_by(INK_PADDING),
                        ))
                    });
                if let Some(svg) = svg {
                    let file_name = format!("{stem}-ink-{}.svg", images.len() + 1);
                    push_images(&mut output, vec![(0.0, image_link(image_dir, &file_name))]);
                    images.push(InkImage { file_name, svg });
                }
            }
            BlockContent::Divider => push_paragraph(&mut output, "---"),
            BlockContent::Space { .. } => {}
        }
        if let Some(group) = by_block.remove(&block.id) {
            push_images(&mut output, group);
        }
    }
    push_images(&mut output, loose);
    while output.ends_with("\n\n") {
        output.pop();
    }
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
    PortableNote {
        markdown: output,
        images,
    }
}

fn image_link(dir: &str, file_name: &str) -> String {
    let path = if dir.is_empty() {
        file_name.to_string()
    } else {
        format!("{}/{file_name}", dir.trim_end_matches('/'))
    };
    path.split('/')
        .map(|segment| {
            segment
                .replace('%', "%25")
                .replace(' ', "%20")
                .replace('(', "%28")
                .replace(')', "%29")
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn push_paragraph(output: &mut String, text: &str) {
    let text = text.trim_end_matches('\n');
    if text.trim().is_empty() {
        return;
    }
    output.push_str(text);
    output.push_str("\n\n");
}

fn push_images(output: &mut String, mut images: Vec<(f32, String)>) {
    images.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (_, link) in images {
        output.push_str(&format!("![Handwriting]({link})\n\n"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{DocumentBlock, InkAnchor};
    use crate::ids::{DocId, InkGroupId};
    use stonepen_core::{
        ActorId, Brush, CollaborationDoc, InkPoint, InkStroke, ItemId, PointerKind,
    };

    fn stroke(group: InkGroupId, y: f32) -> InkStroke {
        let brush = Brush::default_pen();
        let pts = [0.0_f32, 30.0]
            .into_iter()
            .map(|x| InkPoint {
                x,
                y,
                t_ms: 0.0,
                press: 1.0,
                tilt_x: 0.0,
                tilt_y: 0.0,
                twist: 0.0,
                pointer_type: PointerKind::Pen,
            })
            .collect::<Vec<_>>();
        let bbox = stonepen_core::compute_conservative_stroke_bbox(&pts, &brush).unwrap();
        InkStroke {
            id: ItemId::new(),
            parent_id: Some(ItemId(group.0)),
            brush,
            raw_pts: pts.clone(),
            pts,
            local_bbox: bbox,
            world_bbox: bbox,
            xform: stonepen_core::Xform2D::identity(),
            created_at_ms: 0,
            updated_at_ms: 0,
            geom_rev: 0,
        }
    }

    fn lecture(groups: &[(InkGroupId, Option<AnchorTarget>)]) -> InkstoneDocument {
        let mut doc = InkstoneDocument::new(DocId::new_v4());
        doc.title = "Lecture".into();
        doc.blocks.push(DocumentBlock::new(
            BlockId::new_v4(),
            BlockContent::Markdown {
                text: "Attention is all you need".into(),
            },
        ));
        doc.blocks.push(DocumentBlock::new(
            BlockId::new_v4(),
            BlockContent::Math {
                source: "e^{i\\pi} + 1 = 0".into(),
            },
        ));
        let mut layer = crate::document::first_page_ink_layer(doc.id).unwrap();
        let mut replica = CollaborationDoc::from_snapshot(
            ActorId::from_bytes([3; 16]),
            layer.collaboration.snapshot.bytes().unwrap(),
        )
        .unwrap();
        let layer_id = replica.document().active_layer_id;
        for (index, (group, target)) in groups.iter().enumerate() {
            replica
                .add_stroke(layer_id, None, stroke(*group, 10.0 * index as f32))
                .unwrap();
            if let Some(target) = target {
                let target = match target {
                    AnchorTarget::Block { .. } => AnchorTarget::Block {
                        block_id: doc.blocks[0].id,
                    },
                    other => other.clone(),
                };
                layer.anchors.insert(
                    *group,
                    InkAnchor {
                        target,
                        dx: 0.0,
                        dy: 4.0,
                    },
                );
            }
        }
        layer.collaboration.snapshot = crate::InkSnapshot::from_bytes(replica.snapshot().unwrap());
        doc.ink = Some(layer);
        doc
    }

    #[test]
    fn handwriting_lands_after_the_paragraph_it_belongs_to() {
        let doc = lecture(&[
            (
                InkGroupId(uuid::Uuid::new_v4()),
                Some(AnchorTarget::Block {
                    block_id: BlockId::new_v4(),
                }),
            ),
            (InkGroupId(uuid::Uuid::new_v4()), None),
        ]);
        let note = portable_markdown(&doc, "Lecture assets", "Lecture");
        assert_eq!(note.images.len(), 2);
        assert!(
            note.images
                .iter()
                .all(|image| image.svg.contains("<path d=\"M "))
        );
        let paragraph = note.markdown.find("Attention is all you need").unwrap();
        let math = note.markdown.find("$$").unwrap();
        let first_image = note
            .markdown
            .find("![Handwriting](Lecture%20assets/Lecture-ink-")
            .unwrap();
        let last_image = note.markdown.rfind("![Handwriting]").unwrap();
        assert!(
            paragraph < first_image && first_image < math,
            "{}",
            note.markdown
        );
        assert!(last_image > math, "{}", note.markdown);
        assert!(note.markdown.starts_with("# Lecture\n\n"));
        assert!(!note.markdown.contains("inkstone-page-ink"));
        let page = page_ink_svg(&doc).unwrap();
        assert_eq!(page.matches("<path").count(), 2);
    }

    #[test]
    fn notes_without_ink_are_plain_markdown() {
        let mut doc = InkstoneDocument::new(DocId::new_v4());
        doc.title = "Plain".into();
        doc.blocks.push(DocumentBlock::new(
            BlockId::new_v4(),
            BlockContent::Markdown {
                text: "- one\n- two".into(),
            },
        ));
        doc.blocks
            .push(DocumentBlock::new(BlockId::new_v4(), BlockContent::Divider));
        let note = portable_markdown(&doc, "", "Plain");
        assert_eq!(note.markdown, "# Plain\n\n- one\n- two\n\n---\n");
        assert!(note.images.is_empty());
        assert!(page_ink_svg(&doc).is_none());
    }
}
