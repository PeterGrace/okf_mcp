//! The consent page (spec "Consent flow"): server-rendered HTML, a plain
//! form, no script. Step 1 asks for the sign-in code, step 2 states what will
//! be granted, and an error page ends a request that cannot go on.
//!
//! Client names come from client metadata, which anyone can write, so every
//! piece of text goes through [`escape`]. There is no template engine.

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

/// What every page shows about the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageView {
    /// The pending authorization's id (a hidden field).
    pub pending_id: String,
    /// Its CSRF token (a hidden field).
    pub csrf: String,
    /// The client's name, or its id when it gave none.
    pub client_name: String,
    /// The host of the redirect URI the browser will return to.
    pub client_host: String,
    /// The client came from a CIMD URL (its name is that site's own).
    pub verified: bool,
    /// The redirect URI's origin, for the CSP's `form-action`.
    pub redirect_origin: String,
}

/// What a sign-in code grants, for step 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantSummary {
    /// `read` or `write`.
    pub access: &'static str,
    /// `every catalog`, or the names joined with ", ".
    pub catalogs: String,
    /// The actor its writes are recorded as.
    pub actor: String,
    /// Its default catalog.
    pub default_catalog: Option<String>,
}

/// Escapes text for HTML element content and double- or single-quoted
/// attribute values.
///
/// # Arguments
/// * `text` - untrusted text.
///
/// # Returns
/// The text with `& < > " '` as entities.
pub fn escape(text: &str) -> String {
    // Most text has nothing to escape; the capacity covers that case with
    // one allocation.
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// The page's Content-Security-Policy.
///
/// Browsers apply `form-action` to the redirect that follows a form POST,
/// so the redirect URI's origin must be listed, or Approve (and Deny) would
/// be blocked on its way to the client. No `script-src` and no
/// `'unsafe-inline'`: `default-src 'none'` forbids every script and inline
/// style.
///
/// # Arguments
/// * `redirect_origin` - the validated redirect URI's origin; `None` on the
///   error page, which has no form.
///
/// # Returns
/// The policy.
pub(crate) fn content_security_policy(redirect_origin: Option<&str>) -> String {
    let form_action = match redirect_origin {
        Some(origin) => format!("'self' {origin}"),
        None => "'self'".to_string(),
    };
    format!(
        "default-src 'none'; style-src 'self' https://fonts.googleapis.com; \
         font-src https://fonts.gstatic.com; img-src 'self' data:; \
         frame-ancestors 'none'; base-uri 'none'; form-action {form_action};"
    )
}

/// Wraps a page body in the document shell and the response headers.
fn page(status: StatusCode, title: &str, body: &str, redirect_origin: Option<&str>) -> Response {
    // The origin came from a parsed URL, so the policy is always a valid
    // header value; a failure would be a bug, answered with a bare 500
    // rather than a page without its policy.
    let Ok(csp) = HeaderValue::from_str(&content_security_policy(redirect_origin)) else {
        tracing::error!("consent CSP is not a header value");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    (
        status,
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            (
                header::REFERRER_POLICY,
                HeaderValue::from_static("no-referrer"),
            ),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
            (header::CONTENT_SECURITY_POLICY, csp),
        ],
        document(title, body),
    )
        .into_response()
}

/// The HTML document around a page body.
fn document(title: &str, body: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <meta name="color-scheme" content="light dark">
    <title>{title} - okf</title>
    <link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600;700&family=JetBrains+Mono:wght@400;500&family=Space+Grotesk:wght@500;700&display=swap">
    <link rel="stylesheet" href="/ui/vendor/pico.min.css">
    <link rel="stylesheet" href="/ui/styles.css">
    <link rel="stylesheet" href="/ui/consent.css">
</head>
<body>
    <main class="login-card consent-card">
        <p class="brand">okf</p>
{body}
    </main>
</body>
</html>
"#,
        title = escape(title),
    )
}

/// The client block shared by both steps.
fn client_block(view: &PageView) -> String {
    let badge = if view.verified {
        ""
    } else {
        r#"<span class="consent-badge" title="This client registered itself; its name is not checked.">unverified</span>"#
    };
    format!(
        r#"        <p><span class="consent-client" id="consent-client">{name}</span>{badge}<br>
        <span class="muted consent-host">returns to {host}</span></p>"#,
        name = escape(&view.client_name),
        host = escape(&view.client_host),
    )
}

