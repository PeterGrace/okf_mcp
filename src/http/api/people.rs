//! `/api/v1/users`, `/api/v1/teams` and `/api/v1/unassigned` (users and
//! teams design, "REST"): what `okf-mcp user ...`, `team ...` and
//! `unassigned` do, over HTTP, with the same `store::users`, `store::teams`
//! and `store::unassigned` functions.
//!
//! Access (plan Decision 3):
//! - creating, deleting and changing users, teams and memberships, and
//!   listing what is unassigned, need an admin key that reaches every
//!   catalog: users and teams are not catalog-scoped;
//! - listing and reading users and teams needs any admin key, so a scoped
//!   admin can pick the user of a key it creates;
//! - a user's detail shows only the keys, grants and codes the caller
//!   manages (`api::keys`' rule), so it reveals no credential outside a
//!   scoped admin's reach.
//!
//! Every write bumps the administrative generation; the handlers poll after
//! it, so the next request sees the change.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, put};
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::auth::Principal;
use crate::clock::format_timestamp;
use crate::http::api::keys::{manages, manages_grants, KeyView};
use crate::http::api::oauth::SignInCodeView;
use crate::http::api::{require_admin, require_every_catalog, settle};
use crate::http::AppError;
use crate::store::keys as key_rows;
use crate::store::oauth::codes;
use crate::store::teams::{self, MemberChange, NewTeam, Role, TeamRecord};
use crate::store::unassigned;
use crate::store::users::{self, NewUser, UserRecord};

/// One user as the API shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct UserView {
    /// Its name (`[a-z0-9-]+`), which its personal team shares.
    pub name: String,
    /// Free text, possibly empty.
    pub display_name: String,
    /// When it was created (`2026-11-11T12:00:00Z`).
    pub created_at: String,
    /// How many keys, grants (live and revoked) and sign-in codes name it,
    /// whoever manages them.
    pub credentials: i64,
    /// Its teams' names, sorted; the personal team included.
    pub teams: Vec<String>,
    /// Its email address, if set.
    pub email: Option<String>,
    /// Whether its console session acts as an admin.
    pub is_admin: bool,
    /// Whether it has a local password.
    pub has_password: bool,
}

impl From<UserRecord> for UserView {
    fn from(record: UserRecord) -> Self {
        // Destructuring names every field, so one added to the record later
        // fails to compile here until someone decides whether to show it.
        let UserRecord {
            name,
            display_name,
            created_at,
            credentials,
            teams,
            email,
            is_admin,
            has_password,
        } = record;
        Self {
            name,
            display_name,
            created_at: format_timestamp(created_at),
            credentials,
            teams,
            email,
            is_admin,
            has_password,
        }
    }
}

/// `GET /users`' answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct UserList {
    /// Every user, by name.
    pub users: Vec<UserView>,
}

/// `GET /users/{name}`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct UserDetail {
    /// The user.
    pub user: UserView,
    /// Its keys and grants that the caller manages, revoked ones included.
    pub keys: Vec<KeyView>,
    /// Its sign-in codes that the caller manages, in every state.
    pub codes: Vec<SignInCodeView>,
}

/// One member of a team.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct MemberView {
    /// The user's name.
    pub user: String,
    /// `owner` or `member`.
    pub role: &'static str,
}

/// One team as the API shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct TeamView {
    /// Its name.
    pub name: String,
    /// Free text, possibly empty.
    pub display_name: String,
    /// The user whose personal team this is (`None` for a shared team).
    pub personal_of: Option<String>,
    /// When it was created.
    pub created_at: String,
    /// Its members, by user name.
    pub members: Vec<MemberView>,
    /// The catalogs it owns, sorted.
    pub catalogs: Vec<String>,
}

impl From<TeamRecord> for TeamView {
    fn from(record: TeamRecord) -> Self {
        let TeamRecord {
            name,
            display_name,
            personal_of,
            created_at,
            members,
            catalogs,
        } = record;
        Self {
            name,
            display_name,
            personal_of,
            created_at: format_timestamp(created_at),
            members: members
                .into_iter()
                .map(|member| MemberView {
                    user: member.user,
                    role: member.role.as_str(),
                })
                .collect(),
            catalogs,
        }
    }
}

