use std::fmt;
use std::sync::{Arc, OnceLock};

use base64::Engine;
use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use stonepen_core::{ActorId, CollaborationDoc};

#[derive(Default)]
struct Cell {
    bytes: OnceLock<Result<Vec<u8>, String>>,
    doc: OnceLock<Result<CollaborationDoc, String>>,
    projection: OnceLock<String>,
}

#[derive(Clone, Default)]
pub struct InkSnapshot(Arc<Cell>);

impl InkSnapshot {
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        let cell = Cell::default();
        let _ = cell.bytes.set(Ok(bytes));
        Self(Arc::new(cell))
    }

    pub fn from_doc(doc: CollaborationDoc) -> Self {
        let cell = Cell::default();
        let _ = cell.doc.set(Ok(doc));
        Self(Arc::new(cell))
    }

    pub fn bytes(&self) -> Result<&[u8], String> {
        if let Some(Ok(bytes)) = self.0.bytes.get() {
            return Ok(bytes);
        }
        self.0
            .bytes
            .get_or_init(|| match self.0.doc.get() {
                Some(Ok(doc)) => doc.snapshot().map_err(|error| error.to_string()),
                Some(Err(error)) => Err(error.clone()),
                None => Ok(Vec::new()),
            })
            .as_deref()
            .map_err(Clone::clone)
    }

    pub fn doc(&self) -> Result<&CollaborationDoc, String> {
        self.0
            .doc
            .get_or_init(|| {
                let bytes = self.bytes()?;
                CollaborationDoc::from_snapshot(ActorId::from_bytes([0; 16]), bytes)
                    .map_err(|error| error.to_string())
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    pub fn projection(&self) -> Result<&str, String> {
        if let Some(projection) = self.0.projection.get() {
            return Ok(projection);
        }
        let projection = self.doc()?.projection_json().map_err(|e| e.to_string())?;
        Ok(self.0.projection.get_or_init(|| projection))
    }

    pub fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    pub fn is_empty(&self) -> bool {
        self.0.doc.get().is_none()
            && self.0.bytes.get().is_none_or(|bytes| match bytes {
                Ok(bytes) => bytes.is_empty(),
                Err(_) => false,
            })
    }

    pub fn apply(&mut self, update: &[u8]) -> Result<(), String> {
        let owned = Arc::get_mut(&mut self.0).and_then(|cell| match cell.doc.take() {
            Some(Ok(doc)) => Some(doc),
            Some(Err(error)) => {
                let _ = cell.doc.set(Err(error));
                None
            }
            None => None,
        });
        let mut doc = match owned {
            Some(doc) => doc,
            None => self.doc()?.clone(),
        };
        if update.is_empty() {
            *self = Self::from_doc(doc);
            return Ok(());
        }
        let result = doc.apply_update(update).map_err(|error| error.to_string());
        *self = Self::from_doc(doc);
        result
    }
}

impl From<Vec<u8>> for InkSnapshot {
    fn from(bytes: Vec<u8>) -> Self {
        Self::from_bytes(bytes)
    }
}

impl fmt::Debug for InkSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InkSnapshot")
    }
}

impl Serialize for InkSnapshot {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let bytes = self.bytes().map_err(serde::ser::Error::custom)?;
        if serializer.is_human_readable() {
            serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
        } else {
            serializer.serialize_bytes(bytes)
        }
    }
}

struct BytesVisitor;

impl<'de> Visitor<'de> for BytesVisitor {
    type Value = Vec<u8>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("opaque bytes")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        base64::engine::general_purpose::STANDARD
            .decode(value)
            .map_err(E::custom)
    }

    fn visit_bytes<E: de::Error>(self, value: &[u8]) -> Result<Self::Value, E> {
        Ok(value.to_vec())
    }

    fn visit_byte_buf<E: de::Error>(self, value: Vec<u8>) -> Result<Self::Value, E> {
        Ok(value)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut bytes = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(1 << 20));
        while let Some(byte) = seq.next_element::<u8>()? {
            bytes.push(byte);
        }
        Ok(bytes)
    }
}

impl<'de> Deserialize<'de> for InkSnapshot {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bytes = if deserializer.is_human_readable() {
            deserializer.deserialize_any(BytesVisitor)?
        } else {
            deserializer.deserialize_byte_buf(BytesVisitor)?
        };
        Ok(InkSnapshot::from_bytes(bytes))
    }
}

