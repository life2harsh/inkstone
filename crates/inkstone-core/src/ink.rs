use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

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

    pub fn add_page(&mut self, page: InkPage) {
        self.pages.push(page);
    }

    pub fn add_layer(&mut self, page_id: Uuid, layer: InkLayer) {
        if let Some(page) = self.pages.iter_mut().find(|p| p.id == page_id) {
            page.layers.push(layer);
        }
    }

    pub fn remove_stroke(&mut self, stroke_id: Uuid) {
        for page in &mut self.pages {
            for layer in &mut page.layers {
                if let Some(stroke) = layer.strokes.iter_mut().find(|s| s.id == stroke_id) {
                    stroke.deleted = true;
                    return;
                }
            }
        }
    }
}

impl InkStroke {
    pub fn new(
        id: Uuid,
        tool: InkTool,
        points: Vec<InkPoint>,
        width: f64,
        color: String,
        author_device_id: Uuid,
    ) -> Self {
        let now = chrono::Utc::now().timestamp_millis();
        Self {
            id,
            tool,
            points,
            width,
            color,
            opacity: 1.0,
            deleted: false,
            author_device_id,
            created_at_ms: now,
            updated_at_ms: now,
        }
    }

    pub fn add_point(&mut self, point: InkPoint) {
        self.points.push(point);
        self.updated_at_ms = chrono::Utc::now().timestamp_millis();
    }

    pub fn bounding_box(&self) -> Option<(f64, f64, f64, f64)> {
        if self.points.is_empty() {
            return None;
        }
        let min_x = self.points.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
        let min_y = self.points.iter().map(|p| p.y).fold(f64::INFINITY, f64::min);
        let max_x = self
            .points
            .iter()
            .map(|p| p.x)
            .fold(f64::NEG_INFINITY, f64::max);
        let max_y = self
            .points
            .iter()
            .map(|p| p.y)
            .fold(f64::NEG_INFINITY, f64::max);
        Some((min_x, min_y, max_x - min_x, max_y - min_y))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ink_document_new() {
        let id = Uuid::new_v4();
        let doc = InkDocument::new(id);
        assert_eq!(doc.id, id);
        assert_eq!(doc.version, 1);
        assert!(doc.pages.is_empty());
    }

    #[test]
    fn test_ink_stroke_new() {
        let id = Uuid::new_v4();
        let device_id = Uuid::new_v4();
        let stroke = InkStroke::new(
            id,
            InkTool::Pen,
            vec![InkPoint {
                x: 0.0,
                y: 0.0,
                pressure: 1.0,
                tilt_x: None,
                tilt_y: None,
                timestamp_ms: 1000,
            }],
            2.0,
            "#000000".into(),
            device_id,
        );
        assert_eq!(stroke.id, id);
        assert!(!stroke.deleted);
        assert_eq!(stroke.points.len(), 1);
    }

    #[test]
    fn test_bounding_box() {
        let id = Uuid::new_v4();
        let device_id = Uuid::new_v4();
        let mut stroke = InkStroke::new(id, InkTool::Pen, Vec::new(), 2.0, "red".into(), device_id);

        assert!(stroke.bounding_box().is_none());

        stroke.add_point(InkPoint {
            x: 10.0,
            y: 20.0,
            pressure: 0.5,
            tilt_x: None,
            tilt_y: None,
            timestamp_ms: 1000,
        });
        stroke.add_point(InkPoint {
            x: 30.0,
            y: 40.0,
            pressure: 0.8,
            tilt_x: None,
            tilt_y: None,
            timestamp_ms: 1001,
        });

        let (x, y, w, h) = stroke.bounding_box().unwrap();
        assert!((x - 10.0).abs() < f64::EPSILON);
        assert!((y - 20.0).abs() < f64::EPSILON);
        assert!((w - 20.0).abs() < f64::EPSILON);
        assert!((h - 20.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_serde_roundtrip() {
        let doc = InkDocument {
            id: Uuid::new_v4(),
            version: 1,
            pages: vec![InkPage {
                id: Uuid::new_v4(),
                label: Some("Page 1".into()),
                layers: vec![InkLayer {
                    id: Uuid::new_v4(),
                    name: Some("Layer 1".into()),
                    strokes: vec![],
                    visible: true,
                    locked: false,
                }],
            }],
            metadata: HashMap::new(),
        };

        let json = serde_json::to_string(&doc).unwrap();
        let deserialized: InkDocument = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.version, 1);
        assert_eq!(deserialized.pages.len(), 1);
        assert_eq!(
            deserialized.pages[0].label.as_deref(),
            Some("Page 1")
        );
    }

    #[test]
    fn test_remove_stroke_marks_deleted() {
        let mut doc = InkDocument::new(Uuid::new_v4());
        let page_id = Uuid::new_v4();
        let stroke_id = Uuid::new_v4();
        let device_id = Uuid::new_v4();

        doc.add_page(InkPage {
            id: page_id,
            label: None,
            layers: vec![InkLayer {
                id: Uuid::new_v4(),
                name: None,
                strokes: vec![InkStroke::new(
                    stroke_id,
                    InkTool::Pen,
                    vec![],
                    1.0,
                    "blue".into(),
                    device_id,
                )],
                visible: true,
                locked: false,
            }],
        });

        doc.remove_stroke(stroke_id);
        assert!(doc.pages[0].layers[0].strokes[0].deleted);
    }
}
