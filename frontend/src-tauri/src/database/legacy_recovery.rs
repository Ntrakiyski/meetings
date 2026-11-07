use crate::connections_identity::VerifiedLegacyOwnership;
use sqlx::SqlitePool;

/// Recover only rows whose original snapshots match the independently verified mapping.
/// Ambiguous pre-ownership archives are deliberately excluded.
pub async fn recover_verified_legacy(
    pool: &SqlitePool,
    proof: &VerifiedLegacyOwnership,
) -> sqlx::Result<u64> {
    let mut transaction = pool.begin().await?;
    let identity = &proof.identity;
    let meetings = sqlx::query("UPDATE meetings SET connections_issuer=?,organization_id=?,workspace_id=?,created_by=?,ownership_state='verified' WHERE ownership_state='legacy_unverified' AND clerk_org_id=? AND legacy_created_by=?")
        .bind(&identity.issuer).bind(&identity.organization_id).bind(&identity.workspace_id).bind(&identity.user_id)
        .bind(&proof.legacy_organization_id).bind(&proof.legacy_user_id)
        .execute(&mut *transaction).await?.rows_affected();
    // Conflicts preserve the original queue row instead of replacing newer local work.
    sqlx::query("INSERT INTO meeting_sync_queue(connections_issuer,organization_id,workspace_id,created_by,external_id,revision,payload,updated_at) SELECT ?,?,?,?,external_id,revision,payload,updated_at FROM meeting_sync_legacy_queue WHERE clerk_org_id=? AND created_by=? ON CONFLICT(connections_issuer,workspace_id,created_by,external_id) DO NOTHING")
        .bind(&identity.issuer).bind(&identity.organization_id).bind(&identity.workspace_id).bind(&identity.user_id)
        .bind(&proof.legacy_organization_id).bind(&proof.legacy_user_id)
        .execute(&mut *transaction).await?;
    sqlx::query("DELETE FROM meeting_sync_legacy_queue AS legacy WHERE clerk_org_id=? AND created_by=? AND EXISTS(SELECT 1 FROM meeting_sync_queue q WHERE q.ownership_key=? AND q.created_by=? AND q.external_id=legacy.external_id AND q.revision=legacy.revision AND q.payload=legacy.payload)")
        .bind(&proof.legacy_organization_id).bind(&proof.legacy_user_id)
        .bind(identity.scope_key()).bind(&identity.user_id)
        .execute(&mut *transaction).await?;
    transaction.commit().await?;
    Ok(meetings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connections_identity::{LegacyIdentityResponse, OperationIdentity};
    use sqlx::migrate::Migrator;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::borrow::Cow;

    #[tokio::test]
    async fn recovery_requires_original_mapping_and_preserves_other_recordings_and_conflicts() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
        let migrator = Migrator::new(path).await.unwrap();
        let previous = Migrator {
            migrations: Cow::Owned(
                migrator
                    .iter()
                    .filter(|m| m.version < 20261107230000)
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
        previous.run(&pool).await.unwrap();
        for (id, org, user) in [
            ("original", "clerk-a", "clerk-user"),
            ("sibling", "clerk-a2", "clerk-user"),
            ("client", "clerk-b", "clerk-user"),
            ("other-user", "clerk-a", "someone-else"),
        ] {
            sqlx::query("INSERT INTO meetings(id,title,created_at,updated_at,clerk_org_id,created_by) VALUES(?,?,'2026','2026',?,?)").bind(id).bind(id).bind(org).bind(user).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO meeting_sync_queue(clerk_org_id,created_by,external_id,revision,payload) VALUES(?,?,?,1,?)").bind(org).bind(user).bind(id).bind(format!("original-bytes-{id}")).execute(&pool).await.unwrap();
        }
        sqlx::query("INSERT INTO meeting_sync_queue(clerk_org_id,created_by,external_id,revision,payload) VALUES('clerk-a','clerk-user','conflict',1,'original-conflict')").execute(&pool).await.unwrap();
        migrator.run(&pool).await.unwrap();
        let identity = OperationIdentity::new(
            "https://example.com/api/identity",
            "company-a",
            "A1",
            "stable-user",
        );
        let response = || {
            serde_json::from_value::<LegacyIdentityResponse>(serde_json::json!({
            "contract":"meetings-v1","issuer":identity.issuer,"organizationId":identity.organization_id,
            "workspaceId":identity.workspace_id,"userId":identity.user_id,
            "mapping":{"sourceInstance":"independent-source","legacyOrganizationId":"clerk-a","legacyUserId":"clerk-user"}
        })).unwrap()
        };
        for wrong in [
            OperationIdentity::new(&identity.issuer, "company-a", "A2", "stable-user"),
            OperationIdentity::new(&identity.issuer, "company-b", "B1", "stable-user"),
            OperationIdentity::new(
                "https://another.example/api/identity",
                "company-a",
                "A1",
                "stable-user",
            ),
            OperationIdentity::new(&identity.issuer, "company-a", "A1", "other-user"),
        ] {
            assert!(response().verify(&wrong).is_err());
        }
        sqlx::query("INSERT INTO meeting_sync_queue(connections_issuer,organization_id,workspace_id,created_by,external_id,revision,payload) VALUES(?,'company-a','A1','stable-user','conflict',2,'newer-work')").bind(&identity.issuer).execute(&pool).await.unwrap();
        let proof = response().verify(&identity).unwrap().unwrap();
        assert_eq!(recover_verified_legacy(&pool, &proof).await.unwrap(), 1);
        assert_eq!(recover_verified_legacy(&pool, &proof).await.unwrap(), 0);
        assert_eq!(sqlx::query_as::<_,(String,String,String,String)>("SELECT ownership_key,created_by,clerk_org_id,legacy_created_by FROM meetings WHERE id='original'").fetch_one(&pool).await.unwrap(),(identity.scope_key(),"stable-user".into(),"clerk-a".into(),"clerk-user".into()));
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM meetings WHERE ownership_state='legacy_unverified'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            3
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT payload FROM meeting_sync_queue WHERE external_id='original'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            "original-bytes-original"
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT payload FROM meeting_sync_queue WHERE external_id='conflict'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            "newer-work"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM meeting_sync_legacy_queue")
                .fetch_one(&pool)
                .await
                .unwrap(),
            4
        );
        assert!(
            sqlx::query("UPDATE meetings SET legacy_created_by='other' WHERE id='original'")
                .execute(&pool)
                .await
                .is_err()
        );
    }
}
