//! The server-rendered sign-in pages (spec "Signing in"): `/signin` with
//! the password form, `/signin/forgot`, and the emailed `/account/setup`
//! and `/account/reset` links. They use the console's CSP, so the passkey
//! script (Task 8) and provider links (Task 9) fit, and they run no inline
//! script. Without `public_url` only `GET /signin` exists, offering the
//! API-key sign-in (plan Decision 18).
//!
//! Secrets stay out of logs: the forms and the link query have no `Debug`,
//! and the request span records the path without its query (`http::router`),
//! so an emailed link's token never reaches a log line.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::rejection::FormRejection;
use axum::extract::{Query, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Router};
use serde::Deserialize;
use sqlx::{Connection as _, PgConnection};

use crate::app::App;
use crate::error::OkfError;
use crate::http::console_session::{client_ip, cookie, session_cookie, COOKIE};
use crate::http::limits::ip_key;
use crate::http::pages::{self, escape, safe_return_to};
use crate::http::ui::CONTENT_SECURITY_POLICY;
use crate::http::{timeout_layer, AppError};
use crate::mail::queue as mail_queue;
use crate::mail::{self, Template, RESET_PATH, SETUP_PATH};
use crate::store::email_tokens::{self, Purpose};
use crate::store::passwords::{self, NewPassword, PasswordStamp, SignInOutcome};
use crate::store::settings::Brand;
use crate::store::sign_in_events::{self, Attempt, Method, Outcome};
use crate::store::user_sessions::{self, SessionStart};
use crate::store::users::{self, UserAuth};

/// Every refused password sign-in says this, whatever the reason (spec).
pub const SIGN_IN_FAILED: &str = "Email or password is incorrect";
/// An expired, used or unknown emailed link (410).
pub const LINK_EXPIRED: &str = "This link has expired. Ask for a new one.";
/// `/signin/forgot`'s one answer.
pub const FORGOT_SENT: &str = "If an account exists, we sent a link.";
/// After a reset (`/signin?notice=reset`).
const PASSWORD_CHANGED: &str = "Your password was changed. Sign in with it.";
/// Over the per-address limit (429).
const TOO_MANY: &str = "Too many attempts from this address. Wait a few minutes and try again.";
/// A form posted from another site (403).
const CROSS_SITE: &str = "This form can only be sent from this site. Open it here and try again.";
/// The sign-in and setup pages' passkey script (Task 8).
pub(crate) const PASSKEY_SCRIPT: &str = "/ui/js/signin/passkey.js";

/// A configured OIDC provider's button on the sign-in page (Task 9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderButton {
    /// The provider's id (`[a-z0-9-]+`, checked at config load).
    pub id: String,
    /// Its button text.
    pub label: String,
}

/// What the sign-in page offers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SignInView {
    /// The email and password form.
    pub password: bool,
    /// The passkey button (Task 8).
    pub passkey: bool,
    /// Where to go after signing in ([`safe_return_to`]).
    pub return_to: String,
    /// A refusal to show.
    pub error: Option<&'static str>,
    /// A notice to show.
    pub notice: Option<&'static str>,
    /// One button per OIDC provider (Task 9).
    pub providers: Vec<ProviderButton>,
}

/// What the setup page shows. No `Debug`: it holds the setup token.
pub(crate) struct SetupView<'a> {
    /// The user's name.
    pub user: &'a str,
    /// The setup token (a hidden field; the page is `no-store`, `no-referrer`).
    pub token: &'a str,
    /// Where to go after setting up.
    pub return_to: &'a str,
    /// A problem with the last attempt.
    pub error: Option<&'a str>,
    /// The passkey option (Task 8).
    pub passkey: bool,
}

/// The login routes; without `public_url`, only the API-key sign-in page.
///
/// # Arguments
/// * `app` - the shared state (its `public_url`).
///
/// # Returns
/// The router, with the request timeout.
pub(crate) fn routes(app: &App) -> Router<Arc<App>> {
    let router = if app.config().public_url.is_some() {
        Router::new()
            .route("/signin", get(sign_in_page).post(sign_in_post))
            .route("/signin/forgot", get(forgot_page).post(forgot_post))
            // The paths the emailed links point at (`mail::link`, Task 6).
            .route(SETUP_PATH, get(setup_page).post(setup_post))
            .route(RESET_PATH, get(reset_page).post(reset_post))
    } else {
        // The post answers 404, as every login route does here, rather
        // than axum's 405 for a method the path lacks.
        Router::new().route(
            "/signin",
            get(sign_in_page).post(|| async { StatusCode::NOT_FOUND }),
        )
    };
    router.layer(timeout_layer())
}

/// Whether the pages offer a passkey: only when passkeys are on (a
/// domain `public_url`).
fn offers_passkeys(app: &App) -> bool {
    app.passkeys().is_some()
}

/// The OIDC provider buttons, one per configured provider.
fn provider_buttons(app: &App) -> Vec<ProviderButton> {
    super::oidc::buttons(app)
}

/// What `/signin` offers on this server.
fn sign_in_view(app: &App, return_to: String) -> SignInView {
    let login = app.config().public_url.is_some();
    SignInView {
        password: login && app.settings().password_sign_in,
        passkey: login && offers_passkeys(app),
        return_to,
        error: None,
        notice: None,
        providers: if login {
            provider_buttons(app)
        } else {
            Vec::new()
        },
    }
}

/// Whether a form post came from another site, by the browser's
/// `Sec-Fetch-Site` header (login CSRF: another site signing the browser in
/// as the attacker). `/oauth/authorize`'s consent form uses it too. The
/// header is set by the browser, not by page script,
/// and unlike `Origin` it does not turn into `null` under the pages'
/// `Referrer-Policy: no-referrer`. A client that sends none (curl, an old
/// browser) is let through: it is not a browser being steered. The passkey
/// routes (`http::passkeys`) check it too.
pub(crate) fn cross_site(headers: &HeaderMap) -> bool {
    headers
        .get("sec-fetch-site")
        .is_some_and(|value| !matches!(value.as_bytes(), b"same-origin" | b"none"))
}

/// The sign-in page's body.
///
/// # Arguments
/// * `view` - what to offer.
///
/// # Returns
/// The HTML, every value escaped.
pub(crate) fn sign_in_html(view: &SignInView) -> String {
    let return_to = escape(&view.return_to);
    let mut html = String::from("        <h1>Sign in</h1>\n");
    if let Some(notice) = view.notice {
        html.push_str(&format!(
            "        <p class=\"notice\" id=\"signin-notice\" role=\"status\">{}</p>\n",
            escape(notice)
        ));
    }
    if let Some(error) = view.error {
        html.push_str(&format!(
            "        <p class=\"error consent-error\" id=\"signin-error\" role=\"alert\">{}</p>\n",
            escape(error)
        ));
    }
    if view.password {
        html.push_str(&format!(
            r#"        <form method="post" action="/signin" id="signin-form">
            <input type="hidden" name="return_to" value="{return_to}">
            <label>Email
                <input type="email" name="email" id="signin-email" autocomplete="username" required>
            </label>
            <label>Password
                <input type="password" name="password" id="signin-password" autocomplete="current-password" required>
            </label>
            <button type="submit" id="signin-submit">Sign in</button>
        </form>
        <p class="small"><a href="/signin/forgot" id="signin-forgot">Forgot your password?</a></p>
"#
        ));
    }
    if view.passkey {
        html.push_str(&format!(
            r#"        <button type="button" id="signin-passkey" class="secondary" data-return-to="{return_to}">Sign in with a passkey</button>
        <p class="error" id="passkey-error" role="alert" hidden></p>
"#
        ));
    }
    if !view.providers.is_empty() {
        // The query value is URL-encoded, then the whole href HTML-escaped.
        let encoded: String = form_urlencoded::byte_serialize(view.return_to.as_bytes()).collect();
        html.push_str("        <div class=\"signin-providers\">\n");
        for provider in &view.providers {
            html.push_str(&format!(
                "            <a role=\"button\" class=\"secondary\" id=\"signin-oidc-{id}\" \
                 href=\"/auth/oidc/{id}/start?return_to={query}\">Sign in with {label}</a>\n",
                id = escape(&provider.id),
                query = escape(&encoded),
                label = escape(&provider.label),
            ));
        }
        html.push_str("        </div>\n");
    }
    html.push_str(
        "        <p class=\"muted signin-alt\">\
         <a href=\"/ui/\" id=\"signin-api-key\">Use an API key instead</a></p>",
    );
    // People with an admin-minted sign-in code and no account still need a
    // way through an OAuth sign-in (spec "OAuth consent as the user").
    if let Some(href) = crate::oauth::authorize::code_path_href(&view.return_to) {
        html.push_str(&format!(
            "\n        <p class=\"muted signin-alt\">\
             <a href=\"{}\" id=\"signin-use-code\">Use a sign-in code instead</a></p>",
            escape(&href)
        ));
    }
    html
}

/// `/signin` with `status`; the passkey script only when offered.
fn sign_in_response(status: StatusCode, view: &SignInView, brand: &Brand) -> Response {
    let scripts: &[&str] = if view.passkey { &[PASSKEY_SCRIPT] } else { &[] };
    pages::respond(
        status,
        pages::document("Sign in", &sign_in_html(view), scripts, brand),
        CONTENT_SECURITY_POLICY,
    )
}

/// A page with a heading, one message and a link back to `/signin`; the
/// OIDC callback answers with it too.
///
/// # Arguments
/// * `status` - the status.
/// * `title` - the heading and title (escaped).
/// * `message` - the message (escaped).
/// * `brand` - the product name and support contact.
///
/// # Returns
/// The page, under the console CSP.
pub(crate) fn message_page(
    status: StatusCode,
    title: &str,
    message: &str,
    brand: &Brand,
) -> Response {
    let body = format!(
        r#"        <h1>{title}</h1>
        <p class="consent-error" id="signin-message" role="alert">{message}</p>
        <p><a href="/signin">Back to sign in</a></p>"#,
        title = escape(title),
        message = escape(message),
    );
    pages::page(status, title, &body, CONTENT_SECURITY_POLICY, brand)
}

/// `GET /signin`'s query.
#[derive(Debug, Default, Deserialize)]
struct SignInQuery {
    return_to: Option<String>,
    notice: Option<String>,
}

/// `GET /signin`.
async fn sign_in_page(State(app): State<Arc<App>>, Query(query): Query<SignInQuery>) -> Response {
    let mut view = sign_in_view(&app, safe_return_to(query.return_to.as_deref()));
    // Only a known notice is shown: the page never echoes text from its URL.
    view.notice = (query.notice.as_deref() == Some("reset")).then_some(PASSWORD_CHANGED);
    sign_in_response(StatusCode::OK, &view, &app.settings().brand())
}

