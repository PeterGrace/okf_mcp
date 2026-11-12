//! The consent page (spec "Consent flow"): server-rendered HTML, a plain
//! form, no script. Step 1 asks for the sign-in code, step 2 states what will
//! be granted, and an error page ends a request that cannot go on. A
//! signed-in person instead sees [`user_page`] (spec "OAuth consent as the
//! user"): their ceiling's catalogs to choose from.
//!
//! Client names come from client metadata, which anyone can write, so every
//! piece of text goes through `escape`. There is no template engine; the
//! document shell and the response headers come from `crate::http::pages`.

use axum::http::StatusCode;
use axum::response::Response;

use crate::http::pages;
/// Moved to `crate::http::pages`; kept here so `mail::templates` (Task 6)
/// compiles whichever of Tasks 6 and 7a merges first (plan Decision 14).
pub(crate) use crate::http::pages::escape;
use crate::store::ceiling::Ceiling;
use crate::store::keys::Access;

/// What every page shows about the request.
// `Debug` by hand: the pending id and the CSRF token never reach a log.
#[derive(Clone, PartialEq, Eq)]
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

impl std::fmt::Debug for PageView {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PageView")
            .field("pending_id", &"<redacted>")
            .field("csrf", &"<redacted>")
            .field("client_name", &self.client_name)
            .field("client_host", &self.client_host)
            .field("verified", &self.verified)
            .field("redirect_origin", &self.redirect_origin)
            .finish()
    }
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
    /// The user the grant will belong to (`None` for an unassigned code).
    pub user: Option<String>,
    /// Its default catalog.
    pub default_catalog: Option<String>,
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

/// A consent page: [`pages::page`] with [`content_security_policy`].
fn page(status: StatusCode, title: &str, body: &str, redirect_origin: Option<&str>) -> Response {
    pages::page(
        status,
        title,
        body,
        &content_security_policy(redirect_origin),
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

/// A refusal's line above a form, or nothing.
fn error_line(error: Option<&str>) -> String {
    error.map_or_else(String::new, |message| {
        format!(
            r#"        <p class="error consent-error" id="consent-error" role="alert">{}</p>
"#,
            escape(message)
        )
    })
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
    let error = error_line(error);
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
/// * `summary_nonce` - this summary's token (a hidden field), which binds
///   its Approve to the code it shows.
///
/// # Returns
/// The page body.
pub(crate) fn summary_page_html(
    view: &PageView,
    grant: &GrantSummary,
    summary_nonce: &str,
) -> String {
    let default = grant
        .default_catalog
        .as_deref()
        .map_or_else(String::new, |name| {
            format!(", default catalog <strong>{}</strong>", escape(name))
        });
    // An unassigned code keeps the wording it had before codes had users.
    let who = match &grant.user {
        Some(user) => format!(
            "as <strong>{}</strong> (actor <strong>{}</strong>)",
            escape(user),
            escape(&grant.actor)
        ),
        None => format!("as actor <strong>{}</strong>", escape(&grant.actor)),
    };
    format!(
        r#"        <h1>Allow access?</h1>
{client}
        <p class="consent-summary" id="consent-summary"><strong>{name}</strong> ({host}) will get <strong>{access}</strong> access to <strong>{catalogs}</strong> {who}{default}.</p>
        <form method="post" action="/oauth/authorize" id="consent-approve-form">
            {hidden}
            <input type="hidden" name="summary" value="{summary}">
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
        hidden = hidden_fields(view),
        summary = escape(summary_nonce),
    )
}

/// Step 2: what will be granted, with Approve and Deny.
///
/// # Arguments
/// * `view` - the request.
/// * `grant` - what the sign-in code grants.
/// * `summary_nonce` - this summary's token, rendered as the hidden
///   `summary` input that binds its Approve to the code it shows.
///
/// # Returns
/// 200 with the page.
pub fn summary_page(view: &PageView, grant: &GrantSummary, summary_nonce: &str) -> Response {
    page(
        StatusCode::OK,
        "Allow access?",
        &summary_page_html(view, grant, summary_nonce),
        Some(&view.redirect_origin),
    )
}

/// What the signed-in consent page shows beyond [`PageView`].
// `Debug` by hand: the consent nonce never reaches a log.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct UserPage<'a> {
    /// The signed-in user's name.
    pub user: &'a str,
    /// The ceiling it offers: the catalogs and levels the user may grant.
    pub ceiling: &'a Ceiling,
    /// The page's consent nonce (the hidden `summary` input).
    pub nonce: &'a str,
    /// The "Use a sign-in code instead" link (same-origin, unescaped).
    pub code_href: &'a str,
}

impl std::fmt::Debug for UserPage<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UserPage")
            .field("user", &self.user)
            .field("ceiling", &self.ceiling)
            .field("nonce", &"<redacted>")
            .field("code_href", &self.code_href)
            .finish()
    }
}

