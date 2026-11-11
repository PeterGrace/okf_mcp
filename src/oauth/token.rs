//! `POST /oauth/token` and `POST /oauth/revoke` (spec "Endpoints").
//!
//! Tokens are opaque and stored as SHA-256 only. One authorization code
//! starts a token family; each refresh rotates the refresh token within the
//! family. Presenting a spent code or a used refresh token revokes the
//! family (RFC 6749 section 4.1.2; OAuth 2.1 refresh token rotation).

use std::sync::Arc;
use std::time::Instant;

use axum::extract::rejection::FormRejection;
use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Form, Json, Router};
use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;

use crate::app::App;
use crate::auth::CredentialKind;
use crate::error::OkfError;
use crate::keys;
use crate::oauth::error::OAuthError;
use crate::oauth::state::{code_key, AuthorizationCode};
use crate::oauth::{pkce, OAuth};
use crate::store::oauth::tokens::{
    self as token_rows, LiveAccessToken, NewToken, RefreshUse, RevokedToken, TokenKind,
};

/// The routes: `POST /oauth/token` and `POST /oauth/revoke`.
///
/// # Returns
/// A router for [`crate::oauth::routes`] to merge (which adds
/// `Cache-Control: no-store` to every answer).
pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/oauth/token", post(token))
        .route("/oauth/revoke", post(revoke))
}

/// A token request's fields (RFC 6749 sections 4.1.3 and 6). All optional,
/// so a missing one is `invalid_request` from the handler, not a 422 from
/// the extractor.
// `Debug` by hand: codes, verifiers and tokens never reach a log.
#[derive(Default, Deserialize)]
struct TokenForm {
    grant_type: Option<String>,
    code: Option<String>,
    redirect_uri: Option<String>,
    client_id: Option<String>,
    code_verifier: Option<String>,
    resource: Option<String>,
    refresh_token: Option<String>,
}

impl std::fmt::Debug for TokenForm {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `finish_non_exhaustive` prints `..` for the fields left out.
        formatter
            .debug_struct("TokenForm")
            .field("grant_type", &self.grant_type)
            .field("client_id", &self.client_id)
            .field("redirect_uri", &self.redirect_uri)
            .field("resource", &self.resource)
            .finish_non_exhaustive()
    }
}

/// A revocation request (RFC 7009 section 2.1). `client_id` and
/// `token_type_hint` may be sent and are ignored: the stored row says the
/// token's kind, and clients are public.
#[derive(Default, Deserialize)]
struct RevokeForm {
    token: Option<String>,
}

/// A successful token response (RFC 6749 section 5.1).
// `Debug` by hand, for the same reason as `TokenForm`.
#[derive(Serialize)]
struct TokenResponse {
    access_token: String,
    token_type: &'static str,
    expires_in: u64,
    refresh_token: String,
}

impl std::fmt::Debug for TokenResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TokenResponse")
            .field("token_type", &self.token_type)
            .field("expires_in", &self.expires_in)
            .finish_non_exhaustive()
    }
}

impl IntoResponse for TokenResponse {
    fn into_response(self) -> Response {
        // RFC 6749 section 5.1: a response carrying tokens must not be
        // cached, by HTTP/1.1 (`Cache-Control`) or HTTP/1.0 (`Pragma`) caches.
        (
            [
                (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
                (header::PRAGMA, HeaderValue::from_static("no-cache")),
            ],
            Json(self),
        )
            .into_response()
    }
}

/// A required, non-empty field.
///
/// # Arguments
/// * `value` - the field as parsed.
/// * `name` - its name, for the error.
///
/// # Returns
/// The text, borrowed from the form (the `'a` says the result lives as long
/// as `value`, not `name`).
///
/// # Errors
/// [`OAuthError::InvalidRequest`] naming the field.
fn required<'a>(value: &'a Option<String>, name: &str) -> Result<&'a str, OAuthError> {
    value
        .as_deref()
        .filter(|text| !text.is_empty())
        .ok_or_else(|| OAuthError::InvalidRequest(format!("{name} is required")))
}

