use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type UserId = Uuid;
pub type DeviceId = Uuid;
pub type WorkspaceId = Uuid;
pub type AssetId = Uuid;
pub type CommentId = Uuid;
pub type IdentityId = Uuid;
pub type InvitationId = Uuid;
pub type UpdateSeq = i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DocId(pub Uuid);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BlockId(pub Uuid);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UpdateId(pub Uuid);

/// A group of strokes in a document's page ink layer that moves as one unit.
/// Its id is also the Stonepen frame id the strokes are parented to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InkGroupId(pub Uuid);

impl DocId {
    pub fn new_v4() -> Self {
        DocId(Uuid::new_v4())
    }
}

impl BlockId {
    pub fn new_v4() -> Self {
        BlockId(Uuid::new_v4())
    }
}

impl UpdateId {
    pub fn new_v4() -> Self {
        UpdateId(Uuid::new_v4())
    }
}

impl InkGroupId {
    pub fn new_v4() -> Self {
        InkGroupId(Uuid::new_v4())
    }
}

impl fmt::Display for DocId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for BlockId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for InkGroupId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for UpdateId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<Uuid> for DocId {
    fn from(u: Uuid) -> Self {
        DocId(u)
    }
}

impl From<Uuid> for BlockId {
    fn from(u: Uuid) -> Self {
        BlockId(u)
    }
}

impl From<Uuid> for UpdateId {
    fn from(u: Uuid) -> Self {
        UpdateId(u)
    }
}

impl From<DocId> for Uuid {
    fn from(id: DocId) -> Self {
        id.0
    }
}

impl From<BlockId> for Uuid {
    fn from(id: BlockId) -> Self {
        id.0
    }
}

impl From<UpdateId> for Uuid {
    fn from(id: UpdateId) -> Self {
        id.0
    }
}
