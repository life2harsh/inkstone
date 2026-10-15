mod common;

use base64::Engine;
use common::*;
use futures_util::{SinkExt, StreamExt};
use inkstone_core::UpdateId;
use inkstone_core::protocol::{
    AcceptInvitationRequest, AcceptInvitationResponse, ApproveInvitationKeyRequest,
    AuthUserResponse, ChangePasswordRequest, ClientWsMessage, ConsumeEmailVerificationRequest,
    ConsumePasswordResetRequest, CreateDocRequest, CreateInvitationRequest,
    CreateInvitationResponse, CreateWorkspaceRequest, EncryptedGuestName, GuestJoinRequest,
    GuestJoinResponse, InvitationKeyRequestResponse, InvitationRegisterRequest, LoginRequest,
    OwnerSetupRequest, PostUpdateRequest, PublicLinkAccessRequest, PublicLinkRequest,
    PublicLinkResponse, PutWorkspaceKeyEnvelopeRequest, RecoveryEnvelopeRequest,
    RecoveryEnvelopeResponse, RegisterRequest, ServerWsMessage, SubmitInvitationKeyRequest,
    TransferOwnershipRequest, UpdateDocPathProjectionRequest, UpdateMemberRoleRequest,
    WorkspaceKeyEnvelopeResponse, WorkspaceResponse,
};
use reqwest::header::{COOKIE, ORIGIN, SET_COOKIE};
use serde_json::Value;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use uuid::Uuid;

fn workspace_request() -> CreateWorkspaceRequest {
    CreateWorkspaceRequest {
        workspace_id: Uuid::new_v4(),
        encrypted_metadata_b64: base64::engine::general_purpose::STANDARD.encode([1_u8; 16]),
        metadata_nonce_b64: base64::engine::general_purpose::STANDARD.encode([2_u8; 12]),
    }
}

const PASSWORD: &str = "correct horse battery staple";

async fn insert_auth_token(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    email: &str,
    purpose: &str,
    token: &str,
    expires: &str,
) {
    sqlx::query(&format!(
        "INSERT INTO auth_tokens(token_hash,purpose,user_id,email,expires_at,delivery_idempotency_key)
         VALUES($1,$2,$3,$4,NOW()+INTERVAL '{expires}', $5)"
    ))
    .bind(inkstone_server::auth::hash_secret(token.as_bytes()))
    .bind(purpose)
    .bind(user_id)
    .bind(email)
    .bind(Uuid::new_v4())
    .execute(pool)
    .await
    .unwrap();
}

fn registration(email: &str, device_id: Uuid) -> RegisterRequest {
    RegisterRequest {
        email: email.into(),
        password: PASSWORD.into(),
        device_id,
        device_name: Some("test device".into()),
    }
}

