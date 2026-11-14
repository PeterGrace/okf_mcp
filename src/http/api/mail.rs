//! `/api/v1/mail` (spec "Background jobs: Visibility"): what `okf-mcp mail
//! ...` does. The queue shows recipients, templates, attempts and errors,
//! never a body (a setup or reset link is a secret).
//!
//! Access: listing needs any admin; queueing, retrying and deleting need
//! an admin that reaches every catalog.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::auth::Principal;
use crate::clock::format_timestamp;
use crate::http::api::{require_admin, require_every_catalog};
use crate::http::AppError;
use crate::mail::queue::{self, QueuedMail};
use crate::mail::Template;

/// One queued mail, never its body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct QueuedMailView {
    /// The job's id (the `{id}` of the other routes).
    pub id: String,
    /// The recipient.
    pub to: String,
    /// `setup`, `reset`, `method_added` or `test`.
    pub template: String,
    /// `pending`, `sending`, `retrying` or `failed`.
    pub status: String,
    /// Attempts so far.
    pub attempts: i32,
    /// The last provider error, if any.
    pub last_error: Option<String>,
    /// When it runs next (or ran last).
    pub run_at: String,
}

impl From<QueuedMail> for QueuedMailView {
    fn from(mail: QueuedMail) -> Self {
        let QueuedMail {
            id,
            to,
            template,
            status,
            attempts,
            last_error,
            run_at,
        } = mail;
        Self {
            id,
            to,
            template,
            status,
            attempts,
            last_error,
            run_at: format_timestamp(run_at),
        }
    }
}

/// `GET /mail/queue`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct MailQueue {
    /// Every mail not yet sent, oldest due first.
    pub mail: Vec<QueuedMailView>,
}

/// `POST /mail/test`'s body and answer.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TestMail {
    /// The recipient.
    pub to: String,
}

/// This area's routes.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/mail/test", post(send_test))
        .route("/mail/queue", get(list_queue))
        .route("/mail/queue/{id}", delete(delete_mail))
        .route("/mail/queue/{id}/retry", post(retry_mail))
}

/// `POST /mail/test`: queues the test email.
///
/// # Returns
/// 202: the mail is queued, not yet sent.
///
/// # Errors
/// 403 unless the caller is an admin reaching every catalog; 422 for an
/// address that is not one, or without `to`.
async fn send_test(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Json(body): Json<TestMail>,
) -> Result<(StatusCode, Json<TestMail>), AppError> {
    require_every_catalog(&principal)?;
    queue::enqueue(app.pool(), &body.to, &Template::Test, app.clock().now()).await?;
    // The address is not logged.
    tracing::info!(key = %principal.key_id, actor = %principal.actor, "test email queued");
    Ok((StatusCode::ACCEPTED, Json(body)))
}

/// `GET /mail/queue`: every mail not yet sent.
///
/// # Errors
/// 403 below admin; 500 for a database failure.
async fn list_queue(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
) -> Result<Json<MailQueue>, AppError> {
    require_admin(&principal)?;
    Ok(Json(MailQueue {
        mail: queue::list(app.pool())
            .await?
            .into_iter()
            .map(QueuedMailView::from)
            .collect(),
    }))
}

