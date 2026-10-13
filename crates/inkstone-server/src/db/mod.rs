pub mod models;

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{AppError, AppResult};

pub const READ_ROLES: &[&str] = &["owner", "admin", "editor", "viewer"];
pub const WRITE_ROLES: &[&str] = &["owner", "admin", "editor"];

pub async fn create_pool(database_url: &str) -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(20)
        .connect(database_url)
        .await
        .expect("Failed to connect to Postgres")
}

pub async fn run_migrations(pool: &PgPool) {
    sqlx::migrate!("../../migrations")
        .run(pool)
        .await
        .expect("Failed to run migrations");
}

pub async fn create_test_pool() -> PgPool {
    let database_url = std::env::var("TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://inkstone:inkstone@localhost:5433/inkstone_test".into());
    let bootstrap = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("Failed to connect to test database. Is the test Postgres running on port 5433?");
    for extension in ["uuid-ossp", "pgcrypto"] {
        sqlx::query(&format!(
            "CREATE EXTENSION IF NOT EXISTS \"{extension}\" WITH SCHEMA public"
        ))
        .execute(&bootstrap)
        .await
        .expect("Failed to install extensions in the test database");
    }
    let schema = format!("test_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&bootstrap)
        .await
        .expect("Failed to create isolated test schema");
    bootstrap.close().await;
    let connection_schema = schema.clone();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .after_connect(move |connection, _| {
            let search_path = format!("SET search_path TO {connection_schema}, public");
            Box::pin(async move {
                sqlx::query(&search_path).execute(connection).await?;
                Ok(())
            })
        })
        .connect(&database_url)
        .await
        .expect("Failed to connect to test database. Is the test Postgres running on port 5433?");
    run_migrations(&pool).await;
    pool
}

pub async fn verify_workspace_access(
    db: &PgPool,
    workspace_id: Uuid,
    user_id: Uuid,
) -> AppResult<()> {
    let role = workspace_role(db, workspace_id, user_id).await?;
    if !role
        .as_deref()
        .is_some_and(|role| READ_ROLES.contains(&role))
    {
        return Err(AppError::Forbidden(
            "User is not a member of this workspace".into(),
        ));
    }

    Ok(())
}

pub async fn verify_workspace_write(
    db: &PgPool,
    workspace_id: Uuid,
    user_id: Uuid,
) -> AppResult<()> {
    let role = workspace_role(db, workspace_id, user_id).await?;
    if !role
        .as_deref()
        .is_some_and(|role| WRITE_ROLES.contains(&role))
    {
        return Err(AppError::Forbidden(
            "User does not have write access to this workspace".into(),
        ));
    }
    Ok(())
}

pub async fn workspace_role(
    db: &PgPool,
    workspace_id: Uuid,
    user_id: Uuid,
) -> AppResult<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT role FROM workspace_members WHERE workspace_id = $1 AND user_id = $2",
    )
    .bind(workspace_id)
    .bind(user_id)
    .fetch_optional(db)
    .await?)
}

pub async fn verify_doc_access(db: &PgPool, doc_id: Uuid, user_id: Uuid) -> AppResult<()> {
    verify_doc_role(db, doc_id, user_id, false).await
}

pub async fn verify_doc_write(db: &PgPool, doc_id: Uuid, user_id: Uuid) -> AppResult<()> {
    verify_doc_role(db, doc_id, user_id, true).await
}

async fn verify_doc_role(db: &PgPool, doc_id: Uuid, user_id: Uuid, write: bool) -> AppResult<()> {
    let role: Option<String> = sqlx::query_scalar(
        "SELECT wm.role
         FROM docs d
         JOIN workspace_members wm ON wm.workspace_id = d.workspace_id
         WHERE d.id = $1 AND wm.user_id = $2",
    )
    .bind(doc_id)
    .bind(user_id)
    .fetch_optional(db)
    .await?;
    let roles = if write { WRITE_ROLES } else { READ_ROLES };
    if !role.as_deref().is_some_and(|role| roles.contains(&role)) {
        return Err(AppError::Forbidden(
            if write {
                "User does not have write access to this document"
            } else {
                "User does not have access to this document"
            }
            .into(),
        ));
    }

    Ok(())
}

pub async fn doc_role(db: &PgPool, doc_id: Uuid, user_id: Uuid) -> AppResult<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT wm.role
         FROM docs d JOIN workspace_members wm ON wm.workspace_id = d.workspace_id
         WHERE d.id = $1 AND wm.user_id = $2",
    )
    .bind(doc_id)
    .bind(user_id)
    .fetch_optional(db)
    .await?)
}

pub async fn websocket_role(
    db: &PgPool,
    doc_id: Uuid,
    user_id: Uuid,
    device_id: Uuid,
    session_hash: Option<&[u8]>,
) -> AppResult<Option<String>> {
    let role = if let Some(session_hash) = session_hash {
        sqlx::query_scalar(
            "SELECT wm.role
             FROM docs d
             JOIN workspace_members wm ON wm.workspace_id = d.workspace_id
             JOIN auth_sessions s ON s.user_id = wm.user_id AND s.device_id = $3
             WHERE d.id = $1 AND wm.user_id = $2
               AND s.token_hash = $4 AND s.expires_at > NOW()",
        )
        .bind(doc_id)
        .bind(user_id)
        .bind(device_id)
        .bind(session_hash)
        .fetch_optional(db)
        .await?
    } else {
        sqlx::query_scalar(
            "SELECT wm.role
             FROM docs d
             JOIN workspace_members wm ON wm.workspace_id = d.workspace_id
              JOIN devices dv ON dv.user_id = wm.user_id AND dv.id = $3 AND dv.revoked_at IS NULL
             WHERE d.id = $1 AND wm.user_id = $2",
        )
        .bind(doc_id)
        .bind(user_id)
        .bind(device_id)
        .fetch_optional(db)
        .await?
    };
    Ok(role)
}

