mod common;

use common::*;
use inkstone_core::UpdateId;
use inkstone_core::protocol::{
    CreateDocRequest, CreateWorkspaceRequest, PostSnapshotRequest, PostUpdateRequest,
    UpdateDocPathProjectionRequest, UpdateDocTitleProjectionRequest, WorkspaceResponse,
};
use serde_json::Value;
use uuid::Uuid;

fn workspace_request() -> CreateWorkspaceRequest {
    CreateWorkspaceRequest {
        workspace_id: Uuid::new_v4(),
        encrypted_metadata_b64: b64_encode(&[1; 16]),
        metadata_nonce_b64: b64_encode(&[2; 12]),
    }
}

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
        .json(&workspace_request())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let ws: WorkspaceResponse = resp.json().await.unwrap();
    assert_eq!(ws.metadata_version, 1);
    assert_eq!(ws.encrypted_metadata_b64, b64_encode(&[1; 16]));
    assert_eq!(ws.owner_id, dev_user_id());
}

#[tokio::test]
async fn test_create_workspace_no_description() {
    let (base, _pool) = setup().await;

    let resp = client()
        .post(format!("{}/api/workspaces", base))
        .headers(dev_headers())
        .json(&workspace_request())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let ws: WorkspaceResponse = resp.json().await.unwrap();
    assert_eq!(ws.metadata_nonce_b64, b64_encode(&[2; 12]));
}

