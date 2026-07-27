use std::collections::HashMap;
use std::sync::Arc;

use sqlx::PgPool;
use tokio::sync::{RwLock, Semaphore, broadcast};
use uuid::Uuid;

use crate::config::Config;
use crate::sync::OutboundMessage;

#[derive(Debug, Clone)]
pub struct AppState {
    pub db: PgPool,
    pub config: Config,
    pub doc_rooms: DocRoomRegistry,
    pub argon2_semaphore: Arc<Semaphore>,
    pub session_revocations: broadcast::Sender<Vec<u8>>,
}

pub type DocRoomRegistry =
    Arc<RwLock<HashMap<Uuid, tokio::sync::broadcast::Sender<OutboundMessage>>>>;

impl AppState {
    pub fn new(db: PgPool, config: Config) -> Self {
        let (session_revocations, _) = broadcast::channel(256);
        Self {
            db,
            config,
            doc_rooms: Arc::new(RwLock::new(HashMap::new())),
            argon2_semaphore: Arc::new(Semaphore::new(4)),
            session_revocations,
        }
    }
}