async fn register(
    base: &str,
    email: &str,
    device_id: Uuid,
    invitation_token: &str,
) -> (AuthUserResponse, String) {
    let response = client()
        .post(format!("{base}/api/auth/register"))
        .json(&InvitationRegisterRequest {
            registration: registration(email, device_id),
            invitation_token: invitation_token.into(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let cookie = response
        .headers()
        .get(SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let user = response.json().await.unwrap();
    (user, cookie)
}

async fn create_invitation(
    base: &str,
    owner_cookie: &str,
    workspace_id: Uuid,
    email: &str,
    role: &str,
) -> CreateInvitationResponse {
    client()
        .post(format!("{base}/api/workspaces/{workspace_id}/invitations"))
        .header(COOKIE, owner_cookie)
        .json(&CreateInvitationRequest {
            email: email.into(),
            role: role.into(),
            expires_in_hours: Some(24),
        })
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn invite_and_register(
    base: &str,
    owner_cookie: &str,
    email: &str,
    device_id: Uuid,
) -> (AuthUserResponse, String) {
    let workspace: WorkspaceResponse = client()
        .post(format!("{base}/api/workspaces"))
        .header(COOKIE, owner_cookie)
        .json(&workspace_request())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let invitation = create_invitation(base, owner_cookie, workspace.id, email, "viewer").await;
    register(base, email, device_id, &invitation.token).await
}

async fn connect_production_ws(
    base: &str,
    doc_id: Uuid,
    device_id: Uuid,
    cookie: &str,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let url = format!(
        "{}/api/sync/docs/{doc_id}/ws?device_id={device_id}",
        base.replace("http", "ws")
    );
    let mut request = url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert("origin", "https://inkstone.test".parse().unwrap());
    request
        .headers_mut()
        .insert("cookie", cookie.parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let ready = tokio::time::timeout(std::time::Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let ready: ServerWsMessage = serde_json::from_str(&ready.into_text().unwrap()).unwrap();
    assert!(matches!(ready, ServerWsMessage::Ready { doc_id: ready_id, .. } if ready_id == doc_id));
    socket
}

async fn create_workspace_doc(base: &str, owner_cookie: &str) -> (WorkspaceResponse, Uuid) {
    let workspace: WorkspaceResponse = client()
        .post(format!("{base}/api/workspaces"))
        .header(COOKIE, owner_cookie)
        .json(&workspace_request())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let doc_id = Uuid::new_v4();
    let created = client()
        .post(format!("{base}/api/workspaces/{}/docs", workspace.id))
        .header(COOKIE, owner_cookie)
        .json(&CreateDocRequest {
            doc_id,
            encrypted_title_b64: Some(b64_encode(b"encrypted websocket title")),
            title_nonce_b64: Some(b64_encode(b"123456789012")),
            encrypted_path_b64: b64_encode(b"encrypted websocket path"),
            path_nonce_b64: b64_encode(b"abcdefghijkl"),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 200);
    (workspace, doc_id)
}

async fn assert_socket_closes(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) {
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), socket.next())
        .await
        .expect("WebSocket remained open after authorization was revoked");
    match result {
        None | Some(Err(_)) | Some(Ok(WsMessage::Close(_))) => {}
        Some(Ok(message)) => panic!("WebSocket delivered a message after revocation: {message:?}"),
    }
}

async fn owner_setup_register(
    base: &str,
    pool: &sqlx::PgPool,
    email: &str,
    device_id: Uuid,
) -> (AuthUserResponse, String) {
    let token = issue_owner_setup(pool, email).await;
    let response = client()
        .post(format!("{base}/api/auth/owner-setup"))
        .json(&OwnerSetupRequest {
            token,
            email: Some(email.to_uppercase()),
            password: PASSWORD.into(),
            device_id,
            device_name: Some("test device".into()),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let cookie = response
        .headers()
        .get(SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    (response.json().await.unwrap(), cookie)
}

#[tokio::test]
async fn owner_setup_sessions_login_logout_and_fail_closed_dev_auth() {
    let (base, pool) = setup_production().await;
    let premature_registration = client()
        .post(format!("{base}/api/auth/register"))
        .json(&InvitationRegisterRequest {
            registration: registration("premature@example.com", Uuid::new_v4()),
            invitation_token: inkstone_server::auth::new_secret(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(premature_registration.status(), 403);

    let device_id = Uuid::new_v4();
    let removed_bootstrap_api = client()
        .post(format!("{base}/api/auth/bootstrap"))
        .json(&registration("owner@example.com", device_id))
        .send()
        .await
        .unwrap();
    assert_eq!(removed_bootstrap_api.status(), 404);

    let first_token = issue_owner_setup(&pool, "Owner@Example.com").await;
    let wrong_token = client()
        .post(format!("{base}/api/auth/owner-setup"))
        .json(&OwnerSetupRequest {
            token: inkstone_server::auth::new_secret(),
            email: None,
            password: PASSWORD.into(),
            device_id,
            device_name: None,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_token.status(), 403);

    let expired_token = issue_owner_setup(&pool, "Owner@Example.com").await;
    sqlx::query(
        "UPDATE owner_setup SET issued_at = NOW() - INTERVAL '2 hours',
            expires_at = NOW() - INTERVAL '1 hour' WHERE singleton = TRUE",
    )
    .execute(&pool)
    .await
    .unwrap();
    let expired = client()
        .post(format!("{base}/api/auth/owner-setup"))
        .json(&OwnerSetupRequest {
            token: expired_token,
            email: None,
            password: PASSWORD.into(),
            device_id,
            device_name: None,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(expired.status(), 403);

    let token = issue_owner_setup(&pool, "Owner@Example.com").await;
    let revoked = client()
        .post(format!("{base}/api/auth/owner-setup"))
        .json(&OwnerSetupRequest {
            token: first_token,
            email: None,
            password: PASSWORD.into(),
            device_id,
            device_name: None,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), 403);

    let email_override = client()
        .post(format!("{base}/api/auth/owner-setup"))
        .json(&serde_json::json!({
            "token": token,
            "email": "attacker@example.com",
            "password": PASSWORD,
            "device_id": device_id,
            "device_name": null
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(email_override.status(), 400);

    let response = client()
        .post(format!("{base}/api/auth/owner-setup"))
        .json(&OwnerSetupRequest {
            token: token.clone(),
            email: None,
            password: PASSWORD.into(),
            device_id,
            device_name: Some("test device".into()),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let set_cookie = response
        .headers()
        .get(SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(set_cookie.contains("HttpOnly"));
    assert!(set_cookie.contains("SameSite=Strict"));
    assert!(set_cookie.contains("Secure"));
    let cookie = set_cookie.split(';').next().unwrap().to_owned();
    let user: AuthUserResponse = response.json().await.unwrap();
    assert_eq!(user.email, "owner@example.com");

    let password_hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1")
        .bind(user.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(password_hash.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
    let token_hash_length: i32 =
        sqlx::query_scalar("SELECT octet_length(token_hash) FROM auth_sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(token_hash_length, 32);

    let setup_replay = client()
        .post(format!("{base}/api/auth/owner-setup"))
        .json(&OwnerSetupRequest {
            token,
            email: None,
            password: PASSWORD.into(),
            device_id: Uuid::new_v4(),
            device_name: None,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(setup_replay.status(), 409);

    let me = client()
        .get(format!("{base}/api/auth/me"))
        .header(COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(me.status(), 200);

    let dev_header_attempt = client()
        .get(format!("{base}/api/workspaces"))
        .headers(dev_headers())
        .send()
        .await
        .unwrap();
    assert_eq!(dev_header_attempt.status(), 401);

    let logout = client()
        .post(format!("{base}/api/auth/logout"))
        .header(COOKIE, &cookie)
        .header(ORIGIN, "https://inkstone.test")
        .send()
        .await
        .unwrap();
    assert_eq!(logout.status(), 200);
    let expired = client()
        .get(format!("{base}/api/auth/me"))
        .header(COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(expired.status(), 401);

    let login = client()
        .post(format!("{base}/api/auth/login"))
        .json(&LoginRequest {
            email: "owner@example.com".into(),
            password: PASSWORD.into(),
            device_id,
            device_name: None,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), 200);
}

#[tokio::test]
async fn owner_setup_completion_is_atomic() {
    let (base, pool) = setup_production().await;
    let token = issue_owner_setup(&pool, "owner@example.com").await;
    let request = |device_id| OwnerSetupRequest {
        email: None,
        token: token.clone(),
        password: PASSWORD.into(),
        device_id,
        device_name: None,
    };
    let first = client()
        .post(format!("{base}/api/auth/owner-setup"))
        .json(&request(Uuid::new_v4()))
        .send();
    let second = client()
        .post(format!("{base}/api/auth/owner-setup"))
        .json(&request(Uuid::new_v4()))
        .send();
    let (first, second) = tokio::join!(first, second);
    let mut statuses = [first.unwrap().status(), second.unwrap().status()];
    statuses.sort_by_key(reqwest::StatusCode::as_u16);
    assert_eq!(
        statuses,
        [reqwest::StatusCode::OK, reqwest::StatusCode::CONFLICT]
    );
    let owners: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM users WHERE email IS NOT NULL AND password_hash IS NOT NULL",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(owners, 1);
}

#[tokio::test]
async fn ineligible_registration_is_rejected_before_argon2_capacity_is_needed() {
    let (base, _pool, state) = setup_production_with_state().await;
    let permits = state
        .argon2_semaphore
        .clone()
        .acquire_many_owned(4)
        .await
        .unwrap();
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        client()
            .post(format!("{base}/api/auth/register"))
            .json(&InvitationRegisterRequest {
                registration: registration("uninvited@example.com", Uuid::new_v4()),
                invitation_token: inkstone_server::auth::new_secret(),
            })
            .send(),
    )
    .await
    .expect("ineligible registration waited for Argon2 capacity")
    .unwrap();
    drop(permits);
    assert_eq!(response.status(), 403);
}

#[allow(dead_code)]
async fn invitation_is_email_bound_transactional_and_single_use() {
    let (base, pool) = setup_production().await;
    let (owner, owner_cookie) =
        owner_setup_register(&base, &pool, "owner@example.com", Uuid::new_v4()).await;
    let owner_device = owner.device_id;
    let workspace: WorkspaceResponse = client()
        .post(format!("{base}/api/workspaces"))
        .header(COOKIE, &owner_cookie)
        .json(&workspace_request())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let invitation = create_invitation(
        &base,
        &owner_cookie,
        workspace.id,
        "Invitee@Example.com",
        "viewer",
    )
    .await;
    assert_eq!(invitation.email, "invitee@example.com");
    assert_eq!(invitation.token.len(), 43);

    let no_invitation = client()
        .post(format!("{base}/api/auth/register"))
        .json(&InvitationRegisterRequest {
            registration: registration("uninvited@example.com", Uuid::new_v4()),
            invitation_token: inkstone_server::auth::new_secret(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(no_invitation.status(), 403);

    let wrong_email = client()
        .post(format!("{base}/api/auth/register"))
        .json(&InvitationRegisterRequest {
            registration: registration("attacker@example.com", Uuid::new_v4()),
            invitation_token: invitation.token.clone(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_email.status(), 403);

    let (invitee, invitee_cookie) = register(
        &base,
        "invitee@example.com",
        Uuid::new_v4(),
        &invitation.token,
    )
    .await;
    let accepted_after_registration: bool =
        sqlx::query_scalar("SELECT accepted FROM invitations WHERE id = $1")
            .bind(invitation.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!accepted_after_registration);

    let other_invitation = create_invitation(
        &base,
        &owner_cookie,
        workspace.id,
        "other@example.com",
        "viewer",
    )
    .await;
    let (_other, other_cookie) = register(
        &base,
        "other@example.com",
        Uuid::new_v4(),
        &other_invitation.token,
    )
    .await;

    let stolen_device = client()
        .post(format!("{base}/api/auth/login"))
        .json(&LoginRequest {
            email: "invitee@example.com".into(),
            password: PASSWORD.into(),
            device_id: owner_device,
            device_name: None,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(stolen_device.status(), 403);

    let pending: Value = client()
        .get(format!("{base}/api/invitations"))
        .header(COOKIE, &invitee_cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pending["total"], 1);

    let mismatched_workspace = client()
        .post(format!("{base}/api/invitations/accept"))
        .header(COOKIE, &invitee_cookie)
        .json(&AcceptInvitationRequest {
            token: invitation.token.clone(),
            workspace_id: Uuid::new_v4(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(mismatched_workspace.status(), 403);

    let accepted = client()
        .post(format!("{base}/api/invitations/accept"))
        .header(COOKIE, &invitee_cookie)
        .json(&AcceptInvitationRequest {
            token: invitation.token.clone(),
            workspace_id: workspace.id,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), 200);
    let accepted: AcceptInvitationResponse = accepted.json().await.unwrap();
    assert_eq!(accepted.workspace_id, workspace.id);
    assert_eq!(accepted.user_id, invitee.id);
    let replay = client()
        .post(format!("{base}/api/invitations/accept"))
        .header(COOKIE, &invitee_cookie)
        .json(&AcceptInvitationRequest {
            token: invitation.token.clone(),
            workspace_id: workspace.id,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), 200);
    let replay: AcceptInvitationResponse = replay.json().await.unwrap();
    assert_eq!(replay.workspace_id, workspace.id);

    let cross_identity_replay = client()
        .post(format!("{base}/api/invitations/accept"))
        .header(COOKIE, &other_cookie)
        .json(&AcceptInvitationRequest {
            token: invitation.token,
            workspace_id: workspace.id,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(cross_identity_replay.status(), 403);

    let viewer_write = client()
        .post(format!("{base}/api/workspaces/{}/docs", workspace.id))
        .header(COOKIE, &invitee_cookie)
        .json(&CreateDocRequest {
            doc_id: Uuid::new_v4(),
            encrypted_title_b64: Some(b64_encode(b"encrypted-viewer-title")),
            title_nonce_b64: Some(b64_encode(b"123456789012")),
            encrypted_path_b64: b64_encode(b"encrypted-viewer-path"),
            path_nonce_b64: b64_encode(b"abcdefghijkl"),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(viewer_write.status(), 403);
    let viewer_read = client()
        .get(format!("{base}/api/workspaces/{}/docs", workspace.id))
        .header(COOKIE, &invitee_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(viewer_read.status(), 200);

    let members: Value = client()
        .get(format!("{base}/api/workspaces/{}/members", workspace.id))
        .header(COOKIE, &owner_cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(members["total"], 2);
    assert!(
        members["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|member| member["user_id"] == invitee.id.to_string())
    );
    assert!(
        members["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|member| member["user_id"] == owner.id.to_string())
    );
}

#[allow(dead_code)]
async fn registration_rechecks_invitation_expiry_and_serializes_email_claims() {
    let (base, pool) = setup_production().await;
    let (_owner, owner_cookie) =
        owner_setup_register(&base, &pool, "owner@example.com", Uuid::new_v4()).await;
    let workspace: WorkspaceResponse = client()
        .post(format!("{base}/api/workspaces"))
        .header(COOKIE, &owner_cookie)
        .json(&workspace_request())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let expired = create_invitation(
        &base,
        &owner_cookie,
        workspace.id,
        "expired@example.com",
        "viewer",
    )
    .await;
    sqlx::query(
        "UPDATE invitations
         SET created_at = NOW() - INTERVAL '2 hours', expires_at = NOW() - INTERVAL '1 hour'
         WHERE id = $1",
    )
    .bind(expired.id)
    .execute(&pool)
    .await
    .unwrap();
    let expired_registration = client()
        .post(format!("{base}/api/auth/register"))
        .json(&InvitationRegisterRequest {
            registration: registration("expired@example.com", Uuid::new_v4()),
            invitation_token: expired.token,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(expired_registration.status(), 403);

    let invitation = create_invitation(
        &base,
        &owner_cookie,
        workspace.id,
        "race@example.com",
        "viewer",
    )
    .await;
    let first = client()
        .post(format!("{base}/api/auth/register"))
        .json(&InvitationRegisterRequest {
            registration: registration("race@example.com", Uuid::new_v4()),
            invitation_token: invitation.token.clone(),
        })
        .send();
    let second = client()
        .post(format!("{base}/api/auth/register"))
        .json(&InvitationRegisterRequest {
            registration: registration("RACE@example.com", Uuid::new_v4()),
            invitation_token: invitation.token,
        })
        .send();
    let (first, second) = tokio::join!(first, second);
    let mut statuses = [first.unwrap().status(), second.unwrap().status()];
    statuses.sort_by_key(reqwest::StatusCode::as_u16);
    assert_eq!(
        statuses,
        [reqwest::StatusCode::OK, reqwest::StatusCode::CONFLICT]
    );

    let registrations: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE LOWER(email) = 'race@example.com'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(registrations, 1);
    let accepted: bool = sqlx::query_scalar("SELECT accepted FROM invitations WHERE id = $1")
        .bind(invitation.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(!accepted);
}

#[tokio::test]
async fn production_websocket_closes_immediately_on_logout() {
    let (base, pool) = setup_production().await;
    let (owner, owner_cookie) =
        owner_setup_register(&base, &pool, "owner@example.com", Uuid::new_v4()).await;
    let (_workspace, doc_id) = create_workspace_doc(&base, &owner_cookie).await;
    let mut socket = connect_production_ws(&base, doc_id, owner.device_id, &owner_cookie).await;

    let logout = client()
        .post(format!("{base}/api/auth/logout"))
        .header(COOKIE, &owner_cookie)
        .header(ORIGIN, "https://inkstone.test")
        .send()
        .await
        .unwrap();
    assert_eq!(logout.status(), 200);
    assert_socket_closes(&mut socket).await;
}

#[tokio::test]
async fn expired_websocket_session_cannot_write() {
    let (base, pool) = setup_production().await;
    let (owner, owner_cookie) =
        owner_setup_register(&base, &pool, "owner@example.com", Uuid::new_v4()).await;
    let (_workspace, doc_id) = create_workspace_doc(&base, &owner_cookie).await;
    let mut socket = connect_production_ws(&base, doc_id, owner.device_id, &owner_cookie).await;
    let session_token = owner_cookie.split_once('=').unwrap().1;
    sqlx::query("UPDATE auth_sessions SET expires_at = NOW() WHERE token_hash = $1")
        .bind(inkstone_server::auth::hash_secret(session_token.as_bytes()))
        .execute(&pool)
        .await
        .unwrap();

    let message = ClientWsMessage::EncryptedUpdate {
        doc_id,
        update_id: UpdateId::new_v4(),
        encrypted_update_b64: b64_encode(b"encrypted-expired-update"),
        nonce_b64: b64_encode(b"123456789012"),
        aad_version: 1,
    };
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&message).unwrap().into(),
        ))
        .await
        .unwrap();
    assert_socket_closes(&mut socket).await;
    let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM doc_updates WHERE doc_id = $1")
        .bind(doc_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, 0);
}

#[allow(dead_code)]
async fn websocket_rechecks_membership_before_broadcast_delivery() {
    let (base, pool) = setup_production().await;
    let (owner, owner_cookie) =
        owner_setup_register(&base, &pool, "owner@example.com", Uuid::new_v4()).await;
    let (workspace, doc_id) = create_workspace_doc(&base, &owner_cookie).await;
    let invitation = create_invitation(
        &base,
        &owner_cookie,
        workspace.id,
        "member@example.com",
        "viewer",
    )
    .await;
    let (member, member_cookie) = register(
        &base,
        "member@example.com",
        Uuid::new_v4(),
        &invitation.token,
    )
    .await;
    let accepted = client()
        .post(format!("{base}/api/invitations/accept"))
        .header(COOKIE, &member_cookie)
        .json(&AcceptInvitationRequest {
            token: invitation.token,
            workspace_id: workspace.id,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), 200);
    let mut socket = connect_production_ws(&base, doc_id, member.device_id, &member_cookie).await;

    sqlx::query("DELETE FROM workspace_members WHERE workspace_id = $1 AND user_id = $2")
        .bind(workspace.id)
        .bind(member.id)
        .execute(&pool)
        .await
        .unwrap();
    let posted = client()
        .post(format!("{base}/api/docs/{doc_id}/updates"))
        .header(COOKIE, &owner_cookie)
        .json(&PostUpdateRequest {
            encrypted_update_b64: b64_encode(b"encrypted-owner-update"),
            nonce_b64: b64_encode(b"123456789012"),
            aad_version: 1,
            update_id: UpdateId::new_v4(),
            sender_device_id: owner.device_id,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(posted.status(), 200);
    assert_socket_closes(&mut socket).await;
}

#[tokio::test]
async fn members_rewrap_their_own_key_envelope_only_at_the_version_they_read() {
    let (base, pool) = setup_production().await;
    let (_owner, owner_cookie) =
        owner_setup_register(&base, &pool, "owner@example.com", Uuid::new_v4()).await;
    let (member, member_cookie) =
        invite_and_register(&base, &owner_cookie, "member@example.com", Uuid::new_v4()).await;
    let workspace: WorkspaceResponse = client()
        .post(format!("{base}/api/workspaces"))
        .header(COOKIE, &owner_cookie)
        .json(&workspace_request())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, 'editor')",
    )
    .bind(workspace.id)
    .bind(member.id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO encrypted_workspace_keys (workspace_id, user_id, encrypted_key, nonce, aad_version, version)
         VALUES ($1, $2, $3, $4, 1, 1)",
    )
    .bind(workspace.id)
    .bind(member.id)
    .bind(b"sealed-to-one-device".to_vec())
    .bind(b"123456789012".to_vec())
    .execute(&pool)
    .await
    .unwrap();
    let endpoint = format!("{base}/api/workspaces/{}/key-envelope", workspace.id);
    let put = |cookie: &str, replace_version: Option<i64>| {
        client()
            .put(&endpoint)
            .header(COOKIE, cookie)
            .json(&PutWorkspaceKeyEnvelopeRequest {
                encrypted_key_b64: b64_encode(b"wrapped-for-the-account"),
                nonce_b64: b64_encode(b"abcdefghijkl"),
                aad_version: 1,
                replace_version,
            })
            .send()
    };

    assert_eq!(put(&member_cookie, None).await.unwrap().status(), 403);
    assert_eq!(put(&member_cookie, Some(7)).await.unwrap().status(), 409);
    let replaced = put(&member_cookie, Some(1)).await.unwrap();
    assert_eq!(replaced.status(), 200);
    let replaced: WorkspaceKeyEnvelopeResponse = replaced.json().await.unwrap();
    assert_eq!(replaced.version, 2);
    assert_eq!(put(&member_cookie, Some(1)).await.unwrap().status(), 409);

    let stored: WorkspaceKeyEnvelopeResponse = client()
        .get(&endpoint)
        .header(COOKIE, &member_cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stored.user_id, member.id);
    assert_eq!(stored.version, 2);
    assert_eq!(
        stored.encrypted_key_b64,
        b64_encode(b"wrapped-for-the-account")
    );
}

#[allow(dead_code)]
async fn workspace_key_envelopes_are_validated_upserted_and_user_isolated() {
    let (base, pool) = setup_production().await;
    let (owner, owner_cookie) =
        owner_setup_register(&base, &pool, "owner@example.com", Uuid::new_v4()).await;
    let (member, member_cookie) =
        invite_and_register(&base, &owner_cookie, "member@example.com", Uuid::new_v4()).await;
    let (_outsider, outsider_cookie) =
        invite_and_register(&base, &owner_cookie, "outsider@example.com", Uuid::new_v4()).await;
    let workspace: WorkspaceResponse = client()
        .post(format!("{base}/api/workspaces"))
        .header(COOKIE, &owner_cookie)
        .json(&workspace_request())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, 'viewer')",
    )
    .bind(workspace.id)
    .bind(member.id)
    .execute(&pool)
    .await
    .unwrap();

    let endpoint = format!("{base}/api/workspaces/{}/key-envelope", workspace.id);
    let missing = client()
        .get(&endpoint)
        .header(COOKIE, &member_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);

    let owner_ciphertext = b"owner-encrypted-workspace-key";
    let put_owner = client()
        .put(&endpoint)
        .header(COOKIE, &owner_cookie)
        .json(&PutWorkspaceKeyEnvelopeRequest {
            encrypted_key_b64: b64_encode(owner_ciphertext),
            nonce_b64: b64_encode(b"123456789012"),
            aad_version: 1,
            replace_version: None,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(put_owner.status(), 200);
    let owner_response: WorkspaceKeyEnvelopeResponse = put_owner.json().await.unwrap();
    assert_eq!(owner_response.user_id, owner.id);
    assert_eq!(
        owner_response.encrypted_key_b64,
        b64_encode(owner_ciphertext)
    );

    let member_ciphertext = b"member-encrypted-workspace-key";
    let put_member = client()
        .put(&endpoint)
        .header(COOKIE, &member_cookie)
        .json(&PutWorkspaceKeyEnvelopeRequest {
            encrypted_key_b64: b64_encode(member_ciphertext),
            nonce_b64: b64_encode(b"abcdefghijkl"),
            aad_version: 1,
            replace_version: None,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(put_member.status(), 200);
    let member_get: WorkspaceKeyEnvelopeResponse = client()
        .get(&endpoint)
        .header(COOKIE, &member_cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(member_get.user_id, member.id);
    assert_eq!(member_get.encrypted_key_b64, b64_encode(member_ciphertext));

    let owner_get: WorkspaceKeyEnvelopeResponse = client()
        .get(&endpoint)
        .header(COOKIE, &owner_cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(owner_get.user_id, owner.id);
    assert_eq!(owner_get.encrypted_key_b64, b64_encode(owner_ciphertext));

    let second_device_login = client()
        .post(format!("{base}/api/auth/login"))
        .json(&LoginRequest {
            email: "owner@example.com".into(),
            password: PASSWORD.into(),
            device_id: Uuid::new_v4(),
            device_name: Some("second owner device".into()),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(second_device_login.status(), 200);
    let second_device_cookie = second_device_login
        .headers()
        .get(SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let second_device_get: WorkspaceKeyEnvelopeResponse = client()
        .get(&endpoint)
        .header(COOKIE, &second_device_cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(second_device_get.user_id, owner.id);
    assert_eq!(
        second_device_get.encrypted_key_b64,
        b64_encode(owner_ciphertext)
    );

    let outsider_get = client()
        .get(&endpoint)
        .header(COOKIE, &outsider_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(outsider_get.status(), 403);

    let replacement = b"owner-replacement-workspace-key";
    let upsert = client()
        .put(&endpoint)
        .header(COOKIE, &second_device_cookie)
        .json(&PutWorkspaceKeyEnvelopeRequest {
            encrypted_key_b64: b64_encode(replacement),
            nonce_b64: b64_encode(b"210987654321"),
            aad_version: 1,
            replace_version: None,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(upsert.status(), 200);
    let envelope_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM encrypted_workspace_keys WHERE workspace_id = $1")
            .bind(workspace.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(envelope_count, 2);
    let replacement_get: WorkspaceKeyEnvelopeResponse = client()
        .get(&endpoint)
        .header(COOKIE, &owner_cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(replacement_get.encrypted_key_b64, b64_encode(replacement));

    for invalid in [
        PutWorkspaceKeyEnvelopeRequest {
            encrypted_key_b64: b64_encode(b"valid-encrypted-key"),
            nonce_b64: b64_encode(b"short-nonce"),
            aad_version: 1,
            replace_version: None,
        },
        PutWorkspaceKeyEnvelopeRequest {
            encrypted_key_b64: b64_encode(b"valid-encrypted-key"),
            nonce_b64: b64_encode(b"123456789012"),
            aad_version: 2,
            replace_version: None,
        },
        PutWorkspaceKeyEnvelopeRequest {
            encrypted_key_b64: b64_encode(b"too-short"),
            nonce_b64: b64_encode(b"123456789012"),
            aad_version: 1,
            replace_version: None,
        },
        PutWorkspaceKeyEnvelopeRequest {
            encrypted_key_b64: b64_encode(&vec![0_u8; 65_537]),
            nonce_b64: b64_encode(b"123456789012"),
            aad_version: 1,
            replace_version: None,
        },
    ] {
        let response = client()
            .put(&endpoint)
            .header(COOKIE, &owner_cookie)
            .json(&invalid)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
    }
}

#[tokio::test]
async fn document_projection_and_delete_authorization_are_role_aware() {
    let (base, pool) = setup_production().await;
    let (owner, owner_cookie) =
        owner_setup_register(&base, &pool, "owner@example.com", Uuid::new_v4()).await;
    let (creator, creator_cookie) =
        invite_and_register(&base, &owner_cookie, "creator@example.com", Uuid::new_v4()).await;
    let (viewer, viewer_cookie) =
        invite_and_register(&base, &owner_cookie, "viewer@example.com", Uuid::new_v4()).await;
    let workspace: WorkspaceResponse = client()
        .post(format!("{base}/api/workspaces"))
        .header(COOKIE, &owner_cookie)
        .json(&workspace_request())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    for (user_id, role) in [(creator.id, "editor"), (viewer.id, "viewer")] {
        sqlx::query(
            "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, $3)",
        )
        .bind(workspace.id)
        .bind(user_id)
        .bind(role)
        .execute(&pool)
        .await
        .unwrap();
    }

    let mut doc_ids = Vec::new();
    for suffix in ["creator-delete", "admin-delete"] {
        let doc_id = Uuid::new_v4();
        let response = client()
            .post(format!("{base}/api/workspaces/{}/docs", workspace.id))
            .header(COOKIE, &creator_cookie)
            .json(&CreateDocRequest {
                doc_id,
                encrypted_title_b64: Some(b64_encode(
                    format!("encrypted-title-{suffix}").as_bytes(),
                )),
                title_nonce_b64: Some(b64_encode(b"123456789012")),
                encrypted_path_b64: b64_encode(format!("encrypted-path-{suffix}").as_bytes()),
                path_nonce_b64: b64_encode(b"abcdefghijkl"),
            })
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        doc_ids.push(doc_id);
    }
    let creator_doc = doc_ids[0];
    let admin_doc = doc_ids[1];

    let preflight = client()
        .request(
            reqwest::Method::OPTIONS,
            format!("{base}/api/docs/{creator_doc}"),
        )
        .header("origin", "https://inkstone.test")
        .header("access-control-request-method", "PATCH")
        .send()
        .await
        .unwrap();
    assert_eq!(preflight.status(), 200);
    let allowed_methods = preflight
        .headers()
        .get("access-control-allow-methods")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(allowed_methods.contains("PATCH"));
    assert!(allowed_methods.contains("DELETE"));
    assert!(allowed_methods.contains("PUT"));

    let replacement_title = b"opaque-renamed-title-ciphertext";
    let replacement_nonce = b"abcdefghijkl";
    let patched = client()
        .patch(format!("{base}/api/docs/{creator_doc}"))
        .header(COOKIE, &creator_cookie)
        .json(&UpdateDocPathProjectionRequest {
            encrypted_path_b64: b64_encode(replacement_title),
            path_nonce_b64: b64_encode(replacement_nonce),
            projection_version: 1,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(patched.status(), 200);
    let patched_body: Value = patched.json().await.unwrap();
    assert_eq!(
        patched_body["encrypted_path_b64"],
        b64_encode(replacement_title)
    );
    assert_eq!(
        patched_body["path_nonce_b64"],
        b64_encode(replacement_nonce)
    );
    let stored_projection: (Vec<u8>, Vec<u8>) =
        sqlx::query_as("SELECT encrypted_path, path_nonce FROM docs WHERE id = $1")
            .bind(creator_doc)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored_projection.0, replacement_title);
    assert_eq!(stored_projection.1, replacement_nonce);

    let invalid_projection = client()
        .patch(format!("{base}/api/docs/{creator_doc}"))
        .header(COOKIE, &creator_cookie)
        .json(&UpdateDocPathProjectionRequest {
            encrypted_path_b64: b64_encode(b"opaque-valid-path"),
            path_nonce_b64: b64_encode(b"short-nonce"),
            projection_version: 2,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(invalid_projection.status(), 400);

    let viewer_patch = client()
        .patch(format!("{base}/api/docs/{creator_doc}"))
        .header(COOKIE, &viewer_cookie)
        .json(&UpdateDocPathProjectionRequest {
            encrypted_path_b64: b64_encode(b"viewer-cannot-rename"),
            path_nonce_b64: b64_encode(b"123456789012"),
            projection_version: 2,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(viewer_patch.status(), 403);
    let viewer_delete = client()
        .delete(format!("{base}/api/docs/{creator_doc}"))
        .header(COOKIE, &viewer_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(viewer_delete.status(), 403);

    let admin_delete = client()
        .delete(format!("{base}/api/docs/{admin_doc}"))
        .header(COOKIE, &owner_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(admin_delete.status(), 204);

    sqlx::query(
        "INSERT INTO doc_snapshots
            (doc_id, base_seq, encrypted_snapshot, nonce, aad_version)
         VALUES ($1, 0, $2, $3, 1)",
    )
    .bind(creator_doc)
    .bind(b"opaque-snapshot-ciphertext")
    .bind(b"123456789012")
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE workspace_members SET role = 'viewer'
         WHERE workspace_id = $1 AND user_id = $2",
    )
    .bind(workspace.id)
    .bind(creator.id)
    .execute(&pool)
    .await
    .unwrap();
    let creator_delete = client()
        .delete(format!("{base}/api/docs/{creator_doc}"))
        .header(COOKIE, &creator_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(creator_delete.status(), 403);

    sqlx::query(
        "UPDATE workspace_members SET role = 'editor'
         WHERE workspace_id = $1 AND user_id = $2",
    )
    .bind(workspace.id)
    .bind(viewer.id)
    .execute(&pool)
    .await
    .unwrap();
    let editor_delete = client()
        .delete(format!("{base}/api/docs/{creator_doc}"))
        .header(COOKIE, &viewer_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(editor_delete.status(), 204);
    let retry_delete = client()
        .delete(format!("{base}/api/docs/{creator_doc}"))
        .header(COOKIE, &viewer_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(retry_delete.status(), 204);

    let remaining_docs: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM docs WHERE workspace_id = $1")
            .bind(workspace.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let remaining_snapshots: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM doc_snapshots WHERE doc_id = $1")
            .bind(creator_doc)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(remaining_docs, 0);
    assert_eq!(remaining_snapshots, 0);
    assert_ne!(owner.id, creator.id);
}

#[tokio::test]
async fn recovery_tokens_devices_and_public_key_invitation_lifecycle() {
    let (base, pool) = setup_production().await;
    let (owner, owner_cookie) =
        owner_setup_register(&base, &pool, "owner@example.com", Uuid::new_v4()).await;

    let recovery_url = format!("{base}/api/auth/recovery-envelopes");
    let put: RecoveryEnvelopeResponse = client()
        .put(&recovery_url)
        .header(COOKIE, &owner_cookie)
        .json(&RecoveryEnvelopeRequest {
            kind: "passphrase".into(),
            expected_version: None,
            ciphertext_b64: b64_encode(b"opaque-account-vault-key-envelope"),
            nonce_b64: b64_encode(b"123456789012"),
            aad_version: 1,
            wrapping_version: 1,
        })
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(put.version, 1);
    let stale = client()
        .put(&recovery_url)
        .header(COOKIE, &owner_cookie)
        .json(&RecoveryEnvelopeRequest {
            kind: "passphrase".into(),
            expected_version: Some(0),
            ciphertext_b64: b64_encode(b"opaque-stale-account-envelope"),
            nonce_b64: b64_encode(b"abcdefghijkl"),
            aad_version: 1,
            wrapping_version: 1,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), 409);

    let workspace: WorkspaceResponse = client()
        .post(format!("{base}/api/workspaces"))
        .header(COOKIE, &owner_cookie)
        .json(&workspace_request())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let invitation = create_invitation(
        &base,
        &owner_cookie,
        workspace.id,
        "member@example.com",
        "viewer",
    )
    .await;
    let (member, member_cookie) = register(
        &base,
        "member@example.com",
        Uuid::new_v4(),
        &invitation.token,
    )
    .await;
    let request: InvitationKeyRequestResponse = client()
        .post(format!("{base}/api/invitations/key-request"))
        .header(COOKIE, &member_cookie)
        .json(&SubmitInvitationKeyRequest {
            token: invitation.token.clone(),
            workspace_id: workspace.id,
            public_key_b64: b64_encode(&[7_u8; 32]),
            key_version: 1,
        })
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let awaiting_access = client()
        .get(format!("{base}/api/workspaces/{}/docs", workspace.id))
        .header(COOKIE, &member_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(awaiting_access.status(), 403);
    let replay = client()
        .post(format!("{base}/api/invitations/key-request"))
        .header(COOKIE, &member_cookie)
        .json(&SubmitInvitationKeyRequest {
            token: invitation.token,
            workspace_id: workspace.id,
            public_key_b64: b64_encode(&[8_u8; 32]),
            key_version: 1,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), 409);
    let unauthorized = client()
        .post(format!(
            "{base}/api/workspaces/{}/key-requests/{}/approve",
            workspace.id, request.id
        ))
        .header(COOKIE, &member_cookie)
        .json(&ApproveInvitationKeyRequest {
            encrypted_key_b64: b64_encode(b"recipient-bound-opaque-envelope"),
            nonce_b64: b64_encode(b"123456789012"),
            aad_version: 1,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), 403);
    let approved = client()
        .post(format!(
            "{base}/api/workspaces/{}/key-requests/{}/approve",
            workspace.id, request.id
        ))
        .header(COOKIE, &owner_cookie)
        .json(&ApproveInvitationKeyRequest {
            encrypted_key_b64: b64_encode(b"recipient-bound-opaque-envelope"),
            nonce_b64: b64_encode(b"123456789012"),
            aad_version: 1,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(approved.status(), 200);
    let member_envelope: WorkspaceKeyEnvelopeResponse = client()
        .get(format!(
            "{base}/api/workspaces/{}/key-envelope",
            workspace.id
        ))
        .header(COOKIE, &member_cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(member_envelope.user_id, member.id);
    assert_eq!(
        member_envelope.encrypted_key_b64,
        b64_encode(b"recipient-bound-opaque-envelope")
    );
    let role_update = client()
        .patch(format!(
            "{base}/api/workspaces/{}/members/{}",
            workspace.id, member.id
        ))
        .header(COOKIE, &owner_cookie)
        .json(&UpdateMemberRoleRequest {
            role: "editor".into(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(role_update.status(), 200);
    let transfer = client()
        .post(format!(
            "{base}/api/workspaces/{}/transfer-ownership",
            workspace.id
        ))
        .header(COOKIE, &owner_cookie)
        .json(&TransferOwnershipRequest {
            new_owner_user_id: member.id,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(transfer.status(), 200);
    let last_owner_leave = client()
        .post(format!("{base}/api/workspaces/{}/leave", workspace.id))
        .header(COOKIE, &member_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(last_owner_leave.status(), 409);

    let second_device = Uuid::new_v4();
    let login = client()
        .post(format!("{base}/api/auth/login"))
        .json(&LoginRequest {
            email: owner.email.clone(),
            password: PASSWORD.into(),
            device_id: second_device,
            device_name: Some("revoked device".into()),
        })
        .send()
        .await
        .unwrap();
    let second_cookie = login
        .headers()
        .get(SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let devices: Value = client()
        .get(format!("{base}/api/auth/devices"))
        .header(COOKIE, &second_cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        devices
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["current"] == true)
    );
    let revoked = client()
        .delete(format!("{base}/api/auth/devices/{second_device}"))
        .header(COOKIE, &owner_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), 200);
    let revoked_me = client()
        .get(format!("{base}/api/auth/me"))
        .header(COOKIE, &second_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(revoked_me.status(), 401);
}

#[tokio::test]
async fn verification_and_reset_tokens_are_expiring_single_use_and_non_enumerating() {
    let (base, pool) = setup_production().await;
    let (owner, owner_cookie) =
        owner_setup_register(&base, &pool, "owner@example.com", Uuid::new_v4()).await;
    let known = client()
        .post(format!("{base}/api/auth/password-reset/request"))
        .json(&serde_json::json!({"email":"owner@example.com"}))
        .send()
        .await
        .unwrap();
    let unknown = client()
        .post(format!("{base}/api/auth/password-reset/request"))
        .json(&serde_json::json!({"email":"unknown@example.com"}))
        .send()
        .await
        .unwrap();
    assert_eq!(known.status(), unknown.status());
    assert_eq!(known.text().await.unwrap(), unknown.text().await.unwrap());

    sqlx::query("DELETE FROM auth_tokens WHERE user_id=$1")
        .bind(owner.id)
        .execute(&pool)
        .await
        .unwrap();
    let expired = inkstone_server::auth::new_secret();
    insert_auth_token(
        &pool,
        owner.id,
        &owner.email,
        "email_verification",
        &expired,
        "1 hour",
    )
    .await;
    sqlx::query(
        "UPDATE auth_tokens SET created_at=NOW()-INTERVAL '2 hours', expires_at=NOW()-INTERVAL '1 hour'
         WHERE token_hash=$1",
    )
    .bind(inkstone_server::auth::hash_secret(expired.as_bytes()))
    .execute(&pool)
    .await
    .unwrap();
    let expired_response = client()
        .post(format!("{base}/api/auth/email-verification/consume"))
        .json(&ConsumeEmailVerificationRequest { token: expired })
        .send()
        .await
        .unwrap();
    assert_eq!(expired_response.status(), 400);
    let verification = inkstone_server::auth::new_secret();
    insert_auth_token(
        &pool,
        owner.id,
        &owner.email,
        "email_verification",
        &verification,
        "1 hour",
    )
    .await;
    for expected in [200, 400] {
        let response = client()
            .post(format!("{base}/api/auth/email-verification/consume"))
            .json(&ConsumeEmailVerificationRequest {
                token: verification.clone(),
            })
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), expected);
    }
    let reset = inkstone_server::auth::new_secret();
    insert_auth_token(
        &pool,
        owner.id,
        &owner.email,
        "password_reset",
        &reset,
        "1 hour",
    )
    .await;
    let wrong_email = client()
        .post(format!("{base}/api/auth/password-reset/consume"))
        .json(&ConsumePasswordResetRequest {
            token: reset.clone(),
            email: Some("someone-else@example.com".into()),
            password: "replacement horse battery staple".into(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_email.status(), 400);
    let reset_response = client()
        .post(format!("{base}/api/auth/password-reset/consume"))
        .json(&ConsumePasswordResetRequest {
            token: reset.clone(),
            email: None,
            password: "replacement horse battery staple".into(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(reset_response.status(), 200);
    assert!(
        reset_response
            .text()
            .await
            .unwrap()
            .contains("recovery key")
    );
    let replay = client()
        .post(format!("{base}/api/auth/password-reset/consume"))
        .json(&ConsumePasswordResetRequest {
            token: reset,
            email: None,
            password: "another replacement password".into(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), 400);
    let old_session = client()
        .get(format!("{base}/api/auth/me"))
        .header(COOKIE, owner_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(old_session.status(), 401);
}

#[tokio::test]
async fn password_changes_only_with_the_current_password_and_rewraps_the_vault() {
    let (base, pool) = setup_production().await;
    let device_id = Uuid::new_v4();
    let (_, cookie) = owner_setup_register(&base, &pool, "owner@example.com", device_id).await;
    let envelope = |version, text: &[u8]| RecoveryEnvelopeRequest {
        kind: "passphrase".into(),
        expected_version: version,
        ciphertext_b64: b64_encode(text),
        nonce_b64: b64_encode(b"123456789012"),
        aad_version: 1,
        wrapping_version: 1,
    };
    let put = client()
        .put(format!("{base}/api/auth/recovery-envelopes"))
        .header(COOKIE, &cookie)
        .json(&envelope(None, b"vault key wrapped by the old one"))
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), 200);
    let change = |current: &str, version| {
        client()
            .post(format!("{base}/api/auth/password"))
            .header(COOKIE, &cookie)
            .json(&ChangePasswordRequest {
                current_password: current.into(),
                new_password: "brand new password here".into(),
                passphrase_envelope: envelope(version, b"vault key wrapped by the new one"),
            })
            .send()
    };
    assert_eq!(
        change("not the password at all", Some(1))
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(change(PASSWORD, Some(7)).await.unwrap().status(), 409);
    let login = |password: &str| {
        client()
            .post(format!("{base}/api/auth/login"))
            .json(&LoginRequest {
                email: "owner@example.com".into(),
                password: password.into(),
                device_id,
                device_name: None,
            })
            .send()
    };
    assert_eq!(login(PASSWORD).await.unwrap().status(), 200);
    assert_eq!(change(PASSWORD, Some(1)).await.unwrap().status(), 200);
    assert_eq!(login(PASSWORD).await.unwrap().status(), 401);
    assert_eq!(
        login("brand new password here").await.unwrap().status(),
        200
    );
    let stored: Vec<RecoveryEnvelopeResponse> = client()
        .get(format!("{base}/api/auth/recovery-envelopes"))
        .header(COOKIE, &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stored[0].version, 2);
    assert_eq!(
        stored[0].ciphertext_b64,
        b64_encode(b"vault key wrapped by the new one")
    );
}

#[tokio::test]
async fn public_links_read_one_vault_and_nothing_else() {
    let (base, pool) = setup_production().await;
    let (_, cookie) = owner_setup_register(&base, &pool, "owner@example.com", Uuid::new_v4()).await;
    let (workspace, doc_id) = create_workspace_doc(&base, &cookie).await;
    let (other, _) = create_workspace_doc(&base, &cookie).await;
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([7_u8; 32]);
    let link_path = format!("{base}/api/workspaces/{}/public-link", workspace.id);

    let short = client()
        .put(&link_path)
        .header(COOKIE, &cookie)
        .json(&PublicLinkRequest {
            token: "too-short".into(),
            editable: false,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(short.status(), 400);
    let enabled: WorkspaceResponse = client()
        .put(&link_path)
        .header(COOKIE, &cookie)
        .json(&PublicLinkRequest {
            token: token.clone(),
            editable: false,
        })
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(enabled.public_link);
    assert_eq!(enabled.member_count, 1);

    let bearer = format!("Bearer {token}");
    let get = |path: String| {
        client()
            .get(format!("{base}{path}"))
            .header("authorization", &bearer)
            .send()
    };
    let details: PublicLinkResponse = get("/api/public-link".into())
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(details.workspace.id, workspace.id);
    for path in [
        format!("/api/workspaces/{}/docs", workspace.id),
        format!("/api/workspaces/{}/path-projections", workspace.id),
        format!("/api/docs/{doc_id}/updates?after_seq=0"),
    ] {
        assert_eq!(get(path.clone()).await.unwrap().status(), 200, "{path}");
    }
    for path in [
        format!("/api/workspaces/{}/docs", other.id),
        format!("/api/workspaces/{}/members", workspace.id),
        format!("/api/workspaces/{}/invitations", workspace.id),
        "/api/workspaces".into(),
        "/api/auth/me".into(),
        "/api/auth/devices".into(),
    ] {
        assert_eq!(get(path.clone()).await.unwrap().status(), 403, "{path}");
    }
    let create = client()
        .post(format!("{base}/api/workspaces"))
        .header("authorization", &bearer)
        .json(&workspace_request())
        .send()
        .await
        .unwrap();
    assert_eq!(create.status(), 403);
    let as_cookie = client()
        .get(format!("{base}/api/public-link"))
        .header(COOKIE, format!("inkstone_session={token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(as_cookie.status(), 401);

    let members: Value = client()
        .get(format!("{base}/api/workspaces/{}/members", workspace.id))
        .header(COOKIE, &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(members["items"].as_array().unwrap().len(), 1);

    let url = format!(
        "{}/api/sync/docs/{doc_id}/ws?link={token}",
        base.replace("http", "ws")
    );
    let mut request = url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert("origin", "https://inkstone.test".parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let ready = socket.next().await.unwrap().unwrap().into_text().unwrap();
    assert!(ready.contains("ready"), "{ready}");
    let write = ClientWsMessage::EncryptedUpdate {
        doc_id,
        update_id: UpdateId::new_v4(),
        encrypted_update_b64: b64_encode(b"an anonymous edit attempt"),
        nonce_b64: b64_encode(b"123456789012"),
        aad_version: 1,
    };
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&write).unwrap().into(),
        ))
        .await
        .unwrap();
    let reply = tokio::time::timeout(std::time::Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .into_text()
        .unwrap();
    assert!(reply.contains("Public links are read-only"), "{reply}");

    let disabled: WorkspaceResponse = client()
        .delete(&link_path)
        .header(COOKIE, &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!disabled.public_link);
    assert_eq!(get("/api/public-link".into()).await.unwrap().status(), 401);
    assert_socket_closes(&mut socket).await;

    client()
        .put(&link_path)
        .header(COOKIE, &cookie)
        .json(&PublicLinkRequest {
            token,
            editable: false,
        })
        .send()
        .await
        .unwrap();
    let deleted = client()
        .delete(format!("{base}/api/workspaces/{}", workspace.id))
        .header(COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), 204);
    let leftover: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE public_link_workspace_id IS NOT NULL")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(leftover, 0);
}

#[tokio::test]
async fn guests_join_an_editable_link_and_work_only_in_that_vault() {
    let (base, pool) = setup_production().await;
    let (_, cookie) = owner_setup_register(&base, &pool, "owner@example.com", Uuid::new_v4()).await;
    let (workspace, doc_id) = create_workspace_doc(&base, &cookie).await;
    let (other, _) = create_workspace_doc(&base, &cookie).await;
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([9_u8; 32]);
    let link_path = format!("{base}/api/workspaces/{}/public-link", workspace.id);
    let name = EncryptedGuestName {
        ciphertext_b64: b64_encode(b"an encrypted guest name"),
        nonce_b64: b64_encode(b"123456789012"),
    };
    let join = |bearer: String| {
        client()
            .post(format!("{base}/api/public-link/guests"))
            .header("authorization", bearer)
            .json(&GuestJoinRequest { name: name.clone() })
            .send()
    };

    let view_only: WorkspaceResponse = client()
        .put(&link_path)
        .header(COOKIE, &cookie)
        .json(&PublicLinkRequest {
            token: token.clone(),
            editable: false,
        })
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(view_only.public_link && !view_only.public_link_editable);
    assert_eq!(join(format!("Bearer {token}")).await.unwrap().status(), 403);

    let editable: WorkspaceResponse = client()
        .patch(&link_path)
        .header(COOKIE, &cookie)
        .json(&PublicLinkAccessRequest { editable: true })
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(editable.public_link_editable);

    let guest: GuestJoinResponse = join(format!("Bearer {token}"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let bearer = format!("Bearer {}", guest.token);
    let details: PublicLinkResponse = client()
        .get(format!("{base}/api/public-link"))
        .header("authorization", &bearer)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(details.workspace.id, workspace.id);
    assert_eq!(details.user_id, guest.user_id);
    assert_eq!(details.guest, Some(name.clone()));
    assert_eq!(details.workspace.member_count, 1);
    assert_eq!(details.workspace.guest_count, 1);

    let post_update = |label: &'static [u8]| {
        client()
            .post(format!("{base}/api/docs/{doc_id}/updates"))
            .header("authorization", &bearer)
            .json(&PostUpdateRequest {
                encrypted_update_b64: b64_encode(label),
                nonce_b64: b64_encode(b"123456789012"),
                aad_version: 1,
                update_id: UpdateId::new_v4(),
                sender_device_id: guest.device_id,
            })
            .send()
    };
    assert_eq!(
        post_update(b"an encrypted guest edit")
            .await
            .unwrap()
            .status(),
        200
    );
    let new_doc = client()
        .post(format!("{base}/api/workspaces/{}/docs", workspace.id))
        .header("authorization", &bearer)
        .json(&CreateDocRequest {
            doc_id: Uuid::new_v4(),
            encrypted_title_b64: Some(b64_encode(b"encrypted guest title")),
            title_nonce_b64: Some(b64_encode(b"123456789012")),
            encrypted_path_b64: b64_encode(b"encrypted guest path"),
            path_nonce_b64: b64_encode(b"abcdefghijkl"),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(new_doc.status(), 200);

    for path in [
        format!("/api/workspaces/{}/docs", other.id),
        format!("/api/workspaces/{}/members", workspace.id),
        format!("/api/workspaces/{}/invitations", workspace.id),
        "/api/workspaces".into(),
        "/api/auth/me".into(),
        "/api/auth/devices".into(),
    ] {
        let status = client()
            .get(format!("{base}{path}"))
            .header("authorization", &bearer)
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(status, 403, "{path}");
    }
    assert_eq!(join(bearer.clone()).await.unwrap().status(), 403);
    let as_cookie = client()
        .get(format!("{base}/api/public-link"))
        .header(COOKIE, format!("inkstone_session={}", guest.token))
        .send()
        .await
        .unwrap();
    assert_eq!(as_cookie.status(), 401);

    let url = format!(
        "{}/api/sync/docs/{doc_id}/ws?link={}",
        base.replace("http", "ws"),
        guest.token
    );
    let mut request = url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert("origin", "https://inkstone.test".parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let ready = socket.next().await.unwrap().unwrap().into_text().unwrap();
    assert!(ready.contains("ready"), "{ready}");
    let write = ClientWsMessage::EncryptedUpdate {
        doc_id,
        update_id: UpdateId::new_v4(),
        encrypted_update_b64: b64_encode(b"a live guest edit"),
        nonce_b64: b64_encode(b"123456789012"),
        aad_version: 1,
    };
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&write).unwrap().into(),
        ))
        .await
        .unwrap();
    let reply = tokio::time::timeout(std::time::Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .into_text()
        .unwrap();
    assert!(!reply.contains("error"), "{reply}");

    let members: Value = client()
        .get(format!("{base}/api/workspaces/{}/members", workspace.id))
        .header(COOKIE, &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let items = members["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    let listed = items
        .iter()
        .find(|item| item["user_id"] == guest.user_id.to_string())
        .unwrap();
    assert_eq!(listed["role"], "editor");
    assert_eq!(listed["guest_name"]["nonce_b64"], name.nonce_b64);

    client()
        .patch(&link_path)
        .header(COOKIE, &cookie)
        .json(&PublicLinkAccessRequest { editable: false })
        .send()
        .await
        .unwrap();
    assert_eq!(
        post_update(b"an encrypted late guest edit")
            .await
            .unwrap()
            .status(),
        403
    );

    let removed = client()
        .delete(format!(
            "{base}/api/workspaces/{}/members/{}",
            workspace.id, guest.user_id
        ))
        .header(COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(removed.status(), 204);
    assert_socket_closes(&mut socket).await;
    let after_removal = client()
        .get(format!("{base}/api/public-link"))
        .header("authorization", &bearer)
        .send()
        .await
        .unwrap();
    assert_eq!(after_removal.status(), 401);

    client()
        .patch(&link_path)
        .header(COOKIE, &cookie)
        .json(&PublicLinkAccessRequest { editable: true })
        .send()
        .await
        .unwrap();
    let second: GuestJoinResponse = join(format!("Bearer {token}"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let disabled: WorkspaceResponse = client()
        .delete(&link_path)
        .header(COOKIE, &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!disabled.public_link && !disabled.public_link_editable);
    assert_eq!(disabled.guest_count, 0);
    let gone = client()
        .get(format!("{base}/api/public-link"))
        .header("authorization", format!("Bearer {}", second.token))
        .send()
        .await
        .unwrap();
    assert_eq!(gone.status(), 401);
    let leftover: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM workspace_members m JOIN users u ON u.id = m.user_id WHERE u.guest_workspace_id IS NOT NULL")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(leftover, 0);
}
