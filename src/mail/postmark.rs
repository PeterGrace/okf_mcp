//! The Postmark provider: one `POST /email` per message over the crate's
//! reqwest (rustls). Retries 429, 5xx, 401 (a token an operator can fix
//! within the retry window) and network errors; every other refusal
//! (notably 422, an invalid request) is permanent.

use std::time::Duration;

use reqwest::header::{HeaderValue, ACCEPT};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use super::{config_error, Mail, MailError, Mailer};
use crate::error::OkfError;

/// Postmark's single-message endpoint.
pub(crate) const ENDPOINT: &str = "https://api.postmarkapp.com/email";

/// How long one request may take.
const POSTMARK_TIMEOUT: Duration = Duration::from_secs(30);

/// The header carrying the server token.
const TOKEN_HEADER: &str = "X-Postmark-Server-Token";

/// Sends through Postmark. No `Debug`: it holds the token.
pub(crate) struct PostmarkMailer {
    client: reqwest::Client,
    endpoint: String,
    // Marked sensitive, so even reqwest's own debug output hides it.
    token: HeaderValue,
    from: String,
    message_stream: String,
}

/// The request body (Postmark's field names are PascalCase).
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct Body<'a> {
    from: &'a str,
    to: &'a str,
    subject: &'a str,
    text_body: &'a str,
    html_body: &'a str,
    message_stream: &'a str,
}

/// An error answer's body: `{"ErrorCode": 300, "Message": "..."}`.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct Reply {
    error_code: i64,
    message: String,
}

impl PostmarkMailer {
    /// A Postmark mailer.
    ///
    /// # Arguments
    /// * `endpoint` - [`ENDPOINT`] (tests pass a mock server's URL).
    /// * `token` - the server token from `token_env`.
    /// * `from` - `mail.from` (a confirmed sender signature).
    /// * `message_stream` - `mail.postmark.message_stream`.
    ///
    /// # Returns
    /// The mailer.
    ///
    /// # Errors
    /// [`OkfError::Config`] for a token that cannot be a header value, or
    /// an HTTP client that cannot be built.
    pub(crate) fn new(
        endpoint: &str,
        token: &str,
        from: &str,
        message_stream: &str,
    ) -> Result<Self, OkfError> {
        // The error never shows the token itself.
        let mut token = HeaderValue::from_str(token.trim()).map_err(|_| {
            config_error("mail.postmark.token_env holds characters a header cannot".to_string())
        })?;
        token.set_sensitive(true);
        let client = reqwest::Client::builder()
            .timeout(POSTMARK_TIMEOUT)
            .build()
            .map_err(|error| config_error(format!("building the Postmark client: {error}")))?;
        Ok(Self {
            client,
            endpoint: endpoint.to_string(),
            token,
            from: from.to_string(),
            message_stream: message_stream.to_string(),
        })
    }
}

/// Sorts a refusal by its HTTP status.
fn classify(status: StatusCode, reply: &Reply) -> MailError {
    let detail = format!(
        "postmark answered {status} (ErrorCode {}): {}",
        reply.error_code, reply.message
    );
    if status == StatusCode::TOO_MANY_REQUESTS
        || status == StatusCode::UNAUTHORIZED
        || status.is_server_error()
    {
        MailError::Retryable(detail)
    } else {
        MailError::Permanent(detail)
    }
}