/// The hidden fields every form carries.
fn hidden_fields(view: &PageView) -> String {
    format!(
        r#"<input type="hidden" name="pending" value="{pending}">
            <input type="hidden" name="csrf" value="{csrf}">"#,
        pending = escape(&view.pending_id),
        csrf = escape(&view.csrf),
    )
}

/// Step 1's HTML.
///
/// # Arguments
/// * `view` - the request.
/// * `error` - a message from a failed attempt, if any.
///
/// # Returns
/// The page body.
pub(crate) fn code_page_html(view: &PageView, error: Option<&str>) -> String {
    let error = error.map_or_else(String::new, |message| {
        format!(
            r#"        <p class="error consent-error" id="consent-error" role="alert">{}</p>
"#,
            escape(message)
        )
    });
    format!(
        r#"        <h1>Connect a client</h1>
{client}
        <p class="muted">Enter the sign-in code your administrator gave you. It decides which catalogs this client may use.</p>
{error}        <form method="post" action="/oauth/authorize" id="consent-code-form">
            {hidden}
            <label>Sign-in code
                <input type="password" name="sign_in_code" id="sign-in-code" autocomplete="off" required>
            </label>
            <div class="consent-actions">
                <button type="submit" name="action" value="code" id="consent-continue">Continue</button>
                <button type="submit" name="action" value="deny" id="consent-deny" class="secondary" formnovalidate>Deny</button>
            </div>
        </form>"#,
        client = client_block(view),
        hidden = hidden_fields(view),
    )
}

/// Step 1: the sign-in code form.
///
/// # Arguments
/// * `view` - the request.
/// * `error` - a message from a failed attempt, if any.
///
/// # Returns
/// 200 with the page.
pub fn code_page(view: &PageView, error: Option<&str>) -> Response {
    page(
        StatusCode::OK,
        "Connect a client",
        &code_page_html(view, error),
        Some(&view.redirect_origin),
    )
}

/// Step 2's HTML.
///
/// # Arguments
/// * `view` - the request.
/// * `grant` - what the sign-in code grants.
///
/// # Returns
/// The page body.
pub(crate) fn summary_page_html(view: &PageView, grant: &GrantSummary) -> String {
    let default = grant
        .default_catalog
        .as_deref()
        .map_or_else(String::new, |name| {
            format!(", default catalog <strong>{}</strong>", escape(name))
        });
    format!(
        r#"        <h1>Allow access?</h1>
{client}
        <p class="consent-summary" id="consent-summary"><strong>{name}</strong> ({host}) will get <strong>{access}</strong> access to <strong>{catalogs}</strong> as actor <strong>{actor}</strong>{default}.</p>
        <form method="post" action="/oauth/authorize" id="consent-approve-form">
            {hidden}
            <div class="consent-actions">
                <button type="submit" name="action" value="approve" id="consent-approve">Approve</button>
                <button type="submit" name="action" value="deny" id="consent-deny" class="secondary">Deny</button>
            </div>
        </form>"#,
        client = client_block(view),
        name = escape(&view.client_name),
        host = escape(&view.client_host),
        // A `&'static str` from `Access::as_str`: never client text.
        access = grant.access,
        catalogs = escape(&grant.catalogs),
        actor = escape(&grant.actor),
        hidden = hidden_fields(view),
    )
}

/// Step 2: what will be granted, with Approve and Deny.
///
/// # Arguments
/// * `view` - the request.
/// * `grant` - what the sign-in code grants.
///
/// # Returns
/// 200 with the page.
pub fn summary_page(view: &PageView, grant: &GrantSummary) -> Response {
    page(
        StatusCode::OK,
        "Allow access?",
        &summary_page_html(view, grant),
        Some(&view.redirect_origin),
    )
}

/// The error page's HTML.
///
/// # Arguments
/// * `message` - what went wrong, for a person.
///
/// # Returns
/// The page body (no form).
pub(crate) fn error_page_html(message: &str) -> String {
    format!(
        r#"        <h1>Cannot connect</h1>
        <p class="error consent-error" id="consent-error" role="alert">{}</p>
        <p class="muted">Close this window and start again from your client.</p>"#,
        escape(message)
    )
}

