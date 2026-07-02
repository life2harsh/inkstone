pub mod models;

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppResult;

pub async fn verify_workspace_access(
    db: &PgPool,
    workspace_id: Uuid,
    user_id: Uuid,
) -> AppResult<()> {
    let exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM workspace_members WHERE workspace_id = $1 AND user_id = $2)",
    )
    .bind(workspace_id)
    .bind(user_id)
    .fetch_one(db)
    .await?;

    if !exists {
        return Err(crate::error::AppError::Unauthorized(
            "User is not a member of this workspace".into(),
        ));
    }

    Ok(())
}

pub async fn verify_doc_access(db: &PgPool, doc_id: Uuid, user_id: Uuid) -> AppResult<()> {
    let exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(
            SELECT 1 FROM docs d
            JOIN workspace_members wm ON wm.workspace_id = d.workspace_id
            WHERE d.id = $1 AND wm.user_id = $2
        )",
    )
    .bind(doc_id)
    .bind(user_id)
    .fetch_one(db)
    .await?;

    if !exists {
        return Err(crate::error::AppError::Unauthorized(
            "User does not have access to this document".into(),
        ));
    }

    Ok(())
}

pub async fn verify_device_owner(
    db: &PgPool,
    device_id: Uuid,
    user_id: Uuid,
) -> AppResult<()> {
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

/// Atomically allocate the next sequence number for a document.
///
/// Uses `doc_update_counters` with `UPDATE ... RETURNING` so concurrent
/// callers never see the same sequence number. The row is created on
/// first access via `INSERT ... ON CONFLICT DO NOTHING`.
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

pub struct StoredUpdate {
    pub seq: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Insert a doc update idempotently.
///
/// 1. If `(doc_id, client_update_id)` already exists, return its seq and created_at.
/// 2. Otherwise, allocate a seq and insert. If a unique-vs-client_update_id race
///    is lost, read the existing row and return that instead.
pub async fn insert_doc_update_idempotent(
    db: &PgPool,
    doc_id: Uuid,
    sender_device_id: Uuid,
    client_update_id: Uuid,
    encrypted_update: Vec<u8>,
    nonce: Vec<u8>,
    aad_version: i32,
) -> AppResult<StoredUpdate> {
    let existing = sqlx::query_as::<_, (i64, chrono::DateTime<chrono::Utc>)>(
        "SELECT seq, created_at FROM doc_updates WHERE doc_id = $1 AND client_update_id = $2",
    )
    .bind(doc_id)
    .bind(client_update_id)
    .fetch_optional(db)
    .await?;

    if let Some((seq, created_at)) = existing {
        return Ok(StoredUpdate { seq, created_at });
    }

    let seq = allocate_seq(db, doc_id).await?;
    let created_at = chrono::Utc::now();

    let result = sqlx::query(
        "INSERT INTO doc_updates (doc_id, seq, sender_device_id, encrypted_update, nonce, aad_version, client_update_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(doc_id)
    .bind(seq)
    .bind(sender_device_id)
    .bind(&encrypted_update)
    .bind(&nonce)
    .bind(aad_version)
    .bind(client_update_id)
    .execute(db)
    .await;

    match result {
        Ok(_) => Ok(StoredUpdate { seq, created_at }),
        Err(sqlx::Error::Database(ref e)) if e.is_unique_violation() => {
            let row = sqlx::query_as::<_, (i64, chrono::DateTime<chrono::Utc>)>(
                "SELECT seq, created_at FROM doc_updates WHERE doc_id = $1 AND client_update_id = $2",
            )
            .bind(doc_id)
            .bind(client_update_id)
            .fetch_one(db)
            .await?;
            Ok(StoredUpdate { seq: row.0, created_at: row.1 })
        }
        Err(e) => Err(e.into()),
    }
}