/// The signed-in consent page's HTML: a checkbox and a level select per
/// ceiling catalog (only the levels the ceiling allows), a default catalog
/// select, Approve and Deny, and the sign-in code link. With an empty
/// ceiling there is nothing to approve: Deny and the link only.
///
/// # Arguments
/// * `view` - the request.
/// * `user` - the user, ceiling, nonce and link.
/// * `error` - a message from a refused choice, if any.
///
/// # Returns
/// The page body.
pub(crate) fn user_page_html(view: &PageView, user: &UserPage<'_>, error: Option<&str>) -> String {
    let link = format!(
        r#"        <p class="muted"><a href="{}" id="consent-use-code">Use a sign-in code instead</a></p>"#,
        escape(user.code_href)
    );
    if user.ceiling.is_empty() {
        return format!(
            r#"        <h1>Allow access?</h1>
{client}
        <p class="error consent-error" id="consent-empty">You have no catalogs to share yet. Ask an admin for access, or use a sign-in code.</p>
        <form method="post" action="/oauth/authorize" id="consent-user-form">
            {hidden}
            <div class="consent-actions">
                <button type="submit" name="action" value="deny" id="consent-deny" class="secondary">Deny</button>
            </div>
        </form>
{link}"#,
            client = client_block(view),
            hidden = hidden_fields(view),
        );
    }
    // Catalog names are `[a-z0-9-]+` (a schema CHECK), so they are safe in
    // ids and names; they are escaped anyway, as everything is.
    let rows: String = user
        .ceiling
        .iter()
        .map(|(catalog, &allowed)| {
            let name = escape(catalog);
            let write = if allowed >= Access::Write {
                r#"
                        <option value="write">write</option>"#
            } else {
                ""
            };
            format!(
                r#"                <div class="consent-catalog">
                    <label><input type="checkbox" name="grant.{name}" value="on" id="consent-catalog-{name}"> {name}</label>
                    <select name="level.{name}" id="consent-level-{name}" aria-label="Access to {name}">
                        <option value="read">read</option>{write}
                    </select>
                </div>
"#
            )
        })
        .collect();
    let defaults: String = user
        .ceiling
        .keys()
        .map(|catalog| {
            let name = escape(catalog);
            format!(
                r#"                    <option value="{name}">{name}</option>
"#
            )
        })
        .collect();
    format!(
        r#"        <h1>Allow access?</h1>
{client}
        <p class="muted">Signed in as <strong id="consent-user">{user}</strong>. Choose what <strong>{name}</strong> may use. You can grant up to your own access.</p>
{error}        <form method="post" action="/oauth/authorize" id="consent-user-form">
            {hidden}
            <input type="hidden" name="summary" value="{nonce}">
            <fieldset id="consent-catalogs">
                <legend>Catalogs</legend>
{rows}            </fieldset>
            <label>Default catalog
                <select name="default_catalog" id="consent-default">
                    <option value="">None</option>
{defaults}                </select>
            </label>
            <div class="consent-actions">
                <button type="submit" name="action" value="consent" id="consent-approve">Approve</button>
                <button type="submit" name="action" value="deny" id="consent-deny" class="secondary" formnovalidate>Deny</button>
            </div>
        </form>
{link}"#,
        client = client_block(view),
        user = escape(user.user),
        name = escape(&view.client_name),
        error = error_line(error),
        hidden = hidden_fields(view),
        nonce = escape(user.nonce),
    )
}