/// `GET /teams`' answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct TeamList {
    /// Every team, by name.
    pub teams: Vec<TeamView>,
}

/// `PUT /teams/{name}/members/{user}`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct MembershipChanged {
    /// `added`, `role_changed` or `unchanged`.
    pub change: &'static str,
    /// The team as it now is.
    pub team: TeamView,
}

/// `GET /unassigned`'s answer: what predates users and teams and still has
/// neither.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct UnassignedView {
    /// Keys and grants with no user, live and revoked.
    pub keys: Vec<KeyView>,
    /// Sign-in codes with no user.
    pub codes: Vec<SignInCodeView>,
    /// Names of the catalogs with no owner team.
    pub catalogs: Vec<String>,
}

/// `POST /users`' body. `deny_unknown_fields` turns a misspelt field into a
/// 422 instead of silently ignoring it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateUserBody {
    /// `[a-z0-9-]+`, not taken by a user or team.
    name: String,
    /// Free text; empty when left out.
    #[serde(default)]
    display_name: String,
}

/// `POST /teams`' body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateTeamBody {
    /// `[a-z0-9-]+`, not taken by a user or team.
    name: String,
    /// Free text; empty when left out.
    #[serde(default)]
    display_name: String,
    /// The user who is its first owner (required; must exist).
    owner: String,
}

/// `PUT /teams/{name}/members/{user}`'s body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemberBody {
    /// `owner` or `member`; anything else is a 422.
    role: Role,
}

/// This area's routes.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/users", get(list_users).post(create_user))
        .route("/users/{name}", get(user_detail).delete(delete_user))
        .route("/teams", get(list_teams).post(create_team))
        .route("/teams/{name}", get(team_detail).delete(delete_team))
        .route(
            "/teams/{name}/members/{user}",
            put(set_member).delete(remove_member),
        )
        .route("/unassigned", get(list_unassigned))
}

/// The wire spelling of a [`MemberChange`].
fn change_name(change: MemberChange) -> &'static str {
    match change {
        MemberChange::Added => "added",
        MemberChange::RoleChanged => "role_changed",
        MemberChange::Unchanged => "unchanged",
    }
}

/// `GET /users`: every user.
///
/// # Errors
/// 403 below admin; 500 for a database failure.
async fn list_users(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
) -> Result<Json<UserList>, AppError> {
    require_admin(&principal)?;
    let users = users::list(app.pool())
        .await?
        .into_iter()
        .map(UserView::from)
        .collect();
    Ok(Json(UserList { users }))
}

/// `POST /users`: creates a user and its personal team.
///
/// # Returns
/// 201 with the new user.
///
/// # Errors
/// 403 unless the key is an admin reaching every catalog; 409 for an invalid
/// or taken name; 422 for a body without `name`; 500 for a database failure.
async fn create_user(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Json(body): Json<CreateUserBody>,
) -> Result<(StatusCode, Json<UserView>), AppError> {
    require_every_catalog(&principal)?;
    let new = NewUser {
        name: &body.name,
        display_name: &body.display_name,
    };
    let created = users::create(app.pool(), &new, app.clock().now()).await?;
    settle(&app).await;
    Ok((StatusCode::CREATED, Json(created.into())))
}

/// `GET /users/{name}`: the user, with the keys, grants and codes naming it
/// that the caller manages.
///
/// # Errors
/// 403 below admin; 404 for an unknown user; 500 for a database failure.
async fn user_detail(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(name): Path<String>,
) -> Result<Json<UserDetail>, AppError> {
    require_admin(&principal)?;
    let user = users::find(app.pool(), &name).await?;
    // `as_deref` turns the record's `Option<String>` into an `Option<&str>`,
    // comparable with `Some(name)` without allocating.
    let named = |owner: Option<&str>| owner == Some(name.as_str());
    let keys = key_rows::list(app.pool())
        .await?
        .into_iter()
        .filter(|key| named(key.user.as_deref()) && manages(&principal, key))
        .map(KeyView::from)
        .collect();
    let now = app.clock().now();
    let codes = codes::list(app.pool())
        .await?
        .into_iter()
        .filter(|code| {
            named(code.user.as_deref())
                && manages_grants(&principal, code.all_catalogs, &code.catalogs)
        })
        .map(|code| SignInCodeView::new(code, now))
        .collect();
    Ok(Json(UserDetail {
        user: user.into(),
        keys,
        codes,
    }))
}

