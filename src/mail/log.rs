//! The `log` provider: writes each message to the log at `info`, link
//! included. For development and tests: it delivers nothing. It is the one
//! place a message body reaches the log, by design (`from_config` says so
//! when the server starts).

use super::{Mail, MailError, Mailer};

/// Logs instead of sending.
pub(crate) struct LogMailer;

#[async_trait::async_trait]
impl Mailer for LogMailer {
    async fn send(&self, mail: &Mail) -> Result<(), MailError> {
        // `%value` records a field with its `Display` form.
        tracing::info!(
            to = %mail.to,
            subject = %mail.subject,
            text = %mail.text,
            "mail (log provider; not delivered)"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_log_provider_always_succeeds() {
        let mail = Mail {
            to: "alice@example.com".to_string(),
            subject: "okf test email".to_string(),
            text: "hello".to_string(),
            html: "<p>hello</p>".to_string(),
        };

        assert_eq!(LogMailer.send(&mail).await, Ok(()));
    }
}