/// The signed-in consent page.
///
/// # Arguments
/// * `view` - the request.
/// * `user` - the user, ceiling, nonce and link.
/// * `error` - a message from a refused choice, if any.
///
/// # Returns
/// 200 with the page.
pub fn user_page(view: &PageView, user: &UserPage<'_>, error: Option<&str>) -> Response {
    page(
        StatusCode::OK,
        "Allow access?",
        &user_page_html(view, user, error),
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
    fn debug_never_prints_the_pending_id_or_the_csrf_token() {
        let page = PageView {
            pending_id: "pending-id-that-must-not-leak".to_string(),
            csrf: "csrf-token-that-must-not-leak".to_string(),
            ..view()
        };

        let printed = format!("{page:?}");

        assert!(
            !printed.contains("pending-id-that-must-not-leak"),
            "{printed}"
        );
        assert!(
            !printed.contains("csrf-token-that-must-not-leak"),
            "{printed}"
        );
        assert!(printed.contains("client.example.com"), "{printed}");
    }

    #[test]
    fn debug_never_prints_the_user_pages_nonce() {
        let ceiling = Ceiling::new();
        let user = UserPage {
            user: "alice",
            ceiling: &ceiling,
            nonce: "consent-nonce-that-must-not-leak",
            code_href: "/oauth/authorize?a=1&signin=code",
        };

        let printed = format!("{user:?}");

        assert!(
            !printed.contains("consent-nonce-that-must-not-leak"),
            "{printed}"
        );
        assert!(printed.contains("alice"), "{printed}");
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
            user: Some("<i>u</i>".to_string()),
            default_catalog: None,
        };
        for html in [
            code_page_html(&hostile, None),
            summary_page_html(&hostile, &grant, "n1"),
        ] {
            assert!(!html.contains("<script>"), "{html}");
            assert!(!html.contains("<b>a</b>"), "{html}");
            assert!(!html.contains("<i>u</i>"), "{html}");
            assert!(!html.contains("unverified"), "a CIMD client is verified");
        }
    }

    #[test]
    fn pages_have_no_inline_script_or_style() {
        let grant = GrantSummary {
            access: "read",
            catalogs: "main".to_string(),
            actor: "example-actor".to_string(),
            user: Some("alice".to_string()),
            default_catalog: None,
        };
        for html in [
            pages::document("t", &code_page_html(&view(), Some("e")), &[]),
            pages::document("t", &summary_page_html(&view(), &grant, "n1"), &[]),
            pages::document("t", &error_page_html("e"), &[]),
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
            user: Some("alice".to_string()),
            default_catalog: Some("main".to_string()),
        };
        let html = summary_page_html(&view(), &grant, "n1");
        assert!(html.contains("<strong>write</strong>"));
        assert!(html.contains("<strong>main, other</strong>"));
        assert!(html.contains("as <strong>alice</strong> (actor <strong>example-actor</strong>)"));
        assert!(html.contains("default catalog <strong>main</strong>"));
        assert!(html.contains(r#"id="consent-approve""#));
        assert!(html.contains(r#"id="consent-deny""#));
        assert!(html.contains(r#"name="summary" value="n1""#));
    }

    #[test]
    fn the_summary_of_an_unassigned_code_names_only_the_actor() {
        let grant = GrantSummary {
            access: "read",
            catalogs: "main".to_string(),
            actor: "example-actor".to_string(),
            user: None,
            default_catalog: None,
        };
        let html = summary_page_html(&view(), &grant, "n1");
        assert!(html.contains("as actor <strong>example-actor</strong>."));
        assert!(!html.contains("(actor"));
    }

    fn ceiling() -> Ceiling {
        [
            ("main".to_string(), Access::Write),
            ("other".to_string(), Access::Read),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn the_user_page_offers_each_catalog_at_most_at_its_ceiling() {
        let ceiling = ceiling();
        let user = UserPage {
            user: "<alice>",
            ceiling: &ceiling,
            nonce: "n1",
            code_href: "/oauth/authorize?a=1&signin=code",
        };

        let html = user_page_html(&view(), &user, Some("bad <choice>"));

        assert!(html.contains(r#"<strong id="consent-user">&lt;alice&gt;</strong>"#));
        assert!(html.contains(r#"name="grant.main""#));
        assert!(html.contains(r#"name="grant.other""#));
        // `main` offers write, `other` only read.
        let other = &html[html.find(r#"name="level.other""#).expect("other's select")..];
        let other = &other[..other.find("</select>").expect("its end")];
        assert!(!other.contains("write"), "{other}");
        assert!(html.contains(r#"<option value="write">write</option>"#));
        assert!(html.contains(r#"name="summary" value="n1""#));
        assert!(html.contains(r#"href="/oauth/authorize?a=1&amp;signin=code""#));
        assert!(html.contains("bad &lt;choice&gt;"));
        assert!(html.contains(r#"id="consent-approve""#));
        let document = pages::document("t", &html, &[]);
        assert!(!document.contains("<script"));
        assert!(!document.contains("style="));
    }

    #[test]
    fn an_empty_ceiling_offers_only_deny_and_the_code_link() {
        let empty = Ceiling::new();
        let user = UserPage {
            user: "alice",
            ceiling: &empty,
            nonce: "n1",
            code_href: "/oauth/authorize?a=1&signin=code",
        };

        let html = user_page_html(&view(), &user, None);

        assert!(html.contains(r#"id="consent-empty""#));
        assert!(html.contains(r#"id="consent-deny""#));
        assert!(html.contains(r#"id="consent-use-code""#));
        assert!(!html.contains(r#"id="consent-approve""#));
        assert!(!html.contains(r#"name="summary""#), "nothing to approve");
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