pub async fn verify_device_owner(db: &PgPool, device_id: Uuid, user_id: Uuid) -> AppResult<()> {
    let exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM devices WHERE id = $1 AND user_id = $2)",
    )
    .bind(device_id)
    .bind(user_id)
    .fetch_one(db)
    .await?;

    if !exists {
        return Err(crate::error::AppError::Unauthorized(
            "Device does not belong to this user".into(),
        ));
    }

    Ok(())
}

pub async fn allocate_seq(db: &PgPool, doc_id: Uuid) -> AppResult<i64> {
    sqlx::query(
        "INSERT INTO doc_update_counters (doc_id, current_seq) VALUES ($1, 0) ON CONFLICT (doc_id) DO NOTHING",
    )
    .bind(doc_id)
    .execute(db)
    .await?;

    let seq: (i64,) = sqlx::query_as(
        "UPDATE doc_update_counters SET current_seq = current_seq + 1 WHERE doc_id = $1 RETURNING current_seq",
    )
    .bind(doc_id)
    .fetch_one(db)
    .await?;

    Ok(seq.0)
}

pub async fn current_seq(db: &PgPool, doc_id: Uuid) -> AppResult<i64> {
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT current_seq FROM doc_update_counters WHERE doc_id = $1")
            .bind(doc_id)
            .fetch_optional(db)
            .await?;
    Ok(row.map(|r| r.0).unwrap_or(0))
}

pub struct StoredUpdate {
    pub seq: i64,
    pub sender_device_id: Uuid,
    pub encrypted_update: Vec<u8>,
    pub nonce: Vec<u8>,
    pub aad_version: i32,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Atomic transaction-based sequence allocation.
///
/// BEGIN
///   LOCK doc_update_counters row FOR UPDATE
///   Check if update_id already exists
///   If exists: return existing seq (no counter increment)
///   If new: next_seq = current_seq + 1, insert update, update counter
/// COMMIT
///
/// Any failure rolls back both counter movement and update insertion.
pub async fn insert_doc_update_idempotent(
    db: &PgPool,
    doc_id: Uuid,
    sender_device_id: Uuid,
    update_id: Uuid,
    encrypted_update: Vec<u8>,
    nonce: Vec<u8>,
    aad_version: i32,
) -> AppResult<StoredUpdate> {
    let mut tx = db.begin().await?;

    // Ensure counter row exists (locked for update below)
    sqlx::query(
        "INSERT INTO doc_update_counters (doc_id, current_seq) VALUES ($1, 0)
         ON CONFLICT (doc_id) DO NOTHING",
    )
    .bind(doc_id)
    .execute(&mut *tx)
    .await?;

    // Lock the counter row to prevent concurrent sequence allocation
    let counter: (i64,) =
        sqlx::query_as("SELECT current_seq FROM doc_update_counters WHERE doc_id = $1 FOR UPDATE")
            .bind(doc_id)
            .fetch_one(&mut *tx)
            .await?;
    let current_seq = counter.0;

    // Check whether update_id already exists (return full canonical record)
    let existing: Option<(
        i64,
        Uuid,
        Vec<u8>,
        Vec<u8>,
        i32,
        chrono::DateTime<chrono::Utc>,
    )> = sqlx::query_as(
        "SELECT seq, sender_device_id, encrypted_update, nonce, aad_version, created_at
         FROM doc_updates WHERE doc_id = $1 AND update_id = $2",
    )
    .bind(doc_id)
    .bind(update_id)
    .fetch_optional(&mut *tx)
    .await?;

    if let Some((seq, stored_sender, stored_update, stored_nonce, stored_aad, created_at)) =
        existing
    {
        if stored_sender != sender_device_id {
            return Err(AppError::Conflict(
                "update_id is already used by another sender device".into(),
            ));
        }
        tx.commit().await?;
        return Ok(StoredUpdate {
            seq,
            sender_device_id: stored_sender,
            encrypted_update: stored_update,
            nonce: stored_nonce,
            aad_version: stored_aad,
            created_at,
        });
    }

    // Allocate next sequence
    let next_seq = current_seq + 1;
    let created_at = chrono::Utc::now();

    // Insert the update with the new sequence
    sqlx::query(
        "INSERT INTO doc_updates
            (doc_id, seq, sender_device_id, encrypted_update, nonce, aad_version, update_id, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(doc_id)
    .bind(next_seq)
    .bind(sender_device_id)
    .bind(&encrypted_update)
    .bind(&nonce)
    .bind(aad_version)
    .bind(update_id)
    .bind(created_at)
    .execute(&mut *tx)
    .await?;

    // Update the counter atomically
    sqlx::query("UPDATE doc_update_counters SET current_seq = $1 WHERE doc_id = $2")
        .bind(next_seq)
        .bind(doc_id)
        .execute(&mut *tx)
        .await?;

    sqlx::query(
        "UPDATE docs SET updated_at = GREATEST(updated_at, clock_timestamp()) WHERE id = $1",
    )
    .bind(doc_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(StoredUpdate {
        seq: next_seq,
        sender_device_id,
        encrypted_update,
        nonce,
        aad_version,
        created_at,
    })
}
