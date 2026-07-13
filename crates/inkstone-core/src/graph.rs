use serde::{Deserialize, Serialize};
use uuid::Uuid;
use std::collections::{HashMap, HashSet};

pub type NodeId = String;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphIndex {
    pub nodes: HashMap<NodeId, GraphNode>,
    pub edges: Vec<GraphEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: NodeId,
    pub label: String,
    pub doc_id: Option<Uuid>,
    pub node_type: String,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
    pub source: NodeId,
    pub target: NodeId,
    pub edge_type: String,
    pub weight: f64,
}

impl GraphIndex {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            edges: Vec::new(),
        }
    }

    pub fn add_node(&mut self, node: GraphNode) {
        self.nodes.insert(node.id.clone(), node);
    }

    pub fn add_edge(&mut self, edge: GraphEdge) {
        self.edges.push(edge);
    }

    pub fn get_node(&self, id: &str) -> Option<&GraphNode> {
        self.nodes.get(id)
    }

    pub fn neighbors(&self, node_id: &str) -> Vec<&GraphEdge> {
        self.edges
            .iter()
            .filter(|e| e.source == node_id || e.target == node_id)
            .collect()
    }

    pub fn connected_nodes(&self, node_id: &str) -> Vec<&GraphNode> {
        let neighbor_ids: HashSet<&str> = self
            .edges
            .iter()
            .filter(|e| e.source == node_id)
            .map(|e| e.target.as_str())
            .chain(
                self.edges
                    .iter()
                    .filter(|e| e.target == node_id)
                    .map(|e| e.source.as_str()),
            )
            .collect();

        self.nodes
            .values()
            .filter(|n| neighbor_ids.contains(n.id.as_str()))
            .collect()
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    pub fn nodes_by_type(&self, node_type: &str) -> Vec<&GraphNode> {
        self.nodes
            .values()
            .filter(|n| n.node_type == node_type)
            .collect()
    }

    pub fn remove_node(&mut self, node_id: &str) {
        self.nodes.remove(node_id);
        self.edges.retain(|e| e.source != node_id && e.target != node_id);
    }
}

impl Default for GraphIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_and_query_nodes() {
        let mut graph = GraphIndex::new();

        graph.add_node(GraphNode {
            id: "page1".into(),
            label: "Physics Notes".into(),
            doc_id: Some(Uuid::new_v4()),
            node_type: "doc".into(),
            metadata: HashMap::new(),
        });

        graph.add_node(GraphNode {
            id: "page2".into(),
            label: "Math Notes".into(),
            doc_id: Some(Uuid::new_v4()),
            node_type: "doc".into(),
            metadata: HashMap::new(),
        });

        graph.add_edge(GraphEdge {
            source: "page1".into(),
            target: "page2".into(),
            edge_type: "wikilink".into(),
            weight: 1.0,
        });

        assert_eq!(graph.node_count(), 2);
        assert_eq!(graph.edge_count(), 1);
        assert_eq!(graph.neighbors("page1").len(), 1);
        assert_eq!(graph.connected_nodes("page1").len(), 1);
    }

    #[test]
    fn test_nodes_by_type() {
        let mut graph = GraphIndex::new();
        let node_id = Uuid::new_v4();

        graph.add_node(GraphNode {
            id: "tag1".into(),
            label: "physics".into(),
            doc_id: None,
            node_type: "tag".into(),
            metadata: HashMap::new(),
        });

        graph.add_node(GraphNode {
            id: "doc1".into(),
            label: "My Doc".into(),
            doc_id: Some(node_id),
            node_type: "doc".into(),
            metadata: HashMap::new(),
        });

        let tags = graph.nodes_by_type("tag");
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].label, "physics");

        let docs = graph.nodes_by_type("doc");
        assert_eq!(docs.len(), 1);
    }
}
