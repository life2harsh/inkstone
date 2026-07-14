use std::collections::HashMap;

use tokio::sync::broadcast;
use uuid::Uuid;

use crate::state::DocRoomRegistry;
use inkstone_core::protocol::ServerWsMessage;

pub fn join_or_create(
    rooms: &DocRoomRegistry,
    doc_id: Uuid,
) -> (broadcast::Sender<ServerWsMessage>, broadcast::Receiver<ServerWsMessage>) {
    let mut rooms = rooms.blocking_write();
    let tx = rooms
        .entry(doc_id)
        .or_insert_with(|| {
            let (tx, _) = broadcast::channel(256);
            tx
        })
        .clone();
    let rx = tx.subscribe();
    (tx, rx)
}

pub async fn join_or_create_async(
    rooms: &DocRoomRegistry,
    doc_id: Uuid,
) -> (broadcast::Sender<ServerWsMessage>, broadcast::Receiver<ServerWsMessage>) {
    let mut rooms = rooms.write().await;
    let tx = rooms
        .entry(doc_id)
        .or_insert_with(|| {
            let (tx, _) = broadcast::channel(256);
            tx
        })
        .clone();
    let rx = tx.subscribe();
    (tx, rx)
}

pub fn room_count(rooms: &DocRoomRegistry) -> usize {
    rooms.blocking_read().len()
}

pub async fn room_count_async(rooms: &DocRoomRegistry) -> usize {
    rooms.read().await.len()
}

pub fn remove_empty_room(rooms: &DocRoomRegistry, doc_id: &Uuid) {
    let mut rooms = rooms.blocking_write();
    if let Some(tx) = rooms.get(doc_id) {
        if tx.receiver_count() == 0 {
            rooms.remove(doc_id);
        }
    }
}

pub async fn remove_empty_room_async(rooms: &DocRoomRegistry, doc_id: Uuid) {
    let mut rooms = rooms.write().await;
    if let Some(tx) = rooms.get(&doc_id) {
        if tx.receiver_count() == 0 {
            rooms.remove(&doc_id);
        }
    }
}
