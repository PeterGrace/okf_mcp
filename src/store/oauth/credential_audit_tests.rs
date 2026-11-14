//! Atomic credential changes and audit entries for sign-in codes and grants.
use super::*;
use crate::db::{test_db, TEST_USER};
use crate::store::audit::testing::entries;

fn code(name: &str) -> NewSignInCode<'_> {
    NewSignInCode {
        name,
        access: Access::Read,
        grants: Grants::All,
        default_catalog: None,
        actor: "example-client",
        expires_at: Utc::now() + TimeDelta::hours(1),
        user: TEST_USER,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn codes_and_redeemed_grants_record_changes_but_not_noops_or_refusals() {
    let db = test_db().await;
    let actor = AuditActor::cli();
    let now = Utc::now();
    let created = create(&db.pool, &code("code-audit"), &actor, now)
        .await
        .unwrap();
    let created_entries = entries(&db.pool, "oauth.code_created").await;
    assert_eq!(created_entries.len(), 1);
    assert_eq!(created_entries[0].person.as_deref(), Some(TEST_USER));
    assert!(created_entries[0].teams.is_empty());
    assert_eq!(
        created_entries[0].details,
        serde_json::json!({"access":"read", "all_catalogs":true,"catalogs":0})
    );
    assert!(!serde_json::to_string(&created_entries[0].details)
        .unwrap()
        .contains(&created.code));
    let alice = users::tests::seed_user(&db.pool, "alice", None).await;
    assign_user(&db.pool, &created.id, "alice", &actor, now)
        .await
        .unwrap();
    assign_user(&db.pool, &created.id, "alice", &actor, now)
        .await
        .unwrap();
    let assigned = entries(&db.pool, "key.user_assigned").await;
    assert_eq!(assigned.len(), 1);
    assert_eq!(assigned[0].target_kind, "code");
    assert_eq!(assigned[0].person.as_deref(), Some("alice"));
    let client = insert_test_client(
        &db.pool,
        "audit-client",
        ClientKind::Dcr,
        "https://client.example.com/callback",
    )
    .await;
    let grant = redeem(&db.pool, &created.id, &client, now).await.unwrap();
    let grants = entries(&db.pool, "oauth.grant_created").await;
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0].target_kind, "grant");
    assert_eq!(grants[0].target.as_deref(), Some(grant.as_str()));
    assert_eq!(grants[0].person.as_deref(), Some("alice"));
    assert_eq!(grants[0].source, "system");
    assert_eq!(grants[0].actor_label, "system:oauth");
    assert_eq!(grants[0].details["client"], "client.example.com");
    assert!(redeem(&db.pool, &created.id, &client, now).await.is_err());
    assert!(revoke(&db.pool, &created.id, &actor, now).await.is_err());
    assert_eq!(entries(&db.pool, "oauth.grant_created").await.len(), 1);
    crate::store::keys::revoke_by_id(&db.pool, &grant, &actor, now)
        .await
        .unwrap();
    let revoked = entries(&db.pool, "oauth.grant_revoked").await;
    assert_eq!(revoked.len(), 1);
    assert_eq!(revoked[0].target_kind, "grant");
    let second = create(&db.pool, &code("to-revoke"), &actor, now)
        .await
        .unwrap();
    revoke(&db.pool, &second.id, &actor, now).await.unwrap();
    assert!(revoke(&db.pool, &second.id, &actor, now).await.is_err());
    assert_eq!(entries(&db.pool, "oauth.code_revoked").await.len(), 1);
    assert!(!alice.id.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn code_changes_and_redemption_roll_back_when_audit_fails() {
    let db = test_db().await;
    let actor = AuditActor::cli();
    let now = Utc::now();
    let created = create(&db.pool, &code("rollback"), &actor, now)
        .await
        .unwrap();
    users::tests::seed_user(&db.pool, "alice", None).await;
    let client = insert_test_client(
        &db.pool,
        "audit-client",
        ClientKind::Dcr,
        "https://client.example.com/callback",
    )
    .await;
    sqlx::query("ALTER TABLE audit_entries ADD CONSTRAINT reject_code_changes CHECK (action NOT IN ('oauth.code_created', 'oauth.code_revoked', 'oauth.grant_created', 'key.user_assigned')) NOT VALID")
        .execute(&db.pool).await.unwrap();
    assert!(create(&db.pool, &code("not-created"), &actor, now)
        .await
        .is_err());
    assert!(assign_user(&db.pool, &created.id, "alice", &actor, now)
        .await
        .is_err());
    assert!(revoke(&db.pool, &created.id, &actor, now).await.is_err());
    assert!(redeem(&db.pool, &created.id, &client, now).await.is_err());
    let live = find_live(&db.pool, &created.code, now)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(live.user.as_deref(), Some(TEST_USER));
    assert_eq!(list(&db.pool).await.unwrap().len(), 1);
    assert!(crate::store::keys::list(&db.pool).await.unwrap().is_empty());
    assert!(entries(&db.pool, "oauth.grant_created").await.is_empty());
}
