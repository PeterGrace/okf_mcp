//! `/api/v1/settings` (spec "Settings"): what `okf-mcp settings ...` does.
//! The listing also shows the configured OIDC providers and the mail
//! provider and From address, read-only, never a secret.
//!
//! Access: listing needs any admin; setting and resetting need an admin
//! that reaches every catalog (M11's rule for server-wide changes).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::{get, put};
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::auth::Principal;
use crate::clock::format_timestamp;
use crate::error::OkfError;
use crate::http::api::{require_admin, require_every_catalog, settle};
use crate::http::AppError;
use crate::store::settings::{self, SettingKey, SettingRow};

/// One setting as the API shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SettingView {
    /// Its snake_case key.
    pub key: &'static str,
    /// Its value now (the override, else the config default), as text.
    pub value: String,
    /// Whether the database overrides the config file.
    pub overridden: bool,
    /// When it was last set, if it is overridden.
    pub updated_at: Option<String>,
    /// Who set it, if it is overridden.
    pub updated_by: Option<String>,
}

impl From<SettingRow> for SettingView {
    fn from(row: SettingRow) -> Self {
        let SettingRow {
            key,
            value,
            overridden,
            updated_at,
            updated_by,
        } = row;
        Self {
            key: key.as_str(),
            value,
            overridden,
            updated_at: updated_at.map(format_timestamp),
            updated_by,
        }
    }
}

/// One configured OIDC provider, without its client id or secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct OidcProviderView {
    /// Its stable id.
    pub id: String,
    /// Its button text.
    pub label: String,
    /// Its issuer URL.
    pub issuer: String,
}

/// The mail provider, without any secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct MailView {
    /// `smtp`, `postmark` or `log`.
    pub provider: &'static str,
    /// The From address.
    pub from: String,
}

/// `GET /settings`' answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SettingsView {
    /// Every setting, in the store's order.
    pub settings: Vec<SettingView>,
    /// The configured OIDC providers.
    pub oidc: Vec<OidcProviderView>,
    /// The mail provider.
    pub mail: MailView,
}

/// `PUT /settings/{key}`'s body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingBody {
    /// The value, as the CLI takes it (`true`, `20`, `12h`, ...).
    value: String,
}

/// This area's routes.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/settings", get(list_settings))
        .route("/settings/{key}", put(set_setting).delete(reset_setting))
}

/// The key named by a path segment.
///
/// # Errors
/// [`OkfError::NotFound`] for an unknown key.
fn key_of(text: &str) -> Result<SettingKey, OkfError> {
    SettingKey::parse(text).ok_or_else(|| OkfError::NotFound {
        path: format!("settings/{text}"),
    })
}

/// One setting as it now is.
///
/// # Errors
/// [`OkfError::Database`]; [`OkfError::NotFound`] if the listing has no
/// row for a key it defines (a bug).
async fn current(app: &App, key: SettingKey) -> Result<SettingView, OkfError> {
    settings::list(app.pool(), &app.config().auth)
        .await?
        .into_iter()
        .find(|row| row.key == key)
        .map(SettingView::from)
        .ok_or_else(|| OkfError::NotFound {
            path: format!("settings/{}", key.as_str()),
        })
}

/// `GET /settings`: every setting, the OIDC providers and the mail provider.
///
/// # Errors
/// 403 below admin; 500 for a database failure.
async fn list_settings(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
) -> Result<Json<SettingsView>, AppError> {
    require_admin(&principal)?;
    let config = app.config();
    Ok(Json(SettingsView {
        settings: settings::list(app.pool(), &config.auth)
            .await?
            .into_iter()
            .map(SettingView::from)
            .collect(),
        oidc: config
            .auth
            .oidc
            .iter()
            .map(|provider| OidcProviderView {
                id: provider.id.clone(),
                label: provider.label.clone(),
                issuer: provider.issuer.clone(),
            })
            .collect(),
        mail: MailView {
            provider: config.mail.provider.as_str(),
            from: config.mail.from.clone(),
        },
    }))
}

/// `PUT /settings/{key}`: overrides a setting.
///
/// # Errors
/// 403 unless the caller is an admin reaching every catalog; 404 for an
/// unknown key; 422 for a value the setting refuses, or without `value`.
async fn set_setting(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(key): Path<String>,
    Json(body): Json<SettingBody>,
) -> Result<Json<SettingView>, AppError> {
    require_every_catalog(&principal)?;
    let key = key_of(&key)?;
    settings::set(
        app.pool(),
        key,
        &body.value,
        &principal.actor,
        app.clock().now(),
    )
    .await?;
    // `key` names the setting here, so the credential is logged as `key_id`.
    tracing::info!(setting = key.as_str(), value = %body.value, key_id = %principal.key_id,
                   actor = %principal.actor, "setting changed");
    settle(&app).await;
    Ok(Json(current(&app, key).await?))
}

