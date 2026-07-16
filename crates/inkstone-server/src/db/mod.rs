pub mod models;

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppResult;

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
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
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
    // Use a serializable transaction for atomicity
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

    if let Some((seq, sender_device_id, encrypted_update, nonce, aad_version, created_at)) =
        existing
    {
        tx.commit().await?;
        return Ok(StoredUpdate {
            seq,
            sender_device_id,
            encrypted_update,
            nonce,
            aad_version,
            created_at,
        });
    }

    // Allocate next sequence
    let next_seq = current_seq + 1;
    let created_at = chrono::Utc::now();

    // Insert the update with the new sequence
    sqlx::query(
        "INSERT INTO doc_updates (doc_id, seq, sender_device_id, encrypted_update, nonce, aad_version, update_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(doc_id)
    .bind(next_seq)
    .bind(sender_device_id)
    .bind(&encrypted_update)
    .bind(&nonce)
    .bind(aad_version)
    .bind(update_id)
    .execute(&mut *tx)
    .await?;

    // Update the counter atomically
    sqlx::query("UPDATE doc_update_counters SET current_seq = $1 WHERE doc_id = $2")
        .bind(next_seq)
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