/// `POST /mail/queue/{id}/retry`: sends a failed mail again, with a new
/// 24-hour retry window.
///
/// # Returns
/// 204.
///
/// # Errors
/// 403 unless the caller is an admin reaching every catalog; 404 for an
/// unknown id; 409 for a mail that has not failed.
async fn retry_mail(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    require_every_catalog(&principal)?;
    queue::retry(app.pool(), &id, app.clock().now()).await?;
    tracing::info!(mail = %id, key = %principal.key_id, actor = %principal.actor, "mail retried");
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /mail/queue/{id}`: deletes a mail not being sent right now.
///
/// # Returns
/// 204.
///
/// # Errors
/// 403 unless the caller is an admin reaching every catalog; 404 for an
/// unknown id; 409 while a worker is sending it.
async fn delete_mail(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    require_every_catalog(&principal)?;
    queue::delete(app.pool(), &id).await?;
    tracing::info!(mail = %id, key = %principal.key_id, actor = %principal.actor, "mail deleted");
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;

    use crate::http::api::testing::{
        app, assert_matrix_with, every_catalog_admin, keys, send, Row,
    };
    use crate::http::api::testing::{Browser, Payload};
    use crate::mail::{queue, Template};
    use crate::mcp::testing::TestApp;
    use crate::store::audit::AuditActor;
    use crate::store::users;
    use crate::store::users::AdminChange;

    #[tokio::test]
    async fn mail_routes_need_an_admin_to_read_and_one_reaching_every_catalog_to_write() {
        let test = app().await;
        let keys = keys(&test).await;
        queue::enqueue(
            test.app.pool(),
            "ops@example.com",
            &Template::Test,
            Utc::now(),
        )
        .await
        .expect("queue one");
        let id = queue::list(test.app.pool()).await.expect("list")[0]
            .id
            .clone();
        let write = |method: &'static str, uri: String, body: Option<serde_json::Value>| Row {
            method,
            uri,
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
                    uri: "/api/v1/mail/queue".to_string(),
                    body: None,
                    read: 403,
                    write: 403,
                    admin: 200,
                    stranger: 200,
                },
                write(
                    "POST",
                    "/api/v1/mail/test".to_string(),
                    Some(json!({"to": "ops@example.com"})),
                ),
                write("POST", format!("/api/v1/mail/queue/{id}/retry"), None),
                write("DELETE", format!("/api/v1/mail/queue/{id}"), None),
            ],
        )
        .await;
        assert_eq!(
            queue::list(test.app.pool()).await.expect("list").len(),
            1,
            "no refused write queued or deleted anything"
        );
    }

    #[tokio::test]
    async fn a_test_mail_is_queued_retried_and_deleted() {
        let test = app().await;
        let root = every_catalog_admin(&test).await;

        let (queued, body) = send(
            &test,
            "POST",
            "/api/v1/mail/test",
            Some(&root),
            Some(json!({"to": "ops@example.com"})),
        )
        .await;
        let (bad_address, _) = send(
            &test,
            "POST",
            "/api/v1/mail/test",
            Some(&root),
            Some(json!({"to": "ops@example.com, evil@example.com"})),
        )
        .await;
        let (_, listed) = send(&test, "GET", "/api/v1/mail/queue", Some(&root), None).await;
        let id = listed["mail"][0]["id"].as_str().expect("an id").to_string();
        let (not_failed, _) = send(
            &test,
            "POST",
            &format!("/api/v1/mail/queue/{id}/retry"),
            Some(&root),
            None,
        )
        .await;
        sqlx::query("UPDATE apalis.jobs SET status = 'Killed' WHERE id = $1")
            .bind(&id)
            .execute(test.app.pool())
            .await
            .expect("fail the job");
        let (_, failed) = send(&test, "GET", "/api/v1/mail/queue", Some(&root), None).await;
        let (retried, _) = send(
            &test,
            "POST",
            &format!("/api/v1/mail/queue/{id}/retry"),
            Some(&root),
            None,
        )
        .await;
        let (_, pending) = send(&test, "GET", "/api/v1/mail/queue", Some(&root), None).await;
        let (deleted, _) = send(
            &test,
            "DELETE",
            &format!("/api/v1/mail/queue/{id}"),
            Some(&root),
            None,
        )
        .await;
        let (_, empty) = send(&test, "GET", "/api/v1/mail/queue", Some(&root), None).await;
        let (unknown, _) = send(
            &test,
            "DELETE",
            &format!("/api/v1/mail/queue/{id}"),
            Some(&root),
            None,
        )
        .await;

        assert_eq!(queued.as_u16(), 202, "{body}");
        assert_eq!(body, json!({"to": "ops@example.com"}));
        assert_eq!(bad_address.as_u16(), 422, "one plain address only");
        assert_eq!(listed["mail"].as_array().map(Vec::len), Some(1));
        assert_eq!(listed["mail"][0]["to"], "ops@example.com");
        assert_eq!(listed["mail"][0]["template"], "test");
        assert_eq!(listed["mail"][0]["status"], "pending");
        assert!(listed["mail"][0].get("text").is_none(), "never the body");
        assert_eq!(not_failed.as_u16(), 409, "only failed mail is retried");
        assert_eq!(failed["mail"][0]["status"], "failed");
        assert_eq!(retried.as_u16(), 204);
        assert_eq!(pending["mail"][0]["status"], "pending");
        assert_eq!(pending["mail"][0]["attempts"], 0);
        assert_eq!(deleted.as_u16(), 204);
        assert_eq!(empty, json!({"mail": []}));
        assert_eq!(unknown.as_u16(), 404);
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
    async fn a_non_admin_session_is_refused_and_an_admin_session_may_use_the_queue() {
        let test = app().await;
        let (mut plain, mut boss) = sessions(&test).await;
        let to = || Payload::Json(json!({"to": "ops@example.com"}));
        queue::enqueue(
            test.app.pool(),
            "ops@example.com",
            &Template::Test,
            Utc::now(),
        )
        .await
        .expect("queue one");
        let id = queue::list(test.app.pool()).await.expect("list")[0]
            .id
            .clone();
        let one = format!("/api/v1/mail/queue/{id}");
        let retry = format!("{one}/retry");

        let mut refused = Vec::new();
        for (method, uri, payload) in [
            ("GET", "/api/v1/mail/queue", Payload::Empty),
            ("POST", "/api/v1/mail/test", to()),
            ("POST", retry.as_str(), Payload::Empty),
            ("DELETE", one.as_str(), Payload::Empty),
        ] {
            let (status, _) = plain.send(&test, method, uri, payload).await.parts();
            refused.push(status.as_u16());
        }
        let (listed, _) = boss
            .send(&test, "GET", "/api/v1/mail/queue", Payload::Empty)
            .await
            .parts();
        let (queued, _) = boss
            .send(&test, "POST", "/api/v1/mail/test", to())
            .await
            .parts();
        let (deleted, _) = boss
            .send(&test, "DELETE", &one, Payload::Empty)
            .await
            .parts();

        assert_eq!(refused, [403, 403, 403, 403]);
        assert_eq!(listed.as_u16(), 200);
        assert_eq!(queued.as_u16(), 202);
        assert_eq!(deleted.as_u16(), 204);
    }
}
