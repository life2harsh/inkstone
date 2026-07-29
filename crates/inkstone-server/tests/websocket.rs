mod common;

use common::*;
use futures_util::{SinkExt, StreamExt};
use inkstone_core::UpdateId;
use inkstone_core::protocol::{ClientWsMessage, ServerWsMessage};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use uuid::Uuid;

/// Connect and consume the Ready message that every successful WS connection
/// receives before any operation-specific messages.
async fn connect_and_ready(
    base: &str,
    doc_id: Uuid,
) -> (
    impl SinkExt<WsMessage, Error = impl std::error::Error>,
    impl StreamExt<Item = Result<WsMessage, impl std::error::Error>>,
) {
    let (ws_stream, _) = connect_async(&ws_url(base, doc_id))
        .await
        .expect("Failed to connect to WS");
    let (write, mut read) = ws_stream.split();

    // Consume Ready
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), read.next())
        .await
        .expect("Timeout waiting for Ready")
        .expect("Stream ended")
        .expect("Error reading from WS");
    let text = response.into_text().expect("Expected text message");
    let server_msg: ServerWsMessage =
        serde_json::from_str(&text).expect("Failed to parse server message");
    match server_msg {
        ServerWsMessage::Ready {
            doc_id: received_doc_id,
            current_seq,
        } => {
            assert_eq!(received_doc_id, doc_id, "Ready doc_id mismatch");
            // current_seq should be 0 for a fresh document
            assert_eq!(current_seq, 0, "Fresh doc should have current_seq=0");
        }
        other => panic!("Expected Ready as first message, got: {:?}", other),
    }

    (write, read)
}

