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

pub async fn get_next_seq(db: &PgPool, doc_id: Uuid) -> AppResult<i64> {
    let max_seq = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT MAX(seq) FROM doc_updates WHERE doc_id = $1",
    )
    .bind(doc_id)
    .fetch_one(db)
    .await?
    .unwrap_or(0);

    Ok(max_seq + 1)
}
