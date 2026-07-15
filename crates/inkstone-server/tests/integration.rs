mod common;

use common::*;
use inkstone_core::protocol::{
    CreateDocRequest, CreateWorkspaceRequest, PostSnapshotRequest, PostUpdateRequest,
    WorkspaceResponse,
};
use serde_json::Value;
use uuid::Uuid;

// ─── Health ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_health_check() {
    let (base, _pool) = setup().await;
    let resp = client()
        .get(format!("{}/health", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert_eq!(body["version"], "0.1.0");
}

// ─── Workspaces ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_create_workspace() {
    let (base, _pool) = setup().await;

    let resp = client()
        .post(format!("{}/api/workspaces", base))
        .headers(dev_headers())
        .json(&CreateWorkspaceRequest {
            name: "Test Workspace".into(),
            description: Some("A test workspace".into()),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let ws: WorkspaceResponse = resp.json().await.unwrap();
    assert_eq!(ws.name, "Test Workspace");
    assert_eq!(ws.description.as_deref(), Some("A test workspace"));
    assert_eq!(ws.owner_id, dev_user_id());
}

#[tokio::test]
async fn test_create_workspace_no_description() {
    let (base, _pool) = setup().await;

    let resp = client()
        .post(format!("{}/api/workspaces", base))
        .headers(dev_headers())
        .json(&CreateWorkspaceRequest {
            name: "Minimal Workspace".into(),
            description: None,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let ws: WorkspaceResponse = resp.json().await.unwrap();
    assert_eq!(ws.name, "Minimal Workspace");
    assert!(ws.description.is_none());
}

#[tokio::test]
async fn test_list_workspaces() {
    let (base, _pool) = setup().await;

    // Create two workspaces
    for name in &["Alpha", "Beta"] {
        client()
            .post(format!("{}/api/workspaces", base))
            .headers(dev_headers())
            .json(&CreateWorkspaceRequest {
                name: name.to_string(),
                description: None,
            })
            .send()
            .await
            .unwrap();
    }

    let resp = client()
        .get(format!("{}/api/workspaces", base))
        .headers(dev_headers())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(body["total"], 2);
}

#[tokio::test]
async fn test_workspace_auth_required() {
    let (base, _pool) = setup().await;

    // No auth header
    let resp = client()
        .post(format!("{}/api/workspaces", base))
        .json(&CreateWorkspaceRequest {
            name: "Should Fail".into(),
            description: None,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

// ─── Docs ───────────────────────────────────────────────────────────────────

async fn create_ws(base: &str) -> WorkspaceResponse {
    let resp = client()
        .post(format!("{}/api/workspaces", base))
        .headers(dev_headers())
        .json(&CreateWorkspaceRequest {
            name: "Doc Test WS".into(),
            description: None,
        })
        .send()
        .await
        .unwrap();
    resp.json().await.unwrap()
}

fn make_b64_payload(s: &str) -> String {
    b64_encode(s.as_bytes())
}

#[tokio::test]
async fn test_create_doc() {
    let (base, _pool) = setup().await;
    let ws = create_ws(&base).await;

    let resp = client()
        .post(format!("{}/api/workspaces/{}/docs", base, ws.id))
        .headers(dev_headers())
        .json(&CreateDocRequest {
            encrypted_title_b64: make_b64_payload("My Doc"),
            title_nonce_b64: make_b64_payload("123456789012"),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["workspace_id"], ws.id.to_string());
    assert_eq!(body["encrypted_title_b64"], make_b64_payload("My Doc"));
    assert_eq!(body["created_by"], dev_user_id().to_string());
}

#[tokio::test]
async fn test_list_docs() {
    let (base, _pool) = setup().await;
    let ws = create_ws(&base).await;

    // Create two docs
    for title in &["Doc A", "Doc B"] {
        client()
            .post(format!("{}/api/workspaces/{}/docs", base, ws.id))
            .headers(dev_headers())
            .json(&CreateDocRequest {
                encrypted_title_b64: make_b64_payload(title),
                title_nonce_b64: make_b64_payload("123456789012"),
            })
            .send()
            .await
            .unwrap();
    }

    let resp = client()
        .get(format!("{}/api/workspaces/{}/docs", base, ws.id))
        .headers(dev_headers())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["total"], 2);
    assert_eq!(body["items"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn test_get_doc() {
    let (base, _pool) = setup().await;
    let ws = create_ws(&base).await;

    let create_resp = client()
        .post(format!("{}/api/workspaces/{}/docs", base, ws.id))
        .headers(dev_headers())
        .json(&CreateDocRequest {
            encrypted_title_b64: make_b64_payload("Specific Doc"),
            title_nonce_b64: make_b64_payload("123456789012"),
        })
        .send()
        .await
        .unwrap();
    let created: Value = create_resp.json().await.unwrap();
    let doc_id: Uuid = serde_json::from_value(created["id"].clone()).unwrap();

    let resp = client()
        .get(format!("{}/api/docs/{}", base, doc_id))
        .headers(dev_headers())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["id"], doc_id.to_string());
    assert_eq!(body["encrypted_title_b64"], make_b64_payload("Specific Doc"));
}

#[tokio::test]
async fn test_get_doc_not_found() {
    let (base, _pool) = setup().await;
    let fake_id = Uuid::new_v4();

    let resp = client()
        .get(format!("{}/api/docs/{}", base, fake_id))
        .headers(dev_headers())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn test_doc_auth_required() {
    let (base, _pool) = setup().await;
    let ws = create_ws(&base).await;

    // No auth header
    let resp = client()
        .post(format!("{}/api/workspaces/{}/docs", base, ws.id))
        .json(&CreateDocRequest {
            encrypted_title_b64: make_b64_payload("Should Fail"),
            title_nonce_b64: make_b64_payload("123456789012"),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn test_doc_other_user_no_access() {
    let (base, _pool) = setup().await;
    let ws = create_ws(&base).await;

    // Other user tries to list docs in our workspace
    let resp = client()
        .get(format!("{}/api/workspaces/{}/docs", base, ws.id))
        .headers(other_user_headers())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

// ─── Updates ────────────────────────────────────────────────────────────────

async fn create_doc_for_updates(base: &str) -> (WorkspaceResponse, Uuid) {
    let ws = create_ws(base).await;
    let create_resp = client()
        .post(format!("{}/api/workspaces/{}/docs", base, ws.id))
        .headers(dev_headers())
        .json(&CreateDocRequest {
            encrypted_title_b64: make_b64_payload("Updates Doc"),
            title_nonce_b64: make_b64_payload("123456789012"),
        })
        .send()
        .await
        .unwrap();
    let created: Value = create_resp.json().await.unwrap();
    let doc_id: Uuid = serde_json::from_value(created["id"].clone()).unwrap();
    (ws, doc_id)
}

#[tokio::test]
async fn test_post_update() {
    let (base, _pool) = setup().await;
    let (_ws, doc_id) = create_doc_for_updates(&base).await;

    let resp = client()
        .post(format!("{}/api/docs/{}/updates", base, doc_id))
        .headers(dev_headers())
        .json(&PostUpdateRequest {
            encrypted_update_b64: make_b64_payload("update1"),
            nonce_b64: make_b64_payload("nonce12345678"),
            aad_version: 1,
            client_update_id: Uuid::new_v4(),
            sender_device_id: dev_device_id(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["seq"], 1);
}

#[tokio::test]
async fn test_post_update_idempotent() {
    let (base, _pool) = setup().await;
    let (_ws, doc_id) = create_doc_for_updates(&base).await;

    let client_update_id = Uuid::new_v4();

    // First post
    let resp1 = client()
        .post(format!("{}/api/docs/{}/updates", base, doc_id))
        .headers(dev_headers())
        .json(&PostUpdateRequest {
            encrypted_update_b64: make_b64_payload("update1"),
            nonce_b64: make_b64_payload("nonce12345678"),
            aad_version: 1,
            client_update_id,
            sender_device_id: dev_device_id(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(resp1.status(), 200);
    let body1: Value = resp1.json().await.unwrap();
    assert_eq!(body1["seq"], 1);

    // Second post with same client_update_id
    let resp2 = client()
        .post(format!("{}/api/docs/{}/updates", base, doc_id))
        .headers(dev_headers())
        .json(&PostUpdateRequest {
            encrypted_update_b64: make_b64_payload("update1-dup"),
            nonce_b64: make_b64_payload("nonce12345678"),
            aad_version: 1,
            client_update_id,
            sender_device_id: dev_device_id(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(resp2.status(), 200);
    let body2: Value = resp2.json().await.unwrap();
    assert_eq!(body2["seq"], 1); // Same seq, not 2

    // seq 2 should be allocatable with a different client_update_id
    let resp3 = client()
        .post(format!("{}/api/docs/{}/updates", base, doc_id))
        .headers(dev_headers())
        .json(&PostUpdateRequest {
            encrypted_update_b64: make_b64_payload("update2"),
            nonce_b64: make_b64_payload("nonce12345678"),
            aad_version: 1,
            client_update_id: Uuid::new_v4(),
            sender_device_id: dev_device_id(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(resp3.status(), 200);
    let body3: Value = resp3.json().await.unwrap();
    assert_eq!(body3["seq"], 2);
}

#[tokio::test]
async fn test_post_update_requires_device_id() {
    let (base, _pool) = setup().await;
    let (_ws, doc_id) = create_doc_for_updates(&base).await;

    let resp = client()
        .post(format!("{}/api/docs/{}/updates", base, doc_id))
        .headers(dev_user_only_headers()) // no device ID header
        .json(&PostUpdateRequest {
            encrypted_update_b64: make_b64_payload("update"),
            nonce_b64: make_b64_payload("nonce12345678"),
            aad_version: 1,
            client_update_id: Uuid::new_v4(),
            sender_device_id: dev_device_id(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_list_updates() {
    let (base, _pool) = setup().await;
    let (_ws, doc_id) = create_doc_for_updates(&base).await;

    // Post 3 updates
    for i in 1..=3 {
        client()
            .post(format!("{}/api/docs/{}/updates", base, doc_id))
            .headers(dev_headers())
            .json(&PostUpdateRequest {
                encrypted_update_b64: make_b64_payload(&format!("update{}", i)),
                nonce_b64: make_b64_payload("nonce12345678"),
                aad_version: 1,
                client_update_id: Uuid::new_v4(),
                sender_device_id: dev_device_id(),
            })
            .send()
            .await
            .unwrap();
    }

    // List all updates
    let resp = client()
        .get(format!("{}/api/docs/{}/updates", base, doc_id))
        .headers(dev_headers())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["total"], 3);
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["seq"], 1);
    assert_eq!(items[1]["seq"], 2);
    assert_eq!(items[2]["seq"], 3);
}

#[tokio::test]
async fn test_list_updates_after_seq() {
    let (base, _pool) = setup().await;
    let (_ws, doc_id) = create_doc_for_updates(&base).await;

    for i in 1..=5 {
        client()
            .post(format!("{}/api/docs/{}/updates", base, doc_id))
            .headers(dev_headers())
            .json(&PostUpdateRequest {
                encrypted_update_b64: make_b64_payload(&format!("update{}", i)),
                nonce_b64: make_b64_payload("nonce12345678"),
                aad_version: 1,
                client_update_id: Uuid::new_v4(),
                sender_device_id: dev_device_id(),
            })
            .send()
            .await
            .unwrap();
    }

    // Get updates after seq 2
    let resp = client()
        .get(format!(
            "{}/api/docs/{}/updates?after_seq=2",
            base, doc_id
        ))
        .headers(dev_headers())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["total"], 5);
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["seq"], 3);
    assert_eq!(items[1]["seq"], 4);
    assert_eq!(items[2]["seq"], 5);
}

#[tokio::test]
async fn test_list_updates_pagination() {
    let (base, _pool) = setup().await;
    let (_ws, doc_id) = create_doc_for_updates(&base).await;

    for i in 1..=5 {
        client()
            .post(format!("{}/api/docs/{}/updates", base, doc_id))
            .headers(dev_headers())
            .json(&PostUpdateRequest {
                encrypted_update_b64: make_b64_payload(&format!("update{}", i)),
                nonce_b64: make_b64_payload("nonce12345678"),
                aad_version: 1,
                client_update_id: Uuid::new_v4(),
                sender_device_id: dev_device_id(),
            })
            .send()
            .await
            .unwrap();
    }

    // Get updates with limit 2
    let resp = client()
        .get(format!(
            "{}/api/docs/{}/updates?limit=2",
            base, doc_id
        ))
        .headers(dev_headers())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["total"], 5);
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["seq"], 1);
    assert_eq!(items[1]["seq"], 2);
}

// ─── Snapshots ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_post_and_get_snapshot() {
    let (base, _pool) = setup().await;
    let (_ws, doc_id) = create_doc_for_updates(&base).await;

    // Post a snapshot
    let post_resp = client()
        .post(format!("{}/api/docs/{}/snapshot", base, doc_id))
        .headers(dev_headers())
        .json(&PostSnapshotRequest {
            encrypted_snapshot_b64: make_b64_payload("snapshot1"),
            nonce_b64: make_b64_payload("nonce12345678"),
            snapshot_version: 1,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(post_resp.status(), 200);
    let post_body: Value = post_resp.json().await.unwrap();
    assert_eq!(post_body["snapshot_version"], 1);
    assert_eq!(
        post_body["encrypted_snapshot_b64"],
        make_b64_payload("snapshot1")
    );

    // Get the snapshot
    let get_resp = client()
        .get(format!("{}/api/docs/{}/snapshot", base, doc_id))
        .headers(dev_headers())
        .send()
        .await
        .unwrap();
    assert_eq!(get_resp.status(), 200);
    let get_body: Value = get_resp.json().await.unwrap();
    assert_eq!(get_body["snapshot_version"], 1);
    assert_eq!(
        get_body["encrypted_snapshot_b64"],
        make_b64_payload("snapshot1")
    );
}

#[tokio::test]
async fn test_get_snapshot_not_found() {
    let (base, _pool) = setup().await;
    let (_ws, doc_id) = create_doc_for_updates(&base).await;

    let resp = client()
        .get(format!("{}/api/docs/{}/snapshot", base, doc_id))
        .headers(dev_headers())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_snapshot_upsert() {
    let (base, _pool) = setup().await;
    let (_ws, doc_id) = create_doc_for_updates(&base).await;

    // Post same version twice
    for _ in 0..2 {
        client()
            .post(format!("{}/api/docs/{}/snapshot", base, doc_id))
            .headers(dev_headers())
            .json(&PostSnapshotRequest {
                encrypted_snapshot_b64: make_b64_payload("updated"),
                nonce_b64: make_b64_payload("nonce12345678"),
                snapshot_version: 1,
            })
            .send()
            .await
            .unwrap();
    }

    // Get should return latest (upserted) value
    let get_resp = client()
        .get(format!("{}/api/docs/{}/snapshot", base, doc_id))
        .headers(dev_headers())
        .send()
        .await
        .unwrap();
    let body: Value = get_resp.json().await.unwrap();
    assert_eq!(body["snapshot_version"], 1);
}

// ─── Auth / Error Cases ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_no_auth_header_returns_401() {
    let (base, _pool) = setup().await;

    let resp = client()
        .get(format!("{}/api/workspaces", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], 401);
}

#[tokio::test]
async fn test_invalid_uuid_in_header_returns_401() {
    let (base, _pool) = setup().await;

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "x-dev-user-id",
        reqwest::header::HeaderValue::from_static("not-a-uuid"),
    );

    let resp = client()
        .get(format!("{}/api/workspaces", base))
        .headers(headers)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn test_sender_device_id_mismatch() {
    let (base, _pool) = setup().await;
    let (_ws, doc_id) = create_doc_for_updates(&base).await;

    let resp = client()
        .post(format!("{}/api/docs/{}/updates", base, doc_id))
        .headers(dev_headers())
        .json(&PostUpdateRequest {
            encrypted_update_b64: make_b64_payload("update"),
            nonce_b64: make_b64_payload("nonce12345678"),
            aad_version: 1,
            client_update_id: Uuid::new_v4(),
            sender_device_id: Uuid::new_v4(), // different from header
        })
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}
