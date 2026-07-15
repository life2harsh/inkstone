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