/// A lifetime from the configuration as a `chrono` delta.
///
/// # Errors
/// [`OkfError::Config`] for a lifetime `chrono` cannot add to a time.
fn delta(duration: std::time::Duration) -> Result<TimeDelta, OkfError> {
    TimeDelta::from_std(duration).map_err(|_| OkfError::Config {
        path: "[oauth]".to_string(),
        message: "a token lifetime is too long".to_string(),
    })
}

/// What a pair of tokens is bound to.
#[derive(Debug, Clone, Copy)]
struct Binding<'a> {
    grant_id: &'a str,
    family_id: &'a str,
    resource: &'a str,
}

/// A minted pair, written but not yet committed or cached.
struct Minted {
    response: TokenResponse,
    access_hash: [u8; 32],
    access: LiveAccessToken,
    /// When it was minted, for the cache's pruning.
    now: DateTime<Utc>,
    /// The cache's revocation generation, read before the tokens were
    /// written: a family revocation that lands before `remember` keeps the
    /// token out of the cache.
    generation: u64,
}

/// Mints an access and a refresh token in `conn`'s transaction.
///
/// # Arguments
/// * `conn` - the transaction (the caller commits, then calls [`remember`]).
/// * `oauth` - the lifetimes and the access-token cache.
/// * `binding` - the grant, family and resource the tokens are bound to.
/// * `now` - the server clock.
///
/// # Returns
/// The pair and what caching its access token needs.
///
/// # Errors
/// [`OkfError::Io`] if the random source fails; [`OkfError::Database`];
/// [`OkfError::Config`] for an impossible lifetime.
async fn mint(
    conn: &mut PgConnection,
    oauth: &OAuth,
    binding: Binding<'_>,
    now: DateTime<Utc>,
) -> Result<Minted, OkfError> {
    // Read before any write: see `Minted::generation`.
    let generation = oauth.access.generation();
    let access_token = keys::generate_with(keys::ACCESS_TOKEN_PREFIX)?;
    let refresh_token = keys::generate_with(keys::REFRESH_TOKEN_PREFIX)?;
    let access_expires = now + delta(oauth.settings.access_token_ttl)?;
    let refresh_expires = now + delta(oauth.settings.refresh_token_ttl)?;
    let access_hash = keys::hash(&access_token);
    for (sha256, kind, expires_at) in [
        (access_hash, TokenKind::Access, access_expires),
        (
            keys::hash(&refresh_token),
            TokenKind::Refresh,
            refresh_expires,
        ),
    ] {
        let new = NewToken {
            sha256,
            grant_id: binding.grant_id,
            kind,
            family_id: binding.family_id,
            resource: binding.resource,
            expires_at,
        };
        // `&mut *conn` reborrows the connection for one call, so the loop
        // can use it again (Python has no equivalent: a borrow ends here).
        token_rows::insert(&mut *conn, &new, now).await?;
    }
    Ok(Minted {
        response: TokenResponse {
            access_token,
            token_type: "Bearer",
            expires_in: oauth.settings.access_token_ttl.as_secs(),
            refresh_token,
        },
        access_hash,
        access: LiveAccessToken {
            grant_id: binding.grant_id.to_string(),
            resource: binding.resource.to_string(),
            expires_at: access_expires,
        },
        now,
        generation,
    })
}

/// After commit: caches the new access token and hands back the response.
fn remember(oauth: &OAuth, minted: Minted) -> TokenResponse {
    oauth.access.remember(
        minted.access_hash,
        minted.access,
        minted.now,
        minted.generation,
    );
    minted.response
}

/// Revokes a family in the database, then drops its access tokens from the
/// cache.
///
/// # Errors
/// [`OkfError::Database`].
async fn revoke_family(app: &App, oauth: &OAuth, family_id: &str) -> Result<(), OkfError> {
    let revoked = token_rows::revoke_family(app.pool(), family_id, app.clock().now()).await?;
    oauth.access.forget(&revoked);
    Ok(())
}

