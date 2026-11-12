//! The mail templates (spec "Mail"): plain text with a minimal HTML twin.
//! No template engine: `format!` over fixed text, as the consent pages do,
//! with every interpolated value HTML-escaped in the twin.

use crate::oauth::consent::escape;

/// The setup page's path; the link adds `?token=...`.
pub const SETUP_PATH: &str = "/account/setup";

/// The set-password page a reset link opens.
pub const RESET_PATH: &str = "/account/reset";

/// The closing line of the mails nobody may have asked for.
const IGNORE: &str = "If you did not expect this email, you can ignore it.";

/// A message to render. `link`s carry a single-use token, so `Debug` hides them.
#[derive(Clone, PartialEq, Eq)]
pub enum Template {
    /// An admin's invitation: choose a password or register a passkey.
    Setup {
        /// The setup link ([`link`] with [`SETUP_PATH`]).
        link: String,
        /// The user's name, for the greeting.
        user: String,
    },
    /// A forgotten password.
    Reset {
        /// The reset link ([`link`] with [`RESET_PATH`]).
        link: String,
    },
    /// A notice after a password change, a new passkey or a linked identity.
    MethodAdded {
        /// What was added, in words ("a passkey", "a password", "Example ID").
        method: String,
    },
    /// The Settings page's test email.
    Test,
}

// A hand-written `Debug` (Python's `__repr__`) instead of `#[derive(Debug)]`,
// so `{:?}` never prints a link's token.
impl std::fmt::Debug for Template {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Template::Setup { user, .. } => formatter
                .debug_struct("Setup")
                .field("link", &"<redacted>")
                .field("user", user)
                .finish(),
            Template::Reset { .. } => formatter
                .debug_struct("Reset")
                .field("link", &"<redacted>")
                .finish(),
            Template::MethodAdded { method } => formatter
                .debug_struct("MethodAdded")
                .field("method", method)
                .finish(),
            Template::Test => formatter.write_str("Test"),
        }
    }
}

impl Template {
    /// The template's name, as the mail queue lists it.
    ///
    /// # Returns
    /// `"setup"`, `"reset"`, `"method_added"` or `"test"`.
    pub fn name(&self) -> &'static str {
        match self {
            Template::Setup { .. } => "setup",
            Template::Reset { .. } => "reset",
            Template::MethodAdded { .. } => "method_added",
            Template::Test => "test",
        }
    }
}

/// Builds an emailed link from `public_url` (already validated as an
/// origin) and a raw token (base64url, so it needs no escaping). The one
/// link builder (Decision 21).
///
/// # Arguments
/// * `public_url` - the configured origin, with or without a trailing `/`.
/// * `path` - [`SETUP_PATH`] or [`RESET_PATH`].
/// * `token` - the raw token from `store::email_tokens::issue`.
///
/// # Returns
/// The absolute URL, `{public_url}{path}?token={token}`.
pub fn link(public_url: &str, path: &str, token: &str) -> String {
    format!("{}{path}?token={token}", public_url.trim_end_matches('/'))
}

/// Wraps paragraphs of already-escaped HTML in a minimal document.
fn html_document(subject: &str, paragraphs: &[String]) -> String {
    let body: String = paragraphs
        .iter()
        .map(|paragraph| format!("<p>{paragraph}</p>\n"))
        .collect();
    format!(
        "<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>{}</title></head>\n\
         <body>\n{body}</body></html>\n",
        escape(subject)
    )
}

/// An escaped link, shown as its own text.
fn anchor(link: &str) -> String {
    let link = escape(link);
    format!("<a href=\"{link}\">{link}</a>")
}