/// `POST /signin`'s form. No `Debug`: it holds a password. Every field
/// defaults to empty, so a malformed post takes the same path (and the same
/// password check) as a wrong password.
#[derive(Default, Deserialize)]
struct SignInForm {
    #[serde(default)]
    email: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    return_to: Option<String>,
}

/// `POST /signin`: 303 to `return_to` with the session cookie, or 401 with
/// the one message, 429 past the per-address limit, or 403 from another
/// site. Every 401 costs one argon2 verification (`passwords::verify` checks
/// a dummy hash when there is no real one).
async fn sign_in_post(
    State(app): State<Arc<App>>,
    parts: Parts,
    form: Result<Form<SignInForm>, FormRejection>,
) -> Response {
    // One read of the settings for the whole request.
    let settings = app.settings();
    if !settings.password_sign_in {
        return StatusCode::NOT_FOUND.into_response();
    }
    let brand = settings.brand();
    if cross_site(&parts.headers) {
        return message_page(StatusCode::FORBIDDEN, "Sign in", CROSS_SITE, &brand);
    }
    // `unwrap_or_default`: a body that is not a form is an empty form.
    let form = form.map(|Form(form)| form).unwrap_or_default();
    let return_to = safe_return_to(form.return_to.as_deref());
    let ip = client_ip(&parts, &app.config().auth);
    if !app
        .sign_in_limits()
        .sign_in_per_ip
        .try_take(&ip_key(ip), Instant::now())
    {
        let mut view = sign_in_view(&app, return_to);
        view.error = Some(TOO_MANY);
        return sign_in_response(StatusCode::TOO_MANY_REQUESTS, &view, &brand);
    }
    // One normalized address for the check and the audit lookup. One that
    // cannot be anyone's (`None`: a NUL, a control character, too long, ...)
    // is checked as the empty address, which names nobody: the same dummy
    // hash and the same 401 as an unknown email, never a database error.
    let email = users::normalize_email(&form.email).unwrap_or_default();
    let verdict = passwords::verify(app.pool(), &email, &form.password, app.clock().now()).await;
    let outcome = match verdict {
        Ok(SignInOutcome::Ok(verified)) => {
            let started = begin(
                &app,
                &parts,
                &verified.user,
                Method::Password,
                Some(&verified.stamp),
            )
            .await;
            return match started {
                Ok(headers) => (headers, Redirect::to(&return_to)).into_response(),
                // A reset replaced the password after it was checked: the
                // same answer as a wrong password (recorded as failed).
                Err(OkfError::Unauthenticated { .. }) => {
                    audit(
                        &app,
                        Some(&verified.user.id),
                        Method::Password,
                        Outcome::Failed,
                        ip,
                    )
                    .await;
                    let mut view = sign_in_view(&app, return_to);
                    view.error = Some(SIGN_IN_FAILED);
                    sign_in_response(StatusCode::UNAUTHORIZED, &view, &brand)
                }
                Err(error) => AppError(error).into_response(),
            };
        }
        Ok(SignInOutcome::Failed) => Outcome::Failed,
        Ok(SignInOutcome::Locked) => Outcome::Locked,
        Err(error) => return AppError(error).into_response(),
    };
    // The same lookup runs for known and unknown emails, so its time says
    // nothing; it only lets the user see their failed attempts.
    let user_id = match users::find_by_email(app.pool(), &email).await {
        Ok(found) => found.map(|user| user.id),
        Err(error) => return AppError(error).into_response(),
    };
    audit(&app, user_id.as_deref(), Method::Password, outcome, ip).await;
    let mut view = sign_in_view(&app, return_to);
    view.error = Some(SIGN_IN_FAILED);
    sign_in_response(StatusCode::UNAUTHORIZED, &view, &brand)
}

/// Signs a verified user in (a password sign-in): starts a session, ends
/// the one this browser held (rotation) and records the event, in one
/// transaction ([`start_in`]). `password` is the hash the password matched,
/// so the session is refused if a reset replaced it meanwhile.
///
/// # Returns
/// The `Set-Cookie` header to send.
///
/// # Errors
/// [`OkfError::Unauthenticated`] when the password (or the user) changed
/// after the check; [`OkfError::Database`]; [`OkfError::Io`] from the
/// random source.
async fn begin(
    app: &App,
    parts: &Parts,
    user: &UserAuth,
    method: Method,
    password: Option<&PasswordStamp>,
) -> Result<HeaderMap, OkfError> {
    // The new session, the end of the old one and the event commit
    // together; after the commit nothing runs, so a committed sign-in
    // always reaches the browser with its cookie.
    let mut tx = app.pool().begin().await?;
    let signing_in = SignIn {
        user,
        method,
        password,
    };
    let headers = start_in(app, &mut tx, parts, &signing_in).await?;
    tx.commit().await?;
    Ok(headers)
}

/// The client address and `User-Agent` a new session records.
fn client_of<'a>(app: &App, parts: &'a Parts) -> (Option<IpAddr>, &'a str) {
    let user_agent = parts
        .headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    (client_ip(parts, &app.config().auth), user_agent)
}

/// A verified sign-in, for [`start_in`]. No `Debug`: the stamp is a
/// password hash.
pub(crate) struct SignIn<'a> {
    /// The verified user.
    pub user: &'a UserAuth,
    /// How they proved who they are.
    pub method: Method,
    /// For a password sign-in, the hash the password matched
    /// (`user_sessions::start`); `None` otherwise.
    pub password: Option<&'a PasswordStamp>,
}

/// Starts a session inside the caller's transaction, ends the session this
/// browser held before (rotation) and records the sign-in event, in that
/// order: the insert reads the user's row `FOR SHARE` first, then the old
/// session's row is deleted (lock order: the user first). The caller only
/// commits after this: nothing that can block or fail runs after the
/// commit, so a committed sign-in always reaches the browser with its
/// cookie, and a failure (or a wait) before the commit changes nothing.
///
/// A passkey sign-in or a setup link calls this inside the transaction
/// that checked the credential under the user's lock, so nothing can
/// change the credential between that check and the insert.
///
/// # Arguments
/// * `app` - the shared state.
/// * `conn` - the caller's transaction (`&mut tx`).
/// * `parts` - the request (old session cookie, address, `User-Agent`).
/// * `signing_in` - who, how, and (for a password) the matched stamp.
///
/// # Returns
/// The `Set-Cookie` header to send.
///
/// # Errors
/// [`OkfError::Unauthenticated`] when the user is gone or the stamped
/// password changed; [`OkfError::Database`]; [`OkfError::Io`] from the
/// random source.
pub(crate) async fn start_in(
    app: &App,
    conn: &mut PgConnection,
    parts: &Parts,
    signing_in: &SignIn<'_>,
) -> Result<HeaderMap, OkfError> {
    let user = signing_in.user;
    let (ip, user_agent) = client_of(app, parts);
    let absolute = app.settings().console_session_absolute;
    let now = app.clock().now();
    let request = SessionStart {
        user_id: &user.id,
        ip,
        user_agent,
        absolute,
        now,
        password: signing_in.password,
    };
    let session = user_sessions::start(&mut *conn, &request).await?;
    if let Some(old) = cookie(&parts.headers, COOKIE) {
        user_sessions::revoke(&mut *conn, old).await?;
    }
    let attempt = Attempt {
        user_id: Some(&user.id),
        method: signing_in.method,
        outcome: Outcome::Ok,
        ip,
        now,
    };
    // In a savepoint, so a failure to record is logged and never decides
    // the sign-in (as `audit`), yet the row commits with the session.
    let mut savepoint = conn.begin().await?;
    match sign_in_events::record_in(&mut savepoint, &attempt).await {
        Ok(()) => savepoint.commit().await?,
        Err(error) => {
            savepoint.rollback().await?;
            tracing::error!(%error, "recording a sign-in event failed");
        }
    }
    Ok(session_cookie(app, &user.id, &session, absolute))
}

/// Records a refused sign-in attempt as a `sign_in_events` row (which also
/// logs an `info` line). A failure to record is logged; it never decides
/// the sign-in. A successful sign-in records its event in its own
/// transaction instead ([`start_in`]).
///
/// # Arguments
/// * `app` - the shared state.
/// * `user_id` - the user, when known.
/// * `method` - how they tried.
/// * `outcome` - what happened.
/// * `ip` - their address, if known.
pub(crate) async fn audit(
    app: &App,
    user_id: Option<&str>,
    method: Method,
    outcome: Outcome,
    ip: Option<IpAddr>,
) {
    let now = app.clock().now();
    if let Err(error) = sign_in_events::record(app.pool(), user_id, method, outcome, ip, now).await
    {
        tracing::error!(%error, "recording a sign-in event failed");
    }
}

/// Queues the "a sign-in method was added" notice to the user's email,
/// inside the caller's transaction (the one that adds the method), so the
/// mail exists only if the change commits and nothing runs after the
/// commit. A user without an email sends nothing; a failure to queue is
/// logged and rolled back to a savepoint, so it never decides the change.
///
/// # Arguments
/// * `app` - the shared state.
/// * `conn` - the caller's transaction (`&mut tx`).
/// * `user` - the user.
/// * `method` - what was added, in words ("a new password", "a passkey").
///
/// # Errors
/// [`OkfError::Database`] opening or ending the savepoint.
pub(crate) async fn notify_method_added(
    app: &App,
    conn: &mut PgConnection,
    user: &UserAuth,
    method: &str,
) -> Result<(), OkfError> {
    let Some(to) = user.email.as_deref() else {
        return Ok(());
    };
    let template = Template::MethodAdded {
        method: method.to_string(),
    };
    let mut savepoint = conn.begin().await?;
    match mail_queue::enqueue_in(&mut savepoint, to, &template, app.clock().now()).await {
        Ok(()) => savepoint.commit().await?,
        Err(error) => {
            savepoint.rollback().await?;
            tracing::error!(%error, user = %user.id, "queueing the sign-in method notice failed");
        }
    }
    Ok(())
}

/// The forgot page's body: the form, or the one answer.
fn forgot_html(sent: bool) -> String {
    if sent {
        return format!(
            r#"        <h1>Check your email</h1>
        <p id="forgot-sent" role="status">{}</p>
        <p><a href="/signin">Back to sign in</a></p>"#,
            escape(FORGOT_SENT)
        );
    }
    r#"        <h1>Forgot your password?</h1>
        <p class="muted">Enter your email address and we will send you a link to choose a new password.</p>
        <form method="post" action="/signin/forgot" id="forgot-form">
            <label>Email
                <input type="email" name="email" id="forgot-email" autocomplete="username" required>
            </label>
            <button type="submit" id="forgot-submit">Send the link</button>
        </form>
        <p><a href="/signin">Back to sign in</a></p>"#
        .to_string()
}

