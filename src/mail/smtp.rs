//! The SMTP provider, over lettre's async transport (tokio, rustls with
//! ring and the webpki roots: no OpenSSL). Generic over the transport so
//! tests use lettre's `AsyncStubTransport`.

use std::time::Duration;

use lettre::message::{Mailbox, MultiPart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

use super::{config_error, Mail, MailError, Mailer};
use crate::config::SmtpConfig;
use crate::error::OkfError;

/// How long one SMTP conversation may take.
const SMTP_TIMEOUT: Duration = Duration::from_secs(30);

/// Sends through an SMTP transport `T`. No `Debug`: the transport holds
/// the relay's credentials.
pub(crate) struct SmtpMailer<T> {
    transport: T,
    from: Mailbox,
}

impl<T> SmtpMailer<T> {
    /// A mailer over any lettre async transport.
    ///
    /// # Arguments
    /// * `transport` - the transport (the relay, or a stub in tests).
    /// * `from` - the From mailbox.
    ///
    /// # Returns
    /// The mailer.
    pub(crate) fn new(transport: T, from: Mailbox) -> Self {
        Self { transport, from }
    }
}

impl SmtpMailer<AsyncSmtpTransport<Tokio1Executor>> {
    /// The relay `[mail.smtp]` describes. Builds the transport only; it
    /// connects when the first mail is sent.
    ///
    /// # Arguments
    /// * `from` - `mail.from`.
    /// * `config` - `[mail.smtp]`.
    /// * `password` - the password read from `password_env`, if any.
    ///
    /// # Returns
    /// The mailer.
    ///
    /// # Errors
    /// [`OkfError::Config`] for a From address or host lettre refuses.
    pub(crate) fn from_config(
        from: &str,
        config: &SmtpConfig,
        password: Option<String>,
    ) -> Result<Self, OkfError> {
        let from = parse_from(from)?;
        // `starttls_relay` refuses to send unless the upgrade succeeds;
        // `relay` speaks TLS from the first byte.
        let builder = if config.starttls {
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.host)
        } else {
            AsyncSmtpTransport::<Tokio1Executor>::relay(&config.host)
        }
        .map_err(|error| config_error(format!("mail.smtp.host {:?}: {error}", config.host)))?;
        let mut builder = builder.port(config.port).timeout(Some(SMTP_TIMEOUT));
        // `if let (Some(a), Some(b)) = (..)`: both present (validation makes
        // them go together).
        if let (Some(username), Some(password)) = (&config.username, password) {
            builder = builder.credentials(Credentials::new(username.clone(), password));
        }
        Ok(Self::new(builder.build(), from))
    }
}

/// Parses `mail.from`.
///
/// # Arguments
/// * `from` - the configured From address.
///
/// # Returns
/// The mailbox.
///
/// # Errors
/// [`OkfError::Config`].
pub(crate) fn parse_from(from: &str) -> Result<Mailbox, OkfError> {
    from.parse()
        .map_err(|error| config_error(format!("mail.from {from:?} is not an address: {error}")))
}

/// Builds the MIME message: one recipient, text and HTML alternatives.
fn message(from: &Mailbox, mail: &Mail) -> Result<Message, MailError> {
    let to: Mailbox = mail.to.parse().map_err(|error| {
        MailError::Permanent(format!(
            "recipient {:?} is not an address: {error}",
            mail.to
        ))
    })?;
    Message::builder()
        .from(from.clone())
        .to(to)
        .subject(mail.subject.clone())
        .multipart(MultiPart::alternative_plain_html(
            mail.text.clone(),
            mail.html.clone(),
        ))
        .map_err(|error| MailError::Permanent(format!("building the message: {error}")))
}

