use serde::{Deserialize, Serialize};
use uuid::Uuid;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InkDocument {
    pub id: Uuid,
    pub version: i32,
    pub pages: Vec<InkPage>,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InkPage {
    pub id: Uuid,
    pub label: Option<String>,
    pub layers: Vec<InkLayer>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InkLayer {
    pub id: Uuid,
    pub name: Option<String>,
    pub strokes: Vec<InkStroke>,
    pub visible: bool,
    pub locked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InkStroke {
    pub id: Uuid,
    pub tool: InkTool,
    pub points: Vec<InkPoint>,
    pub width: f64,
    pub color: String,
    pub opacity: f64,
    pub deleted: bool,
    pub author_device_id: Uuid,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InkPoint {
    pub x: f64,
    pub y: f64,
    pub pressure: f64,
    pub tilt_x: Option<f64>,
    pub tilt_y: Option<f64>,
    pub timestamp_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum InkTool {
    #[serde(rename = "pen")]
    Pen,

    #[serde(rename = "eraser")]
    Eraser,

    #[serde(rename = "highlighter")]
    Highlighter,

    #[serde(rename = "marker")]
    Marker,
}

impl InkDocument {
    pub fn new(id: Uuid) -> Self {
        Self {
            id,
            version: 1,
            pages: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    pub fn total_strokes(&self) -> usize {
        self.pages
            .iter()
            .flat_map(|p| p.layers.iter())
            .flat_map(|l| l.strokes.iter())
            .count()
    }
}