/// `GET /signin/forgot`.
async fn forgot_page(State(app): State<Arc<App>>) -> Response {
    pages::page(
        StatusCode::OK,
        "Forgot your password?",
        &forgot_html(false),
        CONTENT_SECURITY_POLICY,
        &app.settings().brand(),
    )
}

/// `POST /signin/forgot`'s form. No `Debug`: an email typed here may belong
/// to nobody, and it has no place in a log line.
#[derive(Deserialize)]
struct ForgotForm {
    email: String,
}

/// `POST /signin/forgot`: always the same answer (spec), 429 only past the
/// per-address limit, 403 from another site.
async fn forgot_post(
    State(app): State<Arc<App>>,
    parts: Parts,
    form: Result<Form<ForgotForm>, FormRejection>,
) -> Response {
    let brand = app.settings().brand();
    if cross_site(&parts.headers) {
        return message_page(
            StatusCode::FORBIDDEN,
            "Forgot your password?",
            CROSS_SITE,
            &brand,
        );
    }
    let now = Instant::now();
    let limits = app.sign_in_limits();
    let ip = client_ip(&parts, &app.config().auth);
    if !limits.forgot_per_ip.try_take(&ip_key(ip), now) {
        return message_page(
            StatusCode::TOO_MANY_REQUESTS,
            "Forgot your password?",
            TOO_MANY,
            &brand,
        );
    }
    // `normalize_email`: the same normalized address as `/signin`; one that
    // cannot be anyone's (a NUL, ...) does no work and gets the same answer.
    let email = form
        .ok()
        .and_then(|Form(form)| users::normalize_email(&form.email));
    if let Some(email) = email {
        // Only a plausible address takes a limiter slot, so the per-email
        // map holds short keys. Past the per-email limit the answer is the
        // same, so it never says whether the address is known. The work
        // runs after the answer for the same reason: its duration would.
        if limits.forgot_per_email.try_take(&email, now) {
            let app = Arc::clone(&app);
            tokio::spawn(async move {
                if let Err(error) = send_reset(&app, &email).await {
                    tracing::error!(%error, "sending a password reset link failed");
                }
            });
        }
    }
    pages::page(
        StatusCode::OK,
        "Check your email",
        &forgot_html(true),
        CONTENT_SECURITY_POLICY,
        &brand,
    )
}

/// Issues a reset link and queues it, when `email` names a user.
///
/// # Errors
/// [`OkfError::Database`]; [`OkfError::Io`] from the random source.
async fn send_reset(app: &App, email: &str) -> Result<(), OkfError> {
    let Some(public_url) = app.config().public_url.as_deref() else {
        return Ok(());
    };
    // The lookup and the issue are one transaction under the user's row
    // lock, so the link goes to the address the user has while it is live
    // (an email change deletes it): `email_tokens::issue_for_email`.
    let ttl = app.settings().reset_link_ttl;
    let issued =
        email_tokens::issue_for_email(app.pool(), email, Purpose::Reset, ttl, app.clock().now())
            .await?;
    let Some(issued) = issued else {
        return Ok(());
    };
    let link = mail::link(public_url, RESET_PATH, &issued.token);
    let template = Template::Reset { link };
    mail_queue::enqueue(app.pool(), &issued.to, &template, app.clock().now()).await?;
    tracing::info!(user = %issued.user.id, "password reset link queued");
    Ok(())
}

/// A token in a link's query. No `Debug`: it holds the token.
#[derive(Deserialize)]
struct TokenQuery {
    #[serde(default)]
    token: String,
    #[serde(default)]
    return_to: Option<String>,
}

/// The answer to an emailed link that does not work: 410 for an expired,
/// used or unknown one, else the error.
fn link_error(error: OkfError, brand: &Brand) -> Response {
    match error {
        OkfError::Gone { .. } => {
            message_page(StatusCode::GONE, "Link expired", LINK_EXPIRED, brand)
        }
        other => AppError(other).into_response(),
    }
}

/// A new password and its confirmation: equal, and within the policy.
///
/// # Returns
/// `Err` with what to tell the person (never the password itself).
fn check_new_password(password: &str, confirm: &str) -> Result<(), String> {
    if password != confirm {
        return Err("The two passwords do not match.".to_string());
    }
    match passwords::check_policy(password) {
        Ok(()) => Ok(()),
        Err(OkfError::LimitExceeded { reason, .. }) => Err(reason),
        Err(other) => Err(other.to_string()),
    }
}

/// The error line of a form page.
fn error_line(error: Option<&str>) -> String {
    error.map_or_else(String::new, |message| {
        format!(
            "        <p class=\"error consent-error\" id=\"form-error\" role=\"alert\">{}</p>\n",
            escape(message)
        )
    })
}

/// The setup page's body.
///
/// # Arguments
/// * `view` - the user, token and options.
///
/// # Returns
/// The HTML, every value escaped.
pub(crate) fn setup_html(view: &SetupView<'_>) -> String {
    let return_to = escape(view.return_to);
    let passkey = if view.passkey {
        format!(
            r#"
        <section class="signin-passkey" id="setup-passkey-section">
            <h2>Or use a passkey</h2>
            <label>Passkey name
                <input type="text" id="setup-passkey-name" value="Passkey" maxlength="64">
            </label>
            <button type="button" id="setup-passkey" class="secondary" data-return-to="{return_to}">Register a passkey and sign in</button>
            <p class="error" id="passkey-error" role="alert" hidden></p>
        </section>"#
        )
    } else {
        String::new()
    };
    format!(
        r#"        <h1>Set up your account</h1>
        <p>Welcome, <strong>{user}</strong>. Choose a password to sign in with.</p>
{error}        <form method="post" action="/account/setup" id="setup-form">
            <input type="hidden" name="token" value="{token}">
            <input type="hidden" name="return_to" value="{return_to}">
            <label>Password (at least 12 characters)
                <input type="password" name="password" id="setup-password" autocomplete="new-password" minlength="12" required>
            </label>
            <label>Confirm the password
                <input type="password" name="confirm" id="setup-confirm" autocomplete="new-password" required>
            </label>
            <button type="submit" id="setup-submit">Set password and sign in</button>
        </form>{passkey}"#,
        user = escape(view.user),
        error = error_line(view.error),
        token = escape(view.token),
    )
}

/// The setup page with `status`.
fn setup_response(status: StatusCode, view: &SetupView<'_>, brand: &Brand) -> Response {
    let scripts: &[&str] = if view.passkey { &[PASSKEY_SCRIPT] } else { &[] };
    pages::respond(
        status,
        pages::document("Set up your account", &setup_html(view), scripts, brand),
        CONTENT_SECURITY_POLICY,
    )
}

/// `GET /account/setup?token=...`.
async fn setup_page(State(app): State<Arc<App>>, Query(query): Query<TokenQuery>) -> Response {
    let return_to = safe_return_to(query.return_to.as_deref());
    let brand = app.settings().brand();
    match email_tokens::peek(app.pool(), &query.token, Purpose::Setup, app.clock().now()).await {
        Ok(user) => setup_response(
            StatusCode::OK,
            &SetupView {
                user: &user.name,
                token: &query.token,
                return_to: &return_to,
                error: None,
                passkey: offers_passkeys(&app),
            },
            &brand,
        ),
        Err(error) => link_error(error, &brand),
    }
}

/// `POST /account/setup` and `/account/reset`'s form. No `Debug`: it holds
/// the token and the passwords.
#[derive(Deserialize)]
struct NewPasswordForm {
    token: String,
    password: String,
    confirm: String,
    #[serde(default)]
    return_to: Option<String>,
}

/// `POST /account/setup`: sets the password and signs in. The link is used
/// only once the password passes, so a weak one can be retried.
async fn setup_post(
    State(app): State<Arc<App>>,
    parts: Parts,
    form: Result<Form<NewPasswordForm>, FormRejection>,
) -> Response {
    let brand = app.settings().brand();
    if cross_site(&parts.headers) {
        return message_page(
            StatusCode::FORBIDDEN,
            "Set up your account",
            CROSS_SITE,
            &brand,
        );
    }
    let Ok(Form(form)) = form else {
        return message_page(
            StatusCode::BAD_REQUEST,
            "Set up your account",
            "Open the link from your email again.",
            &brand,
        );
    };
    let return_to = safe_return_to(form.return_to.as_deref());
    let now = app.clock().now();
    let user = match email_tokens::peek(app.pool(), &form.token, Purpose::Setup, now).await {
        Ok(user) => user,
        Err(error) => return link_error(error, &brand),
    };
    if let Err(problem) = check_new_password(&form.password, &form.confirm) {
        let view = SetupView {
            user: &user.name,
            token: &form.token,
            return_to: &return_to,
            error: Some(&problem),
            passkey: offers_passkeys(&app),
        };
        return setup_response(StatusCode::UNPROCESSABLE_ENTITY, &view, &brand);
    }
    // The session starts in the link's transaction, so a reset cannot
    // replace the password between the setup and the session, and the
    // committed setup always reaches the browser with its cookie.
    let used = match use_link(&app, &parts, &form.token, Purpose::Setup, &form.password).await {
        Ok(used) => used,
        Err(error) => return link_error(error, &brand),
    };
    // `unwrap_or_default`: a setup link always starts a session; an empty
    // header map would only mean no cookie to send.
    let cookie = used.cookie.unwrap_or_default();
    (cookie, Redirect::to(&return_to)).into_response()
}

/// Uses an emailed link and stores the new password it was for, in one
/// transaction, so the link is used only if the password change commits:
/// a failure anywhere leaves the old password, the sessions and the link
/// as they were. A setup link also starts the session (and ends this
/// browser's old one) in that transaction ([`start_in`]); a reset link
/// ends every session of the user (`passwords::reset_in`). The password is
/// hashed before the transaction opens, so no row lock is held during
/// argon2.
///
/// # Arguments
/// * `app` - the shared state.
/// * `parts` - the request (for the setup link's session).
/// * `token` - the raw token from the form.
/// * `purpose` - which link the page is for.
/// * `password` - the new password (already through [`check_new_password`]).
///
/// # Returns
/// The link's user, how many sessions ended (0 for setup) and, for setup,
/// the new session's `Set-Cookie` header.
///
/// # Errors
/// [`OkfError::Gone`] for a link that no longer works (also when a
/// concurrent post used it first); [`OkfError::LimitExceeded`],
/// [`OkfError::Io`] or [`OkfError::Database`] otherwise.
async fn use_link(
    app: &App,
    parts: &Parts,
    token: &str,
    purpose: Purpose,
    password: &str,
) -> Result<LinkUsed, OkfError> {
    let hashed = NewPassword::hash(password).await?;
    let now = app.clock().now();
    let mut tx = app.pool().begin().await?;
    // The token's row stays locked until the commit, so of two concurrent
    // posts the second waits and then finds the link used.
    let user = email_tokens::redeem_in(&mut tx, token, purpose, now).await?;
    // `match` on the purpose is exhaustive: a new purpose must say here what
    // its link changes.
    let (ended, cookie) = match purpose {
        Purpose::Setup => {
            passwords::set_in(&mut tx, &user.id, &hashed, now).await?;
            let signing_in = SignIn {
                user: &user,
                method: Method::Setup,
                password: None,
            };
            (0, Some(start_in(app, &mut tx, parts, &signing_in).await?))
        }
        Purpose::Reset => {
            let ended = passwords::reset_in(&mut tx, &user.id, &hashed, now).await?;
            notify_method_added(app, &mut tx, &user, "a new password").await?;
            (ended, None)
        }
    };
    tx.commit().await?;
    Ok(LinkUsed {
        user,
        ended,
        cookie,
    })
}

