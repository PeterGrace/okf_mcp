//! Server-rendered HTML shared by the OAuth consent pages and the sign-in
//! pages (plan Decision 14): one document shell, one escaper, one response
//! builder with the security headers, and the `return_to` check. There is
//! no template engine; every piece of text goes through [`escape`].

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

/// Where a sign-in goes when it was given no usable `return_to`.
// `expect(dead_code)` silences "never used" until a caller lands, and
// itself warns once one does, so it cannot be forgotten.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the sign-in pages (Task 7b) use it; delete this line then"
    )
)]
pub(crate) const DEFAULT_RETURN_TO: &str = "/ui/";

/// Escapes text for HTML element content and double- or single-quoted
/// attribute values.
///
/// # Arguments
/// * `text` - untrusted text.
///
/// # Returns
/// The text with `& < > " '` as entities.
pub(crate) fn escape(text: &str) -> String {
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

/// The HTML document around a page body.
///
/// # Arguments
/// * `title` - the page title (escaped here).
/// * `body` - the page body, already escaped where needed.
/// * `scripts` - paths of same-origin module scripts to load, in order;
///   empty for a page that runs no script (consent).
///
/// # Returns
/// The whole document.
pub(crate) fn document(title: &str, body: &str, scripts: &[&str]) -> String {
    // `map(...).collect::<String>()` concatenates one line per script.
    let scripts: String = scripts
        .iter()
        .map(|src| {
            format!(
                "    <script type=\"module\" src=\"{}\"></script>\n",
                escape(src)
            )
        })
        .collect();
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
    <link rel="stylesheet" href="/ui/signin.css">
{scripts}</head>
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

/// A page that runs no script: [`scripted_page`] with no scripts.
///
/// # Arguments
/// * `status` - the HTTP status.
/// * `title` - the page title.
/// * `body` - the page body.
/// * `csp` - the Content-Security-Policy.
///
/// # Returns
/// The response.
pub(crate) fn page(status: StatusCode, title: &str, body: &str, csp: &str) -> Response {
    scripted_page(status, title, body, &[], csp)
}

/// A page with its security headers: `text/html`, `no-store`,
/// `no-referrer` (an emailed link's token must not leak to another site),
/// `nosniff` and `csp`.
///
/// # Arguments
/// * `status` - the HTTP status.
/// * `title` - the page title.
/// * `body` - the page body.
/// * `scripts` - same-origin module scripts (see [`document`]).
/// * `csp` - the Content-Security-Policy.
///
/// # Returns
/// The response, or a bare 500 (logged) if `csp` is not a header value,
/// which would be a bug: the policies are built from constants and parsed
/// URLs.
pub(crate) fn scripted_page(
    status: StatusCode,
    title: &str,
    body: &str,
    scripts: &[&str],
    csp: &str,
) -> Response {
    // `let ... else`: bind the header value, or answer 500 and stop.
    let Ok(csp) = HeaderValue::from_str(csp) else {
        tracing::error!("a page's CSP is not a header value");
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
        document(title, body, scripts),
    )
        .into_response()
}

/// Where a sign-in may send the browser next: a console path or an OAuth
/// authorize URL on this origin, or else [`DEFAULT_RETURN_TO`].
///
/// # Arguments
/// * `raw` - the `return_to` a page or form carried, if any.
///
/// # Returns
/// `raw` when it is safe, else `/ui/`.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the sign-in pages (Task 7b) use it; delete this line then"
    )
)]
pub(crate) fn safe_return_to(raw: Option<&str>) -> String {
    match raw {
        Some(path) if is_safe_return_to(path) => path.to_string(),
        // `Some(_)` that failed the guard, or `None`.
        _ => DEFAULT_RETURN_TO.to_string(),
    }
}

/// Whether `path` is a same-origin console or authorize path.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the sign-in pages (Task 7b) use it; delete this line then"
    )
)]
fn is_safe_return_to(path: &str) -> bool {
    // Visible ASCII only: no spaces, no control characters (a header
    // injection) and no `\`, which some browsers read as `/` (so `/\host`
    // would leave the origin). Both prefixes start with one `/` followed by
    // a path segment, so the result can never be `//host`.
    path.bytes()
        .all(|byte| byte.is_ascii_graphic() && byte != b'\\')
        && (path.starts_with("/ui/") || path.starts_with("/oauth/authorize?"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_document_loads_only_the_scripts_it_is_given_as_modules() {
        let bare = document("t", "<p>x</p>", &[]);
        assert!(!bare.contains("<script"), "{bare}");
        let scripted = document("t", "<p>x</p>", &["/ui/js/signin/passkey.js"]);
        assert!(
            scripted.contains(r#"<script type="module" src="/ui/js/signin/passkey.js"></script>"#)
        );
        assert!(scripted.contains(r#"href="/ui/signin.css""#));
    }

    #[test]
    fn pages_carry_the_security_headers_and_the_given_policy() {
        let response = page(StatusCode::GONE, "Gone", "<p>x</p>", "default-src 'self'");
        let headers = response.headers();
        assert_eq!(response.status(), StatusCode::GONE);
        assert_eq!(headers["cache-control"], "no-store");
        assert_eq!(headers["referrer-policy"], "no-referrer");
        assert_eq!(headers["x-content-type-options"], "nosniff");
        assert_eq!(headers["content-type"], "text/html; charset=utf-8");
        assert_eq!(headers["content-security-policy"], "default-src 'self'");
    }

    #[test]
    fn a_policy_that_is_not_a_header_value_is_a_bare_500() {
        let response = page(StatusCode::OK, "t", "<p>x</p>", "default-src\n'self'");
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn return_to_keeps_same_origin_console_and_authorize_paths() {
        for kept in ["/ui/", "/ui/#/keys", "/oauth/authorize?client_id=x&state=y"] {
            assert_eq!(safe_return_to(Some(kept)), kept);
        }
    }

    #[test]
    fn return_to_falls_back_to_the_console_for_anything_else() {
        for refused in [
            None,
            Some(""),
            Some("https://evil.example.com/ui/"),
            Some("//evil.example.com/ui/"),
            Some("/ui/\\evil"),
            Some("/ui/ x"),
            Some("/ui/\nSet-Cookie: x=y"),
            Some("/signin"),
            Some("/oauth/authorize"),
            Some("/oauth/authorizex?y"),
            Some("/ui"),
        ] {
            assert_eq!(safe_return_to(refused), DEFAULT_RETURN_TO, "{refused:?}");
        }
    }
}