#[async_trait::async_trait]
impl Mailer for PostmarkMailer {
    async fn send(&self, mail: &Mail) -> Result<(), MailError> {
        let body = Body {
            from: &self.from,
            to: &mail.to,
            subject: &mail.subject,
            text_body: &mail.text,
            html_body: &mail.html,
            message_stream: &self.message_stream,
        };
        let response = self
            .client
            .post(&self.endpoint)
            .header(ACCEPT, "application/json")
            .header(TOKEN_HEADER, self.token.clone())
            .json(&body)
            .send()
            .await
            .map_err(|error| MailError::Retryable(format!("postmark: {error}")))?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        // An unreadable error body still has a status to go by.
        let reply = response.json::<Reply>().await.unwrap_or_default();
        Err(classify(status, &reply))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use axum::extract::State;
    use axum::http::HeaderMap;
    use axum::routing::post;
    use axum::{Json, Router};
    use serde_json::{json, Value};

    /// What the mock saw, and what it answers.
    #[derive(Clone)]
    struct Mock {
        seen: Arc<Mutex<Vec<(HeaderMap, Value)>>>,
        status: axum::http::StatusCode,
        reply: Value,
    }

    async fn handle(
        State(mock): State<Mock>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> (axum::http::StatusCode, Json<Value>) {
        mock.seen.lock().unwrap().push((headers, body));
        (mock.status, Json(mock.reply.clone()))
    }

    /// Serves a mock `/email` on a free port.
    async fn mock_postmark(status: u16, reply: Value) -> (String, Mock) {
        let mock = Mock {
            seen: Arc::new(Mutex::new(Vec::new())),
            status: axum::http::StatusCode::from_u16(status).unwrap(),
            reply,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/email", listener.local_addr().unwrap());
        let router = Router::new()
            .route("/email", post(handle))
            .with_state(mock.clone());
        tokio::spawn(async move { axum::serve(listener, router).await });
        (endpoint, mock)
    }

    fn mail() -> Mail {
        Mail {
            to: "alice@example.com".to_string(),
            subject: "okf test email".to_string(),
            text: "the text".to_string(),
            html: "<p>the html</p>".to_string(),
        }
    }

    fn mailer(endpoint: &str) -> PostmarkMailer {
        PostmarkMailer::new(
            endpoint,
            "server-token",
            "okf <okf@example.com>",
            "outbound",
        )
        .unwrap()
    }

    #[tokio::test]
    async fn the_request_has_postmarks_shape() {
        let (endpoint, mock) = mock_postmark(
            200,
            json!({"To": "alice@example.com", "ErrorCode": 0, "Message": "OK", "MessageID": "m-1"}),
        )
        .await;

        mailer(&endpoint).send(&mail()).await.unwrap();

        let seen = mock.seen.lock().unwrap();
        let (headers, body) = &seen[0];
        assert_eq!(headers["x-postmark-server-token"], "server-token");
        assert_eq!(headers["accept"], "application/json");
        assert_eq!(headers["content-type"], "application/json");
        assert_eq!(
            body,
            &json!({
                "From": "okf <okf@example.com>",
                "To": "alice@example.com",
                "Subject": "okf test email",
                "TextBody": "the text",
                "HtmlBody": "<p>the html</p>",
                "MessageStream": "outbound",
            })
        );
    }

    #[tokio::test]
    async fn an_invalid_request_is_permanent_and_carries_postmarks_message() {
        let (endpoint, _mock) = mock_postmark(
            422,
            json!({"ErrorCode": 300, "Message": "Invalid 'To' address"}),
        )
        .await;

        let refused = mailer(&endpoint).send(&mail()).await;

        assert!(
            matches!(&refused, Err(MailError::Permanent(detail))
                if detail.contains("300") && detail.contains("Invalid 'To' address")),
            "{refused:?}"
        );
    }

    #[tokio::test]
    async fn rate_limits_outages_bad_tokens_and_unreachable_servers_are_retryable() {
        for status in [429, 500, 503, 401] {
            let (endpoint, _mock) = mock_postmark(status, json!({})).await;
            let refused = mailer(&endpoint).send(&mail()).await;
            assert!(
                matches!(refused, Err(MailError::Retryable(_))),
                "{status}: {refused:?}"
            );
        }
        // A port that was just free: nothing listens there now.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let closed = format!("http://{}/email", listener.local_addr().unwrap());
        drop(listener);
        let unreachable = mailer(&closed).send(&mail()).await;
        assert!(
            matches!(unreachable, Err(MailError::Retryable(_))),
            "{unreachable:?}"
        );
    }
}