/// Sorts a transport error: an SMTP 5xx reply is permanent; anything else
/// (connection, TLS, timeout, 4xx, a stub's error) is worth retrying.
fn classify(error: &(dyn std::error::Error + 'static)) -> MailError {
    // `downcast_ref` is Python's `isinstance` check plus the cast: `Some`
    // only when the error really is lettre's SMTP error.
    match error.downcast_ref::<lettre::transport::smtp::Error>() {
        Some(smtp) if smtp.is_permanent() => MailError::Permanent(format!("smtp: {smtp}")),
        _ => MailError::Retryable(format!("smtp: {error}")),
    }
}

#[async_trait::async_trait]
impl<T> Mailer for SmtpMailer<T>
where
    T: AsyncTransport + Send + Sync,
    T::Ok: Send,
    T::Error: std::error::Error + Send + Sync + 'static,
{
    async fn send(&self, mail: &Mail) -> Result<(), MailError> {
        let message = message(&self.from, mail)?;
        self.transport
            .send(message)
            .await
            .map(|_| ())
            .map_err(|error| classify(&error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lettre::transport::stub::AsyncStubTransport;

    fn mail(to: &str) -> Mail {
        Mail {
            to: to.to_string(),
            subject: "Reset your password".to_string(),
            text: "the text".to_string(),
            html: "<p>the html</p>".to_string(),
        }
    }

    fn from() -> Mailbox {
        parse_from("okf <okf@example.com>").unwrap()
    }

    #[tokio::test]
    async fn a_message_goes_to_its_one_recipient_with_both_parts() {
        let transport = AsyncStubTransport::new_ok();
        // The stub's log is shared between clones, so the test keeps one.
        let mailer = SmtpMailer::new(transport.clone(), from());

        mailer.send(&mail("alice@example.com")).await.unwrap();

        let messages = transport.messages().await;
        assert_eq!(messages.len(), 1);
        let (envelope, raw) = &messages[0];
        assert_eq!(envelope.to().len(), 1);
        assert_eq!(envelope.to()[0].to_string(), "alice@example.com");
        assert!(raw.contains("Subject: Reset your password"), "{raw}");
        assert!(raw.contains("multipart/alternative"), "{raw}");
        assert!(
            raw.contains("the text") && raw.contains("the html"),
            "{raw}"
        );
    }

    #[tokio::test]
    async fn a_transport_failure_is_retryable_and_a_bad_recipient_is_permanent() {
        let failing = SmtpMailer::new(AsyncStubTransport::new_error(), from());
        let working = SmtpMailer::new(AsyncStubTransport::new_ok(), from());

        let failed = failing.send(&mail("alice@example.com")).await;
        let bad = working
            .send(&mail("alice@example.com, bob@example.com"))
            .await;

        assert!(matches!(failed, Err(MailError::Retryable(_))), "{failed:?}");
        assert!(matches!(bad, Err(MailError::Permanent(_))), "{bad:?}");
    }

    #[test]
    fn a_relay_is_built_without_connecting() {
        let config = SmtpConfig {
            host: "smtp.example.com".to_string(),
            port: 465,
            starttls: false,
            username: None,
            password_env: None,
        };

        assert!(SmtpMailer::from_config("okf <okf@example.com>", &config, None).is_ok());
        assert!(matches!(
            SmtpMailer::from_config("nobody", &config, None),
            Err(OkfError::Config { .. })
        ));
    }

    /// A one-connection SMTP server that answers `RCPT TO` with `rcpt_reply`
    /// and every other command (`DATA` included) with success.
    async fn fake_relay(rcpt_reply: &'static str) -> u16 {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            write
                .write_all(b"220 relay.example.com ready\r\n")
                .await
                .unwrap();
            while let Ok(Some(line)) = lines.next_line().await {
                let command = line.to_ascii_uppercase();
                if command.starts_with("DATA") {
                    write.write_all(b"354 go ahead\r\n").await.unwrap();
                    // The message, up to the line holding only a dot.
                    while let Ok(Some(body)) = lines.next_line().await {
                        if body == "." {
                            break;
                        }
                    }
                    write.write_all(b"250 queued\r\n").await.unwrap();
                    continue;
                }
                let reply = if command.starts_with("RCPT") {
                    rcpt_reply
                } else if command.starts_with("QUIT") {
                    "221 bye"
                } else {
                    "250 ok"
                };
                write
                    .write_all(format!("{reply}\r\n").as_bytes())
                    .await
                    .unwrap();
                if reply.starts_with("221") {
                    break;
                }
            }
        });
        port
    }

    /// A mailer over a plaintext relay on the loopback (tests only).
    fn plain_mailer(port: u16) -> SmtpMailer<AsyncSmtpTransport<Tokio1Executor>> {
        let transport = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous("127.0.0.1")
            .port(port)
            .timeout(Some(Duration::from_secs(10)))
            .build();
        SmtpMailer::new(transport, from())
    }

    #[tokio::test]
    async fn an_smtp_5xx_is_permanent_and_a_4xx_is_retryable() {
        let refused = plain_mailer(fake_relay("550 5.1.1 no such user").await)
            .send(&mail("alice@example.com"))
            .await;
        let deferred = plain_mailer(fake_relay("451 4.3.0 try again later").await)
            .send(&mail("alice@example.com"))
            .await;
        let accepted = plain_mailer(fake_relay("250 ok").await)
            .send(&mail("alice@example.com"))
            .await;

        assert!(
            matches!(&refused, Err(MailError::Permanent(detail)) if detail.contains("no such user")),
            "{refused:?}"
        );
        assert!(
            matches!(&deferred, Err(MailError::Retryable(detail)) if detail.contains("try again")),
            "{deferred:?}"
        );
        assert_eq!(accepted, Ok(()));
    }
}
