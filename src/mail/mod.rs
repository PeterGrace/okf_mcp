//! Outgoing mail (spec "Mail", Decisions 10 and 11). A [`Mailer`] sends one
//! [`Mail`]; the provider (SMTP, Postmark or the log) is chosen at run time
//! from `[mail]`, so it is a trait object, `Arc<dyn Mailer>`. Requests never
//! send directly: they enqueue a `SendMail` job ([`queue::enqueue`]), and a
//! worker sends it with retries ([`queue::spawn_worker`]).

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::config::{MailConfig, MailProvider};
use crate::error::OkfError;

// `log` is the provider that writes to the log. A local module shadows the
// `log` crate's name inside this module, and this module never uses the crate.
pub mod invite;
mod log;
mod postmark;
pub mod queue;
mod smtp;
pub mod templates;

// Re-exported so callers write `mail::render`, `mail::Template`.
pub use templates::{link, render, Template, RESET_PATH, SETUP_PATH};

/// One rendered message. It is the `SendMail` job's payload, so it is
/// serde; its `Debug` omits the bodies (they may hold a single-use link).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mail {
    /// One recipient address.
    pub to: String,
    /// The subject line.
    pub subject: String,
    /// The plain-text body.
    pub text: String,
    /// The HTML twin.
    pub html: String,
}

impl std::fmt::Debug for Mail {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `finish_non_exhaustive` prints `..`: there are fields not shown.
        formatter
            .debug_struct("Mail")
            .field("to", &self.to)
            .field("subject", &self.subject)
            .finish_non_exhaustive()
    }
}

/// Why a send failed, and whether trying again could help. The text never
/// holds the message body.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MailError {
    /// A network problem, a timeout, a rate limit or a server error: retry.
    #[error("retryable: {0}")]
    Retryable(String),
    /// The provider refused this message for good (a bad address, a
    /// rejected sender): do not retry.
    #[error("permanent: {0}")]
    Permanent(String),
}

/// Sends mail. `#[async_trait]` rewrites `async fn` into a method returning
/// a boxed future, which is what lets the trait be used as `dyn Mailer`
/// (Rust's native `async fn` in traits cannot be called through `dyn`).
#[async_trait::async_trait]
pub trait Mailer: Send + Sync {
    /// Sends one message, once.
    ///
    /// # Arguments
    /// * `mail` - the message.
    ///
    /// # Errors
    /// [`MailError::Retryable`] or [`MailError::Permanent`].
    async fn send(&self, mail: &Mail) -> Result<(), MailError>;
}

/// Builds the configured mailer, reading its secrets from the environment.
/// `App::load` calls it, so `serve` stops at start-up when a secret is
/// missing; commands that only enqueue never call it.
///
/// # Arguments
/// * `config` - `[mail]` (already validated).
///
/// # Returns
/// The mailer, shared.
///
/// # Errors
/// [`OkfError::Config`] when a named environment variable is unset or
/// empty, the From address does not parse, or the provider cannot be built
/// (an SMTP host lettre refuses).
pub fn from_config(config: &MailConfig) -> Result<Arc<dyn Mailer>, OkfError> {
    from_config_with(config, &|name| std::env::var(name).ok())
}