/// `DELETE /settings/{key}`: back to the config file's default.
///
/// # Errors
/// 403 unless the caller is an admin reaching every catalog; 404 for an
/// unknown key.
async fn reset_setting(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(key): Path<String>,
) -> Result<Json<SettingView>, AppError> {
    require_every_catalog(&principal)?;
    let key = key_of(&key)?;
    settings::reset(app.pool(), key).await?;
    tracing::info!(setting = key.as_str(), key_id = %principal.key_id, actor = %principal.actor,
                   "setting reset");
    settle(&app).await;
    Ok(Json(current(&app, key).await?))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use serde_json::{json, Value};

    use crate::http::api::testing::{
        app, assert_matrix_with, every_catalog_admin, keys, send, Row,
    };
    use crate::http::api::testing::{Browser, Payload};
    use crate::mcp::testing::TestApp;
    use crate::store::audit::AuditActor;
    use crate::store::users;
    use crate::store::users::AdminChange;
    use chrono::Utc;

    #[tokio::test]
    async fn settings_need_an_admin_to_read_and_one_reaching_every_catalog_to_write() {
        let test = app().await;
        let keys = keys(&test).await;
        let write = |method: &'static str, body: Option<serde_json::Value>| Row {
            method,
            uri: "/api/v1/settings/max_keys_per_user".to_string(),
            body,
            read: 403,
            write: 403,
            admin: 403,
            stranger: 403,
        };
        assert_matrix_with(
            &test,
            &keys,
            &[
                Row {
                    method: "GET",
                    uri: "/api/v1/settings".to_string(),
                    body: None,
                    read: 403,
                    write: 403,
                    admin: 200,
                    stranger: 200,
                },
                write("PUT", Some(json!({"value": "5"}))),
                write("DELETE", None),
            ],
        )
        .await;
    }

    /// `PUT /api/v1/settings/{key}` with `{value}`, as `root`.
    async fn put(test: &TestApp, root: &str, key: &str, value: &str) -> (StatusCode, Value) {
        let body = json!({ "value": value });
        send(
            test,
            "PUT",
            &format!("/api/v1/settings/{key}"),
            Some(root),
            Some(body),
        )
        .await
    }

    #[tokio::test]
    async fn a_setting_is_set_listed_and_reset() {
        let test = app().await;
        let root = every_catalog_admin(&test).await;
        let find = |listing: &serde_json::Value, key: &str| -> serde_json::Value {
            listing["settings"]
                .as_array()
                .expect("a list")
                .iter()
                .find(|row| row["key"] == key)
                .expect("the setting")
                .clone()
        };

        let (_, before) = send(&test, "GET", "/api/v1/settings", Some(&root), None).await;
        let (set, row) = put(&test, &root, "max_keys_per_user", "5").await;
        let applied = test.app.settings().max_keys_per_user;
        let (invalid, _) = put(&test, &root, "max_keys_per_user", "many").await;
        let (unknown, _) = put(&test, &root, "max_keys", "5").await;
        let (malformed, _) = put(&test, &root, "max%00keys", "5").await;
        let (reset, back) = send(
            &test,
            "DELETE",
            "/api/v1/settings/max_keys_per_user",
            Some(&root),
            None,
        )
        .await;

        assert_eq!(find(&before, "max_keys_per_user")["value"], "20");
        assert_eq!(find(&before, "max_keys_per_user")["overridden"], false);
        assert_eq!(before["oidc"], json!([]));
        assert_eq!(before["mail"]["provider"], "log");
        assert!(before["mail"]["from"].is_string());
        assert_eq!(set.as_u16(), 200, "{row}");
        assert_eq!(row["value"], "5");
        assert_eq!(row["overridden"], true);
        assert_eq!(row["updated_by"], "root");
        assert_eq!(applied, 5, "the route polls, so the cached policy has it");
        assert_eq!(invalid.as_u16(), 422);
        assert_eq!(unknown.as_u16(), 404);
        assert_eq!(malformed.as_u16(), 404);
        assert_eq!(reset.as_u16(), 200);
        assert_eq!(back["value"], "20");
        assert_eq!(back["overridden"], false);
        assert_eq!(back["updated_by"], serde_json::Value::Null);
    }

    /// Browsers for user `plain` (no admin) and user `boss` (an admin).
    async fn sessions(test: &TestApp) -> (Browser, Browser) {
        for name in ["plain", "boss"] {
            users::create(
                test.app.pool(),
                &users::NewUser {
                    name,
                    display_name: "",
                },
                &AuditActor::cli(),
                Utc::now(),
            )
            .await
            .expect("create the user");
        }
        users::set_admin(
            test.app.pool(),
            "boss",
            AdminChange {
                on: true,
                allow_last: false,
            },
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .expect("make boss an admin");
        (
            Browser::session_for(test, "plain").await,
            Browser::session_for(test, "boss").await,
        )
    }

    #[tokio::test]
    async fn a_non_admin_session_is_refused_and_an_admin_session_may_change_settings() {
        let test = app().await;
        let (mut plain, mut boss) = sessions(&test).await;
        let value = || Payload::Json(json!({"value": "5"}));
        let uri = "/api/v1/settings/max_keys_per_user";

        let (plain_read, _) = plain
            .send(&test, "GET", "/api/v1/settings", Payload::Empty)
            .await
            .parts();
        let (plain_write, _) = plain.send(&test, "PUT", uri, value()).await.parts();
        let (plain_reset, _) = plain
            .send(&test, "DELETE", uri, Payload::Empty)
            .await
            .parts();
        let (boss_read, _) = boss
            .send(&test, "GET", "/api/v1/settings", Payload::Empty)
            .await
            .parts();
        let (boss_write, row) = boss.send(&test, "PUT", uri, value()).await.parts();
        let (boss_reset, _) = boss
            .send(&test, "DELETE", uri, Payload::Empty)
            .await
            .parts();

        assert_eq!(
            [
                plain_read.as_u16(),
                plain_write.as_u16(),
                plain_reset.as_u16()
            ],
            [403, 403, 403]
        );
        assert_eq!(boss_read.as_u16(), 200);
        assert_eq!(boss_write.as_u16(), 200, "{row}");
        assert_eq!(row["updated_by"], "boss");
        assert_eq!(boss_reset.as_u16(), 200);
    }
}