/// The grant's client, if the grant is live and has one.
fn grant_client(app: &App, grant_id: &str) -> Option<String> {
    // `?` on an `Option` returns `None` early, as on a `Result`.
    match &app.keys().find_grant(grant_id)?.kind {
        CredentialKind::OAuth { client_id, .. } => client_id.clone(),
        CredentialKind::ApiKey => None,
    }
}

/// `POST /oauth/token`.
async fn token(
    State(app): State<Arc<App>>,
    form: Result<Form<TokenForm>, FormRejection>,
) -> Result<TokenResponse, OAuthError> {
    let oauth = app
        .oauth()
        .ok_or_else(|| OAuthError::InvalidRequest("OAuth is off".to_string()))?;
    if !oauth.token_limit.try_take(Instant::now()) {
        return Err(OAuthError::SlowDown);
    }
    let Form(form) = form.map_err(|_| {
        OAuthError::InvalidRequest("the body must be application/x-www-form-urlencoded".to_string())
    })?;
    match required(&form.grant_type, "grant_type")? {
        "authorization_code" => exchange_code(&app, oauth, &form).await,
        "refresh_token" => refresh(&app, oauth, &form).await,
        other => Err(OAuthError::UnsupportedGrantType(format!(
            "{other:?} is not supported"
        ))),
    }
}

/// `grant_type=authorization_code`.
async fn exchange_code(
    app: &App,
    oauth: &OAuth,
    form: &TokenForm,
) -> Result<TokenResponse, OAuthError> {
    let code = required(&form.code, "code")?;
    let client_id = required(&form.client_id, "client_id")?;
    let redirect_uri = required(&form.redirect_uri, "redirect_uri")?;
    let verifier = required(&form.code_verifier, "code_verifier")?;
    let resource = required(&form.resource, "resource")?;
    let key = code_key(code);
    // The code was live at this instant; the replay check after commit asks
    // the map as of the same instant, so the code expiring meanwhile does not
    // hide a replay.
    let started = Instant::now();
    // Marking the code used under the map's lock makes the exchange
    // single-use even for two racing requests. A used code stays in the map
    // until it expires, so a replay is recognized and revokes its family.
    let taken = oauth.codes.update(&key, started, |stored| {
        if stored.used {
            stored.replayed = true;
            Err(stored.family_id.clone())
        } else {
            stored.used = true;
            Ok(stored.clone())
        }
    });
    let stored: AuthorizationCode = match taken {
        None => {
            return Err(OAuthError::InvalidGrant(
                "unknown or expired code".to_string(),
            ))
        }
        Some(Err(family_id)) => {
            revoke_family(app, oauth, &family_id).await?;
            return Err(OAuthError::InvalidGrant(
                "the code was already used".to_string(),
            ));
        }
        Some(Ok(stored)) => stored,
    };
    // Each refusal below leaves the code spent: a code is good for one
    // attempt, right or wrong.
    if stored.client_id != client_id || stored.redirect_uri != redirect_uri {
        return Err(OAuthError::InvalidGrant(
            "the code was issued to another client or redirect URI".to_string(),
        ));
    }
    if stored.resource != resource {
        return Err(OAuthError::InvalidTarget(
            "resource differs from the authorization request".to_string(),
        ));
    }
    if !pkce::verify(&stored.code_challenge, verifier) {
        return Err(OAuthError::InvalidGrant(
            "code_verifier does not match".to_string(),
        ));
    }
    if app.keys().find_grant(&stored.grant_id).is_none() {
        return Err(OAuthError::InvalidGrant(
            "the grant was revoked".to_string(),
        ));
    }
    let binding = Binding {
        grant_id: &stored.grant_id,
        family_id: &stored.family_id,
        resource: &stored.resource,
    };
    let mut tx = app.pool().begin().await.map_err(OkfError::from)?;
    let minted = mint(&mut tx, oauth, binding, app.clock().now()).await?;
    tx.commit().await.map_err(OkfError::from)?;
    // A replay that arrived while this exchange was minting revoked the
    // family before these tokens existed. The flag is set under the map's
    // lock before that revocation, so either this sees it, or the replay's
    // revocation ran after the commit and caught these tokens. A code gone
    // from the map (pruned once expired) cannot vouch for itself: revoke.
    let replayed = oauth
        .codes
        .update(&key, started, |stored| stored.replayed)
        .unwrap_or(true);
    if replayed {
        revoke_family(app, oauth, &stored.family_id).await?;
        return Err(OAuthError::InvalidGrant(
            "the code was used twice".to_string(),
        ));
    }
    Ok(remember(oauth, minted))
}

