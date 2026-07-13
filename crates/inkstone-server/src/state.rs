use std::collections::HashMap;
use std::sync::Arc;

use sqlx::PgPool;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::config::Config;
use inkstone_core::protocol::ServerWsMessage;

#[derive(Debug, Clone)]
pub struct AppState {
    pub db: PgPool,
    pub config: Config,
    pub doc_rooms: DocRoomRegistry,
}

pub type DocRoomRegistry = Arc<RwLock<HashMap<Uuid, tokio::sync::broadcast::Sender<ServerWsMessage>>>>;

impl AppState {
    pub fn new(db: PgPool, config: Config) -> Self {
        Self {
            db,
            config,
            doc_rooms: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}