async fn create_workspace_and_doc(base: &str) -> (Uuid, Uuid) {
    let ws = client()
        .post(format!("{}/api/workspaces", base))
        .headers(dev_headers())
        .json(&serde_json::json!({
            "workspace_id": Uuid::new_v4(),
            "encrypted_metadata_b64": b64_encode(&[1; 16]),
            "metadata_nonce_b64": b64_encode(&[2; 12]),
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let ws_id: Uuid = serde_json::from_value(ws["id"].clone()).unwrap();

    let doc = client()
        .post(format!("{}/api/workspaces/{}/docs", base, ws_id))
        .headers(dev_headers())
        .json(&serde_json::json!({
            "doc_id": Uuid::new_v4(),
            "encrypted_title_b64": b64_encode(b"encrypted WS Test Doc"),
            "title_nonce_b64": b64_encode(b"123456789012"),
            "encrypted_path_b64": b64_encode(b"encrypted WS Test Path"),
            "path_nonce_b64": b64_encode(b"abcdefghijkl"),
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let doc_id: Uuid = serde_json::from_value(doc["id"].clone()).unwrap();
    (ws_id, doc_id)
}

fn ws_url(base: &str, doc_id: Uuid) -> String {
    format!(
        "{}/api/sync/docs/{}/ws?dev_user_id={}&dev_device_id={}",
        base.replace("http", "ws"),
        doc_id,
        dev_user_id(),
        dev_device_id(),
    )
}

#[tokio::test]
async fn test_ws_send_update_and_receive_broadcast() {
    let (base, _pool) = setup().await;
    let (_ws_id, doc_id) = create_workspace_and_doc(&base).await;

    let (mut write, mut read) = connect_and_ready(&base, doc_id).await;

    let update_id = UpdateId(Uuid::new_v4());
    let msg = ClientWsMessage::EncryptedUpdate {
        doc_id,
        update_id,
        encrypted_update_b64: b64_encode(b"encrypted-ws-update"),
        nonce_b64: b64_encode(b"nonce1234567"),
        aad_version: 1,
    };
    let json = serde_json::to_string(&msg).unwrap();
    write
        .send(tokio_tungstenite::tungstenite::Message::Text(json.into()))
        .await
        .unwrap();

    // We should receive the broadcast (includes sender in this room-of-one)
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), read.next())
        .await
        .expect("Timeout waiting for WS response")
        .expect("Stream ended")
        .expect("Error reading from WS");

    let text = response.into_text().expect("Expected text message");
    let server_msg: ServerWsMessage =
        serde_json::from_str(&text).expect("Failed to parse server message");

    match server_msg {
        ServerWsMessage::EncryptedUpdate {
            doc_id: received_doc_id,
            sender_device_id,
            update_id: received_client_id,
            seq,
            encrypted_update_b64,
            ..
        } => {
            assert_eq!(received_doc_id, doc_id);
            assert_eq!(sender_device_id, dev_device_id());
            assert_eq!(received_client_id, update_id);
            assert_eq!(seq, 1);
            assert_eq!(encrypted_update_b64, b64_encode(b"encrypted-ws-update"));
        }
        other => panic!("Expected EncryptedUpdate, got: {:?}", other),
    }
}

#[tokio::test]
async fn test_http_update_is_broadcast_to_websocket_room() {
    let (base, _pool) = setup().await;
    let (_ws_id, doc_id) = create_workspace_and_doc(&base).await;
    let (_write, mut read) = connect_and_ready(&base, doc_id).await;
    let update_id = UpdateId(Uuid::new_v4());

    let response = client()
        .post(format!("{}/api/docs/{}/updates", base, doc_id))
        .headers(dev_headers())
        .json(&inkstone_core::protocol::PostUpdateRequest {
            encrypted_update_b64: b64_encode(b"encrypted-http-update"),
            nonce_b64: b64_encode(b"nonce1234567"),
            aad_version: 1,
            update_id,
            sender_device_id: dev_device_id(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);

    let message = tokio::time::timeout(std::time::Duration::from_secs(5), read.next())
        .await
        .expect("Timeout waiting for HTTP update broadcast")
        .expect("Stream ended")
        .expect("WebSocket error");
    let server_message: ServerWsMessage =
        serde_json::from_str(&message.into_text().unwrap()).unwrap();
    assert!(matches!(
        server_message,
        ServerWsMessage::EncryptedUpdate {
            update_id: received,
            seq: 1,
            ..
        } if received == update_id
    ));

    let retry = client()
        .post(format!("{}/api/docs/{}/updates", base, doc_id))
        .headers(dev_headers())
        .json(&inkstone_core::protocol::PostUpdateRequest {
            encrypted_update_b64: b64_encode(b"randomized-retry-payload"),
            nonce_b64: b64_encode(b"retry1234567"),
            aad_version: 1,
            update_id,
            sender_device_id: dev_device_id(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(retry.status(), 200);

    let retry_message = tokio::time::timeout(std::time::Duration::from_secs(5), read.next())
        .await
        .expect("Timeout waiting for retry broadcast")
        .expect("Stream ended")
        .expect("WebSocket error");
    let retry_message: ServerWsMessage =
        serde_json::from_str(&retry_message.into_text().unwrap()).unwrap();
    assert!(matches!(
        retry_message,
        ServerWsMessage::EncryptedUpdate {
            update_id: received,
            seq: 1,
            encrypted_update_b64,
            nonce_b64,
            ..
        } if received == update_id
            && encrypted_update_b64 == b64_encode(b"encrypted-http-update")
            && nonce_b64 == b64_encode(b"nonce1234567")
    ));
}

#[tokio::test]
async fn test_ws_ping_pong() {
    let (base, _pool) = setup().await;
    let (_ws_id, doc_id) = create_workspace_and_doc(&base).await;

    let (mut write, mut read) = connect_and_ready(&base, doc_id).await;

    let msg = ClientWsMessage::Ping;
    let json = serde_json::to_string(&msg).unwrap();
    write
        .send(tokio_tungstenite::tungstenite::Message::Text(json.into()))
        .await
        .unwrap();

    let response = tokio::time::timeout(std::time::Duration::from_secs(5), read.next())
        .await
        .expect("Timeout waiting for Pong")
        .expect("Stream ended")
        .expect("Error reading from WS");

    let text = response.into_text().expect("Expected text message");
    let server_msg: ServerWsMessage =
        serde_json::from_str(&text).expect("Failed to parse server message");

    assert!(matches!(server_msg, ServerWsMessage::Pong));
}

#[tokio::test]
async fn test_ws_doc_id_mismatch() {
    let (base, _pool) = setup().await;
    let (_ws_id, doc_id) = create_workspace_and_doc(&base).await;

    let (mut write, mut read) = connect_and_ready(&base, doc_id).await;

    // Send update with a different doc_id than the path
    let wrong_doc_id = Uuid::new_v4();
    let msg = ClientWsMessage::EncryptedUpdate {
        doc_id: wrong_doc_id,
        update_id: UpdateId(Uuid::new_v4()),
        encrypted_update_b64: b64_encode(b"encrypted-wrong-doc"),
        nonce_b64: b64_encode(b"nonce1234567"),
        aad_version: 1,
    };
    let json = serde_json::to_string(&msg).unwrap();
    write
        .send(tokio_tungstenite::tungstenite::Message::Text(json.into()))
        .await
        .unwrap();

    let response = tokio::time::timeout(std::time::Duration::from_secs(5), read.next())
        .await
        .expect("Timeout waiting for error")
        .expect("Stream ended")
        .expect("Error reading from WS");

    let text = response.into_text().expect("Expected text message");
    let server_msg: ServerWsMessage =
        serde_json::from_str(&text).expect("Failed to parse server message");

    match server_msg {
        ServerWsMessage::Error { message } => {
            assert!(message.contains("doc_id"));
        }
        other => panic!("Expected Error, got: {:?}", other),
    }
}

#[tokio::test]
async fn test_ws_invalid_json() {
    let (base, _pool) = setup().await;
    let (_ws_id, doc_id) = create_workspace_and_doc(&base).await;

    let (mut write, mut read) = connect_and_ready(&base, doc_id).await;

    write
        .send(tokio_tungstenite::tungstenite::Message::Text(
            "not json".into(),
        ))
        .await
        .unwrap();

    let response = tokio::time::timeout(std::time::Duration::from_secs(5), read.next())
        .await
        .expect("Timeout waiting for error")
        .expect("Stream ended")
        .expect("Error reading from WS");

    let text = response.into_text().expect("Expected text message");
    let server_msg: ServerWsMessage =
        serde_json::from_str(&text).expect("Failed to parse server message");

    match server_msg {
        ServerWsMessage::Error { message } => {
            assert!(message.contains("Unknown message type"));
        }
        other => panic!("Expected Error, got: {:?}", other),
    }
}

#[tokio::test]
async fn test_ws_invalid_base64() {
    let (base, _pool) = setup().await;
    let (_ws_id, doc_id) = create_workspace_and_doc(&base).await;

    let (mut write, mut read) = connect_and_ready(&base, doc_id).await;

    let msg = ClientWsMessage::EncryptedUpdate {
        doc_id,
        update_id: UpdateId(Uuid::new_v4()),
        encrypted_update_b64: "not-valid-base64!!".into(),
        nonce_b64: b64_encode(b"nonce1234567"),
        aad_version: 1,
    };
    let json = serde_json::to_string(&msg).unwrap();
    write
        .send(tokio_tungstenite::tungstenite::Message::Text(json.into()))
        .await
        .unwrap();

    let response = tokio::time::timeout(std::time::Duration::from_secs(5), read.next())
        .await
        .expect("Timeout waiting for error")
        .expect("Stream ended")
        .expect("Error reading from WS");

    let text = response.into_text().expect("Expected text message");
    let server_msg: ServerWsMessage =
        serde_json::from_str(&text).expect("Failed to parse server message");

    match server_msg {
        ServerWsMessage::Error { message } => {
            assert!(message.contains("encrypted_update_b64"));
        }
        other => panic!("Expected Error, got: {:?}", other),
    }
}

#[tokio::test]
async fn test_ws_presence() {
    let (base, _pool) = setup().await;
    let (_ws_id, doc_id) = create_workspace_and_doc(&base).await;

    let (mut write, mut read) = connect_and_ready(&base, doc_id).await;

    let msg = ClientWsMessage::EncryptedPresence {
        doc_id,
        encrypted_presence_b64: b64_encode(b"encrypted-online"),
        nonce_b64: b64_encode(b"123456789012"),
        aad_version: 1,
        counter: 1,
    };
    let json = serde_json::to_string(&msg).unwrap();
    write
        .send(tokio_tungstenite::tungstenite::Message::Text(json.into()))
        .await
        .unwrap();

    let response = tokio::time::timeout(std::time::Duration::from_secs(5), read.next())
        .await
        .expect("Timeout waiting for presence broadcast")
        .expect("Stream ended")
        .expect("Error reading from WS");

    let text = response.into_text().expect("Expected text message");
    let server_msg: serde_json::Value =
        serde_json::from_str(&text).expect("Failed to parse server message");
    assert_eq!(server_msg["type"], "encrypted_presence");
    assert_eq!(server_msg["doc_id"], doc_id.to_string());
    assert_eq!(server_msg["sender_device_id"], dev_device_id().to_string());
    assert_eq!(
        server_msg["encrypted_presence_b64"],
        b64_encode(b"encrypted-online")
    );
    assert_eq!(server_msg["nonce_b64"], b64_encode(b"123456789012"));
    assert_eq!(server_msg["aad_version"], 1);
    assert_eq!(server_msg["counter"], 1);
}

#[tokio::test]
async fn encrypted_presence_rate_limit_is_separate_from_durable_updates() {
    let (base, _pool) = setup().await;
    let (_ws_id, doc_id) = create_workspace_and_doc(&base).await;
    let (mut write, mut read) = connect_and_ready(&base, doc_id).await;

    for counter in 1..=61 {
        let message = ClientWsMessage::EncryptedPresence {
            doc_id,
            encrypted_presence_b64: b64_encode(b"encrypted-presence"),
            nonce_b64: b64_encode(b"123456789012"),
            aad_version: 1,
            counter,
        };
        write
            .send(WsMessage::Text(
                serde_json::to_string(&message).unwrap().into(),
            ))
            .await
            .unwrap();
    }
    let mut presence_frames = 0;
    let mut rate_errors = 0;
    for _ in 0..61 {
        let message = tokio::time::timeout(std::time::Duration::from_secs(5), read.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&message.into_text().unwrap()).unwrap();
        match value["type"].as_str() {
            Some("encrypted_presence") => presence_frames += 1,
            Some("error")
                if value["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("Presence rate")) =>
            {
                rate_errors += 1
            }
            other => panic!("unexpected relay frame: {other:?}"),
        }
    }
    assert_eq!(presence_frames, 60);
    assert_eq!(rate_errors, 1);

    let update_id = UpdateId::new_v4();
    write
        .send(WsMessage::Text(
            serde_json::to_string(&ClientWsMessage::EncryptedUpdate {
                doc_id,
                update_id,
                encrypted_update_b64: b64_encode(b"encrypted-durable-after-presence-limit"),
                nonce_b64: b64_encode(b"123456789012"),
                aad_version: 1,
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    let durable = tokio::time::timeout(std::time::Duration::from_secs(5), read.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let durable: ServerWsMessage = serde_json::from_str(&durable.into_text().unwrap()).unwrap();
    assert!(
        matches!(durable, ServerWsMessage::EncryptedUpdate { update_id: received, .. } if received == update_id)
    );
}

#[tokio::test]
async fn test_ws_auth_required() {
    let (base, _pool) = setup().await;
    let (_ws_id, doc_id) = create_workspace_and_doc(&base).await;

    // Connect without auth params
    let bad_url = format!("{}/api/sync/docs/{}/ws", base.replace("http", "ws"), doc_id,);
    let result = connect_async(&bad_url).await;
    // Should fail with 400 or similar
    assert!(result.is_err(), "Expected connection to fail without auth");
}

#[tokio::test]
async fn test_ws_echo_suppression() {
    let (base, _pool) = setup().await;
    let (_ws_id, doc_id) = create_workspace_and_doc(&base).await;

    let (mut write, mut read) = connect_and_ready(&base, doc_id).await;

    let update_id = UpdateId(Uuid::new_v4());
    let msg = ClientWsMessage::EncryptedUpdate {
        doc_id,
        update_id,
        encrypted_update_b64: b64_encode(b"encrypted-echo-test"),
        nonce_b64: b64_encode(b"nonce1234567"),
        aad_version: 1,
    };
    let json = serde_json::to_string(&msg).unwrap();
    write
        .send(tokio_tungstenite::tungstenite::Message::Text(json.into()))
        .await
        .unwrap();

    // Receive the broadcast (own-echo now serves as the acknowledgement path)
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), read.next())
        .await
        .expect("Timeout")
        .expect("Stream ended")
        .expect("Error");

    let text = response.into_text().expect("Expected text");
    let server_msg: ServerWsMessage = serde_json::from_str(&text).expect("Failed to parse");

    match server_msg {
        ServerWsMessage::EncryptedUpdate {
            update_id: received_cuid,
            seq,
            ..
        } => {
            // Own update echoed back as acknowledgement
            assert_eq!(received_cuid, update_id);
            assert_eq!(seq, 1);
        }
        other => panic!("Expected EncryptedUpdate, got: {:?}", other),
    }

    // Proves a connection whose document already has committed history still receives Ready with nonzero seq
    let (ws_stream2, _) = connect_async(&ws_url(&base, doc_id))
        .await
        .expect("Failed to connect client 2");
    let (_, mut read2) = ws_stream2.split();

    let response2 = tokio::time::timeout(std::time::Duration::from_secs(5), read2.next())
        .await
        .expect("Timeout waiting for Ready from client 2")
        .expect("Stream ended")
        .expect("Error reading from WS");
    let text2 = response2.into_text().expect("Expected text message");
    let server_msg2: ServerWsMessage =
        serde_json::from_str(&text2).expect("Failed to parse server message");
    match server_msg2 {
        ServerWsMessage::Ready {
            doc_id: received_doc_id,
            current_seq,
        } => {
            assert_eq!(received_doc_id, doc_id, "Ready doc_id mismatch");
            assert_eq!(current_seq, 1, "Expected current_seq to be 1 (nonzero)");
        }
        other => panic!(
            "Expected Ready as first message for client 2, got: {:?}",
            other
        ),
    }
}