/// `grant_type=refresh_token`.
async fn refresh(app: &App, oauth: &OAuth, form: &TokenForm) -> Result<TokenResponse, OAuthError> {
    let presented = required(&form.refresh_token, "refresh_token")?;
    let client_id = required(&form.client_id, "client_id")?;
    let now = app.clock().now();
    let mut tx = app.pool().begin().await.map_err(OkfError::from)?;
    match token_rows::use_refresh(&mut tx, &keys::hash(presented), now).await? {
        RefreshUse::Unknown => Err(OAuthError::InvalidGrant(
            "unknown, expired or revoked refresh token".to_string(),
        )),
        RefreshUse::Reused { family_id } => {
            // Nothing was written; dropping `tx` rolls back and releases the
            // row lock before the revocation updates the row.
            drop(tx);
            revoke_family(app, oauth, &family_id).await?;
            Err(OAuthError::InvalidGrant(
                "the refresh token was already used".to_string(),
            ))
        }
        RefreshUse::Rotated {
            grant_id,
            family_id,
            resource,
        } => {
            // Each refusal returns before `commit`, so dropping `tx` rolls
            // back the `used_at` mark and the rightful client can still use
            // the token.
            if grant_client(app, &grant_id).as_deref() != Some(client_id) {
                return Err(OAuthError::InvalidGrant(
                    "the refresh token belongs to another client".to_string(),
                ));
            }
            if form
                .resource
                .as_deref()
                .is_some_and(|asked| asked != resource)
            {
                return Err(OAuthError::InvalidTarget(
                    "resource differs from the original grant".to_string(),
                ));
            }
            let binding = Binding {
                grant_id: &grant_id,
                family_id: &family_id,
                resource: &resource,
            };
            let minted = mint(&mut tx, oauth, binding, now).await?;
            tx.commit().await.map_err(OkfError::from)?;
            Ok(remember(oauth, minted))
        }
    }
}