/// [`from_config`] with the environment as a function, so tests need not
/// set process-wide variables.
// `&dyn Fn(&str) -> Option<String>`: any callable taking a name, like a
// Python `Callable[[str], Optional[str]]`.
pub(crate) fn from_config_with(
    config: &MailConfig,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Arc<dyn Mailer>, OkfError> {
    match config.provider {
        MailProvider::Log => {
            tracing::info!("mail provider is \"log\": messages, links included, go to the log");
            Ok(Arc::new(log::LogMailer))
        }
        MailProvider::Smtp => {
            let smtp = config
                .smtp
                .as_ref()
                .ok_or_else(|| config_error("provider = \"smtp\" needs [mail.smtp]".to_string()))?;
            // `map` then `transpose`: read the password only when a variable
            // is named, and turn `Option<Result<..>>` into `Result<Option<..>>`.
            let password = smtp
                .password_env
                .as_deref()
                .map(|name| secret(env, name, "mail.smtp.password_env"))
                .transpose()?;
            Ok(Arc::new(smtp::SmtpMailer::from_config(
                &config.from,
                smtp,
                password,
            )?))
        }
        MailProvider::Postmark => {
            let postmark = config.postmark.as_ref().ok_or_else(|| {
                config_error("provider = \"postmark\" needs [mail.postmark]".to_string())
            })?;
            let token = secret(env, &postmark.token_env, "mail.postmark.token_env")?;
            Ok(Arc::new(postmark::PostmarkMailer::new(
                postmark::ENDPOINT,
                &token,
                &config.from,
                &postmark.message_stream,
            )?))
        }
    }
}

/// Reads a secret from the environment variable a config key names. The
/// error names the variable, never its value.
fn secret(env: &dyn Fn(&str) -> Option<String>, name: &str, key: &str) -> Result<String, OkfError> {
    match env(name) {
        Some(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(config_error(format!(
            "{key} names {name}, which is not set or is empty"
        ))),
    }
}

/// A `[mail]` problem found at start-up.
pub(crate) fn config_error(message: String) -> OkfError {
    OkfError::Config {
        path: "[mail]".to_string(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PostmarkConfig, SmtpConfig};

    fn smtp_config() -> MailConfig {
        MailConfig {
            provider: MailProvider::Smtp,
            from: "okf <okf@example.com>".to_string(),
            smtp: Some(SmtpConfig {
                host: "smtp.example.com".to_string(),
                port: 587,
                starttls: true,
                username: Some("okf".to_string()),
                password_env: Some("OKF_TEST_SMTP_PASSWORD".to_string()),
            }),
            ..MailConfig::default()
        }
    }

    fn postmark_config() -> MailConfig {
        MailConfig {
            provider: MailProvider::Postmark,
            postmark: Some(PostmarkConfig {
                token_env: "OKF_TEST_POSTMARK_TOKEN".to_string(),
                message_stream: "outbound".to_string(),
            }),
            ..MailConfig::default()
        }
    }

    #[test]
    fn each_provider_builds_when_its_secret_is_set() {
        let env = |name: &str| name.starts_with("OKF_TEST_").then(|| "s3cr3t".to_string());

        assert!(from_config_with(&MailConfig::default(), &env).is_ok());
        assert!(from_config_with(&smtp_config(), &env).is_ok());
        assert!(from_config_with(&postmark_config(), &env).is_ok());
    }

    #[test]
    fn a_missing_or_empty_secret_stops_start_up_and_names_the_variable() {
        let unset = |_: &str| None;
        let empty = |_: &str| Some("  ".to_string());

        for (config, variable) in [
            (smtp_config(), "OKF_TEST_SMTP_PASSWORD"),
            (postmark_config(), "OKF_TEST_POSTMARK_TOKEN"),
        ] {
            for env in [&unset as &dyn Fn(&str) -> Option<String>, &empty] {
                // `.err()` keeps the error and drops the `Ok` type (a
                // trait object, which has no `Debug`).
                let error = from_config_with(&config, env).err();
                assert!(
                    matches!(&error, Some(OkfError::Config { message, .. }) if message.contains(variable)),
                    "{error:?}"
                );
            }
        }
    }

    #[test]
    fn a_secret_never_reaches_the_error() {
        let env = |_: &str| Some("bad\ntoken-s3cr3t".to_string());

        let error = from_config_with(&postmark_config(), &env).err();

        let shown = format!("{error:?}");
        assert!(
            shown.contains("token_env") && !shown.contains("s3cr3t"),
            "{shown}"
        );
    }

    #[test]
    fn debug_never_shows_a_body() {
        let mail = Mail {
            to: "alice@example.com".to_string(),
            subject: "Reset your password".to_string(),
            text: "https://okf.example.com/account/reset?token=s3cr3t".to_string(),
            html: "<a href=\"https://okf.example.com/account/reset?token=s3cr3t\">".to_string(),
        };

        let shown = format!("{mail:?}");

        assert!(
            shown.contains("alice@example.com") && !shown.contains("s3cr3t"),
            "{shown}"
        );
    }
}