pub mod opaque_bytes {
    use super::*;

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
        } else {
            bytes.serialize(serializer)
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        if deserializer.is_human_readable() {
            deserializer.deserialize_any(BytesVisitor)
        } else {
            Vec::<u8>::deserialize(deserializer)
        }
    }
}

pub mod opaque_bytes_option {
    use super::*;

    #[derive(Deserialize)]
    struct Wrapped(#[serde(with = "opaque_bytes")] Vec<u8>);

    pub fn serialize<S: Serializer>(
        bytes: &Option<Vec<u8>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match bytes {
            Some(bytes) => {
                if serializer.is_human_readable() {
                    serializer
                        .serialize_some(&base64::engine::general_purpose::STANDARD.encode(bytes))
                } else {
                    serializer.serialize_some(bytes)
                }
            }
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Vec<u8>>, D::Error> {
        Ok(Option::<Wrapped>::deserialize(deserializer)?.map(|wrapped| wrapped.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_byte_arrays_and_new_strings_both_load() {
        let (bytes, _) = stonepen_core::new_opaque_document(100.0, 100.0).unwrap();
        let legacy = serde_json::to_string(&bytes).unwrap();
        let from_legacy: InkSnapshot = serde_json::from_str(&legacy).unwrap();
        assert_eq!(from_legacy.bytes().unwrap(), bytes.as_slice());
        let written = serde_json::to_string(&from_legacy).unwrap();
        #[derive(Serialize, Deserialize)]
        struct Update {
            #[serde(with = "opaque_bytes")]
            update: Vec<u8>,
            #[serde(default, with = "opaque_bytes_option")]
            snapshot: Option<Vec<u8>>,
        }
        let old: Update =
            serde_json::from_str(&format!("{{\"update\":{legacy},\"snapshot\":{legacy}}}"))
                .unwrap();
        assert_eq!(old.update, bytes);
        assert_eq!(old.snapshot.as_deref(), Some(bytes.as_slice()));
        let new = serde_json::to_string(&old).unwrap();
        assert!(new.len() < legacy.len() * 2);
        let round: Update = serde_json::from_str(&new).unwrap();
        assert_eq!(round.update, bytes);
        let none: Update = serde_json::from_str(&format!("{{\"update\":{legacy}}}")).unwrap();
        assert_eq!(none.snapshot, None);
        assert!(written.starts_with('"'));
        let back: InkSnapshot = serde_json::from_str(&written).unwrap();
        assert_eq!(back.bytes().unwrap(), bytes.as_slice());
    }

    #[test]
    fn applying_in_place_matches_the_byte_path() {
        let (bytes, _) = stonepen_core::new_opaque_document(100.0, 100.0).unwrap();
        let mut writer =
            CollaborationDoc::from_snapshot(ActorId::from_bytes([7; 16]), &bytes).unwrap();
        let mut snapshot = InkSnapshot::from_bytes(bytes.clone());
        let shared = snapshot.clone();
        let mut expected = bytes;
        for index in 0..3 {
            let mut builder = stonepen_core::stroke::StrokeBuilder::new(
                stonepen_core::brush::Brush::default_pen(),
            );
            for step in 0..5 {
                builder.push(stonepen_core::point::InkPoint {
                    x: index as f32 * 10.0 + step as f32,
                    y: 5.0,
                    t_ms: step as f64,
                    press: 1.0,
                    tilt_x: 0.0,
                    tilt_y: 0.0,
                    twist: 0.0,
                    pointer_type: stonepen_core::point::PointerKind::Pen,
                });
            }
            let stroke = builder.finish(0, None).unwrap();
            let layer = writer.document().active_layer_id;
            writer.add_stroke(layer, None, stroke).unwrap();
            let update = writer.take_local_update().unwrap();
            snapshot.apply(&update).unwrap();
            expected = stonepen_core::apply_opaque_update(&expected, &update)
                .unwrap()
                .0;
        }
        assert_eq!(snapshot.bytes().unwrap(), expected.as_slice());
        assert_eq!(
            snapshot.projection().unwrap(),
            stonepen_core::project_opaque_snapshot(&expected).unwrap()
        );
        assert!(shared.doc().unwrap().document().layers[0].items.is_empty());
        assert!(snapshot.apply(b"garbage").is_err());
        assert_eq!(snapshot.bytes().unwrap(), expected.as_slice());
    }
}
