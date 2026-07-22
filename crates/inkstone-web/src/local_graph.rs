use std::collections::BTreeMap;

use crate::knowledge::{KnowledgeIndex, WikilinkResolution};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphNodeKind {
    Active,
    Outgoing,
    Backlink,
    Connected,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LocalGraphNode {
    pub path: String,
    pub label: String,
    pub kind: GraphNodeKind,
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalGraphEdge {
    pub source: String,
    pub target: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LocalGraph {
    pub nodes: Vec<LocalGraphNode>,
    pub edges: Vec<LocalGraphEdge>,
}

fn path_label(path: &str) -> String {
    path.rsplit('/')
        .next()
        .unwrap_or(path)
        .strip_suffix(".md")
        .unwrap_or_else(|| path.rsplit('/').next().unwrap_or(path))
        .to_string()
}

pub fn build_local_graph(index: &KnowledgeIndex, active_path: &str) -> LocalGraph {
    let mut relationships: BTreeMap<String, (bool, bool)> = BTreeMap::new();
    if let Some(document) = index.path_to_doc.get(active_path) {
        for link in &document.outgoing_links {
            let resolution = if link.is_wikilink {
                index.resolve_wikilink(active_path, &link.target_raw)
            } else {
                index.resolve_markdown_link(active_path, &link.target_raw)
            };
            if let WikilinkResolution::Resolved(path) = resolution {
                if path != active_path {
                    relationships.entry(path).or_default().0 = true;
                }
            }
        }
    }
    for backlink in index.get_backlinks(active_path) {
        relationships.entry(backlink.source_path).or_default().1 = true;
    }

    let mut nodes = vec![LocalGraphNode {
        path: active_path.to_string(),
        label: path_label(active_path),
        kind: GraphNodeKind::Active,
        x: 150.0,
        y: 132.0,
    }];
    let neighbor_count = relationships.len();
    for (index, (path, (outgoing, backlink))) in relationships.into_iter().enumerate() {
        let angle = if neighbor_count == 0 {
            0.0
        } else {
            std::f32::consts::TAU * index as f32 / neighbor_count as f32
                - std::f32::consts::FRAC_PI_2
        };
        nodes.push(LocalGraphNode {
            label: path_label(&path),
            path,
            kind: match (outgoing, backlink) {
                (true, true) => GraphNodeKind::Connected,
                (true, false) => GraphNodeKind::Outgoing,
                (false, true) => GraphNodeKind::Backlink,
                (false, false) => unreachable!("relationship must have an edge"),
            },
            x: 150.0 + angle.cos() * 96.0,
            y: 132.0 + angle.sin() * 86.0,
        });
    }

    let edges = nodes
        .iter()
        .skip(1)
        .map(|node| LocalGraphEdge {
            source: active_path.to_string(),
            target: node.path.clone(),
        })
        .collect();
    LocalGraph { nodes, edges }
}

#[cfg(test)]
mod tests {
    use super::*;
    use inkstone_core::ids::BlockId;

    fn index_document(index: &mut KnowledgeIndex, path: &str, markdown: &str) {
        index.register_path(path);
        index.index_document(
            path,
            &[(
                BlockId::new_v4(),
                inkstone_core::markdown::parse_markdown(markdown),
            )],
        );
    }

    #[test]
    fn local_graph_combines_outgoing_and_backlink_neighbors() {
        let mut index = KnowledgeIndex::new();
        index_document(&mut index, "Home.md", "[[Target]]");
        index_document(&mut index, "Target.md", "[[Home]]");
        index_document(&mut index, "Notes.md", "See [[Home]]");

        let graph = build_local_graph(&index, "Home.md");
        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(graph.edges.len(), 2);
        assert_eq!(graph.nodes[0].kind, GraphNodeKind::Active);
        assert_eq!(graph.nodes[1].path, "Notes.md");
        assert_eq!(graph.nodes[1].kind, GraphNodeKind::Backlink);
        assert_eq!(graph.nodes[2].path, "Target.md");
        assert_eq!(graph.nodes[2].kind, GraphNodeKind::Connected);
    }

    #[test]
    fn isolated_note_has_only_the_active_node() {
        let mut index = KnowledgeIndex::new();
        index_document(&mut index, "Isolated.md", "# Alone");

        let graph = build_local_graph(&index, "Isolated.md");
        assert_eq!(graph.nodes.len(), 1);
        assert!(graph.edges.is_empty());
    }
}
