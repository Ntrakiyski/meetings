use sqlx::{migrate::Migrator, SqlitePool};
use std::borrow::Cow;

const OWNERSHIP_MIGRATION: i64 = 20260720000000;
const CONTENT_TABLES: [&str; 5] = [
    "meetings",
    "transcripts",
    "transcript_chunks",
    "summary_processes",
    "meeting_notes",
];

/// Archive pre-ownership content before the historical destructive migration.
/// These snapshots have no inferred user or workspace and are never uploaded.
/// Checked historical migration SQL and its checksums remain unchanged.
pub async fn migrate_preserving_legacy(pool: &SqlitePool, migrator: &Migrator) -> sqlx::Result<()> {
    let earlier = Migrator {
        migrations: Cow::Owned(
            migrator
                .iter()
                .filter(|migration| migration.version < OWNERSHIP_MIGRATION)
                .cloned()
                .collect(),
        ),
        ignore_missing: true,
        locking: migrator.locking,
        no_tx: migrator.no_tx,
    };
    earlier.run(pool).await?;
    let mut transaction = pool.begin().await?;
    // Acquiring the SQLite write lock also serializes concurrent startup attempts.
    sqlx::query("CREATE TABLE IF NOT EXISTS connections_legacy_preservation (id INTEGER PRIMARY KEY CHECK(id=1), meetings_count INTEGER NOT NULL, preserved_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP)")
        .execute(&mut *transaction).await?;
    let already_applied: i64 =
        sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE version=? AND success=1")
            .bind(OWNERSHIP_MIGRATION)
            .fetch_one(&mut *transaction)
            .await?;
    let already_preserved: i64 =
        sqlx::query_scalar("SELECT count(*) FROM connections_legacy_preservation")
            .fetch_one(&mut *transaction)
            .await?;
    if already_applied == 0 && already_preserved == 0 {
        for table in CONTENT_TABLES {
            // Names are fixed application constants, never external input.
            sqlx::query(&format!(
                "CREATE TABLE connections_legacy_{table} AS SELECT * FROM {table}"
            ))
            .execute(&mut *transaction)
            .await?;
        }
        sqlx::query("INSERT INTO connections_legacy_preservation(id,meetings_count) SELECT 1,count(*) FROM meetings")
            .execute(&mut *transaction).await?;
    }
    transaction.commit().await?;
    migrator.run(pool).await?;
    Ok(())
}

/// Includes unowned archived content and Clerk-era recordings awaiting proof.
pub async fn legacy_recording_count(pool: &SqlitePool) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT (SELECT count(*) FROM (SELECT json_array(clerk_org_id,legacy_created_by,id) AS recording FROM meetings WHERE ownership_state='legacy_unverified' UNION SELECT json_array(clerk_org_id,created_by,external_id) AS recording FROM meeting_sync_legacy_queue)) + coalesce((SELECT meetings_count FROM connections_legacy_preservation WHERE id=1),0)")
        .fetch_one(pool).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    #[tokio::test]
    async fn checked_migrations_preserve_preownership_content_and_repeat_safely() {
        let path = std::path::Path::new(file!())
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("migrations");
        let migrator = Migrator::new(path).await.unwrap();
        let earlier = Migrator {
            migrations: Cow::Owned(
                migrator
                    .iter()
                    .filter(|m| m.version < OWNERSHIP_MIGRATION)
                    .cloned()
                    .collect(),
            ),
            ignore_missing: false,
            locking: true,
            no_tx: false,
        };
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        earlier.run(&pool).await.unwrap();
        sqlx::query("INSERT INTO meetings(id,title,created_at,updated_at,folder_path) VALUES('unowned','Original recording','2026','2026','/recordings/original')").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO transcripts(id,meeting_id,transcript,timestamp) VALUES('segment','unowned','Original transcript','00:01')").execute(&pool).await.unwrap();
        migrate_preserving_legacy(&pool, &migrator).await.unwrap();
        migrate_preserving_legacy(&pool, &migrator).await.unwrap();
        assert_eq!(legacy_recording_count(&pool).await.unwrap(), 1);
        assert_eq!(
            sqlx::query_as::<_, (String, String)>(
                "SELECT title,folder_path FROM connections_legacy_meetings WHERE id='unowned'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            ("Original recording".into(), "/recordings/original".into())
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT transcript FROM connections_legacy_transcripts WHERE meeting_id='unowned'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            "Original transcript"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM meetings")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
        let owner = crate::connections_identity::OperationIdentity::new(
            "https://example.com/api/identity",
            "company",
            "A1",
            "user",
        );
        sqlx::query("INSERT INTO meetings(id,title,created_at,updated_at,clerk_org_id,created_by,connections_issuer,organization_id,workspace_id,ownership_state) VALUES('owned','Verified recording','2026','2026','snapshot','user',?,'company','A1','verified')").bind(&owner.issuer).execute(&pool).await.unwrap();
        migrate_preserving_legacy(&pool, &migrator).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT ownership_key FROM meetings WHERE id='owned'")
                .fetch_one(&pool)
                .await
                .unwrap(),
            owner.scope_key()
        );
        assert!(
            sqlx::query("UPDATE meetings SET workspace_id='A2' WHERE id='owned'")
                .execute(&pool)
                .await
                .is_err()
        );
        assert_eq!(legacy_recording_count(&pool).await.unwrap(), 1);
    }
}