/// `POST /oauth/revoke` (RFC 7009): 200 for any token, an unknown one
/// included. A refresh token takes its whole family with it.
async fn revoke(
    State(app): State<Arc<App>>,
    form: Result<Form<RevokeForm>, FormRejection>,
) -> Result<StatusCode, OAuthError> {
    let oauth = app
        .oauth()
        .ok_or_else(|| OAuthError::InvalidRequest("OAuth is off".to_string()))?;
    if !oauth.token_limit.try_take(Instant::now()) {
        return Err(OAuthError::SlowDown);
    }
    let Form(form) = form.map_err(|_| {
        OAuthError::InvalidRequest("the body must be application/x-www-form-urlencoded".to_string())
    })?;
    let token = required(&form.token, "token")?;
    let hash = keys::hash(token);
    match token_rows::revoke(app.pool(), &hash, app.clock().now()).await? {
        // Revoking twice is harmless, so a family already revoked is fine.
        Some(RevokedToken {
            family_id,
            kind: TokenKind::Refresh,
        }) => revoke_family(&app, oauth, &family_id).await?,
        Some(RevokedToken {
            kind: TokenKind::Access,
            ..
        }) => oauth.access.forget(&[hash]),
        None => {}
    }
    Ok(StatusCode::OK)
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use axum::http::{HeaderMap, StatusCode};

    use super::{TokenForm, TokenResponse};
    use crate::mcp::testing::TestApp;
    use crate::oauth::state::code_key;
    use crate::oauth::testing::{
        authorize_query, form, hidden, oauth_app, send, sign_in_code, CLIENT_ID, REDIRECT_URI,
        RESOURCE, VERIFIER,
    };
    use crate::store::keys::Access;

    /// Runs authorize and consent (code, then Approve from the summary);
    /// returns the `okfac_` code.
    async fn authorization_code(test: &TestApp) -> String {
        let code = sign_in_code(test, Access::Read).await;
        let (_, _, page) = send(
            test,
            "GET",
            &format!("/oauth/authorize?{}", authorize_query()),
            None,
        )
        .await;
        let (pending, csrf) = (hidden(&page, "pending"), hidden(&page, "csrf"));
        let (_, _, summary) = send(
            test,
            "POST",
            "/oauth/authorize",
            Some(form(&[
                ("pending", &pending),
                ("csrf", &csrf),
                ("action", "code"),
                ("sign_in_code", &code),
            ])),
        )
        .await;
        let (status, headers, _) = send(
            test,
            "POST",
            "/oauth/authorize",
            Some(form(&[
                ("pending", &pending),
                ("csrf", &csrf),
                ("summary", &hidden(&summary, "summary")),
                ("action", "approve"),
            ])),
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        let target = url::Url::parse(headers["location"].to_str().unwrap()).unwrap();
        target
            .query_pairs()
            .find(|(key, _)| key == "code")
            .map(|(_, value)| value.into_owned())
            .expect("a code")
    }

    fn exchange(code: &str) -> String {
        exchange_with(code, "", "")
    }

    /// An exchange request with the field `name` set to `value` instead
    /// (an empty `name` changes nothing).
    fn exchange_with(code: &str, name: &str, value: &str) -> String {
        let fields = [
            ("grant_type", "authorization_code"),
            ("code", code),
            ("client_id", CLIENT_ID),
            ("redirect_uri", REDIRECT_URI),
            ("code_verifier", VERIFIER),
            ("resource", RESOURCE),
        ];
        let pairs: Vec<(&str, &str)> = fields
            .iter()
            .map(|&(key, original)| (key, if key == name { value } else { original }))
            .collect();
        form(&pairs)
    }

    fn refresh(token: &str, client_id: &str) -> String {
        form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", token),
            ("client_id", client_id),
        ])
    }

    async fn token(test: &TestApp, body: String) -> (StatusCode, HeaderMap, serde_json::Value) {
        let (status, headers, text) = send(test, "POST", "/oauth/token", Some(body)).await;
        (status, headers, serde_json::from_str(&text).expect("JSON"))
    }

    /// Whether `access` authenticates on this server's resource.
    async fn works(test: &TestApp, access: &str) -> bool {
        let oauth = test.app.oauth().expect("OAuth is on");
        oauth
            .access
            .verify(&test.app, &oauth.settings.resource, access)
            .await
            .unwrap()
            .is_some()
    }

    fn text(body: &serde_json::Value, field: &str) -> String {
        body[field].as_str().expect(field).to_string()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_code_exchanges_once_for_a_working_token_pair() {
        let test = oauth_app().await;
        let code = authorization_code(&test).await;

        let (status, headers, body) = token(&test, exchange(&code)).await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(headers["cache-control"], "no-store");
        assert_eq!(headers["pragma"], "no-cache");
        assert_eq!(body["token_type"], "Bearer");
        assert_eq!(body["expires_in"], 3600);
        let access = text(&body, "access_token");
        assert!(access.starts_with("okfat_"));
        assert!(text(&body, "refresh_token").starts_with("okfrt_"));
        let oauth = test.app.oauth().unwrap();
        let principal = oauth
            .access
            .verify(&test.app, &oauth.settings.resource, &access)
            .await
            .unwrap()
            .expect("the access token authenticates as the grant");
        assert_eq!(principal.actor, "example-actor");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_replayed_code_is_refused_and_revokes_what_it_minted() {
        let test = oauth_app().await;
        let code = authorization_code(&test).await;
        let (_, _, first) = token(&test, exchange(&code)).await;
        let access = text(&first, "access_token");
        let refresh_token = text(&first, "refresh_token");
        assert!(works(&test, &access).await);

        let (status, _, body) = token(&test, exchange(&code)).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "invalid_grant");
        assert!(!works(&test, &access).await, "the access token is revoked");
        let (status, _, _) = token(&test, refresh(&refresh_token, CLIENT_ID)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "so is the refresh token");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_exchange_that_a_replay_overtakes_is_revoked() {
        // The race the `replayed` flag closes: the replay is seen while the
        // first exchange is still minting. Flag the stored code as replayed
        // by hand, as a replay arriving then would, and the exchange revokes
        // what it minted instead of handing it out.
        let test = oauth_app().await;
        let code = authorization_code(&test).await;
        let oauth = test.app.oauth().unwrap();
        oauth
            .codes
            .update(&code_key(&code), Instant::now(), |stored| {
                stored.replayed = true;
            })
            .expect("the code is stored");

        let (status, _, body) = token(&test, exchange(&code)).await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"], "invalid_grant");
        let live: i64 =
            sqlx::query_scalar("SELECT count(*) FROM oauth_tokens WHERE revoked_at IS NULL")
                .fetch_one(test.app.pool())
                .await
                .unwrap();
        assert_eq!(live, 0, "the minted pair is revoked");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn each_binding_of_the_code_is_checked() {
        for (name, value, expected) in [
            ("code_verifier", "x".repeat(43), "invalid_grant"),
            ("client_id", "another-client".to_string(), "invalid_grant"),
            (
                "redirect_uri",
                format!("{REDIRECT_URI}/other"),
                "invalid_grant",
            ),
            (
                "resource",
                "https://other.example.com/mcp".to_string(),
                "invalid_target",
            ),
        ] {
            let test = oauth_app().await;
            let code = authorization_code(&test).await;

            let (status, _, json) = token(&test, exchange_with(&code, name, &value)).await;

            assert_eq!(status, StatusCode::BAD_REQUEST, "{name}");
            assert_eq!(json["error"], expected, "{name}");
            // The code is spent by the failed attempt.
            let (status, _, _) = token(&test, exchange(&code)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{name}: single use");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_code_whose_grant_was_revoked_is_refused() {
        let test = oauth_app().await;
        let code = authorization_code(&test).await;
        let grant: String = sqlx::query_scalar("SELECT id FROM api_keys WHERE kind = 'oauth'")
            .fetch_one(test.app.pool())
            .await
            .unwrap();
        crate::store::keys::revoke_by_id(test.app.pool(), &grant, test.app.clock().now())
            .await
            .unwrap();
        test.app.poll().await.unwrap();

        let (status, _, body) = token(&test, exchange(&code)).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "invalid_grant");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refresh_rotates_and_a_reused_refresh_token_revokes_the_family() {
        let test = oauth_app().await;
        let code = authorization_code(&test).await;
        let (_, _, first) = token(&test, exchange(&code)).await;
        let old_refresh = text(&first, "refresh_token");

        let (status, headers, second) = token(&test, refresh(&old_refresh, CLIENT_ID)).await;
        assert_eq!(status, StatusCode::OK, "{second}");
        assert_eq!(headers["pragma"], "no-cache");
        let new_access = text(&second, "access_token");
        let new_refresh = text(&second, "refresh_token");
        assert_ne!(new_refresh, old_refresh);
        // Cached when minted: only `forget` can stop it now.
        assert!(works(&test, &new_access).await);

        let (status, _, reused) = token(&test, refresh(&old_refresh, CLIENT_ID)).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(reused["error"], "invalid_grant");
        // The whole family is gone: the new pair and the first access token.
        assert!(!works(&test, &new_access).await);
        assert!(!works(&test, &text(&first, "access_token")).await);
        let (status, _, _) = token(&test, refresh(&new_refresh, CLIENT_ID)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_refused_refresh_does_not_spend_the_token() {
        let test = oauth_app().await;
        let code = authorization_code(&test).await;
        let (_, _, first) = token(&test, exchange(&code)).await;
        let refresh_token = text(&first, "refresh_token");

        let (status, _, by_another) = token(&test, refresh(&refresh_token, "another-client")).await;
        let other_resource = form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", &refresh_token),
            ("client_id", CLIENT_ID),
            ("resource", "https://other.example.com/mcp"),
        ]);
        let (target_status, _, for_another) = token(&test, other_resource).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(by_another["error"], "invalid_grant");
        assert_eq!(target_status, StatusCode::BAD_REQUEST);
        assert_eq!(for_another["error"], "invalid_target");
        // Rolled back: the rightful client can still use it, once.
        let (status, _, body) = token(&test, refresh(&refresh_token, CLIENT_ID)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(works(&test, &text(&first, "access_token")).await);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bad_requests_get_rfc_6749_errors() {
        let test = oauth_app().await;
        for (body, expected) in [
            (
                form(&[("grant_type", "password")]),
                "unsupported_grant_type",
            ),
            (form(&[("code", "x")]), "invalid_request"),
            (
                form(&[("grant_type", "authorization_code"), ("code", "okfac_x")]),
                "invalid_request",
            ),
            (exchange("okfac_unknown"), "invalid_grant"),
            (refresh("okfrt_unknown", CLIENT_ID), "invalid_grant"),
            (
                form(&[("grant_type", "refresh_token"), ("client_id", CLIENT_ID)]),
                "invalid_request",
            ),
        ] {
            let (status, headers, json) = token(&test, body.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(json["error"], expected, "{body}");
            assert_eq!(headers["cache-control"], "no-store");
        }
        // Not a form at all.
        let (status, _, text) = send(&test, "POST", "/oauth/token", None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revoking_a_refresh_token_revokes_its_family_and_unknown_tokens_are_fine() {
        let test = oauth_app().await;
        let code = authorization_code(&test).await;
        let (_, _, pair) = token(&test, exchange(&code)).await;
        let access = text(&pair, "access_token");
        let refresh_token = text(&pair, "refresh_token");
        assert!(works(&test, &access).await);

        let (unknown, headers, _) = send(
            &test,
            "POST",
            "/oauth/revoke",
            Some(form(&[("token", "okfat_nothing")])),
        )
        .await;
        let (status, _, _) = send(
            &test,
            "POST",
            "/oauth/revoke",
            Some(form(&[
                ("token", &refresh_token),
                ("token_type_hint", "refresh_token"),
                ("client_id", CLIENT_ID),
            ])),
        )
        .await;
        let (again, _, _) = send(
            &test,
            "POST",
            "/oauth/revoke",
            Some(form(&[("token", &refresh_token)])),
        )
        .await;

        assert_eq!(unknown, StatusCode::OK);
        assert_eq!(headers["cache-control"], "no-store");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(again, StatusCode::OK);
        assert!(!works(&test, &access).await, "the family's access token");
        let (status, _, _) = token(&test, refresh(&refresh_token, CLIENT_ID)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revoking_an_access_token_leaves_the_refresh_token() {
        let test = oauth_app().await;
        let code = authorization_code(&test).await;
        let (_, _, pair) = token(&test, exchange(&code)).await;
        let access = text(&pair, "access_token");
        assert!(works(&test, &access).await);

        let (status, _, _) = send(
            &test,
            "POST",
            "/oauth/revoke",
            Some(form(&[("token", &access)])),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(!works(&test, &access).await);
        let (status, _, _) = token(&test, refresh(&text(&pair, "refresh_token"), CLIENT_ID)).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revoke_without_a_token_is_invalid_request() {
        let test = oauth_app().await;

        let (status, _, body) = send(&test, "POST", "/oauth/revoke", Some(String::new())).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("invalid_request"), "{body}");
    }

    #[test]
    fn debug_never_prints_codes_verifiers_or_tokens() {
        let secret = "okfac_do-not-print";
        let request = TokenForm {
            grant_type: Some("authorization_code".into()),
            code: Some(secret.into()),
            code_verifier: Some(secret.into()),
            refresh_token: Some(secret.into()),
            ..TokenForm::default()
        };
        let response = TokenResponse {
            access_token: secret.into(),
            token_type: "Bearer",
            expires_in: 3600,
            refresh_token: secret.into(),
        };

        let printed = format!("{request:?} {response:?}");

        assert!(!printed.contains(secret), "{printed}");
        assert!(printed.contains("authorization_code"));
    }
}