/// What [`use_link`] did. No `Debug`, as for the forms.
struct LinkUsed {
    /// The link's user.
    user: UserAuth,
    /// How many sessions it ended (a reset; 0 for setup).
    ended: u64,
    /// The setup link's session cookie (`None` for a reset).
    cookie: Option<HeaderMap>,
}

/// The reset page's body.
fn reset_html(token: &str, error: Option<&str>) -> String {
    format!(
        r#"        <h1>Choose a new password</h1>
        <p class="muted">Every session you have open will be signed out.</p>
{error}        <form method="post" action="/account/reset" id="reset-form">
            <input type="hidden" name="token" value="{token}">
            <label>New password (at least 12 characters)
                <input type="password" name="password" id="reset-password" autocomplete="new-password" minlength="12" required>
            </label>
            <label>Confirm the password
                <input type="password" name="confirm" id="reset-confirm" autocomplete="new-password" required>
            </label>
            <button type="submit" id="reset-submit">Change password</button>
        </form>"#,
        error = error_line(error),
        token = escape(token),
    )
}

/// The reset page with `status`.
fn reset_response(status: StatusCode, token: &str, error: Option<&str>, brand: &Brand) -> Response {
    pages::page(
        status,
        "Choose a new password",
        &reset_html(token, error),
        CONTENT_SECURITY_POLICY,
        brand,
    )
}

/// `GET /account/reset?token=...`.
async fn reset_page(State(app): State<Arc<App>>, Query(query): Query<TokenQuery>) -> Response {
    let brand = app.settings().brand();
    match email_tokens::peek(app.pool(), &query.token, Purpose::Reset, app.clock().now()).await {
        Ok(_) => reset_response(StatusCode::OK, &query.token, None, &brand),
        Err(error) => link_error(error, &brand),
    }
}