/// `DELETE /users/{name}`: deletes a user that nothing names, with its
/// personal team.
///
/// # Returns
/// 204.
///
/// # Errors
/// 403 unless the key is an admin reaching every catalog; 404 for an unknown
/// user; 409 while a credential names it, it is in another team, or its
/// personal team owns a catalog; 500 for a database failure.
async fn delete_user(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(name): Path<String>,
) -> Result<StatusCode, AppError> {
    require_every_catalog(&principal)?;
    users::delete(app.pool(), &name).await?;
    settle(&app).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /teams`: every team.
///
/// # Errors
/// 403 below admin; 500 for a database failure.
async fn list_teams(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
) -> Result<Json<TeamList>, AppError> {
    require_admin(&principal)?;
    let teams = teams::list(app.pool())
        .await?
        .into_iter()
        .map(TeamView::from)
        .collect();
    Ok(Json(TeamList { teams }))
}

/// `POST /teams`: creates a shared team with `owner` as its first owner.
///
/// # Returns
/// 201 with the new team.
///
/// # Errors
/// 403 unless the key is an admin reaching every catalog; 404 for an unknown
/// owner; 409 for an invalid or taken name; 422 for a body without `name` or
/// `owner`; 500 for a database failure.
async fn create_team(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Json(body): Json<CreateTeamBody>,
) -> Result<(StatusCode, Json<TeamView>), AppError> {
    require_every_catalog(&principal)?;
    let new = NewTeam {
        name: &body.name,
        display_name: &body.display_name,
        owner: &body.owner,
    };
    let created = teams::create(app.pool(), &new, app.clock().now()).await?;
    settle(&app).await;
    Ok((StatusCode::CREATED, Json(created.into())))
}

/// `GET /teams/{name}`: one team, with its members and catalogs.
///
/// # Errors
/// 403 below admin; 404 for an unknown team; 500 for a database failure.
async fn team_detail(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(name): Path<String>,
) -> Result<Json<TeamView>, AppError> {
    require_admin(&principal)?;
    Ok(Json(teams::find(app.pool(), &name).await?.into()))
}

/// `DELETE /teams/{name}`: deletes a shared team that owns no catalog.
///
/// # Returns
/// 204.
///
/// # Errors
/// 403 unless the key is an admin reaching every catalog; 404 for an unknown
/// team; 409 for a personal team or one that owns a catalog; 500 for a
/// database failure.
async fn delete_team(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(name): Path<String>,
) -> Result<StatusCode, AppError> {
    require_every_catalog(&principal)?;
    teams::delete(app.pool(), &name).await?;
    settle(&app).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /teams/{name}/members/{user}`: adds the user with `role`, or gives
/// a member that role.
///
/// # Errors
/// 403 unless the key is an admin reaching every catalog; 404 for an unknown
/// team or user; 409 for demoting the last owner; 422 for a missing or
/// unknown `role`; 500 for a database failure.
async fn set_member(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    // Two path parameters arrive as a tuple, in the order of the pattern.
    Path((team, user)): Path<(String, String)>,
    Json(body): Json<MemberBody>,
) -> Result<Json<MembershipChanged>, AppError> {
    require_every_catalog(&principal)?;
    let change = teams::set_member(app.pool(), &team, &user, body.role, app.clock().now()).await?;
    settle(&app).await;
    Ok(Json(MembershipChanged {
        change: change_name(change),
        team: teams::find(app.pool(), &team).await?.into(),
    }))
}

/// `DELETE /teams/{name}/members/{user}`: removes a member.
///
/// # Returns
/// The team as it now is.
///
/// # Errors
/// 403 unless the key is an admin reaching every catalog; 404 for an unknown
/// team or user; 409 for a non-member, the last owner, or the user whose
/// personal team this is; 500 for a database failure.
async fn remove_member(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path((team, user)): Path<(String, String)>,
) -> Result<Json<TeamView>, AppError> {
    require_every_catalog(&principal)?;
    teams::remove_member(app.pool(), &team, &user).await?;
    settle(&app).await;
    Ok(Json(teams::find(app.pool(), &team).await?.into()))
}

/// `GET /unassigned`: every key, grant, code and catalog without a user or
/// owner. Unfiltered, so it needs a key that manages all of them.
///
/// # Errors
/// 403 unless the key is an admin reaching every catalog; 500 for a
/// database failure.
async fn list_unassigned(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
) -> Result<Json<UnassignedView>, AppError> {
    require_every_catalog(&principal)?;
    let rows = unassigned::list(app.pool()).await?;
    let now = app.clock().now();
    Ok(Json(UnassignedView {
        keys: rows.keys.into_iter().map(KeyView::from).collect(),
        codes: rows
            .codes
            .into_iter()
            .map(|code| SignInCodeView::new(code, now))
            .collect(),
        catalogs: rows.catalogs,
    }))
}

#[cfg(test)]
mod tests {
    use chrono::{TimeDelta, Utc};
    use serde_json::json;

    use crate::db::{TEST_TEAM, TEST_USER};
    use crate::http::api::testing::{
        app, assert_matrix_with, every_catalog_admin, keys, send, Row,
    };
    use crate::mcp::testing::TestApp;
    use crate::store::keys::{self as key_rows, Access, Grants, NewKey};
    use crate::store::oauth::codes::{self, NewSignInCode};

    /// Creates user `name` through the API as `root`, asserting the 201.
    async fn user(test: &TestApp, root: &str, name: &str) {
        let (status, body) = send(
            test,
            "POST",
            "/api/v1/users",
            Some(root),
            Some(json!({ "name": name })),
        )
        .await;
        assert_eq!(status.as_u16(), 201, "create user {name}: {body}");
    }

    #[tokio::test]
    async fn an_admin_creates_lists_and_deletes_a_user() {
        let test = app().await;
        let root = every_catalog_admin(&test).await;

        let (created, body) = send(
            &test,
            "POST",
            "/api/v1/users",
            Some(&root),
            Some(json!({"name": "alice", "display_name": "Alice Example"})),
        )
        .await;
        let (_, listed) = send(&test, "GET", "/api/v1/users", Some(&root), None).await;
        let (_, personal) = send(&test, "GET", "/api/v1/teams/alice", Some(&root), None).await;
        let (deleted, _) = send(&test, "DELETE", "/api/v1/users/alice", Some(&root), None).await;
        let (gone, error) = send(&test, "GET", "/api/v1/users/alice", Some(&root), None).await;

        assert_eq!(created.as_u16(), 201, "{body}");
        assert_eq!(body["name"], "alice");
        assert_eq!(body["display_name"], "Alice Example");
        assert_eq!(body["credentials"], 0);
        assert_eq!(body["teams"], json!(["alice"]));
        assert_eq!(body["email"], serde_json::Value::Null);
        assert_eq!(body["is_admin"], false);
        assert_eq!(body["has_password"], false);
        assert!(body.get("password_hash").is_none(), "{body}");
        assert!(listed["users"]
            .as_array()
            .expect("a list")
            .iter()
            .any(|u| u["name"] == "alice"));
        assert_eq!(personal["personal_of"], "alice");
        assert_eq!(
            personal["members"],
            json!([{"user": "alice", "role": "owner"}])
        );
        assert_eq!(deleted.as_u16(), 204);
        assert_eq!(gone.as_u16(), 404);
        assert_eq!(error["error"], "UnknownName");
    }

    #[tokio::test]
    async fn listing_needs_any_admin_and_writes_one_reaching_every_catalog() {
        let test = app().await;
        let keys = keys(&test).await;
        let root = every_catalog_admin(&test).await;
        user(&test, &root, "alice").await;
        let (status, body) = send(
            &test,
            "POST",
            "/api/v1/teams",
            Some(&root),
            Some(json!({"name": "family", "owner": "alice"})),
        )
        .await;
        assert_eq!(status.as_u16(), 201, "{body}");

        // Reads: any admin (callers run read, write, stranger, admin).
        let read = |uri: &str| Row {
            method: "GET",
            uri: uri.to_string(),
            body: None,
            read: 403,
            write: 403,
            admin: 200,
            stranger: 200,
        };
        // Writes: no key in the matrix reaches every catalog, so all 403,
        // the scoped admins on `main` and on `other` included.
        let write = |method: &'static str, uri: &str, body: Option<serde_json::Value>| Row {
            method,
            uri: uri.to_string(),
            body,
            read: 403,
            write: 403,
            admin: 403,
            stranger: 403,
        };
        let rows = [
            read("/api/v1/users"),
            read("/api/v1/users/alice"),
            read("/api/v1/teams"),
            read("/api/v1/teams/family"),
            write("POST", "/api/v1/users", Some(json!({"name": "bob"}))),
            write(
                "POST",
                "/api/v1/teams",
                Some(json!({"name": "friends", "owner": "bob"})),
            ),
            write(
                "PUT",
                "/api/v1/teams/family/members/test-user",
                Some(json!({"role": "member"})),
            ),
            write("DELETE", "/api/v1/teams/family/members/test-user", None),
            write("DELETE", "/api/v1/teams/family", None),
            write("DELETE", "/api/v1/teams/friends", None),
            write("DELETE", "/api/v1/users/alice", None),
            write("GET", "/api/v1/unassigned", None),
        ];
        assert_matrix_with(&test, &keys, &rows).await;
        // Every row's subject is untouched; root may do each one, in order.
        for row in &rows {
            let (status, body) =
                send(&test, row.method, &row.uri, Some(&root), row.body.clone()).await;
            assert!(
                status.is_success(),
                "{} {} as root: {status} {body}",
                row.method,
                row.uri
            );
        }
    }

    #[tokio::test]
    async fn membership_routes_and_their_refusals() {
        let test = app().await;
        let root = every_catalog_admin(&test).await;
        let admin = Some(root.as_str());
        user(&test, &root, "alice").await;
        send(
            &test,
            "POST",
            "/api/v1/teams",
            admin,
            Some(json!({"name": "family", "owner": "alice"})),
        )
        .await;

        let (added, answer) = send(
            &test,
            "PUT",
            "/api/v1/teams/family/members/test-user",
            admin,
            Some(json!({"role": "member"})),
        )
        .await;
        let (again, unchanged) = send(
            &test,
            "PUT",
            "/api/v1/teams/family/members/test-user",
            admin,
            Some(json!({"role": "member"})),
        )
        .await;
        let (bad_role, _) = send(
            &test,
            "PUT",
            "/api/v1/teams/family/members/test-user",
            admin,
            Some(json!({"role": "boss"})),
        )
        .await;
        let (last_owner, _) = send(
            &test,
            "DELETE",
            "/api/v1/teams/family/members/alice",
            admin,
            None,
        )
        .await;
        let (unknown, _) = send(
            &test,
            "PUT",
            "/api/v1/teams/family/members/nobody",
            admin,
            Some(json!({"role": "member"})),
        )
        .await;
        let (_, detail) = send(&test, "GET", "/api/v1/teams/family", admin, None).await;
        let (removed, after) = send(
            &test,
            "DELETE",
            "/api/v1/teams/family/members/test-user",
            admin,
            None,
        )
        .await;
        let (personal, _) = send(&test, "DELETE", "/api/v1/teams/alice", admin, None).await;
        let (deleted, _) = send(&test, "DELETE", "/api/v1/teams/family", admin, None).await;

        assert_eq!(added.as_u16(), 200, "{answer}");
        assert_eq!(answer["change"], "added");
        assert_eq!(again.as_u16(), 200);
        assert_eq!(unchanged["change"], "unchanged");
        assert_eq!(bad_role.as_u16(), 422);
        assert_eq!(last_owner.as_u16(), 409);
        assert_eq!(unknown.as_u16(), 404);
        assert_eq!(
            detail["members"],
            json!([
                {"user": "alice", "role": "owner"},
                {"user": "test-user", "role": "member"},
            ])
        );
        assert_eq!(detail["personal_of"], serde_json::Value::Null);
        assert_eq!(removed.as_u16(), 200);
        assert_eq!(
            after["members"],
            json!([{"user": "alice", "role": "owner"}])
        );
        assert_eq!(personal.as_u16(), 409, "a personal team goes with its user");
        assert_eq!(deleted.as_u16(), 204);
    }

    #[tokio::test]
    async fn a_missing_user_or_owner_in_a_create_body_is_a_422() {
        let test = app().await;
        let root = every_catalog_admin(&test).await;

        let (key, _) = send(
            &test,
            "POST",
            "/api/v1/keys",
            Some(&root),
            Some(json!({"name": "k", "access": "read", "all": true})),
        )
        .await;
        let (team, _) = send(
            &test,
            "POST",
            "/api/v1/teams",
            Some(&root),
            Some(json!({"name": "family"})),
        )
        .await;
        let (unknown_owner, _) = send(
            &test,
            "POST",
            "/api/v1/teams",
            Some(&root),
            Some(json!({"name": "family", "owner": "nobody"})),
        )
        .await;

        assert_eq!(key.as_u16(), 422);
        assert_eq!(team.as_u16(), 422);
        assert_eq!(unknown_owner.as_u16(), 404);
    }

    #[tokio::test]
    async fn assigning_a_legacy_key_and_listing_unassigned() {
        let test = app().await;
        crate::store::keys::insert_legacy_test_key(
            test.app.pool(),
            "legacy",
            Access::Read,
            "legacy",
            Utc::now(),
        )
        .await;
        let keys = keys(&test).await;
        let root = every_catalog_admin(&test).await;
        let assign = json!({"user": TEST_USER});

        let (_, before) = send(&test, "GET", "/api/v1/unassigned", Some(&root), None).await;
        // The legacy key reaches every catalog, so the scoped admin does not
        // manage it: a 404, as for an unknown id.
        let (scoped, _) = send(
            &test,
            "PUT",
            "/api/v1/keys/legacy/user",
            Some(&keys.admin),
            Some(assign.clone()),
        )
        .await;
        let (assigned, view) = send(
            &test,
            "PUT",
            "/api/v1/keys/legacy/user",
            Some(&root),
            Some(assign),
        )
        .await;
        let (_, after) = send(&test, "GET", "/api/v1/unassigned", Some(&root), None).await;

        assert_eq!(before["keys"][0]["id"], "legacy");
        assert_eq!(before["keys"].as_array().map(Vec::len), Some(1));
        assert_eq!(scoped.as_u16(), 404);
        assert_eq!(assigned.as_u16(), 200, "{view}");
        assert_eq!(view["user"], TEST_USER);
        assert_eq!(after, json!({"keys": [], "codes": [], "catalogs": []}));
    }

    #[tokio::test]
    async fn an_assign_reaches_the_key_cache_after_the_generation_bump() {
        // Spec "Testing: cache reload": the principal a request runs as
        // carries the new user once the route's poll has reloaded the cache.
        let test = app().await;
        let legacy = crate::store::keys::insert_legacy_test_key(
            test.app.pool(),
            "legacy",
            Access::Read,
            "legacy",
            Utc::now(),
        )
        .await;
        // The legacy insert bumps no generation, so the admin key is created
        // after it: its create bumps, and its poll loads both.
        let root = every_catalog_admin(&test).await;
        assert_eq!(test.app.keys().find(&legacy).expect("cached").user, None);

        let (status, _) = send(
            &test,
            "PUT",
            "/api/v1/keys/legacy/user",
            Some(&root),
            Some(json!({"user": TEST_USER})),
        )
        .await;
        let (_, me) = send(&test, "GET", "/api/v1/whoami", Some(&legacy), None).await;

        assert_eq!(status.as_u16(), 200);
        assert_eq!(
            test.app
                .keys()
                .find(&legacy)
                .expect("cached")
                .user
                .as_deref(),
            Some(TEST_USER)
        );
        assert_eq!(me["user"], TEST_USER);
    }

    #[tokio::test]
    async fn a_users_detail_shows_only_the_credentials_the_caller_manages() {
        let test = app().await;
        let keys = keys(&test).await;
        let root = every_catalog_admin(&test).await;
        user(&test, &root, "alice").await;
        let other = ["other".to_string()];
        for (name, grants) in [
            ("alice-main", Grants::Catalogs(&["main".to_string()])),
            ("alice-other", Grants::Catalogs(&other)),
            ("alice-all", Grants::All),
        ] {
            key_rows::create_with_id(
                test.app.pool(),
                &NewKey {
                    name,
                    access: Access::Read,
                    maintenance_report: false,
                    grants,
                    default_catalog: None,
                    actor: name,
                    user: "alice",
                    self_issued: false,
                    expires_at: None,
                    levels: &[],
                },
                Utc::now(),
            )
            .await
            .expect("create a key");
        }
        for (name, catalog) in [("code-main", "main"), ("code-other", "other")] {
            let grants = [catalog.to_string()];
            codes::create(
                test.app.pool(),
                &NewSignInCode {
                    name,
                    access: Access::Read,
                    grants: Grants::Catalogs(&grants),
                    default_catalog: None,
                    actor: "",
                    expires_at: Utc::now() + TimeDelta::hours(1),
                    user: "alice",
                },
                Utc::now(),
            )
            .await
            .expect("mint a code");
        }
        let names = |detail: &serde_json::Value, field: &str| -> Vec<String> {
            let mut names: Vec<String> = detail[field]
                .as_array()
                .expect("a list")
                .iter()
                .filter_map(|row| row["name"].as_str().map(str::to_string))
                .collect();
            names.sort();
            names
        };

        let (status, scoped) =
            send(&test, "GET", "/api/v1/users/alice", Some(&keys.admin), None).await;
        let (_, everything) = send(&test, "GET", "/api/v1/users/alice", Some(&root), None).await;

        assert_eq!(status.as_u16(), 200, "{scoped}");
        assert_eq!(scoped["user"]["credentials"], 5);
        assert_eq!(names(&scoped, "keys"), ["alice-main"]);
        assert_eq!(names(&scoped, "codes"), ["code-main"]);
        assert_eq!(
            names(&everything, "keys"),
            ["alice-all", "alice-main", "alice-other"]
        );
        assert_eq!(names(&everything, "codes"), ["code-main", "code-other"]);
        // The fixture user's own keys never show under alice.
        assert!(!names(&everything, "keys").contains(&"admin".to_string()));
    }

    #[tokio::test]
    async fn set_owner_on_a_catalog() {
        let test = app().await;
        let keys = keys(&test).await;
        let root = every_catalog_admin(&test).await;
        user(&test, &root, "alice").await;
        let change = Some(json!({"team": "alice"}));

        let (scoped, _) = send(
            &test,
            "PUT",
            "/api/v1/catalogs/main/owner",
            Some(&keys.admin),
            change.clone(),
        )
        .await;
        let (status, body) = send(
            &test,
            "PUT",
            "/api/v1/catalogs/main/owner",
            Some(&root),
            change,
        )
        .await;
        let (unknown_team, _) = send(
            &test,
            "PUT",
            "/api/v1/catalogs/main/owner",
            Some(&root),
            Some(json!({"team": "nobody"})),
        )
        .await;
        let (unknown_catalog, _) = send(
            &test,
            "PUT",
            "/api/v1/catalogs/nowhere/owner",
            Some(&root),
            Some(json!({"team": "alice"})),
        )
        .await;
        let (_, listing) = send(&test, "GET", "/api/v1/catalogs", Some(&root), None).await;
        let (_, team) = send(&test, "GET", "/api/v1/teams/alice", Some(&root), None).await;

        assert_eq!(
            scoped.as_u16(),
            403,
            "a scoped admin cannot change an owner"
        );
        assert_eq!(status.as_u16(), 200, "{body}");
        assert_eq!(
            body,
            json!({"name": "main", "previous": TEST_TEAM, "owner_team": "alice"})
        );
        assert_eq!(unknown_team.as_u16(), 404);
        assert_eq!(unknown_catalog.as_u16(), 404);
        assert_eq!(listing["catalogs"][0]["name"], "main");
        assert_eq!(listing["catalogs"][0]["owner_team"], "alice");
        assert_eq!(team["catalogs"], json!(["main"]));
    }
}
