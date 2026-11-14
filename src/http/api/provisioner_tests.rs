use super::{provisioner_may, PROVISIONER_ROUTES};
use crate::http::api::testing::{app, every_catalog_admin, send};
use serde_json::json;

/// A provisioner key for TEST_USER, polled in.
async fn provisioner(test: &crate::mcp::testing::TestApp) -> String {
    let created = crate::store::keys::create_provisioner(
        test.app.pool(),
        &crate::store::keys::NewProvisioner {
            name: "signup",
            actor: "signup",
            user: crate::db::TEST_USER,
        },
        &crate::store::audit::AuditActor::cli(),
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    test.app.poll().await.unwrap();
    created.key
}

#[tokio::test(flavor = "multi_thread")]
async fn a_provisioner_runs_the_signup_and_offboarding_routes() {
    let test = crate::mcp::testing::app_with(
        &[("main", crate::http::api::testing::MAIN)],
        crate::config::Config {
            public_url: Some("https://example.com".to_string()),
            ..Default::default()
        },
    )
    .await;
    let key = provisioner(&test).await;
    let p = Some(key.as_str());

    let steps: Vec<(&str, &str, Option<serde_json::Value>, u16)> = vec![
        ("POST", "/api/v1/users", Some(json!({"name": "alice"})), 201),
        (
            "PUT",
            "/api/v1/users/alice/email",
            Some(json!({"email": "alice@example.com"})),
            200,
        ),
        ("GET", "/api/v1/users?email=ALICE@example.com", None, 200),
        ("POST", "/api/v1/users/alice/invite", None, 202),
        ("GET", "/api/v1/users/alice", None, 200),
        (
            "POST",
            "/api/v1/teams",
            Some(json!({"name": "acme", "owner": "alice"})),
            201,
        ),
        ("GET", "/api/v1/teams/acme", None, 200),
        ("POST", "/api/v1/users", Some(json!({"name": "bob"})), 201),
        (
            "PUT",
            "/api/v1/teams/acme/members/bob",
            Some(json!({"role": "member"})),
            200,
        ),
        (
            "POST",
            "/api/v1/catalogs",
            Some(json!({"name": "acme-notes", "owner": "acme"})),
            201,
        ),
        (
            "PUT",
            "/api/v1/catalogs/acme-notes/member-access",
            Some(json!({"access": "read"})),
            200,
        ),
        (
            "PUT",
            "/api/v1/teams/acme/suspension",
            Some(json!({"suspended": true})),
            200,
        ),
        (
            "PUT",
            "/api/v1/teams/acme/suspension",
            Some(json!({"suspended": false})),
            200,
        ),
        ("DELETE", "/api/v1/teams/acme/members/bob", None, 200),
        ("DELETE", "/api/v1/catalogs/acme-notes", None, 200),
        ("DELETE", "/api/v1/teams/acme", None, 204),
        ("DELETE", "/api/v1/users/bob", None, 204),
    ];
    for (method, uri, body, want) in steps {
        let (status, answer) = send(&test, method, uri, p, body).await;
        assert_eq!(status.as_u16(), want, "{method} {uri}: {answer}");
    }
    let rows = crate::store::audit::testing::entries(test.app.pool(), "user.created").await;
    let alice = rows
        .iter()
        .find(|r| r.person.as_deref() == Some("alice"))
        .unwrap();
    assert!(alice.actor_operator);
    assert_eq!(alice.source, "api");
    let credential: String = sqlx::query_scalar("SELECT c.credential_id FROM audit_entries e JOIN audit_credentials c ON c.id = e.actor_credential_ref WHERE e.action = 'user.invited'").fetch_one(test.app.pool()).await.unwrap();
    let cached = crate::mcp::testing::cached_principal(&test, &key);
    assert_eq!(credential, cached.key_id);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_provisioner_never_reaches_an_admin_user() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    let key = provisioner(&test).await;
    let p = Some(key.as_str());
    send(
        &test,
        "POST",
        "/api/v1/users",
        Some(&root),
        Some(json!({"name": "staff"})),
    )
    .await;
    send(
        &test,
        "PUT",
        "/api/v1/users/staff/email",
        Some(&root),
        Some(json!({"email": "staff@example.com"})),
    )
    .await;
    send(
        &test,
        "PUT",
        "/api/v1/users/staff/admin",
        Some(&root),
        Some(json!({"admin": true})),
    )
    .await;
    send(
        &test,
        "POST",
        "/api/v1/teams",
        Some(&root),
        Some(json!({"name": "ops", "owner": crate::db::TEST_USER})),
    )
    .await;

    let (lookup, found) = send(
        &test,
        "GET",
        "/api/v1/users?email=staff@example.com",
        p,
        None,
    )
    .await;
    let refused = [
        ("GET", "/api/v1/users/staff", None),
        (
            "PUT",
            "/api/v1/users/staff/email",
            Some(json!({"email": "x@example.com"})),
        ),
        ("POST", "/api/v1/users/staff/invite", None),
        ("DELETE", "/api/v1/users/staff", None),
        (
            "PUT",
            "/api/v1/teams/ops/members/staff",
            Some(json!({"role": "member"})),
        ),
        (
            "POST",
            "/api/v1/teams",
            Some(json!({"name": "mine", "owner": "staff"})),
        ),
    ];

    assert_eq!(lookup, axum::http::StatusCode::OK);
    assert_eq!(
        found["users"],
        json!([]),
        "an admin is not found by a provisioner"
    );
    for (method, uri, body) in refused {
        let (status, answer) = send(&test, method, uri, p, body).await;
        assert_eq!(status.as_u16(), 403, "{method} {uri}: {answer}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_provisioner_is_refused_every_other_route_and_every_catalog() {
    let test = app().await;
    let key = provisioner(&test).await;
    let p = Some(key.as_str());
    let refused = [
        ("GET", "/api/v1/users", None),
        ("GET", "/api/v1/teams", None),
        ("GET", "/api/v1/keys", None),
        (
            "POST",
            "/api/v1/keys",
            Some(
                json!({"name": "x", "access": "admin", "all": true, "user": crate::db::TEST_USER}),
            ),
        ),
        (
            "PUT",
            "/api/v1/settings/product_name",
            Some(json!({"value": "x"})),
        ),
        ("PUT", "/api/v1/users/x/admin", Some(json!({"admin": true}))),
        (
            "PUT",
            "/api/v1/users/x/entitlements/main",
            Some(json!({"access": "read"})),
        ),
        ("DELETE", "/api/v1/users/x/sessions", None),
        ("GET", "/api/v1/unassigned", None),
        ("GET", "/api/v1/catalogs/main/tree", None),
        ("GET", "/api/v1/catalogs/main/export", None),
        ("GET", "/api/v1/search?q=alpha", None),
        ("GET", "/api/v1/whoami", None),
        ("HEAD", "/api/v1/users?email=alice@example.com", None),
        ("GET", "/api/v1/catalogs", None),
        ("POST", "/api/v1/catalogs/main/import", None),
        (
            "PATCH",
            "/api/v1/catalogs/main",
            Some(json!({"description": "x"})),
        ),
        (
            "POST",
            "/api/v1/changesets",
            Some(json!({"catalog": "main"})),
        ),
        ("GET", "/catalogs/main/export", None),
    ];
    for (method, uri, body) in refused {
        let (status, answer) = send(&test, method, uri, p, body).await;
        assert_eq!(status.as_u16(), 403, "{method} {uri}: {answer}");
    }
    // Not an MCP credential either.
    let (mcp, _) = send(
        &test,
        "POST",
        "/mcp",
        p,
        Some(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"})),
    )
    .await;
    assert_eq!(mcp.as_u16(), 403);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_provisioners_user_detail_never_lists_credentials_even_with_no_grants() {
    let test = app().await;
    let key = provisioner(&test).await;
    let (status, body) = send(
        &test,
        "GET",
        &format!("/api/v1/users/{}", crate::db::TEST_USER),
        Some(&key),
        None,
    )
    .await;
    assert_eq!(status.as_u16(), 200);
    assert_eq!(body.get("keys"), Some(&json!([])));
    assert_eq!(body.get("codes"), Some(&json!([])));
    let (status, body) = send(
        &test,
        "GET",
        "/api/v1/users?email=nobody@example.com",
        Some(&key),
        None,
    )
    .await;
    assert_eq!(status.as_u16(), 200);
    assert_eq!(body["users"], json!([]));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_provisioners_detail_waits_for_a_live_admin_promotion() {
    use crate::store::{
        audit::AuditActor,
        users::{self, NewUser},
    };
    use std::{sync::Arc, time::Duration};
    let test = Arc::new(app().await);
    let key = provisioner(&test).await;
    users::create(
        test.app.pool(),
        &NewUser {
            name: "staff",
            display_name: "",
        },
        &AuditActor::cli(),
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    let mut tx = test.app.pool().begin().await.unwrap();
    sqlx::query("SELECT id FROM users WHERE name = 'staff' FOR UPDATE")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET is_admin = true WHERE name = 'staff'")
        .execute(&mut *tx)
        .await
        .unwrap();
    let other = Arc::clone(&test);
    let request =
        tokio::spawn(
            async move { send(&other, "GET", "/api/v1/users/staff", Some(&key), None).await },
        );
    crate::http::api::testing::wait_for_lock_waiters(test.app.pool(), 1).await;
    tx.commit().await.unwrap();
    let (status, body) = tokio::time::timeout(Duration::from_secs(10), request)
        .await
        .expect("detail did not finish after promotion")
        .unwrap();
    assert_eq!(status.as_u16(), 403, "{body}");
}

#[test]
fn the_allow_list_is_the_specs_and_names_routes_by_template() {
    let expected = [
        ("GET", "/api/v1/users"),
        ("GET", "/api/v1/users/{name}"),
        ("POST", "/api/v1/users"),
        ("PUT", "/api/v1/users/{name}/email"),
        ("POST", "/api/v1/users/{name}/invite"),
        ("DELETE", "/api/v1/users/{name}"),
        ("GET", "/api/v1/teams/{name}"),
        ("POST", "/api/v1/teams"),
        ("PUT", "/api/v1/teams/{name}/members/{user}"),
        ("DELETE", "/api/v1/teams/{name}/members/{user}"),
        ("DELETE", "/api/v1/teams/{name}"),
        ("PUT", "/api/v1/teams/{name}/suspension"),
        ("POST", "/api/v1/catalogs"),
        ("DELETE", "/api/v1/catalogs/{catalog}"),
        ("PUT", "/api/v1/catalogs/{catalog}/member-access"),
        ("GET", "/api/v1/audit"),
    ];
    assert_eq!(PROVISIONER_ROUTES, expected);
    for (method, path) in expected {
        assert!(provisioner_may(&method.parse().unwrap(), path));
    }
    assert!(provisioner_may(
        &axum::http::Method::DELETE,
        "/api/v1/users/{name}"
    ));
    assert!(!provisioner_may(
        &axum::http::Method::PUT,
        "/api/v1/users/{name}/admin"
    ));
    assert!(!provisioner_may(&axum::http::Method::GET, "/api/v1/teams"));
}