/// A page that ends the request (no form).
///
/// # Arguments
/// * `status` - the HTTP status.
/// * `message` - what went wrong, for a person.
///
/// # Returns
/// The page with `status`.
pub fn error_page(status: StatusCode, message: &str) -> Response {
    page(status, "Cannot connect", &error_page_html(message), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> PageView {
        PageView {
            pending_id: "p1".to_string(),
            csrf: "c1".to_string(),
            client_name: "<Example & Co>".to_string(),
            client_host: "client.example.com".to_string(),
            verified: false,
            redirect_origin: "https://client.example.com".to_string(),
        }
    }

    #[test]
    fn escape_covers_the_five_html_specials() {
        assert_eq!(
            escape(r#"<a href="x">'&'</a>"#),
            "&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;&lt;/a&gt;"
        );
        assert_eq!(escape("plain"), "plain");
        // Multibyte text passes through untouched.
        assert_eq!(
            escape("Caf\u{e9} \u{65e5}\u{672c}"),
            "Caf\u{e9} \u{65e5}\u{672c}"
        );
    }

    #[test]
    fn the_csp_allows_self_and_exactly_the_redirect_origin() {
        let csp = content_security_policy(Some("https://client.example.com"));
        assert!(csp.contains("form-action 'self' https://client.example.com;"));
        assert!(csp.starts_with("default-src 'none';"));
        assert!(csp.contains("frame-ancestors 'none'"));
        assert!(!csp.contains("script-src"), "the page runs no script");
        assert!(!csp.contains("unsafe-inline"), "no inline style or script");
        assert!(content_security_policy(None).contains("form-action 'self';"));
    }

    #[test]
    fn pages_carry_the_security_headers() {
        let response = code_page(&view(), None);
        let headers = response.headers();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(headers["cache-control"], "no-store");
        assert_eq!(headers["referrer-policy"], "no-referrer");
        assert_eq!(headers["x-content-type-options"], "nosniff");
        assert_eq!(headers["content-type"], "text/html; charset=utf-8");
        assert!(headers["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("form-action 'self' https://client.example.com;"));
    }

    #[test]
    fn client_text_is_escaped_and_unverified_clients_say_so() {
        let html = code_page_html(&view(), Some("bad <code>"));
        assert!(html.contains("&lt;Example &amp; Co&gt;"));
        assert!(!html.contains("<Example"));
        assert!(html.contains("unverified"));
        assert!(html.contains("bad &lt;code&gt;"));
        assert!(html.contains(r#"name="pending" value="p1""#));
        assert!(html.contains(r#"name="csrf" value="c1""#));
    }

    #[test]
    fn a_hostile_host_cannot_break_out_of_its_markup() {
        let hostile = PageView {
            client_host: r#"x"><script>alert(1)</script>"#.to_string(),
            verified: true,
            ..view()
        };
        let grant = GrantSummary {
            access: "read",
            catalogs: "main".to_string(),
            actor: "<b>a</b>".to_string(),
            default_catalog: None,
        };
        for html in [
            code_page_html(&hostile, None),
            summary_page_html(&hostile, &grant),
        ] {
            assert!(!html.contains("<script>"), "{html}");
            assert!(!html.contains("<b>a</b>"), "{html}");
            assert!(!html.contains("unverified"), "a CIMD client is verified");
        }
    }

    #[test]
    fn pages_have_no_inline_script_or_style() {
        let grant = GrantSummary {
            access: "read",
            catalogs: "main".to_string(),
            actor: "example-actor".to_string(),
            default_catalog: None,
        };
        for html in [
            document("t", &code_page_html(&view(), Some("e"))),
            document("t", &summary_page_html(&view(), &grant)),
            document("t", &error_page_html("e")),
        ] {
            assert!(!html.contains("<script"), "{html}");
            assert!(!html.contains("<style"), "{html}");
            assert!(!html.contains("style="), "{html}");
        }
    }

    #[test]
    fn the_summary_names_access_catalogs_actor_and_default() {
        let grant = GrantSummary {
            access: "write",
            catalogs: "main, other".to_string(),
            actor: "example-actor".to_string(),
            default_catalog: Some("main".to_string()),
        };
        let html = summary_page_html(&view(), &grant);
        assert!(html.contains("<strong>write</strong>"));
        assert!(html.contains("<strong>main, other</strong>"));
        assert!(html.contains("<strong>example-actor</strong>"));
        assert!(html.contains("default catalog <strong>main</strong>"));
        assert!(html.contains(r#"id="consent-approve""#));
        assert!(html.contains(r#"id="consent-deny""#));
    }

    #[test]
    fn the_error_page_has_no_form_and_its_status() {
        let response = error_page(StatusCode::BAD_REQUEST, "Unknown <client>.");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(response.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("form-action 'self';"));
        let html = error_page_html("Unknown <client>.");
        assert!(html.contains("Unknown &lt;client&gt;."));
        assert!(!html.contains("<form"));
    }
}