#[tokio::test]
async fn test_list_workspaces() {
    let (base, _pool) = setup().await;

    // Create two workspaces
    for _ in &["Alpha", "Beta"] {
        client()
            .post(format!("{}/api/workspaces", base))
            .headers(dev_headers())
            .json(&workspace_request())
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
        .json(&workspace_request())
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
        .json(&workspace_request())
        .send()
        .await
        .unwrap();
    resp.json().await.unwrap()
}

#[tokio::test]
async fn encrypted_asset_chunks_are_opaque_authorized_and_idempotent() {
    let (base, pool) = setup().await;
    let workspace = create_ws(&base).await;
    let asset_id = Uuid::new_v4();
    let created = client()
        .post(format!("{}/api/workspaces/{}/assets", base, workspace.id))
        .headers(dev_headers())
        .json(&serde_json::json!({
            "asset_id": asset_id,
            "version": 1,
            "chunk_count": 1,
            "encrypted_size": 16,
            "encrypted_metadata_b64": b64_encode(&[7; 16]),
            "metadata_nonce_b64": b64_encode(&[8; 12]),
            "aad_version": 1
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 200);
    let endpoint = format!("{}/api/assets/{}/versions/1/chunks/0", base, asset_id);
    for _ in 0..2 {
        let response = client()
            .put(&endpoint)
            .headers(dev_headers())
            .json(&serde_json::json!({
                "ciphertext_b64": b64_encode(&[9; 16]),
                "nonce_b64": b64_encode(&[10; 12]),
                "aad_version": 1
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 204);
    }
    let denied = client()
        .get(&endpoint)
        .headers(other_user_headers())
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 403);
    let stored: (Vec<u8>, Vec<u8>) = sqlx::query_as(
        "SELECT encrypted_metadata,ciphertext FROM assets JOIN asset_chunks ON asset_chunks.asset_id=assets.id WHERE assets.id=$1"
    ).bind(asset_id).fetch_one(&pool).await.unwrap();
    assert_eq!(stored.0, vec![7; 16]);
    assert_eq!(stored.1, vec![9; 16]);
}

#[tokio::test]
async fn encrypted_asset_limits_are_rejected_before_storage() {
    let (base, _pool) = setup().await;
    let workspace = create_ws(&base).await;
    let response = client()
        .post(format!("{}/api/workspaces/{}/assets", base, workspace.id))
        .headers(dev_headers())
        .json(&serde_json::json!({
            "asset_id": Uuid::new_v4(), "version": 1, "chunk_count": 257,
            "encrypted_size": 16, "encrypted_metadata_b64": b64_encode(&[1; 16]),
            "metadata_nonce_b64": b64_encode(&[2; 12]), "aad_version": 1
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
}

fn make_b64_payload(s: &str) -> String {
    b64_encode(s.as_bytes())
}

fn make_encrypted_payload(s: &str) -> String {
    let mut bytes = s.as_bytes().to_vec();
    bytes.resize(bytes.len().max(16), 0);
    b64_encode(&bytes)
}

#[tokio::test]
async fn test_create_doc() {
    let (base, _pool) = setup().await;
    let ws = create_ws(&base).await;

    let resp = client()
        .post(format!("{}/api/workspaces/{}/docs", base, ws.id))
        .headers(dev_headers())
        .json(&CreateDocRequest {
            doc_id: Uuid::new_v4(),
            encrypted_title_b64: Some(make_encrypted_payload("My Doc")),
            title_nonce_b64: Some(make_b64_payload("123456789012")),
            encrypted_path_b64: make_encrypted_payload("My.md"),
            path_nonce_b64: make_b64_payload("abcdefghijkl"),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["workspace_id"], ws.id.to_string());
    assert_eq!(
        body["encrypted_title_b64"],
        make_encrypted_payload("My Doc")
    );
    assert_eq!(body["created_by"], dev_user_id().to_string());
}

#[tokio::test]
async fn test_create_doc_client_id_is_idempotent_and_conflict_checked() {
    let (base, _pool) = setup().await;
    let ws = create_ws(&base).await;
    let doc_id = Uuid::new_v4();
    let request = CreateDocRequest {
        doc_id,
        encrypted_title_b64: Some(make_encrypted_payload("idempotent title")),
        title_nonce_b64: Some(make_b64_payload("123456789012")),
        encrypted_path_b64: make_encrypted_payload("idempotent.md"),
        path_nonce_b64: make_b64_payload("abcdefghijkl"),
    };
    for _ in 0..2 {
        let response = client()
            .post(format!("{}/api/workspaces/{}/docs", base, ws.id))
            .headers(dev_headers())
            .json(&request)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.json::<Value>().await.unwrap()["id"],
            doc_id.to_string()
        );
    }

    let randomized_title_retry = client()
        .post(format!("{}/api/workspaces/{}/docs", base, ws.id))
        .headers(dev_headers())
        .json(&CreateDocRequest {
            doc_id,
            encrypted_title_b64: Some(make_encrypted_payload("randomized retry title")),
            title_nonce_b64: Some(make_b64_payload("mnopqrstuvwx")),
            encrypted_path_b64: request.encrypted_path_b64.clone(),
            path_nonce_b64: request.path_nonce_b64.clone(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(randomized_title_retry.status(), 200);

    let conflict = client()
        .post(format!("{}/api/workspaces/{}/docs", base, ws.id))
        .headers(dev_headers())
        .json(&CreateDocRequest {
            doc_id,
            encrypted_title_b64: Some(make_encrypted_payload("different title")),
            title_nonce_b64: Some(make_b64_payload("123456789012")),
            encrypted_path_b64: make_encrypted_payload("different.md"),
            path_nonce_b64: make_b64_payload("abcdefghijkl"),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(conflict.status(), 409);
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
                doc_id: Uuid::new_v4(),
                encrypted_title_b64: Some(make_encrypted_payload(title)),
                title_nonce_b64: Some(make_b64_payload("123456789012")),
                encrypted_path_b64: make_encrypted_payload(&format!("{title}.md")),
                path_nonce_b64: make_b64_payload("abcdefghijkl"),
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
            doc_id: Uuid::new_v4(),
            encrypted_title_b64: Some(make_encrypted_payload("Specific Doc")),
            title_nonce_b64: Some(make_b64_payload("123456789012")),
            encrypted_path_b64: make_encrypted_payload("Specific.md"),
            path_nonce_b64: make_b64_payload("abcdefghijkl"),
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
    assert_eq!(
        body["encrypted_title_b64"],
        make_encrypted_payload("Specific Doc")
    );
}

#[tokio::test]
async fn test_path_and_monotonic_title_projections_are_independent() {
    let (base, _pool) = setup().await;
    let (_workspace, doc_id) = create_doc_for_updates(&base).await;
    let original_title = make_encrypted_payload("Updates Doc");
    let replacement_path = make_encrypted_payload("Renamed.md");

    let path_response = client()
        .patch(format!("{base}/api/docs/{doc_id}"))
        .headers(dev_headers())
        .json(&UpdateDocPathProjectionRequest {
            encrypted_path_b64: replacement_path.clone(),
            path_nonce_b64: make_b64_payload("abcdefghijkl"),
            projection_version: 1,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(path_response.status(), 200);
    let path_body: Value = path_response.json().await.unwrap();
    assert_eq!(path_body["encrypted_path_b64"], replacement_path);
    assert_eq!(path_body["encrypted_title_b64"], original_title);

    let newest_path = make_encrypted_payload("Newest.md");
    let newer_path_response = client()
        .patch(format!("{base}/api/docs/{doc_id}"))
        .headers(dev_headers())
        .json(&UpdateDocPathProjectionRequest {
            encrypted_path_b64: newest_path.clone(),
            path_nonce_b64: make_b64_payload("mnopqrstuvwx"),
            projection_version: 2,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(newer_path_response.status(), 200);

    let stale_path_response = client()
        .patch(format!("{base}/api/docs/{doc_id}"))
        .headers(dev_headers())
        .json(&UpdateDocPathProjectionRequest {
            encrypted_path_b64: make_encrypted_payload("Stale.md"),
            path_nonce_b64: make_b64_payload("abcdefghijkl"),
            projection_version: 1,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(stale_path_response.status(), 200);
    assert_eq!(
        stale_path_response.json::<Value>().await.unwrap()["encrypted_path_b64"],
        newest_path
    );

    let update = client()
        .post(format!("{base}/api/docs/{doc_id}/updates"))
        .headers(dev_headers())
        .json(&PostUpdateRequest {
            encrypted_update_b64: make_encrypted_payload("canonical title update"),
            nonce_b64: make_b64_payload("123456789012"),
            aad_version: 1,
            update_id: UpdateId::new_v4(),
            sender_device_id: dev_device_id(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(update.status(), 200);

    let newest_title = make_encrypted_payload("Newest title");
    let title_response = client()
        .patch(format!("{base}/api/docs/{doc_id}/title"))
        .headers(dev_headers())
        .json(&UpdateDocTitleProjectionRequest {
            encrypted_title_b64: newest_title.clone(),
            title_nonce_b64: make_b64_payload("mnopqrstuvwx"),
            base_seq: 1,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(title_response.status(), 200);
    let title_body: Value = title_response.json().await.unwrap();
    assert_eq!(title_body["encrypted_title_b64"], newest_title);
    assert_eq!(title_body["encrypted_path_b64"], newest_path);
    assert_eq!(title_body["title_projection_seq"], 1);

    let stale = client()
        .patch(format!("{base}/api/docs/{doc_id}/title"))
        .headers(dev_headers())
        .json(&UpdateDocTitleProjectionRequest {
            encrypted_title_b64: make_encrypted_payload("Stale title"),
            title_nonce_b64: make_b64_payload("abcdefghijkl"),
            base_seq: 0,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), 200);
    assert_eq!(
        stale.json::<Value>().await.unwrap()["encrypted_title_b64"],
        newest_title
    );

    let future = client()
        .patch(format!("{base}/api/docs/{doc_id}/title"))
        .headers(dev_headers())
        .json(&UpdateDocTitleProjectionRequest {
            encrypted_title_b64: make_encrypted_payload("Future title"),
            title_nonce_b64: make_b64_payload("abcdefghijkl"),
            base_seq: 2,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(future.status(), 400);
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
    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn test_doc_auth_required() {
    let (base, _pool) = setup().await;
    let ws = create_ws(&base).await;

    // No auth header
    let resp = client()
        .post(format!("{}/api/workspaces/{}/docs", base, ws.id))
        .json(&CreateDocRequest {
            doc_id: Uuid::new_v4(),
            encrypted_title_b64: Some(make_encrypted_payload("Should Fail")),
            title_nonce_b64: Some(make_b64_payload("123456789012")),
            encrypted_path_b64: make_encrypted_payload("Failure.md"),
            path_nonce_b64: make_b64_payload("abcdefghijkl"),
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
    assert_eq!(resp.status(), 403);
}

// ─── Updates ────────────────────────────────────────────────────────────────

async fn create_doc_for_updates(base: &str) -> (WorkspaceResponse, Uuid) {
    let ws = create_ws(base).await;
    let create_resp = client()
        .post(format!("{}/api/workspaces/{}/docs", base, ws.id))
        .headers(dev_headers())
        .json(&CreateDocRequest {
            doc_id: Uuid::new_v4(),
            encrypted_title_b64: Some(make_encrypted_payload("Updates Doc")),
            title_nonce_b64: Some(make_b64_payload("123456789012")),
            encrypted_path_b64: make_encrypted_payload("Updates.md"),
            path_nonce_b64: make_b64_payload("abcdefghijkl"),
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
            encrypted_update_b64: make_encrypted_payload("update1"),
            nonce_b64: make_b64_payload("nonce1234567"),
            aad_version: 1,
            update_id: UpdateId(Uuid::new_v4()),
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
    let (base, pool) = setup().await;
    let (_ws, doc_id) = create_doc_for_updates(&base).await;

    let update_id = UpdateId(Uuid::new_v4());

    // First post
    let resp1 = client()
        .post(format!("{}/api/docs/{}/updates", base, doc_id))
        .headers(dev_headers())
        .json(&PostUpdateRequest {
            encrypted_update_b64: make_encrypted_payload("update1"),
            nonce_b64: make_b64_payload("nonce1234567"),
            aad_version: 1,
            update_id,
            sender_device_id: dev_device_id(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(resp1.status(), 200);
    let body1: Value = resp1.json().await.unwrap();
    assert_eq!(body1["seq"], 1);

    let resp2 = client()
        .post(format!("{}/api/docs/{}/updates", base, doc_id))
        .headers(dev_headers())
        .json(&PostUpdateRequest {
            encrypted_update_b64: make_encrypted_payload("update1"),
            nonce_b64: make_b64_payload("nonce1234567"),
            aad_version: 1,
            update_id,
            sender_device_id: dev_device_id(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(resp2.status(), 200);
    let body2: Value = resp2.json().await.unwrap();
    assert_eq!(body2["seq"], 1);

    let randomized_retry = client()
        .post(format!("{}/api/docs/{}/updates", base, doc_id))
        .headers(dev_headers())
        .json(&PostUpdateRequest {
            encrypted_update_b64: make_encrypted_payload("update1-dup"),
            nonce_b64: make_b64_payload("freshnonce12"),
            aad_version: 1,
            update_id,
            sender_device_id: dev_device_id(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(randomized_retry.status(), 200);
    assert_eq!(randomized_retry.json::<Value>().await.unwrap()["seq"], 1);
    let canonical: (Vec<u8>, Vec<u8>) = sqlx::query_as(
        "SELECT encrypted_update, nonce FROM doc_updates WHERE doc_id = $1 AND update_id = $2",
    )
    .bind(doc_id)
    .bind(Uuid::from(update_id))
    .fetch_one(&pool)
    .await
    .unwrap();
    let mut original = b"update1".to_vec();
    original.resize(16, 0);
    assert_eq!(canonical.0, original);
    assert_eq!(canonical.1, b"nonce1234567");

    let other_device = Uuid::new_v4();
    let mut other_device_headers = dev_headers();
    other_device_headers.insert(
        "x-dev-device-id",
        reqwest::header::HeaderValue::from_str(&other_device.to_string()).unwrap(),
    );
    let conflict = client()
        .post(format!("{}/api/docs/{}/updates", base, doc_id))
        .headers(other_device_headers)
        .json(&PostUpdateRequest {
            encrypted_update_b64: make_encrypted_payload("collision"),
            nonce_b64: make_b64_payload("nonce1234567"),
            aad_version: 1,
            update_id,
            sender_device_id: other_device,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(conflict.status(), 409);

    // seq 2 should be allocatable with a different update_id
    let resp3 = client()
        .post(format!("{}/api/docs/{}/updates", base, doc_id))
        .headers(dev_headers())
        .json(&PostUpdateRequest {
            encrypted_update_b64: make_encrypted_payload("update2"),
            nonce_b64: make_b64_payload("nonce1234567"),
            aad_version: 1,
            update_id: UpdateId(Uuid::new_v4()),
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
            encrypted_update_b64: make_encrypted_payload("update"),
            nonce_b64: make_b64_payload("nonce1234567"),
            aad_version: 1,
            update_id: UpdateId(Uuid::new_v4()),
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
                encrypted_update_b64: make_encrypted_payload(&format!("update{}", i)),
                nonce_b64: make_b64_payload("nonce1234567"),
                aad_version: 1,
                update_id: UpdateId(Uuid::new_v4()),
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
                encrypted_update_b64: make_encrypted_payload(&format!("update{}", i)),
                nonce_b64: make_b64_payload("nonce1234567"),
                aad_version: 1,
                update_id: UpdateId(Uuid::new_v4()),
                sender_device_id: dev_device_id(),
            })
            .send()
            .await
            .unwrap();
    }

    let resp = client()
        .get(format!(
            "{}/api/docs/{}/updates?after_seq=2&through_seq=4",
            base, doc_id
        ))
        .headers(dev_headers())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["total"], 2);
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["seq"], 3);
    assert_eq!(items[1]["seq"], 4);
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
                encrypted_update_b64: make_encrypted_payload(&format!("update{}", i)),
                nonce_b64: make_b64_payload("nonce1234567"),
                aad_version: 1,
                update_id: UpdateId(Uuid::new_v4()),
                sender_device_id: dev_device_id(),
            })
            .send()
            .await
            .unwrap();
    }

    // Get updates with limit 2
    let resp = client()
        .get(format!("{}/api/docs/{}/updates?limit=2", base, doc_id))
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

    // Post a snapshot at base_seq=0 (fresh document, current_seq=0)
    let post_resp = client()
        .post(format!("{}/api/docs/{}/snapshot", base, doc_id))
        .headers(dev_headers())
        .json(&PostSnapshotRequest {
            encrypted_snapshot_b64: make_encrypted_payload("snapshot1"),
            nonce_b64: make_b64_payload("nonce1234567"),
            base_seq: 0,
            aad_version: 1,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(post_resp.status(), 200);
    let post_body: Value = post_resp.json().await.unwrap();
    assert_eq!(post_body["base_seq"], 0);
    assert_eq!(
        post_body["encrypted_snapshot_b64"],
        make_encrypted_payload("snapshot1")
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
    assert_eq!(get_body["base_seq"], 0);
    assert_eq!(
        get_body["encrypted_snapshot_b64"],
        make_encrypted_payload("snapshot1")
    );
}

#[tokio::test]
async fn test_snapshot_negative_base_seq_rejected() {
    let (base, _pool) = setup().await;
    let (_ws, doc_id) = create_doc_for_updates(&base).await;

    let resp = client()
        .post(format!("{}/api/docs/{}/snapshot", base, doc_id))
        .headers(dev_headers())
        .json(&PostSnapshotRequest {
            encrypted_snapshot_b64: make_encrypted_payload("negative"),
            nonce_b64: make_b64_payload("nonce1234567"),
            base_seq: -1,
            aad_version: 1,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_snapshot_base_seq_ahead_of_current_rejected() {
    let (base, _pool) = setup().await;
    let (_ws, doc_id) = create_doc_for_updates(&base).await;

    // Fresh document has current_seq=0, so base_seq=1 should be rejected
    let resp = client()
        .post(format!("{}/api/docs/{}/snapshot", base, doc_id))
        .headers(dev_headers())
        .json(&PostSnapshotRequest {
            encrypted_snapshot_b64: make_encrypted_payload("ahead"),
            nonce_b64: make_b64_payload("nonce1234567"),
            base_seq: 1,
            aad_version: 1,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
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

    // Post same version twice (base_seq=0 valid for fresh doc with current_seq=0)
    for _ in 0..2 {
        client()
            .post(format!("{}/api/docs/{}/snapshot", base, doc_id))
            .headers(dev_headers())
            .json(&PostSnapshotRequest {
                encrypted_snapshot_b64: make_encrypted_payload("updated"),
                nonce_b64: make_b64_payload("nonce1234567"),
                base_seq: 0,
                aad_version: 1,
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
    assert_eq!(body["base_seq"], 0);
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
            encrypted_update_b64: make_encrypted_payload("update"),
            nonce_b64: make_b64_payload("nonce1234567"),
            aad_version: 1,
            update_id: UpdateId(Uuid::new_v4()),
            sender_device_id: Uuid::new_v4(), // different from header
        })
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}
