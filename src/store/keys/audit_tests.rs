//! Credential audit invariants; each test has an isolated database.
use super::*;
use crate::db::{test_db, TEST_TEAM, TEST_USER};
use crate::store::audit::testing::entries;
use crate::store::users::tests::seed_user;
use std::time::Duration;

fn key<'a>(name: &'a str, grants: Grants<'a>) -> NewKey<'a> {
    NewKey {
        name,
        access: Access::Write,
        maintenance_report: false,
        grants,
        default_catalog: None,
        actor: "example-agent",
        user: TEST_USER,
        self_issued: false,
        expires_at: None,
        levels: &[],
    }
}

async fn generation(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT admin_generation FROM server_state")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn credential_audit_changes_noops_refusals_and_scopes() {
    let db = test_db().await;
    let now = Utc::now();
    let actor = AuditActor::system("credential-test");
    for name in ["main", "other"] {
        catalogs::create(
            &db.pool,
            &catalogs::NewCatalog {
                name,
                description: "",
                owner_team: TEST_TEAM,
            },
            &actor,
            now,
        )
        .await
        .unwrap();
    }
    let names = ["main".to_string()];
    let created = create_with_id(
        &db.pool,
        &key("audit-key", Grants::Catalogs(&names)),
        &actor,
        now,
    )
    .await
    .unwrap();
    let recorded = entries(&db.pool, "key.created").await;
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].target.as_deref(), Some(created.id.as_str()));
    assert_eq!(recorded[0].person.as_deref(), Some(TEST_USER));
    assert_eq!(recorded[0].teams, [TEST_USER]);
    assert_eq!(
        recorded[0].details,
        serde_json::json!({"access":"write", "all_catalogs":false,
        "catalogs":1,"maintenance_report":false,"self_issued":false,"expires":false})
    );
    assert!(!serde_json::to_string(&recorded[0].details)
        .unwrap()
        .contains(&created.key));
    assert_eq!(recorded[0].source, "system");
    grant_by_id(&db.pool, &created.id, "other", &actor, now)
        .await
        .unwrap();
    let before = generation(&db.pool).await;
    grant_by_id(&db.pool, &created.id, "other", &actor, now)
        .await
        .unwrap();
    assert_eq!(generation(&db.pool).await, before);
    assert_eq!(entries(&db.pool, "key.catalog_granted").await.len(), 1);
    ungrant_by_id(&db.pool, &created.id, "other", &actor, now)
        .await
        .unwrap();
    let before = generation(&db.pool).await;
    ungrant_by_id(&db.pool, &created.id, "other", &actor, now)
        .await
        .unwrap();
    assert_eq!(generation(&db.pool).await, before);
    assert_eq!(entries(&db.pool, "key.catalog_ungranted").await.len(), 1);
    set_maintenance_report_by_id(&db.pool, &created.id, true, &actor, now)
        .await
        .unwrap();
    let before = generation(&db.pool).await;
    set_maintenance_report_by_id(&db.pool, &created.id, true, &actor, now)
        .await
        .unwrap();
    assert_eq!(generation(&db.pool).await, before);
    let flags = entries(&db.pool, "key.report_flag_changed").await;
    assert_eq!(flags.len(), 1);
    assert_eq!(
        flags[0].details,
        serde_json::json!({"from":false,"to":true})
    );
    seed_user(&db.pool, "alice", None).await;
    assign_user_by_id(&db.pool, &created.id, "alice", &actor, now)
        .await
        .unwrap();
    let before = generation(&db.pool).await;
    assign_user_by_id(&db.pool, &created.id, "alice", &actor, now)
        .await
        .unwrap();
    assert_eq!(generation(&db.pool).await, before);
    let assigned = entries(&db.pool, "key.user_assigned").await;
    assert_eq!(assigned.len(), 1);
    assert_eq!(assigned[0].person.as_deref(), Some("alice"));
    revoke_by_id(&db.pool, &created.id, &actor, now)
        .await
        .unwrap();
    let before = generation(&db.pool).await;
    assert!(revoke_by_id(&db.pool, &created.id, &actor, now)
        .await
        .is_err());
    assert!(grant_by_id(&db.pool, &created.id, "other", &actor, now)
        .await
        .is_err());
    assert_eq!(generation(&db.pool).await, before);
    assert_eq!(entries(&db.pool, "key.revoked").await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn credential_change_rolls_back_when_recording_fails() {
    let db = test_db().await;
    let actor = AuditActor::cli();
    let now = Utc::now();
    let created = create_with_id(&db.pool, &key("rollback-key", Grants::All), &actor, now)
        .await
        .unwrap();
    let before = generation(&db.pool).await;
    sqlx::query("ALTER TABLE audit_entries ADD CONSTRAINT reject_credential_changes CHECK (action NOT IN ('key.revoked', 'key.created', 'key.report_flag_changed', 'key.user_assigned')) NOT VALID")
        .execute(&db.pool).await.unwrap();
    assert!(revoke_by_id(&db.pool, &created.id, &actor, now)
        .await
        .is_err());
    assert!(
        set_maintenance_report_by_id(&db.pool, &created.id, true, &actor, now)
            .await
            .is_err()
    );
    seed_user(&db.pool, "alice", None).await;
    let before_assignment = generation(&db.pool).await;
    assert!(
        assign_user_by_id(&db.pool, &created.id, "alice", &actor, now)
            .await
            .is_err()
    );
    assert_eq!(generation(&db.pool).await, before_assignment);
    let record = list(&db.pool).await.unwrap().pop().unwrap();
    assert!(record.revoked_at.is_none());
    assert!(!record.maintenance_report);
    assert_eq!(record.user.as_deref(), Some(TEST_USER));
    assert!(
        create_with_id(&db.pool, &key("never-created", Grants::All), &actor, now)
            .await
            .is_err()
    );
    assert_eq!(list(&db.pool).await.unwrap().len(), 1);
    assert!(before_assignment > before); // Only creating alice committed.
    assert!(entries(&db.pool, "key.revoked").await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pre_m14_key_that_only_ever_is_a_target_is_forgotten_with_its_user() {
    let db = test_db().await;
    seed_user(&db.pool, "alice", None).await;
    sqlx::query("INSERT INTO api_keys (id, name, sha256, access, all_catalogs, actor, created_at, user_id) SELECT 'alice-legacy', 'alice-legacy', sha256('alice-legacy'::bytea), 'read', true, 'alice-legacy', now(), id FROM users WHERE name = 'alice'")
        .execute(&db.pool).await.unwrap();
    revoke_by_id(&db.pool, "alice-legacy", &AuditActor::cli(), Utc::now())
        .await
        .unwrap();
    users::delete(&db.pool, "alice", false, &AuditActor::cli(), Utc::now())
        .await
        .unwrap();
    let stored: String = sqlx::query_scalar("SELECT string_agg(row_to_json(e)::text || coalesce(row_to_json(tc)::text, '') || coalesce(row_to_json(tp)::text, ''), '') FROM audit_entries e LEFT JOIN audit_credentials tc ON tc.id = e.target_credential_ref LEFT JOIN audit_people tp ON tp.id = e.target_person_id")
        .fetch_one(&db.pool).await.unwrap();
    let credentials: String = sqlx::query_scalar(
        "SELECT coalesce(string_agg(row_to_json(c)::text, ''), '') FROM audit_credentials c",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(stored.contains("key.revoked"), "{stored}");
    assert!(
        !stored.contains("alice") && !credentials.contains("alice"),
        "{stored}\n{credentials}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_credential_reference_is_eager_and_existing_resolution_does_not_wait() {
    let db = test_db().await;
    let actor = AuditActor::cli();
    let created = create_with_id(&db.pool, &key("eager", Grants::All), &actor, Utc::now())
        .await
        .unwrap();
    let mut holder = db.pool.begin().await.unwrap();
    let reference: i64 = sqlx::query_scalar(
        "SELECT id FROM audit_credentials WHERE credential_id = $1 FOR NO KEY UPDATE",
    )
    .bind(&created.id)
    .fetch_one(&mut *holder)
    .await
    .unwrap();
    let changed = tokio::time::timeout(
        Duration::from_secs(10),
        revoke_by_id(&db.pool, &created.id, &actor, Utc::now()),
    )
    .await;
    holder.rollback().await.unwrap();
    changed
        .expect("revoke must not wait on an existing audit reference")
        .unwrap();
    let recorded: i64 = sqlx::query_scalar(
        "SELECT target_credential_ref FROM audit_entries WHERE action = 'key.revoked'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(recorded, reference);
}

#[tokio::test(flavor = "multi_thread")]
async fn credential_change_locks_actor_before_credential_and_generation() {
    let db = test_db().await;
    let user = seed_user(&db.pool, "alice", None).await;
    let created = create_with_id(
        &db.pool,
        &key("lock-order", Grants::All),
        &AuditActor::cli(),
        Utc::now(),
    )
    .await
    .unwrap();
    let mut actor = AuditActor::system("credential-test");
    actor.user_id = Some(user.id.clone());
    let mut holder = db.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM users WHERE id = $1 FOR UPDATE")
        .bind(&user.id)
        .execute(&mut *holder)
        .await
        .unwrap();
    let pool = db.pool.clone();
    let id = created.id.clone();
    let changing = tokio::spawn(async move { revoke_by_id(&pool, &id, &actor, Utc::now()).await });
    tokio::time::timeout(
        Duration::from_secs(10),
        crate::http::api::testing::wait_for_lock_waiters(&db.pool, 1),
    )
    .await
    .expect("revoke must queue on the actor user");
    let lock = tokio::time::timeout(Duration::from_secs(10), async {
        sqlx::query("SELECT id FROM api_keys WHERE id = $1 FOR UPDATE")
            .bind(&created.id)
            .execute(&mut *holder)
            .await
            .unwrap();
        sqlx::query("SELECT admin_generation FROM server_state FOR UPDATE")
            .execute(&mut *holder)
            .await
            .unwrap();
    })
    .await;
    holder.rollback().await.unwrap();
    let changed = tokio::time::timeout(Duration::from_secs(10), changing).await;
    lock.expect("revoke must lock neither key nor generation before the actor");
    changed
        .expect("revoke must finish after releasing the user")
        .unwrap()
        .unwrap();
}

/// Each variant changes only one side of lock_credential_people's comparison.
#[derive(Clone, Copy)]
enum SnapshotChange {
    Owner,
    Grants,
}

/// Pauses a revoke after its snapshot, commits a competing change using an
/// unrelated actor, then checks that only the competing change survives.
async fn assert_snapshot_mismatch_refuses_revoke(change: SnapshotChange) {
    let db = test_db().await;
    let alice = seed_user(&db.pool, "alice", None).await;
    seed_user(&db.pool, "bob", None).await;
    let competing_actor = AuditActor::system("competing-credential-change");
    for name in ["main", "other"] {
        catalogs::create(
            &db.pool,
            &catalogs::NewCatalog {
                name,
                description: "",
                owner_team: TEST_TEAM,
            },
            &competing_actor,
            Utc::now(),
        )
        .await
        .unwrap();
    }
    let names = ["main".to_string()];
    let created = create_with_id(
        &db.pool,
        &key("snapshot-mismatch", Grants::Catalogs(&names)),
        &competing_actor,
        Utc::now(),
    )
    .await
    .unwrap();
    let before_generation = generation(&db.pool).await;
    let before_entries: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_entries")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let mut actor = AuditActor::system("waiting-credential-change");
    actor.user_id = Some(alice.id.clone());
    let mut holder = db.pool.begin().await.unwrap();
    let holder_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *holder)
        .await
        .unwrap();
    sqlx::query("SELECT id FROM users WHERE id = $1 FOR UPDATE")
        .bind(&alice.id)
        .execute(&mut *holder)
        .await
        .unwrap();
    let pool = db.pool.clone();
    let id = created.id.clone();
    let mut changing =
        tokio::spawn(async move { revoke_by_id(&pool, &id, &actor, Utc::now()).await });
    let competing = tokio::time::timeout(Duration::from_secs(10), async {
        // Seeing this specific backend blocked by our holder in lock_people
        // proves both snapshots have been read; no scheduling sleeps decide
        // when the competing change may run.
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity \
                 WHERE datname = current_database() AND wait_event_type = 'Lock' \
                   AND $1 = ANY(pg_blocking_pids(pid)) \
                   AND query = 'SELECT is_admin FROM users WHERE id = $1 FOR KEY SHARE')",
            )
            .bind(holder_pid)
            .fetch_one(&db.pool)
            .await?;
            if waiting {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        match change {
            SnapshotChange::Owner => {
                assign_user_by_id(&db.pool, &created.id, "bob", &competing_actor, Utc::now()).await
            }
            SnapshotChange::Grants => {
                grant_by_id(&db.pool, &created.id, "other", &competing_actor, Utc::now()).await
            }
        }
    })
    .await;
    // Release the holder and drain the spawned task before asserting, even
    // when synchronization or the competing change timed out/failed.
    holder.rollback().await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), &mut changing).await;
    if result.is_err() {
        changing.abort();
        tokio::time::timeout(Duration::from_secs(10), changing)
            .await
            .expect("aborted revoke task must finish")
            .expect_err("revoke task must have been cancelled");
    }
    competing
        .expect("revoke must block on its actor and the competing change must commit")
        .expect("competing credential change must succeed");
    let outcome = result
        .expect("revoke must finish after releasing its actor")
        .expect("revoke task must not panic");
    assert!(
        matches!(&outcome, Err(OkfError::Refused { reason })
            if reason == "credential changed while locking; retry the request"),
        "snapshot mismatch must refuse the revoke for retry, got {outcome:?}"
    );
    let record = list(&db.pool).await.unwrap().pop().unwrap();
    assert!(
        record.revoked_at.is_none(),
        "refused revoke must not mutate the key"
    );
    assert!(!record.maintenance_report);
    assert_eq!(record.access, Access::Write);
    let (expected_user, expected_catalogs, competing_action) = match change {
        SnapshotChange::Owner => ("bob", vec!["main"], "key.user_assigned"),
        SnapshotChange::Grants => (TEST_USER, vec!["main", "other"], "key.catalog_granted"),
    };
    assert_eq!(record.user.as_deref(), Some(expected_user));
    assert_eq!(record.catalogs, expected_catalogs);
    assert_eq!(generation(&db.pool).await, before_generation + 1);
    assert!(entries(&db.pool, "key.revoked").await.is_empty());
    assert_eq!(entries(&db.pool, competing_action).await.len(), 1);
    let after_entries: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_entries")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        after_entries,
        before_entries + 1,
        "only the competing change may record"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn owner_snapshot_mismatch_refuses_revoke_without_mutation_audit_or_generation_bump() {
    assert_snapshot_mismatch_refuses_revoke(SnapshotChange::Owner).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn grant_snapshot_mismatch_refuses_revoke_without_mutation_audit_or_generation_bump() {
    assert_snapshot_mismatch_refuses_revoke(SnapshotChange::Grants).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_absent_legacy_reference_first_resolution_can_wait() {
    let db = test_db().await;
    sqlx::query("INSERT INTO api_keys (id, name, sha256, access, all_catalogs, actor, created_at) VALUES ('legacy', 'legacy', sha256('legacy'::bytea), 'read', true, 'example-agent', now())")
        .execute(&db.pool).await.unwrap();
    let mut first = db.pool.begin().await.unwrap();
    let reference: i64 = sqlx::query_scalar("SELECT audit_credential('legacy', NULL, NULL)")
        .fetch_one(&mut *first)
        .await
        .unwrap();
    let mut second = db.pool.begin().await.unwrap();
    sqlx::query("SET LOCAL lock_timeout = '200ms'")
        .execute(&mut *second)
        .await
        .unwrap();
    let waiting = tokio::time::timeout(
        Duration::from_secs(10),
        sqlx::query_scalar::<_, i64>("SELECT audit_credential('legacy', NULL, NULL)")
            .fetch_one(&mut *second),
    )
    .await;
    second.rollback().await.unwrap();
    first.commit().await.unwrap();
    let error = waiting
        .expect("first resolution is bounded by lock_timeout")
        .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("55P03")
    );
    let resolved: i64 = sqlx::query_scalar("SELECT audit_credential('legacy', NULL, NULL)")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(resolved, reference);
}