/// Renders a template.
///
/// # Arguments
/// * `template` - what to send.
///
/// # Returns
/// `(subject, plain text, HTML)`.
pub fn render(template: &Template) -> (String, String, String) {
    match template {
        Template::Setup { link, user } => {
            let subject = "Set up your okf account";
            let invited = "An administrator invited you to okf. Open this link to choose a \
                           password or register a passkey:";
            let expiry = "The link works once and expires soon. If it has expired, ask an \
                          administrator for a new one.";
            let text = format!("Hello {user},\n\n{invited}\n\n{link}\n\n{expiry}\n\n{IGNORE}\n");
            let html = html_document(
                subject,
                &[
                    format!("Hello {},", escape(user)),
                    invited.to_string(),
                    anchor(link),
                    expiry.to_string(),
                    IGNORE.to_string(),
                ],
            );
            (subject.to_string(), text, html)
        }
        Template::Reset { link } => {
            let subject = "Reset your password";
            let asked = "Someone asked to reset the password of your okf account. Open this \
                         link to choose a new one:";
            let expiry = "The link works once and expires soon. Using it signs you out everywhere.";
            let unasked = "If you did not ask for this, ignore this email; your password is \
                           unchanged.";
            let text = format!("{asked}\n\n{link}\n\n{expiry}\n\n{unasked}\n");
            let html = html_document(
                subject,
                &[
                    asked.to_string(),
                    anchor(link),
                    expiry.to_string(),
                    unasked.to_string(),
                ],
            );
            (subject.to_string(), text, html)
        }
        Template::MethodAdded { method } => {
            let subject = "A sign-in method was added to your account";
            let notice = "If this was not you, reset your password and ask an administrator \
                          to check your account.";
            let text = format!("{method} was added to your okf account.\n\n{notice}\n");
            let html = html_document(
                subject,
                &[
                    format!("{} was added to your okf account.", escape(method)),
                    notice.to_string(),
                ],
            );
            (subject.to_string(), text, html)
        }
        Template::Test => {
            let subject = "okf test email";
            let line = "This is a test email from okf. Mail delivery works.";
            (
                subject.to_string(),
                format!("{line}\n"),
                html_document(subject, &[line.to_string()]),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_joins_the_public_url_a_path_and_the_token() {
        assert_eq!(
            link("https://okf.example.com/", SETUP_PATH, "tok_en-1"),
            "https://okf.example.com/account/setup?token=tok_en-1"
        );
        assert_eq!(
            link("http://127.0.0.1:8080", RESET_PATH, "abc"),
            "http://127.0.0.1:8080/account/reset?token=abc"
        );
    }

    #[test]
    fn every_template_has_a_subject_a_text_and_an_html_twin() {
        let setup_link = link("https://okf.example.com", SETUP_PATH, "s3cr3t");
        let templates = [
            Template::Setup {
                link: setup_link.clone(),
                user: "alice".to_string(),
            },
            Template::Reset {
                link: link("https://okf.example.com", RESET_PATH, "r3s3t"),
            },
            Template::MethodAdded {
                method: "a passkey".to_string(),
            },
            Template::Test,
        ];
        let subjects = [
            "Set up your okf account",
            "Reset your password",
            "A sign-in method was added to your account",
            "okf test email",
        ];

        for (template, subject) in templates.iter().zip(subjects) {
            let (rendered_subject, text, html) = render(template);
            assert_eq!(rendered_subject, subject);
            assert!(
                !text.is_empty() && html.starts_with("<!doctype html>"),
                "{template:?}"
            );
        }
        let (_, text, html) = render(&templates[0]);
        assert!(text.contains(&setup_link) && text.contains("alice"));
        assert!(html.contains(&format!("href=\"{setup_link}\"")));
        let (_, text, html) = render(&templates[1]);
        assert!(text.contains("token=r3s3t") && html.contains("token=r3s3t"));
    }

    #[test]
    fn html_escapes_what_it_interpolates() {
        let template = Template::MethodAdded {
            method: "<b>passkey</b> & co".to_string(),
        };
        let greeting = Template::Setup {
            link: "https://okf.example.com/account/setup?token=a&b=\"c\"".to_string(),
            user: "<alice>".to_string(),
        };

        let (_, text, html) = render(&template);
        let (_, _, setup_html) = render(&greeting);

        assert!(
            text.contains("<b>passkey</b> & co"),
            "plain text is not escaped"
        );
        assert!(html.contains("&lt;b&gt;passkey&lt;/b&gt; &amp; co"));
        assert!(!html.contains("<b>"));
        assert!(setup_html.contains("Hello &lt;alice&gt;,"));
        assert!(setup_html.contains(
            "href=\"https://okf.example.com/account/setup?token=a&amp;b=&quot;c&quot;\""
        ));
    }

    #[test]
    fn names_are_stable_and_debug_hides_links() {
        let setup = Template::Setup {
            link: "https://okf.example.com/x?token=s3cr3t".to_string(),
            user: "alice".to_string(),
        };
        let reset = Template::Reset {
            link: "https://okf.example.com/x?token=r3s3t".to_string(),
        };

        assert_eq!(
            [
                setup.name(),
                reset.name(),
                Template::MethodAdded {
                    method: String::new()
                }
                .name(),
                Template::Test.name()
            ],
            ["setup", "reset", "method_added", "test"]
        );
        let shown = format!("{setup:?} {reset:?}");
        assert!(
            !shown.contains("s3cr3t") && !shown.contains("r3s3t") && shown.contains("alice"),
            "{shown}"
        );
    }
}