/// `POST /account/reset`: sets the password, ends every session (spec),
/// sends the notice and returns to `/signin`.
async fn reset_post(
    State(app): State<Arc<App>>,
    parts: Parts,
    form: Result<Form<NewPasswordForm>, FormRejection>,
) -> Response {
    let brand = app.settings().brand();
    if cross_site(&parts.headers) {
        return message_page(
            StatusCode::FORBIDDEN,
            "Choose a new password",
            CROSS_SITE,
            &brand,
        );
    }
    let Ok(Form(form)) = form else {
        return message_page(
            StatusCode::BAD_REQUEST,
            "Choose a new password",
            "Open the link from your email again.",
            &brand,
        );
    };
    let now = app.clock().now();
    if let Err(error) = email_tokens::peek(app.pool(), &form.token, Purpose::Reset, now).await {
        return link_error(error, &brand);
    }
    if let Err(problem) = check_new_password(&form.password, &form.confirm) {
        return reset_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            &form.token,
            Some(&problem),
            &brand,
        );
    }
    // A reset signs nobody in: the person signs in with the new password,
    // through the stamped password path.
    let LinkUsed { user, ended, .. } =
        match use_link(&app, &parts, &form.token, Purpose::Reset, &form.password).await {
            Ok(used) => used,
            Err(error) => return link_error(error, &brand),
        };
    // The notice was queued in the reset's transaction (`use_link`).
    tracing::info!(user = %user.id, ended, "password reset; every session ended");
    Redirect::to("/signin?notice=reset").into_response()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use chrono::Utc;
    use sqlx::PgPool;
    use tokio_util::sync::CancellationToken;
    // `oneshot` sends one request through a router without a socket.
    use tower::ServiceExt;

    use super::*;
    use crate::config::Config;
    use crate::http::api::testing::{
        allow, app, fail_on, login_app, user_with_password, wait_for_lock_waiters, Browser,
        Payload, Reply, MAIN, PASSWORD,
    };
    use crate::http::console_session::{start_session_with, COOKIE};
    use crate::mcp::testing::{app_with, TestApp};
    use crate::store::audit::AuditActor;
    use crate::store::settings::{self, SettingKey};
    use crate::store::sign_in_events::{Method, Outcome};

    async fn post_sign_in(
        browser: &mut Browser,
        test: &TestApp,
        email: &str,
        password: &str,
    ) -> Reply {
        let form = vec![
            ("email", email.to_string()),
            ("password", password.to_string()),
        ];
        browser
            .send(test, "POST", "/signin", Payload::Form(form))
            .await
    }

    async fn queued_mail(pool: &PgPool) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM apalis.jobs")
            .fetch_one(pool)
            .await
            .expect("count the queued mail")
    }

    /// Waits (up to 10 s) for `want` queued mails: `forgot` queues after it
    /// answers.
    async fn wait_for_mail(pool: &PgPool, want: i64) {
        for _ in 0..200 {
            if queued_mail(pool).await >= want {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("{want} mails never queued; {} did", queued_mail(pool).await);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_right_password_signs_in_with_an_http_only_cookie() {
        let test = login_app().await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let mut browser = Browser::default();

        let reply = post_sign_in(&mut browser, &test, "Alice@Example.com", PASSWORD).await;

        assert_eq!(reply.status, StatusCode::SEE_OTHER);
        assert_eq!(reply.location(), "/ui/");
        let line = reply.set_cookie(COOKIE).expect("a session cookie");
        for attribute in [
            "HttpOnly",
            "SameSite=Lax",
            "Path=/",
            "Secure",
            "Max-Age=604800",
        ] {
            assert!(line.contains(attribute), "{attribute} in {line}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_cookie_is_not_secure_on_a_loopback_http_origin() {
        let test = app_with(
            &[("main", MAIN)],
            Config {
                public_url: Some("http://127.0.0.1:8080".to_string()),
                ..Config::default()
            },
        )
        .await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;

        let reply = post_sign_in(
            &mut Browser::default(),
            &test,
            "alice@example.com",
            PASSWORD,
        )
        .await;

        let line = reply.set_cookie(COOKIE).expect("a session cookie");
        assert!(!line.contains("Secure"), "{line}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn every_refused_sign_in_answers_401_with_one_message() {
        let test = login_app().await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        user_with_password(&test, "bob", "bob@example.com", None).await;
        let mut browser = Browser::default();

        let wrong = post_sign_in(
            &mut browser,
            &test,
            "alice@example.com",
            "not-the-password-at-all",
        )
        .await;
        let unknown = post_sign_in(&mut browser, &test, "nobody@example.com", PASSWORD).await;
        let no_password = post_sign_in(&mut browser, &test, "bob@example.com", PASSWORD).await;
        let malformed = browser
            .send(
                &test,
                "POST",
                "/signin",
                Payload::Form(vec![("email", "alice@example.com".to_string())]),
            )
            .await;
        for _ in 0..8 {
            post_sign_in(
                &mut browser,
                &test,
                "alice@example.com",
                "still-not-the-password",
            )
            .await;
        }
        let locked = post_sign_in(&mut browser, &test, "alice@example.com", PASSWORD).await;

        for reply in [&wrong, &unknown, &no_password, &malformed, &locked] {
            assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
            assert!(reply.text().contains(SIGN_IN_FAILED), "{}", reply.text());
            assert!(reply.set_cookie(COOKIE).is_none());
        }
        // Apart from the echoed form, the bodies are byte-for-byte alike.
        assert_eq!(wrong.text(), unknown.text());
        assert_eq!(wrong.text(), locked.text());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_email_postgres_would_refuse_is_an_unknown_email() {
        let test = login_app().await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let mut browser = Browser::default();

        let unknown = post_sign_in(&mut browser, &test, "nobody@example.com", PASSWORD).await;
        let mut refused = Vec::new();
        // A NUL (Postgres refuses it in text), one after a real address, a
        // control character, and an address past the length limit.
        for email in [
            "\0".to_string(),
            "alice@example.com\0".to_string(),
            "alice\u{7}@example.com".to_string(),
            format!("{}@example.com", "a".repeat(300)),
        ] {
            refused.push(post_sign_in(&mut browser, &test, &email, PASSWORD).await);
        }
        // The store answers "nobody" too, never a database error, and the
        // check still spends the dummy hash (`Failed`, not an early return).
        let pool = test.app.pool();
        let found = users::find_by_email(pool, "alice@example.com\0").await;
        let checked = passwords::verify(pool, "\0", PASSWORD, Utc::now()).await;

        for reply in &refused {
            assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{}", reply.text());
            assert_eq!(reply.text(), unknown.text());
            assert!(reply.set_cookie(COOKIE).is_none());
        }
        assert!(matches!(found, Ok(None)), "{found:?}");
        assert!(matches!(checked, Ok(SignInOutcome::Failed)), "{checked:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forgot_answers_alike_for_an_email_postgres_would_refuse() {
        let test = login_app().await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let forgot = |email: &str| Payload::Form(vec![("email", email.to_string())]);

        let unknown = Browser::default()
            .send(
                &test,
                "POST",
                "/signin/forgot",
                forgot("nobody@example.com"),
            )
            .await;
        let nul = Browser::default()
            .send(
                &test,
                "POST",
                "/signin/forgot",
                forgot("alice@example.com\0"),
            )
            .await;
        let issued = email_tokens::issue_for_email(
            test.app.pool(),
            "alice@example.com\0",
            Purpose::Reset,
            Duration::from_secs(3600),
            Utc::now(),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(300)).await;

        assert_eq!(nul.status, StatusCode::OK);
        assert_eq!(nul.text(), unknown.text());
        assert!(matches!(issued, Ok(None)), "{issued:?}");
        assert_eq!(queued_mail(test.app.pool()).await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_link_token_with_a_nul_is_410() {
        let test = login_app().await;
        let form = |token: &str| {
            Payload::Form(vec![
                ("token", token.to_string()),
                ("password", PASSWORD.to_string()),
                ("confirm", PASSWORD.to_string()),
            ])
        };

        let mut statuses = Vec::new();
        for path in ["/account/setup", "/account/reset"] {
            let page = Browser::default()
                .send(
                    &test,
                    "GET",
                    &format!("{path}?token=abc%00def"),
                    Payload::Empty,
                )
                .await;
            let post = Browser::default()
                .send(&test, "POST", path, form("abc\0def"))
                .await;
            statuses.push((path, page.status, post.status));
        }

        for (path, page, post) in statuses {
            assert_eq!(page, StatusCode::GONE, "GET {path}");
            assert_eq!(post, StatusCode::GONE, "POST {path}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sign_in_attempts_are_recorded_for_the_user() {
        let test = login_app().await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let mut browser = Browser::default();
        post_sign_in(
            &mut browser,
            &test,
            "alice@example.com",
            "not-the-password-at-all",
        )
        .await;
        post_sign_in(&mut browser, &test, "alice@example.com", PASSWORD).await;

        let events = sign_in_events::recent(test.app.pool(), "alice", 10)
            .await
            .expect("events");

        assert_eq!(events.len(), 2);
        // Newest first.
        assert!(matches!(
            (&events[0].method, &events[0].outcome),
            (Method::Password, Outcome::Ok)
        ));
        assert!(matches!(
            (&events[1].method, &events[1].outcome),
            (Method::Password, Outcome::Failed)
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_thirty_first_attempt_from_one_address_is_429() {
        let test = login_app().await;
        let mut browser = Browser::default();
        for attempt in 0..30 {
            let reply = post_sign_in(
                &mut browser,
                &test,
                "nobody@example.com",
                "whatever-password",
            )
            .await;
            assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "attempt {attempt}");
        }

        let limited = post_sign_in(
            &mut browser,
            &test,
            "nobody@example.com",
            "whatever-password",
        )
        .await;

        assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn without_public_url_only_the_api_key_option_exists() {
        let test = app().await;
        let mut browser = Browser::default();

        let page = browser.send(&test, "GET", "/signin", Payload::Empty).await;
        let post = post_sign_in(&mut browser, &test, "alice@example.com", PASSWORD).await;
        let forgot = browser
            .send(&test, "GET", "/signin/forgot", Payload::Empty)
            .await;
        let setup = browser
            .send(&test, "GET", "/account/setup?token=x", Payload::Empty)
            .await;
        let reset = browser
            .send(&test, "GET", "/account/reset?token=x", Payload::Empty)
            .await;

        assert_eq!(page.status, StatusCode::OK);
        assert!(page.text().contains(r#"id="signin-api-key""#));
        assert!(!page.text().contains(r#"id="signin-form""#));
        assert_eq!(post.status, StatusCode::NOT_FOUND);
        assert_eq!(forgot.status, StatusCode::NOT_FOUND);
        assert_eq!(setup.status, StatusCode::NOT_FOUND);
        assert_eq!(reset.status, StatusCode::NOT_FOUND);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn turning_password_sign_in_off_hides_the_form_and_refuses_the_post() {
        let test = login_app().await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        settings::set(
            test.app.pool(),
            SettingKey::PasswordSignIn,
            "false",
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .expect("turn passwords off");
        test.app.poll().await.expect("poll");
        let mut browser = Browser::default();

        let page = browser.send(&test, "GET", "/signin", Payload::Empty).await;
        let post = post_sign_in(&mut browser, &test, "alice@example.com", PASSWORD).await;

        assert!(!page.text().contains(r#"id="signin-form""#));
        assert_eq!(post.status, StatusCode::NOT_FOUND);
        assert!(post.set_cookie(COOKIE).is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn return_to_survives_sign_in_only_when_it_is_same_origin() {
        let test = login_app().await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let sign_in_to = |return_to: &str| {
            Payload::Form(vec![
                ("email", "alice@example.com".to_string()),
                ("password", PASSWORD.to_string()),
                ("return_to", return_to.to_string()),
            ])
        };

        let kept = Browser::default()
            .send(&test, "POST", "/signin", sign_in_to("/ui/#/keys"))
            .await;
        let page = Browser::default()
            .send(
                &test,
                "GET",
                "/signin?return_to=%2Foauth%2Fauthorize%3Fx%3D1",
                Payload::Empty,
            )
            .await;

        assert_eq!(kept.location(), "/ui/#/keys");
        assert!(page
            .text()
            .contains(r#"name="return_to" value="/oauth/authorize?x=1""#));
        // Each value is sent as the form's text, so the `%..` ones arrive
        // still encoded once (a double-encoded link).
        for hostile in [
            "https://evil.example/",
            "//evil.example",
            "/\\evil.example",
            "\\\\evil.example",
            "javascript:alert(1)",
            "%2F%2Fevil.example",
            "/%5Cevil.example",
            "%2Fui%2F",
            "/ui/\r\nSet-Cookie: x=y",
            "https://okf.example.com.evil.example/ui/",
        ] {
            let dropped = Browser::default()
                .send(&test, "POST", "/signin", sign_in_to(hostile))
                .await;
            assert_eq!(dropped.status, StatusCode::SEE_OTHER, "{hostile:?}");
            assert_eq!(dropped.location(), "/ui/", "{hostile:?}");
        }
        // In the page's query the encoded forms are decoded once, then
        // checked: none survives into the form.
        for hostile in [
            "%2F%2Fevil.example",
            "%2F%5Cevil.example",
            "https%3A%2F%2Fevil.example",
            "javascript%3Aalert(1)",
        ] {
            let page = Browser::default()
                .send(
                    &test,
                    "GET",
                    &format!("/signin?return_to={hostile}"),
                    Payload::Empty,
                )
                .await;
            assert!(
                page.text().contains(r#"name="return_to" value="/ui/""#),
                "{hostile}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_form_posted_from_another_site_is_refused() {
        let test = login_app().await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let post = |path: &str, site: &str, body: &str| {
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/x-www-form-urlencoded")
                .header("sec-fetch-site", site)
                .body(Body::from(body.to_string()))
                .expect("a request")
        };
        let credentials = format!("email=alice%40example.com&password={PASSWORD}");

        let mut statuses = Vec::new();
        for (path, site, body) in [
            ("/signin", "cross-site", credentials.as_str()),
            ("/signin", "same-site", credentials.as_str()),
            ("/signin/forgot", "cross-site", "email=alice%40example.com"),
            (
                "/account/setup",
                "cross-site",
                "token=x&password=y&confirm=y",
            ),
            (
                "/account/reset",
                "cross-site",
                "token=x&password=y&confirm=y",
            ),
            ("/signin", "same-origin", credentials.as_str()),
        ] {
            let response = crate::http::router(Arc::clone(&test.app), &CancellationToken::new())
                .oneshot(post(path, site, body))
                .await
                .expect("the router answers");
            statuses.push(response.status());
        }

        assert_eq!(
            statuses,
            [
                StatusCode::FORBIDDEN,
                StatusCode::FORBIDDEN,
                StatusCode::FORBIDDEN,
                StatusCode::FORBIDDEN,
                StatusCode::FORBIDDEN,
                StatusCode::SEE_OTHER,
            ]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_setup_link_sets_a_password_and_signs_in_once() {
        let test = login_app().await;
        let alice = user_with_password(&test, "alice", "alice@example.com", None).await;
        let token = email_tokens::issue(
            test.app.pool(),
            &alice.id,
            Purpose::Setup,
            Duration::from_secs(3600),
            Utc::now(),
        )
        .await
        .expect("issue a setup token");
        let mut browser = Browser::default();
        let setup = |password: &str| {
            Payload::Form(vec![
                ("token", token.clone()),
                ("password", password.to_string()),
                ("confirm", password.to_string()),
            ])
        };

        let page = browser
            .send(
                &test,
                "GET",
                &format!("/account/setup?token={token}"),
                Payload::Empty,
            )
            .await;
        let done = browser
            .send(&test, "POST", "/account/setup", setup(PASSWORD))
            .await;
        let again = Browser::default()
            .send(&test, "POST", "/account/setup", setup(PASSWORD))
            .await;
        let session = browser
            .send(&test, "GET", "/api/v1/session", Payload::Empty)
            .await;

        assert_eq!(page.status, StatusCode::OK);
        assert!(page.text().contains(r#"id="setup-form""#));
        assert!(page.text().contains("<strong>alice</strong>"));
        assert_eq!(
            page.headers["content-security-policy"],
            CONTENT_SECURITY_POLICY
        );
        // Passkeys are on for this domain, so only their script loads.
        assert_eq!(page.text().matches("<script").count(), 1);
        assert!(page.text().contains(PASSKEY_SCRIPT));
        assert_eq!(done.status, StatusCode::SEE_OTHER);
        assert_eq!(done.location(), "/ui/");
        assert_eq!(session.json()["user"], "alice");
        assert_eq!(again.status, StatusCode::GONE, "the link is single-use");
        assert!(again.text().contains(LINK_EXPIRED));
        let events = sign_in_events::recent(test.app.pool(), "alice", 5)
            .await
            .expect("events");
        assert!(matches!(
            (&events[0].method, &events[0].outcome),
            (Method::Setup, Outcome::Ok)
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_weak_or_mismatched_password_keeps_the_setup_link_usable() {
        let test = login_app().await;
        let alice = user_with_password(&test, "alice", "alice@example.com", None).await;
        let token = email_tokens::issue(
            test.app.pool(),
            &alice.id,
            Purpose::Setup,
            Duration::from_secs(3600),
            Utc::now(),
        )
        .await
        .expect("issue a setup token");
        let post = |password: &str, confirm: &str| {
            Payload::Form(vec![
                ("token", token.clone()),
                ("password", password.to_string()),
                ("confirm", confirm.to_string()),
            ])
        };

        let short = Browser::default()
            .send(&test, "POST", "/account/setup", post("short", "short"))
            .await;
        let mismatch = Browser::default()
            .send(
                &test,
                "POST",
                "/account/setup",
                post(PASSWORD, "violet-lantern-orbit-43"),
            )
            .await;

        assert_eq!(short.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(mismatch.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(mismatch.text().contains("do not match"));
        assert!(
            !mismatch.text().contains(PASSWORD),
            "never echoes a password"
        );
        email_tokens::peek(test.app.pool(), &token, Purpose::Setup, Utc::now())
            .await
            .expect("the link still works");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unknown_link_is_410() {
        let test = login_app().await;

        let setup = Browser::default()
            .send(&test, "GET", "/account/setup?token=nope", Payload::Empty)
            .await;
        let reset = Browser::default()
            .send(&test, "GET", "/account/reset?token=nope", Payload::Empty)
            .await;

        assert_eq!(setup.status, StatusCode::GONE);
        assert_eq!(reset.status, StatusCode::GONE);
        assert!(reset.text().contains(LINK_EXPIRED));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_expired_link_is_410() {
        let test = login_app().await;
        let alice = user_with_password(&test, "alice", "alice@example.com", None).await;
        // Issued two hours ago with a one-hour lifetime.
        let token = email_tokens::issue(
            test.app.pool(),
            &alice.id,
            Purpose::Setup,
            Duration::from_secs(3600),
            Utc::now() - chrono::TimeDelta::hours(2),
        )
        .await
        .expect("issue a setup token");
        let form = Payload::Form(vec![
            ("token", token.clone()),
            ("password", PASSWORD.to_string()),
            ("confirm", PASSWORD.to_string()),
        ]);

        let page = Browser::default()
            .send(
                &test,
                "GET",
                &format!("/account/setup?token={token}"),
                Payload::Empty,
            )
            .await;
        let post = Browser::default()
            .send(&test, "POST", "/account/setup", form)
            .await;

        assert_eq!(page.status, StatusCode::GONE);
        assert_eq!(post.status, StatusCode::GONE);
        assert!(post.set_cookie(COOKIE).is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_expired_reset_link_is_410_and_changes_nothing() {
        let test = login_app().await;
        let alice = user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let mut signed_in = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        // Issued two hours ago with a one-hour lifetime.
        let token = email_tokens::issue(
            test.app.pool(),
            &alice.id,
            Purpose::Reset,
            Duration::from_secs(3600),
            Utc::now() - chrono::TimeDelta::hours(2),
        )
        .await
        .expect("issue a reset token");
        let form = Payload::Form(vec![
            ("token", token.clone()),
            ("password", "amber-quarry-signal-77".to_string()),
            ("confirm", "amber-quarry-signal-77".to_string()),
        ]);

        let post = Browser::default()
            .send(&test, "POST", "/account/reset", form)
            .await;
        let session = signed_in
            .send(&test, "GET", "/api/v1/session", Payload::Empty)
            .await;

        assert_eq!(post.status, StatusCode::GONE);
        assert!(post.text().contains(LINK_EXPIRED));
        assert_eq!(session.status, StatusCode::OK, "nothing was reset");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forgot_answers_alike_for_known_and_unknown_emails_and_mails_only_the_known() {
        let test = login_app().await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let forgot = |email: &str| Payload::Form(vec![("email", email.to_string())]);

        let unknown = Browser::default()
            .send(
                &test,
                "POST",
                "/signin/forgot",
                forgot("nobody@example.com"),
            )
            .await;
        let known = Browser::default()
            .send(&test, "POST", "/signin/forgot", forgot("alice@example.com"))
            .await;
        wait_for_mail(test.app.pool(), 1).await;
        tokio::time::sleep(Duration::from_millis(300)).await;

        assert_eq!(unknown.status, StatusCode::OK);
        assert_eq!(known.status, StatusCode::OK);
        assert_eq!(unknown.text(), known.text());
        assert!(known.text().contains(FORGOT_SENT));
        assert_eq!(queued_mail(test.app.pool()).await, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forgot_mails_at_most_three_links_per_email_an_hour() {
        let test = login_app().await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;

        // Letter case and spaces do not make a new address.
        for email in [
            "alice@example.com",
            "Alice@Example.com",
            " alice@example.com ",
            "ALICE@EXAMPLE.COM",
            "alice@example.com",
        ] {
            let form = Payload::Form(vec![("email", email.to_string())]);
            let reply = Browser::default()
                .send(&test, "POST", "/signin/forgot", form)
                .await;
            assert_eq!(reply.status, StatusCode::OK);
        }
        wait_for_mail(test.app.pool(), 3).await;
        tokio::time::sleep(Duration::from_millis(300)).await;

        assert_eq!(queued_mail(test.app.pool()).await, 3);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_twenty_first_forgot_from_one_address_is_429() {
        let test = login_app().await;

        for attempt in 0..20 {
            let form = Payload::Form(vec![("email", format!("nobody{attempt}@example.com"))]);
            let reply = Browser::default()
                .send(&test, "POST", "/signin/forgot", form)
                .await;
            assert_eq!(reply.status, StatusCode::OK, "attempt {attempt}");
        }
        let form = Payload::Form(vec![("email", "nobody@example.com".to_string())]);
        let limited = Browser::default()
            .send(&test, "POST", "/signin/forgot", form)
            .await;

        assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_reset_link_changes_the_password_and_ends_every_session() {
        let test = login_app().await;
        let alice = user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let mut first = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        let mut second = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        let token = email_tokens::issue(
            test.app.pool(),
            &alice.id,
            Purpose::Reset,
            Duration::from_secs(3600),
            Utc::now(),
        )
        .await
        .expect("issue a reset token");
        let new_password = "amber-quarry-signal-77";
        let form = || {
            Payload::Form(vec![
                ("token", token.clone()),
                ("password", new_password.to_string()),
                ("confirm", new_password.to_string()),
            ])
        };

        let page = Browser::default()
            .send(
                &test,
                "GET",
                &format!("/account/reset?token={token}"),
                Payload::Empty,
            )
            .await;
        let done = Browser::default()
            .send(&test, "POST", "/account/reset", form())
            .await;
        let again = Browser::default()
            .send(&test, "POST", "/account/reset", form())
            .await;

        assert!(page.text().contains(r#"id="reset-form""#));
        assert_eq!(done.status, StatusCode::SEE_OTHER);
        assert_eq!(done.location(), "/signin?notice=reset");
        assert_eq!(again.status, StatusCode::GONE, "the link is single-use");
        for browser in [&mut first, &mut second] {
            let reply = browser
                .send(&test, "GET", "/api/v1/session", Payload::Empty)
                .await;
            assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
        }
        let old = post_sign_in(
            &mut Browser::default(),
            &test,
            "alice@example.com",
            PASSWORD,
        )
        .await;
        let new = post_sign_in(
            &mut Browser::default(),
            &test,
            "alice@example.com",
            new_password,
        )
        .await;
        assert_eq!(old.status, StatusCode::UNAUTHORIZED);
        assert_eq!(new.status, StatusCode::SEE_OTHER);
        // The "a new password" notice went to alice.
        assert_eq!(queued_mail(test.app.pool()).await, 1);
    }

    /// Alice's sessions in the database, live or not.
    async fn sessions_of(pool: &PgPool, user_id: &str) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM user_sessions WHERE user_id = $1")
            .bind(user_id)
            .fetch_one(pool)
            .await
            .expect("count the sessions")
    }

    /// The password alice signs in with, checked: what `POST /signin`
    /// holds between the check and the session insert.
    async fn verified(test: &TestApp, password: &str) -> passwords::Verified {
        match passwords::verify(test.app.pool(), "alice@example.com", password, Utc::now()).await {
            Ok(SignInOutcome::Ok(verified)) => verified,
            other => panic!("the password was refused: {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_sign_in_checked_before_a_reset_gets_no_session_after_it() {
        let test = login_app().await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let checked = verified(&test, PASSWORD).await;
        let pool = test.app.pool();

        // The reset commits between the password check and the insert.
        passwords::reset(pool, &checked.user.id, "amber-quarry-signal-77", Utc::now())
            .await
            .expect("reset");
        let late =
            start_session_with(&test.app, &checked.user, Some(&checked.stamp), None, "").await;

        assert!(
            matches!(late, Err(OkfError::Unauthenticated { .. })),
            "{:?}",
            late.map(|_| ())
        );
        assert_eq!(sessions_of(pool, &checked.user.id).await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_sign_in_waiting_on_a_reset_in_progress_gets_no_session() {
        let test = login_app().await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let checked = verified(&test, PASSWORD).await;
        let pool = test.app.pool().clone();
        // Hold alice's row, so the reset and then the insert queue up behind
        // it in that order, whatever the scheduler does.
        let mut holder = pool.begin().await.expect("begin");
        sqlx::query("SELECT 1 FROM users WHERE id = $1 FOR UPDATE")
            .bind(&checked.user.id)
            .execute(&mut *holder)
            .await
            .expect("hold the row");
        // `tokio::spawn` runs each future on its own task, like
        // `asyncio.create_task`; `move` hands it owned copies.
        let reset = tokio::spawn({
            let pool = pool.clone();
            let user_id = checked.user.id.clone();
            async move { passwords::reset(&pool, &user_id, "amber-quarry-signal-77", Utc::now()).await }
        });
        wait_for_lock_waiters(&pool, 1).await;
        let insert = tokio::spawn({
            let app = Arc::clone(&test.app);
            let checked = checked.clone();
            async move {
                start_session_with(&app, &checked.user, Some(&checked.stamp), None, "")
                    .await
                    .map(|_| ())
            }
        });
        wait_for_lock_waiters(&pool, 2).await;

        holder.commit().await.expect("release the row");
        reset.await.expect("the reset task").expect("reset");
        let late = insert.await.expect("the insert task");

        assert!(
            matches!(late, Err(OkfError::Unauthenticated { .. })),
            "{late:?}"
        );
        assert_eq!(sessions_of(&pool, &checked.user.id).await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_session_inserted_before_a_reset_is_ended_by_it() {
        let test = login_app().await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let checked = verified(&test, PASSWORD).await;
        let pool = test.app.pool();
        start_session_with(&test.app, &checked.user, Some(&checked.stamp), None, "")
            .await
            .expect("the session starts while the password is current");

        let ended = passwords::reset(pool, &checked.user.id, "amber-quarry-signal-77", Utc::now())
            .await
            .expect("reset");

        assert_eq!(ended, 1);
        assert_eq!(sessions_of(pool, &checked.user.id).await, 0);
    }

    /// Which of two waiting transactions takes alice's row first.
    #[derive(Debug, Clone, Copy)]
    enum First {
        Forgot,
        EmailChange,
    }

    /// Holds alice's row, queues `first` and then the other of a forgotten
    /// password link and an email change behind it, and releases the row.
    ///
    /// # Returns
    /// What the forgot flow issued, if anything.
    async fn forgot_racing_an_email_change(first: First) -> (TestApp, Option<String>) {
        let test = login_app().await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let pool = test.app.pool().clone();
        let mut holder = pool.begin().await.expect("begin");
        sqlx::query("SELECT 1 FROM users WHERE name = 'alice' FOR UPDATE")
            .execute(&mut *holder)
            .await
            .expect("hold the row");
        let forgot = || {
            let pool = pool.clone();
            tokio::spawn(async move {
                email_tokens::issue_for_email(
                    &pool,
                    "alice@example.com",
                    Purpose::Reset,
                    Duration::from_secs(3600),
                    Utc::now(),
                )
                .await
            })
        };
        let change = || {
            let pool = pool.clone();
            tokio::spawn(async move {
                users::set_email(
                    &pool,
                    "alice",
                    Some("alice@example.net"),
                    &AuditActor::cli(),
                    Utc::now(),
                )
                .await
            })
        };
        // Each task is queued only once the one before it waits on the row.
        let (issued, changed) = match first {
            First::Forgot => {
                let issued = forgot();
                wait_for_lock_waiters(&pool, 1).await;
                let changed = change();
                wait_for_lock_waiters(&pool, 2).await;
                (issued, changed)
            }
            First::EmailChange => {
                let changed = change();
                wait_for_lock_waiters(&pool, 1).await;
                let issued = forgot();
                wait_for_lock_waiters(&pool, 2).await;
                (issued, changed)
            }
        };

        holder.commit().await.expect("release the row");
        let issued = issued.await.expect("the forgot task").expect("issue");
        changed
            .await
            .expect("the change task")
            .expect("change the email");

        let token = issued.map(|link| {
            assert_eq!(link.to, "alice@example.com", "mailed to the address it had");
            link.token
        });
        (test, token)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_reset_link_issued_before_an_email_change_dies_with_it() {
        let (test, token) = forgot_racing_an_email_change(First::Forgot).await;

        let token = token.expect("issued while the email was still alice's");
        let peeked = email_tokens::peek(test.app.pool(), &token, Purpose::Reset, Utc::now()).await;

        assert!(matches!(peeked, Err(OkfError::Gone { .. })), "{peeked:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn no_reset_link_is_issued_for_an_email_changed_meanwhile() {
        let (test, token) = forgot_racing_an_email_change(First::EmailChange).await;

        let live: i64 = sqlx::query_scalar("SELECT count(*) FROM email_tokens")
            .fetch_one(test.app.pool())
            .await
            .expect("count the links");

        assert!(token.is_none(), "the old address no longer names alice");
        assert_eq!(live, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_reset_after_setup_ends_the_session_the_setup_started() {
        let test = login_app().await;
        let alice = user_with_password(&test, "alice", "alice@example.com", None).await;
        let pool = test.app.pool();
        let token = email_tokens::issue(
            pool,
            &alice.id,
            Purpose::Setup,
            Duration::from_secs(3600),
            Utc::now(),
        )
        .await
        .expect("issue a setup token");

        // The session commits with the setup, so no reset can land between
        // the two; a reset after it ends the session.
        let used = use_link(&test.app, &no_request(), &token, Purpose::Setup, PASSWORD)
            .await
            .expect("use the setup link");
        let started = sessions_of(pool, &alice.id).await;
        passwords::reset(pool, &alice.id, "amber-quarry-signal-77", Utc::now())
            .await
            .expect("reset");

        assert!(used.cookie.is_some(), "the setup started a session");
        assert_eq!(started, 1);
        assert_eq!(sessions_of(pool, &alice.id).await, 0);
    }

    /// A request with no headers, for calling [`use_link`] directly.
    fn no_request() -> Parts {
        Request::builder()
            .body(())
            .expect("a request")
            .into_parts()
            .0
    }

    /// The other side of a race with a link's redemption.
    #[derive(Debug, Clone, Copy)]
    enum Rival {
        Issue,
        EmailChange,
    }

    /// Holds alice's row, then queues `rival` and, behind it, a redemption
    /// of her reset token, and releases the row. Before the user-first lock
    /// order the redemption locked the token at once and then queued for
    /// alice's row behind the rival, which, once it had the row, waited on
    /// the token to delete it: a deadlock (40P01). Now the redemption locks
    /// nothing before alice's row.
    ///
    /// # Returns
    /// The redemption's result and the token, once both sides finished.
    async fn redemption_racing(rival: Rival) -> (TestApp, Result<(), OkfError>, String) {
        let test = login_app().await;
        let alice = user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let pool = test.app.pool().clone();
        let token = email_tokens::issue(
            &pool,
            &alice.id,
            Purpose::Reset,
            Duration::from_secs(3600),
            Utc::now(),
        )
        .await
        .expect("issue a reset token");
        let mut holder = pool.begin().await.expect("begin");
        sqlx::query("SELECT 1 FROM users WHERE id = $1 FOR UPDATE")
            .bind(&alice.id)
            .execute(&mut *holder)
            .await
            .expect("hold alice's row");
        let rival = {
            let pool = pool.clone();
            tokio::spawn(async move {
                match rival {
                    Rival::Issue => email_tokens::issue_for_email(
                        &pool,
                        "alice@example.com",
                        Purpose::Reset,
                        Duration::from_secs(3600),
                        Utc::now(),
                    )
                    .await
                    .map(|_| ()),
                    Rival::EmailChange => {
                        users::set_email(
                            &pool,
                            "alice",
                            Some("alice@example.net"),
                            &AuditActor::cli(),
                            Utc::now(),
                        )
                        .await
                    }
                }
            })
        };
        wait_for_lock_waiters(&pool, 1).await;
        let redemption = {
            let app = Arc::clone(&test.app);
            let token = token.clone();
            tokio::spawn(async move {
                use_link(
                    &app,
                    &no_request(),
                    &token,
                    Purpose::Reset,
                    "amber-quarry-signal-77",
                )
                .await
                .map(|_| ())
            })
        };
        wait_for_lock_waiters(&pool, 2).await;

        holder.commit().await.expect("release alice's row");
        rival
            .await
            .expect("the rival task")
            .expect("the rival completes (no deadlock)");
        let redeemed = redemption.await.expect("the redemption task");
        (test, redeemed, token)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_redemption_racing_a_new_link_completes_without_a_deadlock() {
        let (test, redeemed, token) = redemption_racing(Rival::Issue).await;
        let pool = test.app.pool();

        // The new link replaced the one being redeemed, so it is gone and
        // the password is unchanged.
        assert!(
            matches!(redeemed, Err(OkfError::Gone { .. })),
            "{redeemed:?}"
        );
        assert!(email_tokens::peek(pool, &token, Purpose::Reset, Utc::now())
            .await
            .is_err());
        let old = passwords::verify(pool, "alice@example.com", PASSWORD, Utc::now())
            .await
            .expect("check the password");
        assert!(matches!(old, SignInOutcome::Ok(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_redemption_racing_an_email_change_completes_without_a_deadlock() {
        let (test, redeemed, _) = redemption_racing(Rival::EmailChange).await;
        let pool = test.app.pool();

        // The change deleted the link, so the password is unchanged.
        assert!(
            matches!(redeemed, Err(OkfError::Gone { .. })),
            "{redeemed:?}"
        );
        let old = passwords::verify(pool, "alice@example.net", PASSWORD, Utc::now())
            .await
            .expect("check the password");
        assert!(matches!(old, SignInOutcome::Ok(_)));
    }

    fn new_password_form(token: &str, password: &str) -> Payload {
        Payload::Form(vec![
            ("token", token.to_string()),
            ("password", password.to_string()),
            ("confirm", password.to_string()),
        ])
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_reset_that_fails_changes_nothing_and_keeps_the_link() {
        let test = login_app().await;
        let alice = user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let mut signed_in = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        let pool = test.app.pool();
        let token = email_tokens::issue(
            pool,
            &alice.id,
            Purpose::Reset,
            Duration::from_secs(3600),
            Utc::now(),
        )
        .await
        .expect("issue a reset token");
        let new_password = "amber-quarry-signal-77";
        // The session delete fails, after the token and the hash were
        // written in the same transaction.
        fail_on(pool, "BEFORE DELETE ON user_sessions FOR EACH STATEMENT").await;

        let failed = Browser::default()
            .send(
                &test,
                "POST",
                "/account/reset",
                new_password_form(&token, new_password),
            )
            .await;
        let session = signed_in
            .send(&test, "GET", "/api/v1/session", Payload::Empty)
            .await;
        let old = passwords::verify(pool, "alice@example.com", PASSWORD, Utc::now())
            .await
            .expect("check the old password");
        let link = email_tokens::peek(pool, &token, Purpose::Reset, Utc::now()).await;
        allow(pool, "user_sessions").await;
        let retried = Browser::default()
            .send(
                &test,
                "POST",
                "/account/reset",
                new_password_form(&token, new_password),
            )
            .await;

        assert_eq!(failed.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(session.status, StatusCode::OK, "the session is intact");
        assert!(
            matches!(old, SignInOutcome::Ok(_)),
            "the old password works"
        );
        assert!(link.is_ok(), "the link was not used");
        assert_eq!(retried.status, StatusCode::SEE_OTHER, "{}", retried.text());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_setup_that_fails_sets_nothing_and_keeps_the_link() {
        let test = login_app().await;
        let alice = user_with_password(&test, "alice", "alice@example.com", None).await;
        let pool = test.app.pool();
        let token = email_tokens::issue(
            pool,
            &alice.id,
            Purpose::Setup,
            Duration::from_secs(3600),
            Utc::now(),
        )
        .await
        .expect("issue a setup token");
        // The password write fails, after the token was used in the same
        // transaction.
        fail_on(
            pool,
            "BEFORE UPDATE ON users FOR EACH ROW \
             WHEN (OLD.password_hash IS DISTINCT FROM NEW.password_hash)",
        )
        .await;

        let failed = Browser::default()
            .send(
                &test,
                "POST",
                "/account/setup",
                new_password_form(&token, PASSWORD),
            )
            .await;
        let link = email_tokens::peek(pool, &token, Purpose::Setup, Utc::now()).await;
        allow(pool, "users").await;
        let password = passwords::verify(pool, "alice@example.com", PASSWORD, Utc::now())
            .await
            .expect("check the password");
        let retried = Browser::default()
            .send(
                &test,
                "POST",
                "/account/setup",
                new_password_form(&token, PASSWORD),
            )
            .await;

        assert_eq!(failed.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(failed.set_cookie(COOKIE).is_none());
        assert!(link.is_ok(), "the link was not used");
        assert_eq!(password, SignInOutcome::Failed, "no password was set");
        assert_eq!(retried.status, StatusCode::SEE_OTHER, "{}", retried.text());
    }

    /// The trigger that makes ending the browser's old session fail.
    const FAIL_SESSION_DELETE: &str = "BEFORE DELETE ON user_sessions FOR EACH STATEMENT";

    #[tokio::test(flavor = "multi_thread")]
    async fn a_password_sign_in_whose_old_session_cannot_end_changes_nothing() {
        let test = login_app().await;
        let alice = user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let pool = test.app.pool();
        let mut browser = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        let old = browser.cookie(COOKIE).expect("a session").to_string();
        fail_on(pool, FAIL_SESSION_DELETE).await;

        let failed = post_sign_in(&mut browser, &test, "alice@example.com", PASSWORD).await;
        let after_failure = sessions_of(pool, &alice.id).await;
        allow(pool, "user_sessions").await;
        let retried = post_sign_in(&mut browser, &test, "alice@example.com", PASSWORD).await;
        let session = browser
            .send(&test, "GET", "/api/v1/session", Payload::Empty)
            .await;

        // All or nothing: no new session without the old one ending.
        assert_eq!(failed.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(failed.set_cookie(COOKIE).is_none());
        assert_eq!(after_failure, 1, "only the old session");
        assert_eq!(retried.status, StatusCode::SEE_OTHER, "{}", retried.text());
        assert_ne!(browser.cookie(COOKIE), Some(old.as_str()));
        assert_eq!(session.json()["user"], "alice");
        assert_eq!(sessions_of(pool, &alice.id).await, 1, "the old one ended");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_setup_whose_old_session_cannot_end_keeps_the_link() {
        let test = login_app().await;
        let alice = user_with_password(&test, "alice", "alice@example.com", None).await;
        user_with_password(&test, "bob", "bob@example.com", Some(PASSWORD)).await;
        let pool = test.app.pool();
        let token = email_tokens::issue(
            pool,
            &alice.id,
            Purpose::Setup,
            Duration::from_secs(3600),
            Utc::now(),
        )
        .await
        .expect("issue a setup token");
        // The browser is still signed in as someone else.
        let mut browser = Browser::signed_in(&test, "bob@example.com", PASSWORD).await;
        fail_on(pool, FAIL_SESSION_DELETE).await;

        let failed = browser
            .send(
                &test,
                "POST",
                "/account/setup",
                new_password_form(&token, PASSWORD),
            )
            .await;
        let link = email_tokens::peek(pool, &token, Purpose::Setup, Utc::now()).await;
        allow(pool, "user_sessions").await;
        let password = passwords::verify(pool, "alice@example.com", PASSWORD, Utc::now())
            .await
            .expect("check the password");
        let retried = browser
            .send(
                &test,
                "POST",
                "/account/setup",
                new_password_form(&token, PASSWORD),
            )
            .await;
        let session = browser
            .send(&test, "GET", "/api/v1/session", Payload::Empty)
            .await;

        assert_eq!(failed.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(failed.set_cookie(COOKIE).is_none());
        assert!(link.is_ok(), "the link was not used");
        assert_eq!(password, SignInOutcome::Failed, "no password was set");
        assert_eq!(retried.status, StatusCode::SEE_OTHER, "{}", retried.text());
        assert_eq!(session.json()["user"], "alice");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_password_sign_in_waiting_on_the_event_table_commits_nothing_until_it_answers() {
        let test = Arc::new(login_app().await);
        let alice = user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let pool = test.app.pool().clone();
        let mut holder = pool.begin().await.expect("begin");
        sqlx::query("LOCK TABLE sign_in_events IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *holder)
            .await
            .expect("lock the table");
        let sign_in = tokio::spawn({
            let test = Arc::clone(&test);
            async move {
                let mut browser = Browser::default();
                let reply = post_sign_in(&mut browser, &test, "alice@example.com", PASSWORD).await;
                (reply, browser)
            }
        });
        wait_for_lock_waiters(&pool, 1).await;
        let while_blocked = sessions_of(&pool, &alice.id).await;

        holder.commit().await.expect("release the table");
        let (reply, mut browser) = sign_in.await.expect("the sign-in task");
        let session = browser
            .send(&test, "GET", "/api/v1/session", Payload::Empty)
            .await;

        assert_eq!(while_blocked, 0, "nothing committed while the event waited");
        assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.text());
        assert!(reply.set_cookie(COOKIE).is_some());
        assert_eq!(session.json()["user"], "alice");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_reset_queues_its_notice_in_its_own_transaction() {
        let test = Arc::new(login_app().await);
        let alice = user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let pool = test.app.pool().clone();
        let token = email_tokens::issue(
            &pool,
            &alice.id,
            Purpose::Reset,
            Duration::from_secs(3600),
            Utc::now(),
        )
        .await
        .expect("issue a reset token");
        let new_password = "amber-quarry-signal-77";
        let mut holder = pool.begin().await.expect("begin");
        sqlx::query("LOCK TABLE apalis.jobs IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *holder)
            .await
            .expect("lock the queue");
        let reset = tokio::spawn({
            let test = Arc::clone(&test);
            let token = token.clone();
            async move {
                Browser::default()
                    .send(
                        &test,
                        "POST",
                        "/account/reset",
                        new_password_form(&token, new_password),
                    )
                    .await
            }
        });
        wait_for_lock_waiters(&pool, 1).await;
        let while_blocked = email_tokens::peek(&pool, &token, Purpose::Reset, Utc::now()).await;

        holder.commit().await.expect("release the queue");
        let reply = reset.await.expect("the reset task");

        assert!(while_blocked.is_ok(), "the link is unused until the commit");
        assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.text());
        assert_eq!(
            queued_mail(&pool).await,
            1,
            "the notice committed with the reset"
        );
        let changed = passwords::verify(&pool, "alice@example.com", new_password, Utc::now())
            .await
            .expect("check the password");
        assert!(matches!(changed, SignInOutcome::Ok(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn signing_in_again_ends_the_previous_session() {
        let test = login_app().await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let mut browser = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        let first = browser.cookie(COOKIE).expect("a session").to_string();

        let again = browser
            .send(
                &test,
                "POST",
                "/signin",
                Payload::Form(vec![
                    ("email", "alice@example.com".to_string()),
                    ("password", PASSWORD.to_string()),
                ]),
            )
            .await;
        let mut old = Browser::with_cookie(COOKIE, &first);
        let old_reply = old
            .send(&test, "GET", "/api/v1/session", Payload::Empty)
            .await;
        let new_reply = browser
            .send(&test, "GET", "/api/v1/session", Payload::Empty)
            .await;

        assert_eq!(again.status, StatusCode::SEE_OTHER);
        assert_ne!(
            browser.cookie(COOKIE),
            Some(first.as_str()),
            "a new id at sign-in"
        );
        assert_eq!(old_reply.status, StatusCode::UNAUTHORIZED);
        assert_eq!(new_reply.status, StatusCode::OK);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_pages_carry_the_console_policy_and_load_only_the_passkey_script() {
        let test = login_app().await;

        for path in ["/signin", "/signin/forgot", "/account/reset?token=nope"] {
            let reply = Browser::default()
                .send(&test, "GET", path, Payload::Empty)
                .await;
            assert_eq!(
                reply.headers["content-security-policy"], CONTENT_SECURITY_POLICY,
                "{path}"
            );
            assert_eq!(reply.headers["referrer-policy"], "no-referrer", "{path}");
            assert_eq!(reply.headers["cache-control"], "no-store", "{path}");
            // Only the sign-in page offers a passkey (the setup page too,
            // with a token: `http::passkeys`'s tests).
            let scripts = usize::from(path == "/signin");
            assert_eq!(reply.text().matches("<script").count(), scripts, "{path}");
            assert_eq!(reply.text().contains(PASSKEY_SCRIPT), path == "/signin");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn every_sign_in_page_names_the_product_and_shows_the_contact() {
        let test = login_app().await;
        for (key, value) in [
            (SettingKey::ProductName, "Acme Notes"),
            (SettingKey::SupportContact, "https://example.com/help"),
        ] {
            settings::set(test.app.pool(), key, value, &AuditActor::cli(), Utc::now())
                .await
                .unwrap();
        }
        test.app.poll().await.unwrap();

        // The sign-in and forgot pages, a link page (an unknown link's
        // message page) and a refused form post's message page.
        let mut browser = Browser::default();
        let mut replies = Vec::new();
        for path in [
            "/signin",
            "/signin/forgot",
            "/account/setup?token=nope",
            "/account/reset?token=nope",
        ] {
            replies.push((path, browser.send(&test, "GET", path, Payload::Empty).await));
        }
        // No password fields: the form is refused before the token is read.
        let refused = browser
            .send(
                &test,
                "POST",
                "/account/reset",
                Payload::Form(vec![("token", "x".to_string())]),
            )
            .await;
        replies.push(("POST /account/reset", refused));

        for (path, reply) in replies {
            let text = reply.text();
            assert!(
                text.contains("<p class=\"brand\">Acme Notes</p>"),
                "{path}: {text}"
            );
            assert!(text.contains(" - Acme Notes</title>"), "{path}: {text}");
            assert!(
                text.contains("Questions? Contact <a href=\"https://example.com/help\">"),
                "{path}: {text}"
            );
            assert!(!text.contains("okf"), "{path}: {text}");
        }
    }

    #[test]
    fn the_sign_in_page_renders_every_option_it_is_given() {
        let view = SignInView {
            password: true,
            passkey: true,
            return_to: "/ui/#/keys".to_string(),
            error: Some(SIGN_IN_FAILED),
            notice: None,
            providers: vec![ProviderButton {
                id: "example".to_string(),
                label: "Example <ID>".to_string(),
            }],
        };

        let html = sign_in_html(&view);

        assert!(html.contains(r#"id="signin-form""#));
        assert!(
            html.contains(r#"id="signin-passkey" class="secondary" data-return-to="/ui/#/keys""#)
        );
        assert!(html.contains(r#"href="/auth/oidc/example/start?return_to=%2Fui%2F%23%2Fkeys""#));
        assert!(html.contains("Sign in with Example &lt;ID&gt;"));
        assert!(html.contains(SIGN_IN_FAILED));
        assert!(html.contains(r#"id="signin-api-key""#));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_sign_in_page_offers_the_code_path_for_an_authorize_return() {
        let test = crate::oauth::testing::oauth_app().await;
        let back: String =
            form_urlencoded::byte_serialize(b"/oauth/authorize?client_id=x").collect();

        let (_, _, oauth_page) =
            crate::oauth::testing::send(&test, "GET", &format!("/signin?return_to={back}"), None)
                .await;
        let (_, _, console_page) =
            crate::oauth::testing::send(&test, "GET", "/signin?return_to=%2Fui%2F", None).await;

        assert!(oauth_page.contains(r#"id="signin-use-code""#));
        assert!(oauth_page.contains(r#"href="/oauth/authorize?client_id=x&amp;signin=code""#));
        assert!(!console_page.contains("signin-use-code"));
    }

    #[test]
    fn only_a_browser_saying_another_site_is_cross_site() {
        let with = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert("sec-fetch-site", value.parse().expect("a header value"));
            headers
        };

        assert!(!cross_site(&HeaderMap::new()));
        assert!(!cross_site(&with("same-origin")));
        assert!(!cross_site(&with("none")));
        assert!(cross_site(&with("same-site")));
        assert!(cross_site(&with("cross-site")));
    }
}
