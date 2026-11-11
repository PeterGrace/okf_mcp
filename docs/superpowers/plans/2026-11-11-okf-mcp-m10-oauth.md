# OKF MCP Server, Milestone 10 (OAuth 2.1 and directory readiness): Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. In this project each task is one GitHub sub-issue and one PR (`github-issue-to-pr`).

**Goal:** OAuth-only MCP clients (ChatGPT connectors, the Claude and ChatGPT connector directories) can connect to `okf-mcp` through a built-in OAuth 2.1 authorization server, while static `okf_` API keys keep working unchanged. Every tool also carries a title and hints for the connector directories.

**Architecture:**
- **Resource server and authorization server in one binary.** The `/mcp` and `/api/v1` bearer middleware dispatches by token prefix: `okf_` to the existing key cache, `okfat_` to OAuth access tokens. Both go through the `BearerVerifier` trait, the seam for a later external issuer.
- **A grant is a key row.** Redeeming a sign-in code creates an `api_keys` row with `kind = 'oauth'`. Everything downstream (`Principal`, reach, access, session binding, changeset ownership, revocation, the console list) is unchanged.
- **Opaque tokens, stored hashed.** Access tokens (1 hour) and rotating refresh tokens (30 days) live in `oauth_tokens`, keyed by SHA-256. Pending authorizations and authorization codes live in memory: the server is single-replica.
- **Clients by CIMD or DCR.** An `https://` `client_id` is fetched as a Client ID Metadata Document behind SSRF hardening. Otherwise it must be a client registered through RFC 7591.
- **Consent is server-rendered HTML** with Pico CSS and a plain form, gated by an admin-issued single-use sign-in code.

**Tech Stack:** Rust 2021 (toolchain 1.99.1), axum 0.8.9, rmcp 3.8.0, sqlx 0.9.0 (Postgres), reqwest 0.13.5 (rustls), everything M1 to M9 use. One new dependency, `url` (Task 3: CIMD and redirect URI parsing; `reqwest` already depends on it). No JavaScript on the consent page. The console panel (Task 7) uses the existing vanilla-JS console.

**Spec:** `docs/superpowers/specs/2026-11-11-okf-oauth-design.md`.

---

## Roadmap position

Milestone 10. `main` is at `3262209` (v0.12.1 plus the AGPL license and the move to the hypotheosis organization; no `src/` changes) plus the spec commit `c24fef1`. M10 adds OAuth beside the static keys and annotates the MCP tools. It changes no tool's parameters or results.

## Scope

| Task | Spec delivery step |
|---|---|
| 1 | Tool annotations and their coverage test |
| 2 | Foundation: migration, configuration, `BearerVerifier`, OAuth access-token lookup, `Principal.kind`, the `resource_metadata` 401 header, the `src/oauth/` skeleton (shared state, errors, rate limits), cleanup in the poll loop |
| 3 | Well-known documents, DCR, the CIMD fetcher |
| 4 | Sign-in codes: store, CLI (`okf-mcp oauth ...`), JSON API |
| 5 | Authorize and the consent page |
| 6 | Token, refresh and revoke; the end-to-end OAuth tests |
| 7 | The console's sign-in codes panel and grant display |
| 8 | Operations documentation, spec wrap-up, release v0.13.0 preparation |

Deliberately deferred (spec "Non-goals"):

| Item | Where it goes |
|---|---|
| External identity provider (JWT and JWKS) | A later round: a third `BearerVerifier` implementation. |
| Confidential clients, `private_key_jwt` | Not planned. |
| Admin or maintenance-reporting OAuth grants | Not planned; the database refuses them. |
| Narrowing a grant on the consent page | Not planned. |
| Multiple replicas | Not planned (in-memory pending authorizations and codes). |

## Decisions taken while planning (2026-11-11)

1. **One task per spec delivery step,** plus a release-preparation task. The spec's step 2 grows into a foundation task (like M9 Task 1). It creates every module and store file the later tasks fill, so parallel tasks never edit a shared module list.
2. **`family_id` is `text`, not `uuid`.** The project has no `uuid` crate. Families use `crate::maintenance::types::new_id()` (32 random hex characters), as maintenance ids do.
3. **Authorization codes carry a prefix too:** `okfac_`. All five secret kinds come from one generator, `keys::generate_with(prefix)`, whose 32 random bytes follow the prefix as unpadded base64url.
4. **`require_key` takes `State<Arc<App>>`,** not `State<Arc<Keys>>`. It needs the OAuth access-token lookup and the challenge header as well as the key cache. Its name stays, so the existing routers and tests change only the state they pass.
5. **The `BearerVerifier` trait uses return-position `impl Future`** (stable since Rust 1.75), not `async-trait`. Dispatch is a `match` on the prefix, with no `dyn`.
6. **The OAuth routes are mounted only when OAuth is on** (`public_url` set). With it off, `/.well-known/*` and `/oauth/*` are 404, exactly as today.
7. **`public_url`'s host must be in `allowed_hosts`.** Start-up refuses a configuration where it is not, because the MCP DNS-rebinding check would otherwise refuse every OAuth client's request.
8. **Rate limits are three in-memory token buckets** (`src/oauth/ratelimit.rs`): register, token and consent POST. They are global, not per client address: behind an ingress the address is the proxy's. No crate is added.
9. **The consent page escapes through one helper,** `oauth::consent::escape`. There is no template engine. The page's text comes from client metadata, which is attacker-controlled.
10. **Outward steps** (tagging, publishing the image, rollout) wait for the maintainer's go-ahead, as in M7 to M9. Task 8 only prepares the release.

## Simplifications and deviations relative to the design

Filled in as tasks land, as in M9 (dated `> **Corrected (issue #N).**` notes in place, and a list here).

1. `family_id` is text (Decision 2). The spec's DDL says `uuid`; Task 8 updates the spec.
2. The spec's in-memory "authorization codes, keyed by SHA-256" are keyed by the `okfac_` code's SHA-256 (Decision 3).
3. `revert` is a staged edit in the tool annotations (Task 1): it adds an earlier revision to the changeset for review and publish. The spec's table listed it as destructive. The classes are 17 reads, 20 staged edits, 4 destructive.
4. **Grant names** are `<code name>.<client host>-<first 8 characters of the code id>` (Task 4), not `<code name>@<client host>`: `@` fails `api_keys_name_check`. Migration 0008 also limits the live name/actor unique index to `kind = 'key'` (Task 2).
5. **The CLI** follows `key create`: `okf-mcp oauth code create NAME --actor A --access read|write (--catalogs a,b | --all) [--default C] [--expires 24h]`, with the name positional and `--catalogs` comma-separated (Task 4). The clap variant is `Oauth`, so the subcommand is `oauth`.
6. **`oauth_clients.cache_expires_at`** holds a CIMD document's freshness (Task 3); the spec's DDL lacks it.
7. **The consent page** follows the system light/dark theme with no toggle (it has no JavaScript), and its CSP allows the console's Google Fonts hosts (Task 5).
8. **Bounded in-memory maps:** `ExpiringMap` has a capacity; a full pending map answers the authorize GET with 503 (Tasks 2 and 5).
9. **Rate limits** (Task 2): register, a burst of 10 then one every 6 seconds; token and revoke, 60 then 2 a second; consent POST, 20 then 1 a second.
10. **`sign_in_codes_redeemed_grant`** (Task 2, corrected issue #246, 2026-11-11) is `grant_id IS NULL OR redeemed_at IS NOT NULL`, a check the spec's DDL lacks; Task 8 adds it to the spec.
11. **`okf-mcp key revoke|grant|ungrant|maintenance-report` resolve API keys only** (Task 4, issue #248, 2026-11-11): `store::keys::resolve_active` filters `kind = 'key'` by id and by name, so a key name never matches, collides with or is made ambiguous by an OAuth grant. Grants are reached by exact id only: `okf-mcp oauth grant revoke` and the `/api/v1/keys/{id}` routes (`Selector::Exact`). Task 8 adds this to the spec.
12. **The discovery documents are `Cache-Control: no-store`** (Task 3, corrected issue #247, 2026-11-11), like every OAuth route, not `public, max-age=300`: Task 2's layer sets it on everything under `oauth::routes()`. They still carry `Access-Control-Allow-Origin: *`.

## Verified facts the code relies on

`REG` stands for `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/`.

**rmcp 3.8.0**
- `#[tool(...)]` takes `name`, `title`, `description`, `input_schema` and `annotations` (REG/rmcp-macros-3.8.0/src/tool.rs:63-90). The annotations are emitted as `.with_annotations(<expr>)` (tool.rs:118-119).
- `ToolAnnotations { title, read_only_hint, destructive_hint, idempotent_hint, open_world_hint }`, each `Option` (REG/rmcp-3.8.0/src/model/tool.rs:54-90).
- `ToolRouter::list_all() -> Vec<Tool>` (REG/rmcp-3.8.0/src/handler/server/router/tool.rs:581) lists every registered tool, for the coverage test.

**This repository** (at `c24fef1`)
- `src/auth.rs`:
  - `Principal { key_id, access, maintenance_report, reach, default_catalog, actor }` derives `Debug, Clone, PartialEq, Eq`;
  - `impl From<KeyRecord> for Principal`;
  - `KeyCache { by_hash: HashMap<[u8; 32], Arc<Principal>> }` is built by `from_records(Vec<KeyRecord>)`;
  - `Keys { cache: RwLock<Arc<KeyCache>> }` has `load`, `reload` and `find`;
  - `bearer(&HeaderMap) -> Option<&str>`;
  - `require_key(State<Arc<Keys>>, Request, Next) -> Response` answers 401 with `WWW-Authenticate: Bearer` and body `missing or unknown API key\n` (`src/auth.rs:266`).
- `require_key` is wired in two places, `src/http/mod.rs:217` and `src/http/api/mod.rs:52-55`, both with `Arc::clone(app.keys())`.
- `src/store/keys.rs`:
  - `KeyRecord { id, name, sha256: Vec<u8>, access, maintenance_report, all_catalogs, catalogs, default_catalog, actor, created_at, revoked_at }`;
  - `list(pool)` selects through the private `KeyRow` `FromRow` struct;
  - `create_with_id(pool, &NewKey, now) -> CreatedKey { id, key }` checks the name, grants and default, inserts the row and its `key_catalogs`, and bumps the admin generation in one transaction;
  - `catalog_ids(&mut PgConnection, &[String])` and `bump_admin_generation(&mut tx)` are private helpers.
- `src/http/api/keys.rs:57-88`: `impl From<KeyRecord> for KeyView` destructures every field, so adding a field to `KeyRecord` fails to compile there until `KeyView` handles it.
- `src/app.rs`:
  - `App::load(pool, config, clock)` builds `Keys`, `SessionKeys` and `Overlays`;
  - `App::poll()` runs `reload_keys_if_changed()` then `prune_empty_changesets()` under the `polling` mutex, each logging and swallowing its own failure;
  - accessors are `pool()`, `config()`, `clock()`, `keys()`, `sessions()`.
- `src/config.rs`: `Config` is `#[serde(default)]` without `deny_unknown_fields`. Durations use `#[serde(with = "humantime_serde")]`. `allowed_hosts: Vec<String>` is the MCP `Host` check.
- `src/keys.rs`: `generate()` (`KEY_PREFIX` plus 32 random bytes in base64url), `hash(&str) -> [u8; 32]`, `matches(&[u8], &str)`.
- `crate::maintenance::types::new_id() -> Result<String, OkfError>`: 32 random hex characters.
- `src/http/sessions.rs` binds an MCP session to `principal.key_id`, which for a grant is the grant's id.
- The migrations run in order from `migrations/`, the last being `0007_maintenance_assessment_evidence.sql`.
- Test support:
  - `src/mcp/testing.rs`: `app_with(&[(catalog, files)], Config) -> TestApp`, `principal_as(...)`;
  - `src/http/api/testing.rs`: `app()`, `keys(&TestApp)`, `send(...)`, `assert_matrix(...)`;
  - `tests/support/server.rs`: `TestServer::start(Vec<(&str, Files)>, Config)`, `.base`, `.key(id, Access, catalogs, default)`;
  - `tests/ui_e2e/harness.rs`: `drive(base, journey)` and `Ui`.

## Conventions for every task

The M4 to M9 plans' "Conventions for every task" apply unchanged:
- no `.unwrap()` in library code, and `thiserror` errors;
- a doc comment on every public and `pub(crate)` item, with Arguments, Returns and Errors where they apply;
- comments that explain Rust-specific points to a Python expert;
- no emoji, and no unicode that imitates emoji, in code, UI text or docs;
- never a secret-shaped string in code, fixtures or docs (test tokens are generated at run time);
- outward steps wait for the maintainer's go-ahead.

**Nothing site-specific in this repository** (it is public). Docs, fixtures, tests and changelogs say `example.com` (or `okf.example.com`, `client.example.com`) and use synthetic catalog names (`main`, `other`) and actors (`example-actor`). They never name a real host, catalog, person or private repository.

**Secrets never reach a log.** No `tracing` call takes a token, code, sign-in code, CSRF token or `code_verifier`. Any type holding one implements `Debug` by hand and prints `<redacted>` for that field.

**OAuth error shape.** Every `/oauth/*` JSON error is `OAuthError` (Task 2), serialized as `{"error": "<code>", "error_description": "<text>"}`. Every `/oauth/*` response carries `Cache-Control: no-store`.

**Gates** (all must pass before every push; paste the counts into the PR):
- `cargo fmt --all --check`
- `cargo clippy --all-targets -- -D warnings`
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
- `cargo test -- --test-threads=6` (needs Docker for testcontainers)
- `just test-ui` with Chromium available, when a task touches `src/http/ui/` or the consent page (Tasks 5 and 7). Their journeys must run, not skip.

**Changelog:** one entry per task, `changelog/<date -u +%Y-%m-%dT%H%M%SZ>-m10-task-<NN>-<slug>.md`, with Added/Changed/Notes sections.

Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. PR bodies end with the Claude Code attribution line.

The interface block below already includes the corrections the task drafts agreed on while this plan was written (2026-11-11). The code in this plan has not been compiled. If an implementation has to differ, fix this plan in the same PR (inline, or with a dated `> **Corrected (issue #N).**` note), then grep later tasks for the changed symbol.

## Interfaces shared across tasks

Task 2 defines all of these except where marked; later tasks fill in bodies only.

```rust
// src/keys.rs (Task 2)
pub const KEY_PREFIX: &str = "okf_";                 // unchanged
pub const ACCESS_TOKEN_PREFIX: &str = "okfat_";
pub const REFRESH_TOKEN_PREFIX: &str = "okfrt_";
pub const SIGN_IN_CODE_PREFIX: &str = "okfsc_";
pub const AUTHORIZATION_CODE_PREFIX: &str = "okfac_";
pub fn generate_with(prefix: &str) -> Result<String, OkfError>;   // prefix + 43 base64url chars
pub fn generate() -> Result<String, OkfError>;                    // generate_with(KEY_PREFIX)

// src/config.rs (Task 2)
pub struct Config { /* existing fields */ pub public_url: Option<String>, pub oauth: OAuthConfig }
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)] #[serde(default)]
pub struct OAuthConfig {
    #[serde(with = "humantime_serde")] pub access_token_ttl: Duration,   // 1h
    #[serde(with = "humantime_serde")] pub refresh_token_ttl: Duration,  // 30d
    #[serde(with = "humantime_serde")] pub code_max_expiry: Duration,    // 7d
    pub dcr_enabled: bool,                                               // true
    pub cimd_allow_private: bool,                                        // false
}
impl Config { pub fn validate(&self) -> Result<(), String>; }     // existing fn, extended: public_url https (or loopback http), host in allowed_hosts

// src/store/keys.rs (Task 2)
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum KeyKind { Key, OAuth }
impl KeyKind { pub fn as_str(self) -> &'static str; pub fn parse(text: &str) -> Option<Self>; }
pub struct KeyRecord { /* existing */ pub sha256: Option<Vec<u8>>, pub kind: KeyKind,
                       pub client_id: Option<String>, pub client_name: Option<String> }
pub(crate) async fn catalog_ids(conn: &mut PgConnection, names: &[String]) -> Result<Vec<i64>, OkfError>; // was private
// `bump_admin_generation` and `is_unique_violation` already exist, pub(crate), in `store::catalogs`.
// Task 4 makes `is_valid_name` pub(crate) and adds a shared `resolve_grants` helper here.

// src/store/oauth/mod.rs (Task 2): `pub mod clients; pub mod codes; pub mod tokens; #[cfg(test)] pub(crate) mod testing;`
// src/store/oauth/testing.rs (Task 2, test-only): insert_client, insert_grant, oauth_config(public_url), mint_access
// src/store/oauth/tokens.rs (Task 2: insert, find_access, revoke, prune; Task 6: use_refresh, revoke_family)
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum TokenKind { Access, Refresh }
pub struct NewToken<'a> { pub sha256: [u8; 32], pub grant_id: &'a str, pub kind: TokenKind,
                          pub family_id: &'a str, pub resource: &'a str, pub expires_at: DateTime<Utc> }
pub async fn insert(conn: &mut PgConnection, new: &NewToken<'_>, now: DateTime<Utc>) -> Result<(), OkfError>;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveAccessToken { pub grant_id: String, pub resource: String, pub expires_at: DateTime<Utc> }
pub async fn find_access(pool: &PgPool, sha256: &[u8; 32], now: DateTime<Utc>) -> Result<Option<LiveAccessToken>, OkfError>;
pub struct RevokedToken { pub family_id: String, pub kind: TokenKind }
pub async fn revoke(pool: &PgPool, sha256: &[u8; 32], now: DateTime<Utc>) -> Result<Option<RevokedToken>, OkfError>; // idempotent
pub async fn prune(pool: &PgPool, expired_before: DateTime<Utc>) -> Result<u64, OkfError>;
// Task 6:
pub enum RefreshUse { Rotated { grant_id: String, family_id: String, resource: String }, Reused { family_id: String }, Unknown }
pub async fn use_refresh(tx: &mut PgConnection, sha256: &[u8; 32], now: DateTime<Utc>) -> Result<RefreshUse, OkfError>;
pub async fn revoke_family(pool: &PgPool, family_id: &str, now: DateTime<Utc>) -> Result<Vec<[u8; 32]>, OkfError>; // revoked hashes

// src/store/oauth/clients.rs (Task 2: types, client_columns!/ClientRow, `find`, `prune_unbound_dcr`; Task 3: the rest)
// Corrected (issue #246, 2026-11-11): the select list is a private `client_columns!()` macro, not a
// `CLIENT_COLUMNS` const; sqlx 0.9 refuses a `format!`ed `String` as SQL, so queries use `concat!`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum ClientKind { Dcr, Cimd }
#[derive(Debug, Clone, PartialEq)]
pub struct ClientRecord { pub client_id: String, pub kind: ClientKind, pub client_name: Option<String>,
                          pub redirect_uris: Vec<String>, pub metadata: serde_json::Value,
                          pub fetched_at: Option<DateTime<Utc>>, pub cache_expires_at: Option<DateTime<Utc>>,
                          pub created_at: DateTime<Utc> }
pub async fn find(pool: &PgPool, client_id: &str) -> Result<Option<ClientRecord>, OkfError>;
pub async fn prune_unbound_dcr(pool: &PgPool, created_before: DateTime<Utc>) -> Result<u64, OkfError>;
// Task 3:
pub struct NewDcrClient<'a> { /* client_name, redirect_uris, metadata */ }
pub async fn insert_dcr(pool: &PgPool, new: &NewDcrClient<'_>, now: DateTime<Utc>, max_unbound: i64) -> Result<Option<ClientRecord>, OkfError>; // id: gen_random_uuid()::text; None at the cap
// Corrected (issue #247, 2026-11-11, PR #257 review): the cap check and the insert are one statement under a
// transaction-scoped advisory lock, so concurrent registrations cannot pass the cap.
pub async fn upsert_cimd(pool: &PgPool, client: &ClientRecord) -> Result<(), OkfError>;
pub async fn count_unbound_dcr(pool: &PgPool) -> Result<i64, OkfError>;

// src/store/oauth/codes.rs (Task 2: `prune` only; Task 4: the rest)
pub struct NewSignInCode<'a> { pub name: &'a str, pub access: Access, pub grants: Grants<'a>,
                               pub default_catalog: Option<&'a str>, pub actor: &'a str, pub expires_at: DateTime<Utc> }
pub struct CreatedSignInCode { pub id: String, pub code: String }        // Debug by hand: code redacted
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum CodeState { Live, Expired, Redeemed, Revoked }  // + as_str()
pub const DEFAULT_EXPIRY: Duration;  // 24 hours (Task 4); also `expiry(now, requested, max)`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInCodeRecord { pub id: String, pub name: String, pub access: Access, pub all_catalogs: bool,
                              pub catalogs: Vec<String>, pub default_catalog: Option<String>, pub actor: String,
                              pub created_at: DateTime<Utc>, pub expires_at: DateTime<Utc>,
                              pub redeemed_at: Option<DateTime<Utc>>, pub grant_id: Option<String>,
                              pub revoked_at: Option<DateTime<Utc>> }
impl SignInCodeRecord { pub fn state(&self, now: DateTime<Utc>) -> CodeState; }
pub async fn create(pool: &PgPool, new: &NewSignInCode<'_>, now: DateTime<Utc>) -> Result<CreatedSignInCode, OkfError>;
pub async fn list(pool: &PgPool) -> Result<Vec<SignInCodeRecord>, OkfError>;
pub async fn find_live(pool: &PgPool, presented: &str, now: DateTime<Utc>) -> Result<Option<SignInCodeRecord>, OkfError>;
pub async fn revoke(pool: &PgPool, id: &str, now: DateTime<Utc>) -> Result<(), OkfError>;
pub async fn redeem(pool: &PgPool, code_id: &str, client: &ClientRecord, now: DateTime<Utc>) -> Result<String, OkfError>; // the grant id
pub async fn prune(pool: &PgPool, expired_before: DateTime<Utc>) -> Result<u64, OkfError>;

// src/auth.rs (Task 2)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialKind { ApiKey, OAuth { client_id: Option<String>, client_name: Option<String> } }
pub struct Principal { /* existing */ pub kind: CredentialKind }
pub trait BearerVerifier {
    fn verify(&self, token: &str) -> impl Future<Output = Result<Option<Arc<Principal>>, OkfError>> + Send;
}
impl BearerVerifier for Keys { /* `okf_` keys, from the cache */ }
impl Keys { pub fn find_grant(&self, grant_id: &str) -> Option<Arc<Principal>>; }
pub fn challenge(app: &App) -> HeaderValue;       // `Bearer` or `Bearer resource_metadata="..."`
pub async fn require_key(State(app): State<Arc<App>>, request: Request, next: Next) -> Response;

// src/oauth/mod.rs (Task 2)
pub mod access; pub mod authorize; pub mod clients; pub mod consent; pub mod error;
pub mod expiring; pub mod metadata; pub mod pkce; pub mod ratelimit; pub mod state; pub mod token;
// Task 5 adds `#[cfg(test)] pub(crate) mod testing;` with src/oauth/testing.rs.
pub const PROTECTED_RESOURCE_PATH: &str = "/.well-known/oauth-protected-resource";
pub const AUTHORIZATION_SERVER_PATH: &str = "/.well-known/oauth-authorization-server";
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthSettings { pub issuer: String, pub resource: String, pub resource_metadata_url: String,
                           pub access_token_ttl: Duration, pub refresh_token_ttl: Duration,
                           pub code_max_expiry: Duration, pub dcr_enabled: bool, pub cimd_allow_private: bool }
pub struct OAuth {
    pub settings: OAuthSettings,
    pub challenge: HeaderValue,                               // the precomputed 401 challenge
    pub access: access::AccessTokens,                         // Task 2
    pub pending: expiring::ExpiringMap<state::PendingAuthorization>,  // keyed by pending id
    pub codes: expiring::ExpiringMap<state::AuthorizationCode>,       // keyed by hex SHA-256 of the okfac_ code
    pub cimd: clients::CimdFetcher,   // Task 2 stub `new(allow_private, allow_http) -> Result<Self, OkfError>`; Task 3 fills.
                                      // allow_http = cimd_allow_private && issuer starts with "http://"
    pub register_limit: ratelimit::TokenBucket,
    pub token_limit: ratelimit::TokenBucket,
    pub consent_limit: ratelimit::TokenBucket,
}
impl OAuth { pub fn new(config: &Config) -> Result<Option<Self>, OkfError>; }   // None without public_url
pub fn routes() -> Router<Arc<App>>;   // merges metadata::routes(), clients::routes(), authorize::routes(), token::routes()

// src/app.rs (Task 2)
impl App { pub fn oauth(&self) -> Option<&OAuth>; }   // poll() also prunes OAuth state

// src/oauth/error.rs (Task 2)
#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    InvalidRequest(String), InvalidClient(String), InvalidGrant(String), UnauthorizedClient(String),
    UnsupportedGrantType(String), UnsupportedResponseType(String), InvalidTarget(String),
    InvalidClientMetadata(String), InvalidRedirectUri(String), AccessDenied(String), SlowDown,
    Server(#[from] OkfError),
}
impl OAuthError { pub fn code(&self) -> &'static str; pub fn status(&self) -> StatusCode; }
impl IntoResponse for OAuthError { /* JSON {error, error_description}, no-store; Server logs and hides detail */ }

// src/oauth/expiring.rs (Task 2)
pub struct ExpiringMap<V> { /* Mutex<HashMap<String, (Instant, V)>>, ttl */ }
impl<V> ExpiringMap<V> {
    pub fn new(ttl: Duration, capacity: usize) -> Self;
    pub fn insert(&self, key: String, value: V, now: Instant) -> bool;    // false (nothing stored) when full of live entries
    pub fn take(&self, key: &str, now: Instant) -> Option<V>;             // removes; None if expired
    pub fn update<R>(&self, key: &str, now: Instant, f: impl FnOnce(&mut V) -> R) -> Option<R>;
    pub fn remove(&self, key: &str);
}

// src/oauth/ratelimit.rs (Task 2)
pub struct TokenBucket { /* Mutex<(f64 tokens, Instant)>, capacity, refill_per_sec */ }
impl TokenBucket { pub fn new(capacity: u32, refill_per_sec: f64) -> Self; pub fn try_take(&self, now: Instant) -> bool; }

// src/oauth/state.rs (Task 2)
pub struct PendingAuthorization { pub csrf: String, pub client: ClientRecord, pub redirect_uri: String,
    pub state: Option<String>, pub code_challenge: String, pub resource: String,
    pub sign_in_code_id: Option<String>, pub failed_attempts: u8 }           // Clone; Debug by hand: csrf redacted
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationCode { pub grant_id: String, pub client_id: String, pub redirect_uri: String,
    pub code_challenge: String, pub resource: String, pub family_id: String,
    pub used: bool }                        // a used code stays until expiry, so a replay is detected
pub fn code_key(code: &str) -> String;   // hex SHA-256 of an okfac_ code: the key of OAuth.codes
                                          // Corrected (issue #246, 2026-11-11): `pub`, so the stub crate has no dead-code warning before Task 5 uses it
pub const PENDING_TTL: Duration;            // 10 minutes
pub const AUTHORIZATION_CODE_TTL: Duration; // 60 seconds
pub const MAX_FAILED_ATTEMPTS: u8;          // 5

// src/oauth/access.rs (Task 2)
pub struct AccessTokens { /* cache: Mutex<Cache { tokens: HashMap<[u8; 32], LiveAccessToken>, generation: u64 }> */ }
impl AccessTokens {
    pub fn generation(&self) -> u64;                                      // read BEFORE a lookup or mint
    pub fn remember(&self, sha256: [u8; 32], token: LiveAccessToken, now: DateTime<Utc>,
                    seen_generation: u64);                                // caches only if no `forget` since
    pub fn forget(&self, hashes: &[[u8; 32]]);                            // Task 6 calls on revoke; bumps the generation
    pub async fn verify(&self, app: &App, resource: &str, token: &str) -> Result<Option<Arc<Principal>>, OkfError>;
}
// The trait's second implementation borrows what the lookup needs:
pub struct OAuthVerifier<'a> { pub app: &'a App, pub oauth: &'a OAuth }
impl BearerVerifier for OAuthVerifier<'_> { /* AccessTokens::verify */ }

// src/oauth/pkce.rs (Task 5)
pub fn is_valid_challenge(challenge: &str) -> bool;                       // 43 base64url chars
pub fn verify(challenge: &str, verifier: &str) -> bool;                    // S256, constant-time; verifier 43-128 unreserved chars

// src/oauth/clients.rs (Task 3)
pub struct CimdFetcher { /* reqwest client, in-flight set */ }
pub async fn resolve(app: &App, oauth: &OAuth, client_id: &str) -> Result<ClientRecord, OAuthError>;  // CIMD (https, or http when allow_http) or DCR
pub fn validate_redirect_uri(uri: &str) -> Result<(), OAuthError>;        // https, or http on loopback; no fragment
pub fn routes() -> Router<Arc<App>>;                                      // POST /oauth/register

// src/oauth/consent.rs (Task 5)
pub fn escape(text: &str) -> String;
// src/oauth/authorize.rs (Task 5)
pub fn routes() -> Router<Arc<App>>;                                      // GET and POST /oauth/authorize
// src/oauth/token.rs (Task 6)
pub fn routes() -> Router<Arc<App>>;                                      // POST /oauth/token, POST /oauth/revoke
// src/oauth/metadata.rs (Task 3)
pub fn routes() -> Router<Arc<App>>;                                      // both /.well-known documents
```

```rust
// tests/support/server.rs (Task 6): TestServer::start_with(catalogs, impl FnOnce(&str /* base */) -> Config)
// tests/support/oauth.rs (Task 6), for the end-to-end tests
pub struct OAuthClient { /* reqwest client, base, client_id, redirect_uri, verifier */ }
pub async fn start_oauth_server(catalogs: Vec<(&str, Files)>) -> (TestServer, CimdHost);
// CimdHost: a local axum server serving a client metadata document at /client.json
```

## File map

| File | Tasks |
|---|---|
| `Cargo.toml`, `Cargo.lock` | 3 (`url`), 8 (version 0.13.0) |
| `src/mcp/*.rs`, `src/mcp/maintenance/*.rs` (`#[tool(annotations(...))]`), `src/mcp/annotations.rs` (new, coverage test), `src/investigator/contract_tools.json` (recaptured) | 1 |
| `migrations/0008_oauth.sql` | 2 |
| `src/keys.rs`, `src/config.rs`, `src/auth.rs`, `src/app.rs`, `src/lib.rs` (`pub mod oauth;`) | 2 |
| `src/store/keys.rs` | 2, 4 (`is_valid_name`, `resolve_grants`, refusing maintenance reporting on a grant, `resolve_active` limited to `kind = 'key'`) |
| `src/store/mod.rs` (`pub mod oauth;`) | 2 |
| `src/store/oauth/mod.rs`, `testing.rs` (test helpers) | 2 |
| `src/store/oauth/tokens.rs` | 2, then 6 |
| `src/store/oauth/clients.rs` | 2, then 3 |
| `src/store/oauth/codes.rs` | 2, then 4 |
| `src/oauth/mod.rs`, `error.rs`, `expiring.rs`, `ratelimit.rs`, `state.rs`, `access.rs` | 2 |
| `src/oauth/metadata.rs`, `clients.rs` | 2 (stubs), 3 |
| `tests/cimd_no_proxy.rs` (new: the fetcher ignores proxy variables; a binary of its own because it sets the environment) | 3 |
| `src/oauth/pkce.rs`, `authorize.rs`, `consent.rs` | 2 (stubs), 5 |
| `src/oauth/testing.rs` (test helpers), `src/http/ui/consent.css` | 5 |
| `src/oauth/token.rs` | 2 (stub), 6 |
| `src/http/mod.rs` (`require_key` state, OAuth routes mounted) | 2 |
| `src/http/api/mod.rs` (`require_key` state; `oauth` area merged) | 2, 4 |
| `src/http/api/keys.rs` (`KeyView.kind`, `client_name`) | 2, 4 (shared `manages_grants`, `requested_grants`, `parse_access`) |
| `src/http/api/identity.rs` (`WhoAmI.kind`) | 2 |
| `src/http/api/oauth.rs` (new: sign-in code endpoints) | 4 |
| `src/cli.rs` (`okf-mcp oauth ...`) | 4 |
| `src/http/ui/js/pages/keys.js`, `styles.css` | 7 |
| `tests/server_oauth.rs` (new), `tests/support/oauth.rs` (new), `tests/support/mod.rs`, `tests/support/server.rs` (`start_with`) | 6 |
| `tests/ui_e2e/consent.rs` (new), `tests/ui_e2e/keys.rs`, `tests/ui_e2e/main.rs` | 5, 7 |
| `README.md`, `docs/operations.md` | 4 (README "Command line" mention; `/api/v1/oauth/codes` rows under "Web console"), 8 (the "OAuth connectors" section with the how-to and CLI reference) |
| spec `docs/superpowers/specs/2026-11-11-okf-oauth-design.md` | 8 |
| `changelog/` | every task |

## Task dependencies

```
T1 (annotations) ─────────────────────────────────────────────┐
T2 (foundation) ──┬─> T3 (metadata, DCR, CIMD) ──┐            │
                  └─> T4 (sign-in codes) ────────┼─> T5 (authorize, consent) ─> T6 (token, e2e) ─┼─> T8 (docs, release prep)
                                   └─> T7 (console panel) ────────────────────────────────────────┘
```

- **T1 and T2 can run in parallel.** T1 touches only `src/mcp/`; T2 touches no tool.
- **T3 and T4 can run in parallel after T2.** They fill disjoint files. Both append to `README.md` / `docs/operations.md` only in T4; T3 writes no docs.
- **T5 needs T3** (client resolution) **and T4** (`find_live`, `redeem`).
- **T6 needs T5** (authorization codes in `OAuth.codes`).
- **T7 needs T4** (the JSON API) and can run beside T5 and T6.
- **T8 waits for all.**

---

### Task 1: Tool annotations and their coverage test

Every MCP tool gets a human-readable title and the four behaviour hints
(`readOnlyHint`, `destructiveHint`, `idempotentHint`, `openWorldHint`) that
the connector directories check. No tool's name, parameters, results or
description change. This task is independent of the OAuth tasks and can run
beside Task 2.

**Facts this task relies on** (verified at `c24fef1`):

- rmcp-macros 3.8.0's `#[tool(...)]` takes `title = "..."` (REG/rmcp-macros-3.8.0/src/tool.rs:67) and `annotations(...)` (tool.rs:74). The annotations are parsed by darling into `ToolAnnotationsAttribute { title, read_only_hint, destructive_hint, idempotent_hint, open_world_hint }`, each `Option` (tool.rs:146-180), so the syntax is `annotations(title = "...", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)`.
- The tool-level `title` becomes `.with_title(...)` on the generated `Tool` (tool.rs:109-111). The annotations become `.with_annotations(ToolAnnotations::from_raw(...))` (tool.rs:118-119, 211-226).
- `rmcp::model::Tool` has a top-level `title: Option<String>` (REG/rmcp-3.8.0/src/model/tool.rs:22) and `annotations: Option<ToolAnnotations>` (tool.rs:33). `ToolAnnotations` serializes camelCase (`#[serde(rename_all = "camelCase")]`, tool.rs:51), so the wire names are `readOnlyHint`, `destructiveHint`, `idempotentHint` and `openWorldHint`.
- **The title is set in both places, with the same text.** MCP 2025-06-18 and later put the display name in `Tool.title`. The older 2025-03-26 revision had only `annotations.title`, and clients fall back to it. Setting both serves every client the directories test with. The coverage test checks that the two are equal, so they cannot drift.
- `OkfServer::tools() -> ToolRouter<Self>` is `pub` and needs no `App` (`src/mcp/mod.rs:67-81`). `ToolRouter::list_all() -> Vec<Tool>` returns every enabled tool, sorted by name (REG/rmcp-3.8.0/src/handler/server/router/tool.rs:581-590).
- There are 41 `#[tool(` attributes (`grep -rn "#\[tool(" src/mcp | wc -l`), each with only `description = ...` today:
  - `catalogs.rs:92` `list_catalogs`; `search.rs:170` `search`;
  - `documents.rs:474` `get_concept`, `:497` `read_document`;
  - `history.rs:153` `history`;
  - `reports.rs:356` `open_questions`, `:370` `oldest`, `:384` `sprawl`, `:398` `links_report`, `:413` `lint`;
  - `changesets.rs:153` `list_changesets`, `:237` `open_changeset`, `:317` `publish`, `:712` `discard`, `:728` `revert`, `:745` `pending_changes`;
  - `writes.rs:216` `create_concept`, `:234` `edit_document`, `:251` `replace_body`, `:267` `set_frontmatter`, `:661` `stamp`, `:677` `review`, `:693` `question`, `:708` `unquestion`, `:724` `add_source`, `:739` `remove_source`, `:756` `log`, `:772` `index`;
  - `restructure.rs:172` `move_concept`, `:192` `split_concept`, `:211` `delete_concept`;
  - `maintenance/mod.rs:85` `maintenance_claim`, `:97` `maintenance_renew`, `:109` `maintenance_read_revision`, `:121` `maintenance_submit`, `:133` `maintenance_close`, `:145` `maintenance_list_runs`, `:157` `maintenance_get_run`, `:169` `maintenance_list_findings`, `:182` `maintenance_get_finding`, `:194` `maintenance_dispose`.
- **`revert` stages; it does not publish.** Its description (`changesets.rs:728-733`) says it adds an earlier revision "to the changeset `changeset` names, for review and publish like any other edit". It is therefore classified as a staged edit (not destructive), unlike the spec's table, which listed it with `publish`. Task 8 records this in the spec (Deviation 3).

**Classification** (spec "Tool annotations", with the `revert` correction):

| Class | read_only | destructive | idempotent | open_world |
|---|---|---|---|---|
| Read | true | false | true | false |
| Staged edit (unpublished, undone by `discard`) | false | false | false | false |
| Irreversible or deleting | false | true | false | false |

**Files:**
- Create: `src/mcp/annotations.rs` (test-only: the classification and the coverage tests)
- Modify: `src/mcp/mod.rs` (declare `#[cfg(test)] mod annotations;`)
- Modify: every file with a `#[tool(`, which are `src/mcp/catalogs.rs`, `search.rs`, `documents.rs`, `history.rs`, `reports.rs`, `changesets.rs`, `writes.rs`, `restructure.rs` and `maintenance/mod.rs`
- Modify: `src/investigator/contract_tools.json` (the investigator's checked-in tool contracts)
- Create: `changelog/<date -u +%Y-%m-%dT%H%M%SZ>-m10-task-01-tool-annotations.md`

> **Corrected (issue #245, 2026-11-11).** The plan missed one file. `investigator::contract::tests::investigator_contract_rejects_missing_or_changed_tools` compares each of the investigator runner's nine allowed tools, serialized whole, with `src/investigator/contract_tools.json`. Once the tools carry `title` and `annotations`, that comparison fails. Regenerate the fixture with its ignored capture test, `cargo test --lib investigator_contract_capture_fixture -- --ignored`. The diff should add only `title` and `annotations` to the nine entries. `validate_tools` itself compares only the input and output schemas, so the runner's behaviour does not change.

- [ ] **Step 1: Write the failing coverage test**

Create `src/mcp/annotations.rs`:

> **Corrected (issue #245, 2026-11-11).** The block below is now as `cargo fmt` lays it out (the `DESTRUCTIVE` list on one line, two `assert_eq!`s wrapped); the code is unchanged.

```rust
//! The connector directories' tool metadata (M10 Task 1): every tool has a
//! title and all four behaviour hints, and is classified exactly once here,
//! so a new tool cannot ship without deciding what it does to the catalog.
//!
//! The hints themselves live on each `#[tool(...)]` attribute; this module
//! is the single list they are checked against.

use std::collections::BTreeSet;

use crate::mcp::OkfServer;

/// Tools that only read: never change a catalog, a changeset or a run.
const READS: &[&str] = &[
    "get_concept",
    "history",
    "links_report",
    "lint",
    "list_catalogs",
    "list_changesets",
    "maintenance_get_finding",
    "maintenance_get_run",
    "maintenance_list_findings",
    "maintenance_list_runs",
    "maintenance_read_revision",
    "oldest",
    "open_questions",
    "pending_changes",
    "read_document",
    "search",
    "sprawl",
];

/// Tools whose effect stays unpublished (a changeset entry, a maintenance
/// lease or report) and can be undone by `discard` or by letting it lapse.
const STAGED: &[&str] = &[
    "add_source",
    "create_concept",
    "edit_document",
    "index",
    "log",
    "maintenance_claim",
    "maintenance_dispose",
    "maintenance_renew",
    "maintenance_submit",
    "move_concept",
    "open_changeset",
    "question",
    "remove_source",
    "replace_body",
    "review",
    "revert",
    "set_frontmatter",
    "split_concept",
    "stamp",
    "unquestion",
];

/// Tools that cannot be undone from inside the server, or that delete.
const DESTRUCTIVE: &[&str] = &["delete_concept", "discard", "maintenance_close", "publish"];

/// The hints a tool's class implies (`open_world` is always false: every
/// tool acts only on this server's catalogs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Hints {
    read_only: bool,
    destructive: bool,
    idempotent: bool,
}

/// The hints for `name`'s class, or `None` for an unclassified tool.
fn expected(name: &str) -> Option<Hints> {
    // `contains(&name)`: the slices hold `&str`, so the needle is a `&&str`.
    if READS.contains(&name) {
        Some(Hints {
            read_only: true,
            destructive: false,
            idempotent: true,
        })
    } else if STAGED.contains(&name) {
        Some(Hints {
            read_only: false,
            destructive: false,
            idempotent: false,
        })
    } else if DESTRUCTIVE.contains(&name) {
        Some(Hints {
            read_only: false,
            destructive: true,
            idempotent: false,
        })
    } else {
        None
    }
}

/// Is every word of `title` capitalized ("List Catalogs")?
fn is_title_case(title: &str) -> bool {
    !title.is_empty()
        && title
            .split(' ')
            .all(|word| word.chars().next().is_some_and(|c| c.is_ascii_uppercase()))
}

#[test]
fn every_tool_is_classified_exactly_once() {
    let registered: BTreeSet<String> = OkfServer::tools()
        .list_all()
        .into_iter()
        // `name` is a `Cow<'static, str>`; `into_owned` gives the `String`.
        .map(|tool| tool.name.into_owned())
        .collect();
    let mut classified = BTreeSet::new();

    for name in READS.iter().chain(STAGED).chain(DESTRUCTIVE) {
        // `insert` returns false for a value already in the set.
        assert!(
            classified.insert((*name).to_string()),
            "{name} is in more than one class"
        );
    }

    assert_eq!(
        registered, classified,
        "the registered tools and the classified tools differ"
    );
    assert_eq!(registered.len(), 41);
}

#[test]
fn every_tool_has_a_title_and_all_four_hints() {
    for tool in OkfServer::tools().list_all() {
        let name = tool.name.as_ref();
        let title = tool
            .title
            .as_deref()
            .unwrap_or_else(|| panic!("{name} has no title"));
        let annotations = tool
            .annotations
            .as_ref()
            .unwrap_or_else(|| panic!("{name} has no annotations"));
        let hints = expected(name).unwrap_or_else(|| panic!("{name} is not classified"));

        assert!(is_title_case(title), "{name}: {title:?} is not Title Case");
        assert_eq!(
            annotations.title.as_deref(),
            Some(title),
            "{name}: the annotation title differs from the tool title"
        );
        assert_eq!(annotations.read_only_hint, Some(hints.read_only), "{name}");
        assert_eq!(
            annotations.destructive_hint,
            Some(hints.destructive),
            "{name}"
        );
        assert_eq!(
            annotations.idempotent_hint,
            Some(hints.idempotent),
            "{name}"
        );
        assert_eq!(annotations.open_world_hint, Some(false), "{name}");
    }
}

#[test]
fn a_read_only_tool_is_never_destructive() {
    for tool in OkfServer::tools().list_all() {
        let annotations = tool.annotations.expect("annotated");

        if annotations.read_only_hint == Some(true) {
            assert_eq!(annotations.destructive_hint, Some(false), "{}", tool.name);
        }
    }
}

#[test]
fn the_hints_go_on_the_wire_in_camel_case() {
    let search = OkfServer::tools()
        .list_all()
        .into_iter()
        .find(|tool| tool.name == "search")
        .expect("search is registered");

    let json = serde_json::to_value(&search).expect("a tool serializes");

    assert_eq!(json["title"], "Search Concepts");
    assert_eq!(json["annotations"]["title"], "Search Concepts");
    assert_eq!(json["annotations"]["readOnlyHint"], true);
    assert_eq!(json["annotations"]["destructiveHint"], false);
    assert_eq!(json["annotations"]["idempotentHint"], true);
    assert_eq!(json["annotations"]["openWorldHint"], false);
}
```

In `src/mcp/mod.rs`, after `pub(crate) mod catalogs;` (line 6), add:

```rust
// Test-only: the tool classification the annotations are checked against.
#[cfg(test)]
mod annotations;
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test --lib mcp::annotations`
Expected: these fail:
- `every_tool_has_a_title_and_all_four_hints` panics with `... has no title`;
- `a_read_only_tool_is_never_destructive` panics on `expect("annotated")`;
- `the_hints_go_on_the_wire_in_camel_case` fails on `json["title"]`.

`every_tool_is_classified_exactly_once` passes already: the classification matches the 41 registered names.

- [ ] **Step 3: Annotate every tool**

Each tool's attribute gains two things, and its `description` is unchanged:
- a first line `title = "<Title>",`;
- a last argument `annotations(...)` with the same title and its class's hints.

Here is `list_catalogs` (`src/mcp/catalogs.rs:92`) in full:

```rust
    #[tool(
        title = "List Catalogs",
        description = "List the catalogs this API key can reach (name, description, file \
                       count, last change) and this connection's selection: the catalogs \
                       a call without a `catalog` argument reads, and the default write target.",
        annotations(
            title = "List Catalogs",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
```

The `description = "..."` line gains a trailing comma, because it is no
longer the last argument. Apply the same shape to every tool below. The
three hint sets are always exactly one of these:

- **Read:** `read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false`
- **Staged:** `read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = false`
- **Destructive:** `read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false`

| File:line | Tool | Title (both places) | Class |
|---|---|---|---|
| `catalogs.rs:92` | `list_catalogs` | List Catalogs | Read |
| `search.rs:170` | `search` | Search Concepts | Read |
| `documents.rs:474` | `get_concept` | Get Concept | Read |
| `documents.rs:497` | `read_document` | Read Document | Read |
| `history.rs:153` | `history` | Document History | Read |
| `reports.rs:356` | `open_questions` | List Open Questions | Read |
| `reports.rs:370` | `oldest` | Oldest Concepts | Read |
| `reports.rs:384` | `sprawl` | Sprawl Report | Read |
| `reports.rs:398` | `links_report` | Links Report | Read |
| `reports.rs:413` | `lint` | Lint Catalog | Read |
| `changesets.rs:153` | `list_changesets` | List Changesets | Read |
| `changesets.rs:745` | `pending_changes` | Show Pending Changes | Read |
| `maintenance/mod.rs:109` | `maintenance_read_revision` | Read Maintenance Revision | Read |
| `maintenance/mod.rs:145` | `maintenance_list_runs` | List Maintenance Runs | Read |
| `maintenance/mod.rs:157` | `maintenance_get_run` | Get Maintenance Run | Read |
| `maintenance/mod.rs:169` | `maintenance_list_findings` | List Maintenance Findings | Read |
| `maintenance/mod.rs:182` | `maintenance_get_finding` | Get Maintenance Finding | Read |
| `changesets.rs:237` | `open_changeset` | Open Changeset | Staged |
| `changesets.rs:728` | `revert` | Revert Document | Staged |
| `writes.rs:216` | `create_concept` | Create Concept | Staged |
| `writes.rs:234` | `edit_document` | Edit Document | Staged |
| `writes.rs:251` | `replace_body` | Replace Concept Body | Staged |
| `writes.rs:267` | `set_frontmatter` | Set Frontmatter | Staged |
| `writes.rs:661` | `stamp` | Stamp Concept | Staged |
| `writes.rs:677` | `review` | Record Review | Staged |
| `writes.rs:693` | `question` | Add Question | Staged |
| `writes.rs:708` | `unquestion` | Resolve Question | Staged |
| `writes.rs:724` | `add_source` | Add Source | Staged |
| `writes.rs:739` | `remove_source` | Remove Source | Staged |
| `writes.rs:756` | `log` | Add Log Entry | Staged |
| `writes.rs:772` | `index` | Add Index Entry | Staged |
| `restructure.rs:172` | `move_concept` | Move Concept | Staged |
| `restructure.rs:192` | `split_concept` | Split Concept | Staged |
| `maintenance/mod.rs:85` | `maintenance_claim` | Claim Maintenance Batch | Staged |
| `maintenance/mod.rs:97` | `maintenance_renew` | Renew Maintenance Lease | Staged |
| `maintenance/mod.rs:121` | `maintenance_submit` | Submit Maintenance Report | Staged |
| `maintenance/mod.rs:194` | `maintenance_dispose` | Dispose Maintenance Finding | Staged |
| `changesets.rs:317` | `publish` | Publish Changeset | Destructive |
| `changesets.rs:712` | `discard` | Discard Changeset Entries | Destructive |
| `restructure.rs:211` | `delete_concept` | Delete Concept | Destructive |
| `maintenance/mod.rs:133` | `maintenance_close` | Close Maintenance Run | Destructive |

That is 17 Read, 20 Staged and 4 Destructive: 41 tools.

The maintenance descriptions are single long string literals. Keep them as
they are and only add the comma after the closing quote. Here is
`maintenance_close` (`src/mcp/maintenance/mod.rs:133`) as one destructive
example:

```rust
    #[tool(
        title = "Close Maintenance Run",
        description = "Report-only: close an owned run with the reporting flag, or another credential’s run with ordinary Write. Invalidates remaining leases, never publishes canonical changes.",
        annotations(
            title = "Close Maintenance Run",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
```

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test --lib mcp::annotations`
Expected: `test result: ok. 4 passed; 0 failed`.

Run: `cargo test --lib mcp`
Expected: every existing MCP test still passes. No tool's schema or behaviour changed.

- [ ] **Step 5: Check the wire format end to end**

Run: `cargo test --test server`
Expected: PASS. The end-to-end MCP tests list and call tools through a real
rmcp client, which deserializes the new fields.

- [ ] **Step 6: Gates**

Run:
```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
cargo test -- --test-threads=6
```
Expected: all clean. Paste the test counts into the PR.

If `cargo fmt` reflows an attribute, accept its layout. rustfmt leaves
string literals alone.

- [ ] **Step 7: Changelog**

Create `changelog/<date -u +%Y-%m-%dT%H%M%SZ>-m10-task-01-tool-annotations.md`:

```markdown
# M10 Task 1: Tool annotations (issue #N)

## Added

- Every MCP tool (41) has a `title` and `annotations` with the same title
  and `readOnlyHint`, `destructiveHint`, `idempotentHint` and
  `openWorldHint`, for the Claude and ChatGPT connector directories.
- `src/mcp/annotations.rs` (test-only): the tool classification (17 reads,
  20 staged edits, 4 irreversible or deleting) and tests that every
  registered tool is classified exactly once, has a Title Case title in
  both places and carries its class's hints, and that the hints serialize
  as camelCase.

## Changed

- No tool's name, description, parameters or results changed.

## Notes

- `revert` is a staged edit (it adds an earlier revision to the changeset
  for review and publish), not destructive as the spec's table had it. The
  spec is corrected in Task 8.
- `openWorldHint` is false everywhere: every tool acts only on this
  server's catalogs.
```

Write `N` as this task's issue number (`gh issue list --search "M10 Task 1"`).

- [ ] **Step 8: Commit**

```bash
git add src/mcp src/investigator/contract_tools.json changelog
git commit -m "feat(mcp): a title and behaviour hints on every tool (M10 task 1)

Every tool has a title, on the tool and in its annotations, and explicit
readOnly, destructive, idempotent and openWorld hints, for the connector
directories. A test-only classification lists every tool once and the
tests check each tool against it, so a new tool must be classified.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Foundation: schema, configuration, bearer verification, the `src/oauth/` skeleton

The task changes no behaviour for a server without `public_url`: static keys authenticate exactly as before, and every 401 still says `WWW-Authenticate: Bearer`. With `public_url` set, the server:
- accepts `okfat_` access tokens stored in `oauth_tokens`, checking their audience;
- answers a 401 with the `resource_metadata` challenge;
- mounts the (still empty) `/oauth/*` and `/.well-known/*` routers;
- prunes expired OAuth rows once an hour from the poll loop.

No token or client is created by any endpoint yet. Tests insert them with SQL and the store functions.

**Interface corrections (relative to the first draft of "Interfaces shared across tasks"; already folded into it):**

1. `Config::validate(&self) -> Result<(), String>`. This is the existing signature (`src/config.rs:174`); `Config::load` turns the `String` into `OkfError::Config`. The skeleton's `Result<(), OkfError>` is wrong.
2. `bump_admin_generation` already exists as `pub(crate)` in `crate::store::catalogs` (`src/store/catalogs.rs:286`), not in `store::keys`. Only `store::keys::catalog_ids` changes visibility.
3. `ExpiringMap::new(ttl: Duration, capacity: usize)`, and `insert(...) -> bool` (`false` when full). Pending authorizations are created by an unauthenticated GET, so the maps must be bounded.
4. `AccessTokens::remember(&self, sha256: [u8; 32], token: LiveAccessToken, now: DateTime<Utc>)`: it needs `now` to prune expired entries. `AccessTokens::verify(&self, app: &App, resource: &str, token: &str)`: it needs the audience to check against.
5. `store::oauth::tokens::revoke` returns `Option<RevokedToken>`, where `pub struct RevokedToken { pub family_id: String, pub kind: TokenKind }`. Task 6 needs the kind to decide whether to revoke the family. It also returns the row when the token was already revoked, so revocation is idempotent.
6. `store::oauth::clients::prune_unbound_dcr` moves from Task 3 to Task 2, so the poll wiring is written once. Task 3 keeps `insert_dcr`, `upsert_cimd` and `count_unbound_dcr`.
7. `OAuth` gains `pub challenge: HeaderValue`, the precomputed 401 challenge. `src/oauth/mod.rs` also defines `pub const PROTECTED_RESOURCE_PATH: &str = "/.well-known/oauth-protected-resource"` and `pub const AUTHORIZATION_SERVER_PATH: &str = "/.well-known/oauth-authorization-server"`, for Task 3.
8. **A grant is not named `<code name>@<client host>`:** `api_keys_name_check` allows only `[A-Za-z0-9._-]`. Task 4's `redeem` names it `<code name>.<client host>-<first 8 characters of the code id>` (its Decision 1). Migration 0008 also limits the live name/actor unique index to `kind = 'key'`, so grants never collide on it however they are named.
9. Dispatch is "`okfat_` goes to OAuth, anything else goes to the key cache". The cache holds only `okf_` keys, and every key ever issued was made by `keys::generate` with that prefix (since `e89ff71`), so the result is the spec's "anything else is 401". Existing tests that use unprefixed test keys keep working.
10. A new `#[cfg(test)]` helper module, `src/store/oauth/testing.rs`, for Tasks 3 to 6: `insert_client`, `insert_grant`, `oauth_config` and `mint_access` (Steps 8 and 12). Task 2 does **not** create `src/oauth/testing.rs` or declare it in `src/oauth/mod.rs`. Task 5 creates that file and adds `#[cfg(test)] pub(crate) mod testing;`, and should build on these helpers rather than duplicate them.
11. `oauth_clients` gains `cache_expires_at timestamptz`, and `ClientRecord` gains `pub cache_expires_at: Option<DateTime<Utc>>`. `store::oauth::clients` defines a private `client_columns!()` macro (corrected from a `CLIENT_COLUMNS` const, issue #246, 2026-11-11: sqlx 0.9 takes only a `&'static str` or an `AssertSqlSafe` as SQL, so the column list is spliced with `concat!` at compile time), `ClientRow` (`sqlx::FromRow`) and `impl From<ClientRow> for ClientRecord`, which `find` uses and Task 3 reuses. Task 2 does not define `insert_dcr`; Task 3 owns it, as `insert_dcr(pool, &NewDcrClient<'_>, now) -> Result<ClientRecord, OkfError>`.
12. The stub `CimdFetcher::new(allow_private: bool, allow_http: bool) -> Result<Self, OkfError>`. `OAuth::new` passes `allow_private = cimd_allow_private` and `allow_http = cimd_allow_private && issuer starts with "http://"`.
13. `AuthorizationCode` derives `Debug, Clone, PartialEq, Eq` and gains `pub used: bool`: a used code stays in the map until it expires, so a replay is detected. `PendingAuthorization` derives `Clone` and keeps its hand-written `Debug`. `src/oauth/state.rs` adds `pub fn code_key(code: &str) -> String` (corrected from `pub(crate)`, issue #246, 2026-11-11: nothing calls it before Task 5, and a `pub(crate)` item would fail `-D warnings` as dead code), the hex SHA-256 of an `okfac_` code and the key of `OAuth.codes`.
14. `CredentialKind::OAuth { client_id: Option<String>, client_name: Option<String> }`.
15. Rate limits: register, a burst of 10 then one every 6 seconds; token (and revoke), a burst of 60 then 2 a second; consent POST, a burst of 20 then 1 a second.

**Facts this task relies on** (verified at `c24fef1`):
- **Existing schema and auth:**
  - `api_keys.sha256` is `bytea NOT NULL UNIQUE CHECK (length(sha256) = 32)` (`migrations/0001_schema.sql:44`).
  - Postgres treats NULLs as distinct in a UNIQUE constraint by default (`NULLS DISTINCT`), so many grant rows with a NULL `sha256` do not conflict. `length(NULL) = 32` is NULL, which a CHECK accepts.
  - The live name/actor index is `api_keys_live_name_actor ON api_keys (name, actor) WHERE revoked_at IS NULL` (`migrations/0005_key_names.sql`). `store::keys::create_with_id` matches that constraint name (`src/store/keys.rs:239`), so the replacement index keeps it.
  - Every key holder authenticates by `KeyCache::by_hash` (`src/auth.rs:139-156`), which skips a row whose hash is not 32 bytes.
- **Tests and fixtures that must change:**
  - `src/db.rs:61-97` asserts the exact table list, so the migration's new tables must be added there.
  - `src/db.rs:101-111` applies migrations by `include_str!` with `sqlx::raw_sql`, which is the pattern for a test of `0008` over older rows.
  - `Principal { .. }` literals appear in `src/mcp/testing.rs:125`, `src/selection.rs:178` and `src/auth.rs` tests (struct update syntax). `src/http/archives.rs:308` uses `..(*principal(..)).clone()`, which needs no change.
  - `KeyRecord { .. }` literals appear in `src/auth.rs:296`, `src/auth.rs:332` (update syntax) and `src/cli.rs:972`. `record.sha256` is read in `src/auth.rs:151` and `src/store/keys.rs:797`.
- **Code being extended:**
  - `require_key` is layered twice, at `src/http/mod.rs:217` (`from_fn_with_state(keys, require_key)`, with `keys = Arc::clone(app.keys())` at :202) and at `src/http/api/mod.rs:52-55`. `api::routes` takes `&App` (`src/http/api/mod.rs:38`) and is called as `api::routes(&app)` with `app: Arc<App>` (`src/http/mod.rs:226`).
  - `Config::validate` returns `Result<(), String>` and is called only by `Config::load` (`src/config.rs:251`). Tests build `Config { .. }` with struct update syntax, so new fields need defaults only.
  - `App::poll` runs `reload_keys_if_changed` and `prune_empty_changesets` under `polling` (`src/app.rs:249-256`), and each logs and swallows its own failure (`src/app.rs:304-331`).
  - `OkfError::Config { path: String, message: String }` (`src/error.rs:121`).
- **Crate APIs:**
  - **http 1.5.0** (axum 0.8.9's, `Cargo.lock:1189`):
    - `Uri::scheme_str`, `host`, `port_u16`, `path` and `query` (REG/http-1.5.0/src/uri/mod.rs:502, :590, :652, :439, :700);
    - `Authority: FromStr` with `host()` and `port_u16()` (REG/http-1.5.0/src/uri/authority.rs:409, :141, :197);
    - `HeaderValue::from_str` (REG/http-1.5.0/src/header/value.rs:105).
  - **rmcp 3.8.0's `Host` check** (REG/rmcp-3.8.0/src/transport/streamable_http_server/tower.rs:927-974): an `allowed_hosts` entry is parsed as an `Authority`, and the host is compared lowercased with brackets trimmed. A port in the entry must equal the request's port; an entry without a port matches any port. `Config::validate` mirrors this rule for `public_url`.
  - **axum 0.8.9:** `middleware::from_fn_with_state(state, f)` (REG/axum-0.8.9/src/middleware/from_fn.rs:164). The `async fn` middleware takes `State<Arc<App>>`.
  - **sqlx-postgres 0.9.0:** decodes `text[]` as `Vec<String>` (REG/sqlx-postgres-0.9.0/src/types/array.rs:182). `jsonb` decodes as `serde_json::Value` with the `json` feature the project enables (`Cargo.toml`).
  - **Rust 1.99.1** (`rustc --version`): a trait method may be declared as `fn verify(..) -> impl Future<Output = ..> + Send` and implemented with `async fn` (stable since 1.75). In edition 2021, `Future` is not in the prelude and must be imported.

**Files:**
- Create: `migrations/0008_oauth.sql`
- Create: `src/store/oauth/mod.rs`, `src/store/oauth/tokens.rs`, `src/store/oauth/clients.rs`, `src/store/oauth/codes.rs`, `src/store/oauth/testing.rs`
- Create: `src/oauth/mod.rs`, `error.rs`, `expiring.rs`, `ratelimit.rs`, `state.rs`, `access.rs`, and stubs `metadata.rs`, `clients.rs`, `pkce.rs`, `authorize.rs`, `consent.rs`, `token.rs`
- Modify: `src/keys.rs`, `src/config.rs`, `src/lib.rs`, `src/store/mod.rs`, `src/store/keys.rs`, `src/auth.rs`, `src/app.rs`, `src/db.rs` (tests), `src/http/mod.rs`, `src/http/api/mod.rs`, `src/http/api/keys.rs`, `src/http/api/identity.rs`, `src/mcp/testing.rs`, `src/selection.rs` (tests), `src/cli.rs` (tests)
- Modify: `docs/superpowers/plans/2026-11-11-okf-mcp-m10-oauth.md` (the interface corrections above)
- Create: `changelog/<stamp>-m10-task-02-oauth-foundation.md`

- [ ] **Step 1: Write the failing migration tests**

In `src/db.rs`, in `migrations_create_the_schema_and_rerun_as_a_no_op`, replace the expected table list with:

```rust
            [
                "api_keys",
                "catalogs",
                "changeset_entries",
                "changesets",
                "documents",
                "key_catalogs",
                "maintenance_dispositions",
                "maintenance_evidence",
                "maintenance_findings",
                "maintenance_requests",
                "maintenance_runs",
                "maintenance_state",
                "maintenance_work",
                "oauth_clients",
                "oauth_tokens",
                "revisions",
                "server_state",
                "sign_in_code_catalogs",
                "sign_in_codes",
            ]
```

Add this test to the same `mod tests`:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn oauth_schema_keeps_existing_keys_and_guards_grant_rows() {
        // Arrange: the schema as of 0007, with one key, then 0008 on top.
        let db = pg_support::TestDb::start().await;
        for migration in [
            include_str!("../migrations/0001_schema.sql"),
            include_str!("../migrations/0002_changeset_identity.sql"),
            include_str!("../migrations/0003_changeset_handles.sql"),
            include_str!("../migrations/0004_drop_changeset_labels.sql"),
            include_str!("../migrations/0005_key_names.sql"),
            include_str!("../migrations/0006_maintenance.sql"),
            include_str!("../migrations/0007_maintenance_assessment_evidence.sql"),
        ] {
            sqlx::raw_sql(migration).execute(&db.pool).await.unwrap();
        }
        let now = chrono::Utc::now();
        sqlx::query(
            "INSERT INTO api_keys (name, sha256, access, all_catalogs, actor, created_at) \
             VALUES ('existing', $1, 'write', true, 'example-actor', $2)",
        )
        .bind(vec![7u8; 32])
        .bind(now)
        .execute(&db.pool)
        .await
        .unwrap();

        // Act
        sqlx::raw_sql(include_str!("../migrations/0008_oauth.sql"))
            .execute(&db.pool)
            .await
            .unwrap();

        // Assert: the old key is a `key` row with its hash and no client.
        let (kind, sha256, client_id): (String, Option<Vec<u8>>, Option<String>) =
            sqlx::query_as("SELECT kind, sha256, client_id FROM api_keys WHERE name = 'existing'")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(kind, "key");
        assert_eq!(sha256, Some(vec![7u8; 32]));
        assert_eq!(client_id, None);

        // A grant row needs no hash, may not have one, and may not be admin
        // or report maintenance.
        let grant = |sha256: Option<Vec<u8>>, access: &'static str, report: bool| {
            let pool = db.pool.clone();
            async move {
                sqlx::query(
                    "INSERT INTO api_keys \
                     (name, sha256, access, maintenance_report, all_catalogs, actor, created_at, kind) \
                     VALUES ('laptop', $1, $2, $3, true, 'example-actor', $4, 'oauth')",
                )
                .bind(sha256)
                .bind(access)
                .bind(report)
                .bind(now)
                .execute(&pool)
                .await
            }
        };
        let constraint = |result: Result<_, sqlx::Error>| {
            result
                .expect_err("the row must be refused")
                .as_database_error()
                .and_then(|error| error.constraint().map(str::to_string))
        };
        assert!(grant(None, "write", false).await.is_ok());
        // Two live grants with one name and actor: allowed (the unique index
        // covers `kind = 'key'` rows only).
        assert!(grant(None, "read", false).await.is_ok());
        assert_eq!(
            constraint(grant(Some(vec![8u8; 32]), "write", false).await).as_deref(),
            Some("api_keys_kind_hash")
        );
        assert_eq!(
            constraint(grant(None, "admin", false).await).as_deref(),
            Some("api_keys_oauth_caps")
        );
        assert_eq!(
            constraint(grant(None, "read", true).await).as_deref(),
            Some("api_keys_oauth_caps")
        );
        // A key row still needs its hash, and a second live key of one
        // name and actor is still refused.
        let key = |sha256: Option<Vec<u8>>| {
            let pool = db.pool.clone();
            async move {
                sqlx::query(
                    "INSERT INTO api_keys (name, sha256, access, all_catalogs, actor, created_at) \
                     VALUES ('existing', $1, 'read', true, 'example-actor', $2)",
                )
                .bind(sha256)
                .bind(now)
                .execute(&pool)
                .await
            }
        };
        assert_eq!(constraint(key(None).await).as_deref(), Some("api_keys_kind_hash"));
        assert_eq!(
            constraint(key(Some(vec![9u8; 32])).await).as_deref(),
            Some("api_keys_live_name_actor")
        );
    }
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test --lib db::tests -- --test-threads=2`
Expected: FAIL. `include_str!` cannot find `../migrations/0008_oauth.sql` (a compile error).

- [ ] **Step 3: Write the migration**

Create `migrations/0008_oauth.sql`:

```sql
-- OAuth 2.1 (M10): registered and metadata-document clients, admin-issued
-- sign-in codes, grants (api_keys rows of kind 'oauth') and their tokens.
-- Every timestamp is supplied by the server clock; every secret is stored
-- only as its SHA-256.

-- A client is a dynamically registered one (a UUID id) or a Client ID
-- Metadata Document (the document's URL is the id), cached here.
CREATE TABLE oauth_clients (
    client_id     text PRIMARY KEY CHECK (client_id <> ''),
    kind          text NOT NULL CHECK (kind IN ('dcr', 'cimd')),
    client_name   text,
    redirect_uris text[] NOT NULL CHECK (cardinality(redirect_uris) > 0),
    metadata      jsonb NOT NULL,
    -- When a metadata document was last fetched, and until when the cached
    -- copy may be used (its Cache-Control, clamped by Task 3); never set for DCR.
    fetched_at       timestamptz,
    cache_expires_at timestamptz,
    created_at    timestamptz NOT NULL,
    CONSTRAINT oauth_clients_fetched_cimd CHECK (
        (kind = 'cimd') = (fetched_at IS NOT NULL)
        AND (kind = 'cimd') = (cache_expires_at IS NOT NULL)
    )
);

-- A grant is a key row with no hash of its own: its tokens carry the
-- credential. `length(NULL) = 32` is NULL, which a CHECK accepts, and UNIQUE
-- treats NULLs as distinct, so the existing sha256 constraints stay.
ALTER TABLE api_keys
    ADD COLUMN kind text NOT NULL DEFAULT 'key' CHECK (kind IN ('key', 'oauth')),
    ADD COLUMN client_id text REFERENCES oauth_clients (client_id) ON DELETE SET NULL,
    ALTER COLUMN sha256 DROP NOT NULL,
    ADD CONSTRAINT api_keys_kind_hash CHECK ((kind = 'key') = (sha256 IS NOT NULL)),
    -- No code path can make an OAuth grant that administers or reports.
    ADD CONSTRAINT api_keys_oauth_caps
        CHECK (kind = 'key' OR (access <> 'admin' AND NOT maintenance_report));
-- Lets pruning find the clients no grant names.
CREATE INDEX api_keys_client_id ON api_keys (client_id) WHERE client_id IS NOT NULL;

-- Grants are named after their sign-in code; two codes of one name and actor
-- may both be redeemed, so the live name/actor rule covers keys only. The
-- index keeps its name, which `store::keys::create_with_id` matches.
DROP INDEX api_keys_live_name_actor;
CREATE UNIQUE INDEX api_keys_live_name_actor
    ON api_keys (name, actor) WHERE revoked_at IS NULL AND kind = 'key';

-- A one-time credential bound to a grant template. Admin is never offered.
CREATE TABLE sign_in_codes (
    id                 text PRIMARY KEY DEFAULT gen_random_uuid()::text,
    name               text NOT NULL CHECK (name ~ '^[A-Za-z0-9._-]+$'),
    sha256             bytea NOT NULL UNIQUE CHECK (length(sha256) = 32),
    access             text NOT NULL CHECK (access IN ('read', 'write')),
    all_catalogs       boolean NOT NULL,
    default_catalog_id bigint REFERENCES catalogs (id) ON DELETE SET NULL,
    actor              text NOT NULL,
    created_at         timestamptz NOT NULL,
    expires_at         timestamptz NOT NULL CHECK (expires_at > created_at),
    redeemed_at        timestamptz,
    grant_id           text REFERENCES api_keys (id) ON DELETE SET NULL,
    revoked_at         timestamptz,
    -- A grant id only on a redeemed code. A redeemed code may lose it: the
    -- `ON DELETE SET NULL` above clears it if the grant row is ever deleted.
    CONSTRAINT sign_in_codes_redeemed_grant CHECK (grant_id IS NULL OR redeemed_at IS NOT NULL)
);
-- Lets deleting a catalog find the codes whose default it clears.
CREATE INDEX sign_in_codes_default_catalog_id ON sign_in_codes (default_catalog_id)
    WHERE default_catalog_id IS NOT NULL;
-- Lets pruning find unredeemed codes past their expiry.
CREATE INDEX sign_in_codes_unredeemed_expires_at ON sign_in_codes (expires_at)
    WHERE redeemed_at IS NULL;

CREATE TABLE sign_in_code_catalogs (
    code_id    text NOT NULL REFERENCES sign_in_codes (id) ON DELETE CASCADE,
    catalog_id bigint NOT NULL REFERENCES catalogs (id) ON DELETE CASCADE,
    PRIMARY KEY (code_id, catalog_id)
);
CREATE INDEX sign_in_code_catalogs_catalog_id ON sign_in_code_catalogs (catalog_id);

-- Access and refresh tokens. A family is every token minted from one
-- authorization code; rotating a refresh token keeps the family.
CREATE TABLE oauth_tokens (
    sha256     bytea PRIMARY KEY CHECK (length(sha256) = 32),
    grant_id   text NOT NULL REFERENCES api_keys (id) ON DELETE CASCADE,
    kind       text NOT NULL CHECK (kind IN ('access', 'refresh')),
    family_id  text NOT NULL CHECK (family_id <> ''),
    resource   text NOT NULL,
    created_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL CHECK (expires_at > created_at),
    -- Refresh tokens: when the token was rotated.
    used_at    timestamptz,
    revoked_at timestamptz
);
CREATE INDEX oauth_tokens_family_id ON oauth_tokens (family_id);
CREATE INDEX oauth_tokens_grant_id ON oauth_tokens (grant_id);
CREATE INDEX oauth_tokens_expires_at ON oauth_tokens (expires_at);
```

> **Corrected (issue #246, 2026-11-11, PR #254 review).** `sign_in_codes_redeemed_grant` was `(redeemed_at IS NULL) = (grant_id IS NULL)`, which the `grant_id ... ON DELETE SET NULL` action would violate on deleting a grant row. It is now `grant_id IS NULL OR redeemed_at IS NOT NULL`: a grant id only on a redeemed code, and a redeemed code may outlive its grant row. Task 4's `redeem` still sets both together. `oauth_schema_keeps_existing_keys_and_guards_grant_rows` also asserts both halves (an unredeemed code with a grant id is refused; deleting a redeemed code's grant clears `grant_id` and keeps `redeemed_at`). The spec's DDL has no such check; Task 8 adds it there.

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test --lib db::tests -- --test-threads=2`
Expected: PASS (`migrations_create_the_schema_and_rerun_as_a_no_op`, `oauth_schema_keeps_existing_keys_and_guards_grant_rows`, and the existing migration tests).

- [ ] **Step 5: Prefixed secret generation**

In `src/keys.rs`, add a failing test to `mod tests`:

```rust
    #[test]
    fn every_secret_kind_has_its_own_prefix() {
        for prefix in [
            KEY_PREFIX,
            ACCESS_TOKEN_PREFIX,
            REFRESH_TOKEN_PREFIX,
            SIGN_IN_CODE_PREFIX,
            AUTHORIZATION_CODE_PREFIX,
        ] {
            let secret = generate_with(prefix).unwrap();

            assert!(secret.starts_with(prefix), "{prefix}");
            assert_eq!(secret.len(), prefix.len() + 43, "{prefix}");
        }
        // No prefix is a prefix of another, so dispatch by prefix is exact.
        assert!(!ACCESS_TOKEN_PREFIX.starts_with(KEY_PREFIX));
        assert!(!REFRESH_TOKEN_PREFIX.starts_with(KEY_PREFIX));
        assert!(!SIGN_IN_CODE_PREFIX.starts_with(KEY_PREFIX));
        assert!(!AUTHORIZATION_CODE_PREFIX.starts_with(KEY_PREFIX));
    }
```

Run: `cargo test --lib keys::tests`. Expected: FAIL (`generate_with` and the prefixes are not defined).

Replace the module doc, the `KEY_PREFIX` constant and `generate` in `src/keys.rs` with:

```rust
//! Secrets: API keys, OAuth access and refresh tokens, sign-in codes and
//! authorization codes. Each is a recognizable prefix plus 32 random bytes.
//! A secret is shown once, when created; only its SHA-256 is stored, and
//! secrets are never logged.
```

```rust
/// Every API key starts with this, so people and secret scanners can recognize one.
pub const KEY_PREFIX: &str = "okf_";

/// Every OAuth access token starts with this (Task 2's bearer dispatch).
pub const ACCESS_TOKEN_PREFIX: &str = "okfat_";

/// Every OAuth refresh token starts with this.
pub const REFRESH_TOKEN_PREFIX: &str = "okfrt_";

/// Every admin-issued sign-in code starts with this.
pub const SIGN_IN_CODE_PREFIX: &str = "okfsc_";

/// Every OAuth authorization code starts with this.
pub const AUTHORIZATION_CODE_PREFIX: &str = "okfac_";

/// Generates a new API key: [`generate_with`] with [`KEY_PREFIX`].
///
/// # Returns
/// The key. Show it once; store only [`hash`] of it.
///
/// # Errors
/// [`OkfError::Io`] if the OS random source fails.
pub fn generate() -> Result<String, OkfError> {
    generate_with(KEY_PREFIX)
}

/// Generates a new secret: `prefix` plus 32 random bytes in unpadded
/// base64url (43 characters).
///
/// # Arguments
/// * `prefix` - one of this module's `*_PREFIX` constants.
///
/// # Returns
/// The secret. Show it once; store only [`hash`] of it.
///
/// # Errors
/// [`OkfError::Io`] if the OS random source fails.
pub fn generate_with(prefix: &str) -> Result<String, OkfError> {
    // A fixed-size array on the stack, filled in place (`&mut` lends it out
    // for writing), like `bytearray(32)` passed to `os.urandom`'s C side.
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| OkfError::Io {
        path: "<os random source>".to_string(),
        source: std::io::Error::other(error),
    })?;
    Ok(format!("{prefix}{}", URL_SAFE_NO_PAD.encode(bytes)))
}
```

Run: `cargo test --lib keys::tests`. Expected: PASS (4 tests).

- [ ] **Step 6: Configuration: failing tests**

Add to `src/config.rs` `mod tests`:

```rust
    /// The defaults with OAuth on at `public_url`, its host allowed.
    fn oauth_on(public_url: &str, allowed: &[&str]) -> Config {
        Config {
            public_url: Some(public_url.to_string()),
            allowed_hosts: allowed.iter().map(|host| host.to_string()).collect(),
            ..Config::default()
        }
    }

    #[test]
    fn oauth_is_off_by_default_with_sensible_lifetimes() {
        let config = Config::default();

        assert_eq!(config.public_url, None);
        assert_eq!(config.oauth.access_token_ttl, Duration::from_secs(60 * 60));
        assert_eq!(
            config.oauth.refresh_token_ttl,
            Duration::from_secs(30 * 24 * 60 * 60)
        );
        assert_eq!(
            config.oauth.code_max_expiry,
            Duration::from_secs(7 * 24 * 60 * 60)
        );
        assert!(config.oauth.dcr_enabled);
        assert!(!config.oauth.cimd_allow_private);
        assert_eq!(config.validate(), Ok(()));
    }

    #[test]
    fn the_oauth_section_reads_from_the_file() {
        let text = "public_url = \"https://okf.example.com\"\n\
                    allowed_hosts = [\"okf.example.com\"]\n\
                    [oauth]\n\
                    access_token_ttl = \"15m\"\n\
                    refresh_token_ttl = \"7d\"\n\
                    code_max_expiry = \"2d\"\n\
                    dcr_enabled = false\n\
                    cimd_allow_private = true\n";

        let config = Config::from_toml(text).unwrap();

        assert_eq!(config.public_url.as_deref(), Some("https://okf.example.com"));
        assert_eq!(config.oauth.access_token_ttl, Duration::from_secs(15 * 60));
        assert_eq!(
            config.oauth.refresh_token_ttl,
            Duration::from_secs(7 * 24 * 60 * 60)
        );
        assert_eq!(
            config.oauth.code_max_expiry,
            Duration::from_secs(2 * 24 * 60 * 60)
        );
        assert!(!config.oauth.dcr_enabled);
        assert!(config.oauth.cimd_allow_private);
        assert_eq!(config.validate(), Ok(()));
    }

    #[test]
    fn public_url_must_be_an_allowed_https_origin() {
        // Accepted: https on an allowed host, with or without a trailing
        // slash or an explicit port that an entry allows; http on loopback.
        for config in [
            oauth_on("https://okf.example.com", &["okf.example.com"]),
            oauth_on("https://okf.example.com/", &["okf.example.com"]),
            oauth_on("https://OKF.example.com", &["okf.example.com"]),
            oauth_on("https://okf.example.com:8443", &["okf.example.com:8443"]),
            oauth_on("https://okf.example.com:8443", &["okf.example.com"]),
            oauth_on("http://127.0.0.1:8080", &["127.0.0.1"]),
            oauth_on("http://localhost:8080", &["localhost"]),
            oauth_on("http://[::1]:8080", &["::1"]),
        ] {
            assert_eq!(config.validate(), Ok(()), "{:?}", config.public_url);
        }
        // Refused, each with a reason that names the problem.
        for (config, needle) in [
            (oauth_on("http://okf.example.com", &["okf.example.com"]), "https"),
            (oauth_on("ftp://okf.example.com", &["okf.example.com"]), "https"),
            (oauth_on("https://okf.example.com/okf", &["okf.example.com"]), "origin"),
            (oauth_on("https://okf.example.com/?a=b", &["okf.example.com"]), "origin"),
            (oauth_on("okf.example.com", &["okf.example.com"]), "scheme"),
            (oauth_on("https://other.example.com", &["okf.example.com"]), "allowed_hosts"),
            (oauth_on("https://okf.example.com", &["okf.example.com:8443"]), "allowed_hosts"),
            (oauth_on("not a url", &["okf.example.com"]), "URL"),
        ] {
            let error = config.validate().expect_err("refused");
            assert!(error.contains(needle), "{:?}: {error}", config.public_url);
        }
    }

    #[test]
    fn oauth_lifetimes_must_be_positive() {
        for change in [
            |oauth: &mut OAuthConfig| oauth.access_token_ttl = Duration::ZERO,
            |oauth: &mut OAuthConfig| oauth.refresh_token_ttl = Duration::ZERO,
            |oauth: &mut OAuthConfig| oauth.code_max_expiry = Duration::ZERO,
        ] {
            let mut config = Config::default();
            change(&mut config.oauth);

            let error = config.validate().expect_err("refused");

            assert!(error.contains("more than zero"), "{error}");
        }
    }
```

Run: `cargo test --lib config::tests`. Expected: FAIL (no `public_url`, `oauth` or `OAuthConfig`).

- [ ] **Step 7: Configuration: implementation**

In `src/config.rs`, add to the imports:

```rust
// `Uri` and `Authority` are the `http` crate's parsed URL and `host[:port]`
// types, re-exported by axum; the MCP `Host` check parses `allowed_hosts`
// with the same `Authority`.
use axum::http::uri::{Authority, Uri};
```

Add after `DEFAULT_OVERLAY_CACHE_SIZE`:

```rust
/// The default lifetime of an OAuth access token.
pub const DEFAULT_ACCESS_TOKEN_TTL: Duration = Duration::from_secs(60 * 60);

/// The default lifetime of an OAuth refresh token (restarted by each rotation).
pub const DEFAULT_REFRESH_TOKEN_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// The default cap on a sign-in code's `--expires`.
pub const DEFAULT_CODE_MAX_EXPIRY: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// The `[oauth]` section: used only when `public_url` is set.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct OAuthConfig {
    /// How long an access token is accepted (`"1h"`).
    #[serde(with = "humantime_serde")]
    pub access_token_ttl: Duration,
    /// How long a refresh token is accepted after it is issued (`"30d"`);
    /// each rotation issues a new one, so the lifetime slides.
    #[serde(with = "humantime_serde")]
    pub refresh_token_ttl: Duration,
    /// The longest `okf-mcp oauth code create --expires` accepts (`"7d"`).
    #[serde(with = "humantime_serde")]
    pub code_max_expiry: Duration,
    /// Whether `POST /oauth/register` (RFC 7591) is offered.
    pub dcr_enabled: bool,
    /// Lets a Client ID Metadata Document live on a private or loopback
    /// address. Only for test rigs: it opens the fetcher to the internal network.
    pub cimd_allow_private: bool,
}

impl Default for OAuthConfig {
    fn default() -> Self {
        Self {
            access_token_ttl: DEFAULT_ACCESS_TOKEN_TTL,
            refresh_token_ttl: DEFAULT_REFRESH_TOKEN_TTL,
            code_max_expiry: DEFAULT_CODE_MAX_EXPIRY,
            dcr_enabled: true,
            cimd_allow_private: false,
        }
    }
}
```

Add two fields at the end of `Config`:

```rust
    /// The externally visible origin (`"https://okf.example.com"`). Setting
    /// it turns OAuth on: it is the issuer, and `<public_url>/mcp` is the
    /// resource tokens are issued for. Its host must be in `allowed_hosts`.
    pub public_url: Option<String>,
    /// The OAuth settings (`[oauth]`).
    pub oauth: OAuthConfig,
```

and to `Default for Config`:

```rust
            public_url: None,
            oauth: OAuthConfig::default(),
```

In `Config::validate`, extend the doc comment's `# Errors` list with "a zero OAuth lifetime, or a `public_url` that is not an `https` origin (`http` on a loopback host only) whose host `allowed_hosts` admits", and add before the final `Ok(())`:

```rust
        for (name, value) in [
            ("oauth.access_token_ttl", self.oauth.access_token_ttl),
            ("oauth.refresh_token_ttl", self.oauth.refresh_token_ttl),
            ("oauth.code_max_expiry", self.oauth.code_max_expiry),
        ] {
            if value.is_zero() {
                return Err(format!("{name} must be more than zero"));
            }
            if chrono::Duration::from_std(value).is_err() {
                return Err(format!("{name} exceeds supported duration"));
            }
        }
        self.validate_public_url()?;
```

Add these to `impl Config` and as free functions below it:

```rust
    /// Checks `public_url`: an `https` origin (`http` only on a loopback
    /// host, for local testing), with no path or query, whose host an
    /// `allowed_hosts` entry admits under the MCP `Host` check's rule (same
    /// host; the entry's port, if it has one, equal to the URL's).
    ///
    /// # Errors
    /// The problem in words.
    fn validate_public_url(&self) -> Result<(), String> {
        let Some(public_url) = self.public_url.as_deref() else {
            return Ok(());
        };
        let uri: Uri = public_url
            .parse()
            .map_err(|_| format!("public_url {public_url:?} is not a URL"))?;
        // A tuple pattern matches both `Option`s at once.
        let (Some(scheme), Some(host)) = (uri.scheme_str(), uri.host()) else {
            return Err(format!("public_url {public_url:?} needs a scheme and a host"));
        };
        if !matches!(uri.path(), "" | "/") || uri.query().is_some() {
            return Err(format!(
                "public_url {public_url:?} must be an origin, with no path or query"
            ));
        }
        let host = normalize_host(host);
        let secure = scheme.eq_ignore_ascii_case("https")
            || (scheme.eq_ignore_ascii_case("http") && is_loopback(&host));
        if !secure {
            return Err(format!(
                "public_url {public_url:?} must use https (http only on a loopback host)"
            ));
        }
        let port = uri.port_u16();
        let admitted = self.allowed_hosts.iter().any(|entry| {
            // An entry that is not an authority (rmcp's fallback) is a bare host.
            let (entry_host, entry_port) = match entry.trim().parse::<Authority>() {
                Ok(authority) => (normalize_host(authority.host()), authority.port_u16()),
                Err(_) => (normalize_host(entry.trim()), None),
            };
            entry_host == host && entry_port.is_none_or(|entry_port| Some(entry_port) == port)
        });
        if !admitted {
            return Err(format!(
                "public_url's host {host:?} is not in allowed_hosts, so MCP requests to it \
                 would be refused; add it"
            ));
        }
        Ok(())
    }
```

```rust
/// A host as the MCP `Host` check compares it: lowercase, IPv6 brackets trimmed.
fn normalize_host(host: &str) -> String {
    host.trim_matches(|c| c == '[' || c == ']').to_ascii_lowercase()
}

/// Is `host` (normalized) this machine?
fn is_loopback(host: &str) -> bool {
    // `parse::<IpAddr>()` reads either address family; `is_ok_and` is
    // Python's `try: ... except: False` around a test.
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}
```

(`Option::is_none_or` is stable since Rust 1.82.)

Run: `cargo test --lib config::tests`. Expected: PASS.

- [ ] **Step 8: Key rows with a kind and a client: failing test**

Add to `src/store/keys.rs` `mod tests`:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn list_shows_a_grant_with_its_kind_and_client_and_no_hash() {
        let db = test_db().await;
        catalogs::create(&db.pool, "main", "", now()).await.unwrap();
        let names = ["main".to_string()];
        let key = create(&db.pool, &new_key("laptop", Grants::Catalogs(&names), None), now())
            .await
            .unwrap();
        let client = crate::store::oauth::testing::insert_client(
            &db.pool,
            "https://client.example.com/client.json",
            "Example Client",
            now(),
        )
        .await;
        let grant = crate::store::oauth::testing::insert_grant(
            &db.pool,
            &client,
            Access::Read,
            &["main"],
            now(),
        )
        .await;

        let records = list(&db.pool).await.unwrap();

        let key_row = records.iter().find(|record| record.name == "laptop" && record.kind == KeyKind::Key).unwrap();
        assert!(crate::keys::matches(key_row.sha256.as_deref().unwrap(), &key));
        assert_eq!(key_row.client_id, None);
        assert_eq!(key_row.client_name, None);
        let grant_row = records.iter().find(|record| record.id == grant).unwrap();
        assert_eq!(grant_row.kind, KeyKind::OAuth);
        assert_eq!(grant_row.sha256, None);
        assert_eq!(grant_row.client_id.as_deref(), Some(client.as_str()));
        assert_eq!(grant_row.client_name.as_deref(), Some("Example Client"));
        assert_eq!(grant_row.catalogs, ["main"]);
        assert_eq!(grant_row.access, Access::Read);
    }
```

The test uses `src/store/oauth/testing.rs`, which this step creates:

```rust
//! Test helpers for the OAuth store (`#[cfg(test)]`): rows a test needs
//! before the endpoints that make them exist, written with plain SQL.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::store::keys::Access;

/// Stores a Client ID Metadata Document client whose one redirect URI is
/// `https://client.example.com/callback`.
///
/// # Arguments
/// * `pool` - the database.
/// * `client_id` - the document URL (the client's id).
/// * `client_name` - its display name.
/// * `now` - its creation and fetch time.
///
/// # Returns
/// The client id.
///
/// # Panics
/// If the insert fails: a broken test setup.
pub(crate) async fn insert_client(
    pool: &PgPool,
    client_id: &str,
    client_name: &str,
    now: DateTime<Utc>,
) -> String {
    sqlx::query(
        "INSERT INTO oauth_clients \
         (client_id, kind, client_name, redirect_uris, metadata, fetched_at, cache_expires_at, \
          created_at) \
         VALUES ($1, 'cimd', $2, ARRAY['https://client.example.com/callback'], \
                 jsonb_build_object('client_id', $1::text, 'client_name', $2::text), $3, \
                 $3 + interval '1 hour', $3)",
    )
    .bind(client_id)
    .bind(client_name)
    .bind(now)
    .execute(pool)
    .await
    .expect("insert a test client");
    client_id.to_string()
}

/// Stores a live OAuth grant for `client_id` and bumps the administrative
/// generation, as redeeming a sign-in code will (Task 4), so the next
/// `App::poll` loads it.
///
/// # Arguments
/// * `pool` - the database.
/// * `client_id` - its client (already stored).
/// * `access` - `Read` or `Write` (the schema refuses `Admin`).
/// * `catalogs` - the catalogs it reaches (each must exist).
/// * `now` - its creation time.
///
/// # Returns
/// The grant's id.
///
/// # Panics
/// If an insert fails: a broken test setup.
pub(crate) async fn insert_grant(
    pool: &PgPool,
    client_id: &str,
    access: Access,
    catalogs: &[&str],
    now: DateTime<Utc>,
) -> String {
    let mut tx = pool.begin().await.expect("begin");
    let id: String = sqlx::query_scalar(
        "INSERT INTO api_keys (name, access, all_catalogs, actor, created_at, kind, client_id) \
         VALUES ('laptop', $1, false, 'example-actor', $2, 'oauth', $3) RETURNING id",
    )
    .bind(access.as_str())
    .bind(now)
    .bind(client_id)
    .fetch_one(&mut *tx)
    .await
    .expect("insert a test grant");
    sqlx::query(
        "INSERT INTO key_catalogs (key_id, catalog_id) \
         SELECT $1, id FROM catalogs WHERE name = ANY($2)",
    )
    .bind(&id)
    .bind(catalogs)
    .execute(&mut *tx)
    .await
    .expect("grant the test grant its catalogs");
    crate::store::catalogs::bump_admin_generation(&mut tx)
        .await
        .expect("bump the generation");
    tx.commit().await.expect("commit");
    id
}
```

Create `src/store/oauth/mod.rs` now too (the store files of Step 10 are added to it then; until then it declares `testing` only):

```rust
//! The OAuth tables (M10): clients (`clients`), sign-in codes (`codes`) and
//! tokens (`tokens`). Grants are `api_keys` rows of kind `oauth`
//! (`store::keys`).

#[cfg(test)]
pub(crate) mod testing;
```

and add `pub mod oauth;` to `src/store/mod.rs` after `pub mod maintenance;`.

Run: `cargo test --lib store::keys::tests::list_shows_a_grant`. Expected: FAIL to compile (`KeyKind`, `client_id`, `client_name`).

- [ ] **Step 9: Key rows: implementation and fallout**

In `src/store/keys.rs`:

1. Add after `impl Access`:

```rust
/// What a key row is: an API key, or an OAuth grant (a key row with no
/// hash, reached through its tokens).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    /// An `okf_` API key.
    Key,
    /// A grant made by redeeming a sign-in code.
    OAuth,
}

impl KeyKind {
    /// The spelling stored in `api_keys.kind`.
    ///
    /// # Returns
    /// `"key"` or `"oauth"`.
    pub fn as_str(self) -> &'static str {
        match self {
            KeyKind::Key => "key",
            KeyKind::OAuth => "oauth",
        }
    }

    /// Parses the stored spelling.
    ///
    /// # Arguments
    /// * `text` - an `api_keys.kind` value.
    ///
    /// # Returns
    /// The kind, or `None` for anything else.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "key" => Some(KeyKind::Key),
            "oauth" => Some(KeyKind::OAuth),
            _ => None,
        }
    }
}
```

2. In `KeyRecord`, replace the `sha256` field and add three fields after `revoked_at`:

```rust
    /// The key's SHA-256; `None` for an OAuth grant, which has no secret of its own.
    pub sha256: Option<Vec<u8>>,
```

```rust
    /// An API key or an OAuth grant.
    pub kind: KeyKind,
    /// A grant's OAuth client (`None` for a key, or once the client is gone).
    pub client_id: Option<String>,
    /// That client's display name, if it gave one.
    pub client_name: Option<String>,
```

3. In `KeyRow`, make `sha256: Option<Vec<u8>>` and add `kind: String, client_id: Option<String>, client_name: Option<String>`.

4. Make `catalog_ids` `pub(crate)` (Task 4's `codes::create` resolves catalogs with it), with this doc comment:

```rust
/// The ids of the catalogs called `names`, in the same order, in one query.
/// Shared with the sign-in codes (`store::oauth::codes`).
///
/// # Arguments
/// * `conn` - a connection or open transaction.
/// * `names` - catalog names.
///
/// # Returns
/// Their ids, in `names`' order.
///
/// # Errors
/// [`OkfError::CatalogNotSelected`] for the first name with no catalog;
/// [`OkfError::Database`].
pub(crate) async fn catalog_ids(
```

5. Replace the query and mapping in `list`:

```rust
    let rows = sqlx::query_as::<_, KeyRow>(
        "SELECT k.id, k.name, k.sha256, k.access, k.maintenance_report, k.all_catalogs, \
                d.name AS default_catalog, k.actor, k.created_at, k.revoked_at, \
                k.kind, k.client_id, oc.client_name, \
                COALESCE(array_agg(c.name ORDER BY c.name) FILTER (WHERE c.name IS NOT NULL), \
                         '{}'::text[]) AS catalogs \
         FROM api_keys k \
         LEFT JOIN catalogs d ON d.id = k.default_catalog_id \
         LEFT JOIN oauth_clients oc ON oc.client_id = k.client_id \
         LEFT JOIN key_catalogs g ON g.key_id = k.id \
         LEFT JOIN catalogs c ON c.id = g.catalog_id \
         GROUP BY k.id, d.name, oc.client_name ORDER BY k.name, k.actor, k.created_at, k.id",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| KeyRecord {
            id: row.id,
            name: row.name,
            sha256: row.sha256,
            access: Access::parse(&row.access).expect("the schema checks api_keys.access"),
            maintenance_report: row.maintenance_report,
            all_catalogs: row.all_catalogs,
            catalogs: row.catalogs,
            default_catalog: row.default_catalog,
            actor: row.actor,
            created_at: row.created_at,
            revoked_at: row.revoked_at,
            kind: KeyKind::parse(&row.kind).expect("the schema checks api_keys.kind"),
            client_id: row.client_id,
            client_name: row.client_name,
        })
        .collect())
```

(Grouping by `k.id`, the primary key, lets the query select every `k` column. `oc.client_name` comes from another table, so it is grouped too.)

6. In the existing test that ends `assert!(crate::keys::matches(&record.sha256, &key));` (around line 797), make it:

```rust
        assert!(crate::keys::matches(
            record.sha256.as_deref().expect("a key row has a hash"),
            &key
        ));
        assert_eq!(record.kind, KeyKind::Key);
```

Fallout elsewhere:

- `src/cli.rs` test `key_list_columns_have_the_documented_order`: `sha256: None,` and add `kind: keys::KeyKind::Key, client_id: None, client_name: None,`. The CLI's output is unchanged. Task 4 adds `oauth grant list`.
- `src/http/api/keys.rs`, in `KeyView`, add after `access`:

```rust
    /// `key` or `oauth` (a grant made by redeeming a sign-in code).
    pub kind: &'static str,
    /// A grant's client, as it names itself (`None` for a key).
    pub client_name: Option<String>,
```

  In its `From<KeyRecord>`, add `kind, client_id: _, client_name,` to the destructuring (with the comment "`client_id: _`: a URL or UUID the console has no use for") and `kind: kind.as_str(), client_name,` to the struct.
- `src/auth.rs`:
  - in `KeyCache::from_records`, `record.sha256.as_slice()` becomes `record.sha256.as_deref()?` (see Step 12, which replaces the function whole);
  - in the test helper `record`, `sha256: Some(keys::hash(key).to_vec()),` plus `kind: KeyKind::Key, client_id: None, client_name: None,`;
  - in `the_cache_finds_live_keys_only`, `sha256: Some(vec![0; 5]),`;
  - add `KeyKind` to the `use crate::store::keys::{...}` line.

Run: `cargo test --lib store::keys` and `cargo test --lib http::api::keys`. Expected: PASS. The new `list_shows_a_grant...` test passes, and the API key tests are unchanged, since JSON gains two fields and no test compares a whole object.

- [ ] **Step 10: OAuth store: tokens, clients, codes**

Create `src/store/oauth/tokens.rs`:

```rust
//! `oauth_tokens`: access and refresh tokens, by SHA-256. A family is every
//! token minted from one authorization code (Task 6 rotates within it).

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};

use crate::error::OkfError;

/// An access or a refresh token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// `okfat_`: presented on `/mcp` and `/api/v1`.
    Access,
    /// `okfrt_`: exchanged at `/oauth/token` for a new pair.
    Refresh,
}

impl TokenKind {
    /// The spelling stored in `oauth_tokens.kind`.
    ///
    /// # Returns
    /// `"access"` or `"refresh"`.
    pub fn as_str(self) -> &'static str {
        match self {
            TokenKind::Access => "access",
            TokenKind::Refresh => "refresh",
        }
    }

    /// Parses the stored spelling.
    ///
    /// # Arguments
    /// * `text` - an `oauth_tokens.kind` value.
    ///
    /// # Returns
    /// The kind, or `None` for anything else.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "access" => Some(TokenKind::Access),
            "refresh" => Some(TokenKind::Refresh),
            _ => None,
        }
    }
}

/// A token to store. It holds the hash only, never the token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewToken<'a> {
    /// `keys::hash` of the token.
    pub sha256: [u8; 32],
    /// The grant (`api_keys.id`) it acts for.
    pub grant_id: &'a str,
    /// Access or refresh.
    pub kind: TokenKind,
    /// Its family (one per authorization code).
    pub family_id: &'a str,
    /// The resource it was issued for (the audience).
    pub resource: &'a str,
    /// When it stops being accepted.
    pub expires_at: DateTime<Utc>,
}

/// A live access token's facts, as the bearer check needs them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveAccessToken {
    /// The grant it acts for.
    pub grant_id: String,
    /// The resource it was issued for.
    pub resource: String,
    /// When it stops being accepted.
    pub expires_at: DateTime<Utc>,
}

/// A token [`revoke`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevokedToken {
    /// Its family.
    pub family_id: String,
    /// Access or refresh (revoking a refresh token revokes its family, Task 6).
    pub kind: TokenKind,
}

/// Stores a token.
///
/// # Arguments
/// * `conn` - a connection or an open transaction (minting stores an access
///   and a refresh token together).
/// * `new` - the token.
/// * `now` - its creation time (server clock).
///
/// # Errors
/// [`OkfError::Database`], including a hash collision (a duplicate primary key).
pub async fn insert(
    conn: &mut PgConnection,
    new: &NewToken<'_>,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    sqlx::query(
        "INSERT INTO oauth_tokens \
         (sha256, grant_id, kind, family_id, resource, created_at, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(new.sha256.as_slice())
    .bind(new.grant_id)
    .bind(new.kind.as_str())
    .bind(new.family_id)
    .bind(new.resource)
    .bind(now)
    .bind(new.expires_at)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// A live access token by hash: not revoked, not expired. Whether its grant
/// is still live is the key cache's to say (`Keys::find_grant`).
///
/// # Arguments
/// * `pool` - the database.
/// * `sha256` - `keys::hash` of the presented token.
/// * `now` - the request's time (server clock).
///
/// # Returns
/// Its facts, or `None`.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn find_access(
    pool: &PgPool,
    sha256: &[u8; 32],
    now: DateTime<Utc>,
) -> Result<Option<LiveAccessToken>, OkfError> {
    let row: Option<(String, String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT grant_id, resource, expires_at FROM oauth_tokens \
         WHERE sha256 = $1 AND kind = 'access' AND revoked_at IS NULL AND expires_at > $2",
    )
    .bind(sha256.as_slice())
    .bind(now)
    .fetch_optional(pool)
    .await?;
    // `Option::map` with a closure that destructures the tuple.
    Ok(row.map(|(grant_id, resource, expires_at)| LiveAccessToken {
        grant_id,
        resource,
        expires_at,
    }))
}

/// Revokes one token (RFC 7009). Revoking an already revoked token keeps
/// its first revocation time and still reports it.
///
/// # Arguments
/// * `pool` - the database.
/// * `sha256` - `keys::hash` of the token.
/// * `now` - the revocation time (server clock).
///
/// # Returns
/// Its family and kind, or `None` for an unknown token.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn revoke(
    pool: &PgPool,
    sha256: &[u8; 32],
    now: DateTime<Utc>,
) -> Result<Option<RevokedToken>, OkfError> {
    let row: Option<(String, String)> = sqlx::query_as(
        "UPDATE oauth_tokens SET revoked_at = COALESCE(revoked_at, $2) \
         WHERE sha256 = $1 RETURNING family_id, kind",
    )
    .bind(sha256.as_slice())
    .bind(now)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(family_id, kind)| RevokedToken {
        family_id,
        kind: TokenKind::parse(&kind).expect("the schema checks oauth_tokens.kind"),
    }))
}

/// Deletes every token that expired before `expired_before`.
///
/// # Arguments
/// * `pool` - the database.
/// * `expired_before` - the cut-off (the poll loop passes a week ago).
///
/// # Returns
/// How many were deleted.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn prune(pool: &PgPool, expired_before: DateTime<Utc>) -> Result<u64, OkfError> {
    let deleted = sqlx::query("DELETE FROM oauth_tokens WHERE expires_at < $1")
        .bind(expired_before)
        .execute(pool)
        .await?;
    Ok(deleted.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeDelta, TimeZone};

    use crate::db::test_db;
    use crate::store::catalogs;
    use crate::store::keys::Access;
    use crate::store::oauth::testing::{insert_client, insert_grant};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 11, 12, 0, 0).unwrap()
    }

    /// A database with one grant; returns it with its id.
    async fn with_grant() -> (crate::db::pg_support::TestDb, String) {
        let db = test_db().await;
        catalogs::create(&db.pool, "main", "", now()).await.unwrap();
        let client =
            insert_client(&db.pool, "https://client.example.com/client.json", "Example", now())
                .await;
        let grant = insert_grant(&db.pool, &client, Access::Write, &["main"], now()).await;
        (db, grant)
    }

    async fn store(db: &crate::db::pg_support::TestDb, new: &NewToken<'_>) {
        let mut conn = db.pool.acquire().await.unwrap();
        insert(&mut conn, new, now()).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn find_access_returns_live_access_tokens_only() {
        let (db, grant) = with_grant().await;
        let token = |byte: u8, kind: TokenKind, expires_at: DateTime<Utc>| NewToken {
            sha256: [byte; 32],
            grant_id: &grant,
            kind,
            family_id: "family-1",
            resource: "https://okf.example.com/mcp",
            expires_at,
        };
        let later = now() + TimeDelta::hours(1);
        store(&db, &token(1, TokenKind::Access, later)).await;
        store(&db, &token(2, TokenKind::Refresh, later)).await;
        store(&db, &token(3, TokenKind::Access, now() + TimeDelta::seconds(1))).await;
        store(&db, &token(4, TokenKind::Access, later)).await;
        revoke(&db.pool, &[4; 32], now()).await.unwrap();

        let live = find_access(&db.pool, &[1; 32], now()).await.unwrap();
        let refresh = find_access(&db.pool, &[2; 32], now()).await.unwrap();
        let expired =
            find_access(&db.pool, &[3; 32], now() + TimeDelta::seconds(1)).await.unwrap();
        let revoked = find_access(&db.pool, &[4; 32], now()).await.unwrap();
        let unknown = find_access(&db.pool, &[5; 32], now()).await.unwrap();

        assert_eq!(
            live,
            Some(LiveAccessToken {
                grant_id: grant.clone(),
                resource: "https://okf.example.com/mcp".to_string(),
                expires_at: later,
            })
        );
        assert_eq!((refresh, expired, revoked, unknown), (None, None, None, None));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revoke_reports_the_family_and_kind_and_is_idempotent() {
        let (db, grant) = with_grant().await;
        store(
            &db,
            &NewToken {
                sha256: [6; 32],
                grant_id: &grant,
                kind: TokenKind::Refresh,
                family_id: "family-2",
                resource: "https://okf.example.com/mcp",
                expires_at: now() + TimeDelta::days(30),
            },
        )
        .await;
        let expected = Some(RevokedToken {
            family_id: "family-2".to_string(),
            kind: TokenKind::Refresh,
        });

        let first = revoke(&db.pool, &[6; 32], now()).await.unwrap();
        let second = revoke(&db.pool, &[6; 32], now() + TimeDelta::hours(1)).await.unwrap();
        let unknown = revoke(&db.pool, &[7; 32], now()).await.unwrap();

        assert_eq!(first, expected);
        assert_eq!(second, expected);
        assert_eq!(unknown, None);
        let revoked_at: DateTime<Utc> =
            sqlx::query_scalar("SELECT revoked_at FROM oauth_tokens WHERE sha256 = $1")
                .bind([6u8; 32].as_slice())
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(revoked_at, now(), "the first revocation time stays");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prune_deletes_only_tokens_expired_before_the_cut_off() {
        let (db, grant) = with_grant().await;
        for (byte, expires_at) in [
            (1u8, now() - TimeDelta::days(8)),
            (2, now() - TimeDelta::days(6)),
            (3, now() + TimeDelta::hours(1)),
        ] {
            sqlx::query(
                "INSERT INTO oauth_tokens \
                 (sha256, grant_id, kind, family_id, resource, created_at, expires_at) \
                 VALUES ($1, $2, 'access', 'f', 'https://okf.example.com/mcp', $3, $4)",
            )
            .bind([byte; 32].as_slice())
            .bind(&grant)
            .bind(expires_at - TimeDelta::hours(1))
            .bind(expires_at)
            .execute(&db.pool)
            .await
            .unwrap();
        }

        let pruned = prune(&db.pool, now() - TimeDelta::days(7)).await.unwrap();

        assert_eq!(pruned, 1);
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM oauth_tokens")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(left, 2);
    }
}
```

Create `src/store/oauth/clients.rs`:

```rust
//! `oauth_clients`: dynamically registered clients (RFC 7591) and cached
//! Client ID Metadata Documents. Task 2 reads and prunes; Task 3 writes.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::error::OkfError;

/// How a client got its id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientKind {
    /// Registered at `POST /oauth/register`; its id is a UUID.
    Dcr,
    /// A Client ID Metadata Document; its id is the document's `https` URL.
    Cimd,
}

impl ClientKind {
    /// The spelling stored in `oauth_clients.kind`.
    ///
    /// # Returns
    /// `"dcr"` or `"cimd"`.
    pub fn as_str(self) -> &'static str {
        match self {
            ClientKind::Dcr => "dcr",
            ClientKind::Cimd => "cimd",
        }
    }

    /// Parses the stored spelling.
    ///
    /// # Arguments
    /// * `text` - an `oauth_clients.kind` value.
    ///
    /// # Returns
    /// The kind, or `None` for anything else.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "dcr" => Some(ClientKind::Dcr),
            "cimd" => Some(ClientKind::Cimd),
            _ => None,
        }
    }
}

/// One client.
// `PartialEq` but not `Eq`: `serde_json::Value` holds floats.
#[derive(Debug, Clone, PartialEq)]
pub struct ClientRecord {
    /// Its id (a UUID, or the metadata document's URL).
    pub client_id: String,
    /// DCR or CIMD.
    pub kind: ClientKind,
    /// The name it gives itself (shown on the consent page, escaped).
    pub client_name: Option<String>,
    /// Its registered redirect URIs, matched exactly.
    pub redirect_uris: Vec<String>,
    /// Its whole metadata, as registered or fetched.
    pub metadata: serde_json::Value,
    /// When its document was last fetched (CIMD only).
    pub fetched_at: Option<DateTime<Utc>>,
    /// Until when the cached document may be used without a refetch (CIMD only).
    pub cache_expires_at: Option<DateTime<Utc>>,
    /// When it was first stored.
    pub created_at: DateTime<Utc>,
}

/// The select list every client query reads, in [`ClientRow`]'s order
/// (Task 3's writes `RETURNING` it too).
// A macro, not a `const`: sqlx 0.9 takes only a `&'static str` (or an
// audited `AssertSqlSafe`) as SQL, and `concat!("SELECT ", client_columns!(),
// ...)` builds that literal at compile time, where `format!` would build a
// `String` at run time.
macro_rules! client_columns {
    () => {
        "client_id, kind, client_name, redirect_uris, metadata, fetched_at, cache_expires_at, \
         created_at"
    };
}

/// One `oauth_clients` row as stored.
// `derive(sqlx::FromRow)` maps columns to fields by name, like a dataclass
// built from a row dict.
#[derive(sqlx::FromRow)]
struct ClientRow {
    client_id: String,
    kind: String,
    client_name: Option<String>,
    redirect_uris: Vec<String>,
    metadata: serde_json::Value,
    fetched_at: Option<DateTime<Utc>>,
    cache_expires_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

impl From<ClientRow> for ClientRecord {
    fn from(row: ClientRow) -> Self {
        Self {
            client_id: row.client_id,
            kind: ClientKind::parse(&row.kind).expect("the schema checks oauth_clients.kind"),
            client_name: row.client_name,
            redirect_uris: row.redirect_uris,
            metadata: row.metadata,
            fetched_at: row.fetched_at,
            cache_expires_at: row.cache_expires_at,
            created_at: row.created_at,
        }
    }
}

/// A client by id.
///
/// # Arguments
/// * `pool` - the database.
/// * `client_id` - its id.
///
/// # Returns
/// The client, or `None`.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn find(pool: &PgPool, client_id: &str) -> Result<Option<ClientRecord>, OkfError> {
    let row = sqlx::query_as::<_, ClientRow>(concat!(
        "SELECT ",
        client_columns!(),
        " FROM oauth_clients WHERE client_id = $1"
    ))
    .bind(client_id)
    .fetch_optional(pool)
    .await?;
    // `Option::map(ClientRecord::from)` converts the row, if there is one.
    Ok(row.map(ClientRecord::from))
}

/// Deletes the dynamically registered clients created before
/// `created_before` that no grant names: registrations that never led to a
/// sign-in. Metadata-document clients are a cache and stay.
///
/// # Arguments
/// * `pool` - the database.
/// * `created_before` - the cut-off (the poll loop passes a day ago).
///
/// # Returns
/// How many were deleted.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn prune_unbound_dcr(
    pool: &PgPool,
    created_before: DateTime<Utc>,
) -> Result<u64, OkfError> {
    let deleted = sqlx::query(
        "DELETE FROM oauth_clients c WHERE c.kind = 'dcr' AND c.created_at < $1 \
         AND NOT EXISTS (SELECT 1 FROM api_keys k WHERE k.client_id = c.client_id)",
    )
    .bind(created_before)
    .execute(pool)
    .await?;
    Ok(deleted.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeDelta, TimeZone};

    use crate::db::test_db;
    use crate::store::catalogs;
    use crate::store::keys::Access;
    use crate::store::oauth::testing::{insert_client, insert_grant};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 11, 12, 0, 0).unwrap()
    }

    async fn insert_dcr(pool: &PgPool, client_id: &str, created_at: DateTime<Utc>) {
        sqlx::query(
            "INSERT INTO oauth_clients (client_id, kind, redirect_uris, metadata, created_at) \
             VALUES ($1, 'dcr', ARRAY['https://client.example.com/callback'], '{}', $2)",
        )
        .bind(client_id)
        .bind(created_at)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn find_reads_a_client_back() {
        let db = test_db().await;
        let id =
            insert_client(&db.pool, "https://client.example.com/client.json", "Example", now())
                .await;

        let found = find(&db.pool, &id).await.unwrap().expect("stored");
        let missing = find(&db.pool, "no-such-client").await.unwrap();

        assert_eq!(found.kind, ClientKind::Cimd);
        assert_eq!(found.client_name.as_deref(), Some("Example"));
        assert_eq!(found.redirect_uris, ["https://client.example.com/callback"]);
        assert_eq!(found.metadata["client_id"], id.as_str());
        assert_eq!(found.fetched_at, Some(now()));
        assert_eq!(found.cache_expires_at, Some(now() + TimeDelta::hours(1)));
        assert_eq!(missing, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prune_keeps_bound_recent_and_document_clients() {
        let db = test_db().await;
        catalogs::create(&db.pool, "main", "", now()).await.unwrap();
        let old = now() - TimeDelta::hours(25);
        insert_dcr(&db.pool, "old-unbound", old).await;
        insert_dcr(&db.pool, "old-bound", old).await;
        insert_dcr(&db.pool, "recent-unbound", now()).await;
        insert_client(&db.pool, "https://client.example.com/client.json", "Example", old).await;
        insert_grant(&db.pool, "old-bound", Access::Read, &["main"], now()).await;

        let pruned = prune_unbound_dcr(&db.pool, now() - TimeDelta::hours(24)).await.unwrap();

        assert_eq!(pruned, 1);
        assert!(find(&db.pool, "old-unbound").await.unwrap().is_none());
        for kept in ["old-bound", "recent-unbound", "https://client.example.com/client.json"] {
            assert!(find(&db.pool, kept).await.unwrap().is_some(), "{kept}");
        }
    }
}
```

Create `src/store/oauth/codes.rs`. Task 2 has only `prune`; Task 4 adds the types and the rest:

```rust
//! `sign_in_codes`: admin-issued one-time codes, each bound to a grant
//! template (catalogs, access, default catalog, actor). Task 2 prunes;
//! Task 4 creates, lists, finds, revokes and redeems.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::error::OkfError;

/// Deletes the unredeemed codes that expired before `expired_before`.
/// Redeemed codes stay: they record which code made which grant.
///
/// # Arguments
/// * `pool` - the database.
/// * `expired_before` - the cut-off (the poll loop passes a week ago).
///
/// # Returns
/// How many were deleted.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn prune(pool: &PgPool, expired_before: DateTime<Utc>) -> Result<u64, OkfError> {
    let deleted =
        sqlx::query("DELETE FROM sign_in_codes WHERE redeemed_at IS NULL AND expires_at < $1")
            .bind(expired_before)
            .execute(pool)
            .await?;
    Ok(deleted.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeDelta, TimeZone};

    use crate::db::test_db;
    use crate::store::catalogs;
    use crate::store::keys::Access;
    use crate::store::oauth::testing::{insert_client, insert_grant};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 11, 12, 0, 0).unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prune_deletes_only_old_unredeemed_codes() {
        let db = test_db().await;
        catalogs::create(&db.pool, "main", "", now()).await.unwrap();
        let client =
            insert_client(&db.pool, "https://client.example.com/client.json", "Example", now())
                .await;
        let grant = insert_grant(&db.pool, &client, Access::Read, &["main"], now()).await;
        let old = now() - TimeDelta::days(8);
        for (byte, expires_at, grant_id) in [
            (1u8, old, None),
            (2, old, Some(grant.as_str())),
            (3, now() - TimeDelta::days(6), None),
            (4, now() + TimeDelta::hours(1), None),
        ] {
            sqlx::query(
                "INSERT INTO sign_in_codes \
                 (name, sha256, access, all_catalogs, actor, created_at, expires_at, redeemed_at, grant_id) \
                 VALUES ('laptop', $1, 'read', true, 'example-actor', $2, $3, $4, $5)",
            )
            .bind([byte; 32].as_slice())
            .bind(expires_at - TimeDelta::hours(24))
            .bind(expires_at)
            .bind(grant_id.map(|_| expires_at - TimeDelta::hours(1)))
            .bind(grant_id)
            .execute(&db.pool)
            .await
            .unwrap();
        }

        let pruned = prune(&db.pool, now() - TimeDelta::days(7)).await.unwrap();

        assert_eq!(pruned, 1);
        let left: Vec<Vec<u8>> =
            sqlx::query_scalar("SELECT sha256 FROM sign_in_codes ORDER BY sha256")
                .fetch_all(&db.pool)
                .await
                .unwrap();
        assert_eq!(left, [vec![2u8; 32], vec![3u8; 32], vec![4u8; 32]]);
    }
}
```

Replace `src/store/oauth/mod.rs` with:

```rust
//! The OAuth tables (M10): clients (`clients`), sign-in codes (`codes`) and
//! tokens (`tokens`). Grants are `api_keys` rows of kind `oauth`
//! (`store::keys`).

pub mod clients;
pub mod codes;
#[cfg(test)]
pub(crate) mod testing;
pub mod tokens;
```

> **Corrected (issue #246, 2026-11-11).** `src/store/oauth/testing.rs` takes the Step 12 imports (`TimeDelta`, `App`, `Config`, `keys`, `tokens::{self, NewToken, TokenKind}`) when it is created here, since `mod.rs` declares `tokens` in the same step; `public_url` validation also refuses user info (`https://user@host`), and the middleware test keeps its revoked-key case (`stored_key(app, name)` takes a name).

Run: `cargo test --lib store::oauth -- --test-threads=4`
Expected: PASS (6 tests).

- [ ] **Step 11: The `src/oauth/` skeleton: shared state, errors, rate limits**

Create `src/oauth/error.rs`:

```rust
//! The OAuth endpoints' errors (RFC 6749 section 5.2, RFC 7591 section 3.2.2,
//! RFC 8707 section 2): `{"error": "<code>", "error_description": "<text>"}`.

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::error::OkfError;

/// An OAuth error. Each variant's text is safe to show the client: it never
/// holds a secret. [`OAuthError::Server`] hides its detail.
// `thiserror` writes `Display` from the `#[error]` lines: `{0}` is the
// variant's one field, like a Python f-string.
#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    /// A parameter is missing, repeated or malformed.
    #[error("{0}")]
    InvalidRequest(String),
    /// The client is unknown or not allowed this flow.
    #[error("{0}")]
    InvalidClient(String),
    /// The code, refresh token or PKCE verifier is wrong, used or expired.
    #[error("{0}")]
    InvalidGrant(String),
    /// The client may not use this grant type.
    #[error("{0}")]
    UnauthorizedClient(String),
    /// A grant type other than `authorization_code` or `refresh_token`.
    #[error("{0}")]
    UnsupportedGrantType(String),
    /// A response type other than `code`.
    #[error("{0}")]
    UnsupportedResponseType(String),
    /// A `resource` other than this server's (RFC 8707).
    #[error("{0}")]
    InvalidTarget(String),
    /// Registration metadata that cannot be accepted (RFC 7591).
    #[error("{0}")]
    InvalidClientMetadata(String),
    /// A registration redirect URI that cannot be accepted (RFC 7591).
    #[error("{0}")]
    InvalidRedirectUri(String),
    /// The person denied the request, or the sign-in failed for good.
    #[error("{0}")]
    AccessDenied(String),
    /// Rate-limited; retry later.
    #[error("too many requests; retry later")]
    SlowDown,
    /// Anything internal (the database). Logged; the client sees only
    /// "internal error".
    // `#[from]` writes `From<OkfError> for OAuthError`, so `?` on a store
    // call inside an OAuth handler converts automatically.
    #[error("internal error")]
    Server(#[from] OkfError),
}

impl OAuthError {
    /// The `error` code.
    ///
    /// # Returns
    /// The RFC's spelling, e.g. `"invalid_grant"`.
    pub fn code(&self) -> &'static str {
        match self {
            OAuthError::InvalidRequest(_) => "invalid_request",
            OAuthError::InvalidClient(_) => "invalid_client",
            OAuthError::InvalidGrant(_) => "invalid_grant",
            OAuthError::UnauthorizedClient(_) => "unauthorized_client",
            OAuthError::UnsupportedGrantType(_) => "unsupported_grant_type",
            OAuthError::UnsupportedResponseType(_) => "unsupported_response_type",
            OAuthError::InvalidTarget(_) => "invalid_target",
            OAuthError::InvalidClientMetadata(_) => "invalid_client_metadata",
            OAuthError::InvalidRedirectUri(_) => "invalid_redirect_uri",
            OAuthError::AccessDenied(_) => "access_denied",
            OAuthError::SlowDown => "slow_down",
            OAuthError::Server(_) => "server_error",
        }
    }

    /// The HTTP status: 401 for `invalid_client`, 429 for `slow_down`, 500
    /// for `server_error`, else 400 (spec "Errors").
    ///
    /// # Returns
    /// The status.
    pub fn status(&self) -> StatusCode {
        match self {
            OAuthError::InvalidClient(_) => StatusCode::UNAUTHORIZED,
            OAuthError::SlowDown => StatusCode::TOO_MANY_REQUESTS,
            OAuthError::Server(_) => StatusCode::INTERNAL_SERVER_ERROR,
            OAuthError::InvalidRequest(_)
            | OAuthError::InvalidGrant(_)
            | OAuthError::UnauthorizedClient(_)
            | OAuthError::UnsupportedGrantType(_)
            | OAuthError::UnsupportedResponseType(_)
            | OAuthError::InvalidTarget(_)
            | OAuthError::InvalidClientMetadata(_)
            | OAuthError::InvalidRedirectUri(_)
            | OAuthError::AccessDenied(_) => StatusCode::BAD_REQUEST,
        }
    }

    /// The body: `{"error", "error_description"}`.
    ///
    /// # Returns
    /// The JSON value.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "error": self.code(),
            "error_description": self.to_string(),
        })
    }
}

impl IntoResponse for OAuthError {
    fn into_response(self) -> Response {
        if let OAuthError::Server(error) = &self {
            // `%error` logs it with `Display`; an `OkfError` never holds a secret.
            tracing::error!(%error, "oauth request failed");
        } else {
            tracing::debug!(code = self.code(), "oauth request refused");
        }
        let mut response = (self.status(), Json(self.to_json())).into_response();
        let headers = response.headers_mut();
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        if matches!(self, OAuthError::SlowDown) {
            headers.insert(header::RETRY_AFTER, HeaderValue::from_static("10"));
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_statuses_follow_the_rfcs() {
        let cases = [
            (OAuthError::InvalidRequest("x".into()), "invalid_request", 400),
            (OAuthError::InvalidClient("x".into()), "invalid_client", 401),
            (OAuthError::InvalidGrant("x".into()), "invalid_grant", 400),
            (OAuthError::UnauthorizedClient("x".into()), "unauthorized_client", 400),
            (OAuthError::UnsupportedGrantType("x".into()), "unsupported_grant_type", 400),
            (OAuthError::UnsupportedResponseType("x".into()), "unsupported_response_type", 400),
            (OAuthError::InvalidTarget("x".into()), "invalid_target", 400),
            (OAuthError::InvalidClientMetadata("x".into()), "invalid_client_metadata", 400),
            (OAuthError::InvalidRedirectUri("x".into()), "invalid_redirect_uri", 400),
            (OAuthError::AccessDenied("x".into()), "access_denied", 400),
            (OAuthError::SlowDown, "slow_down", 429),
        ];
        for (error, code, status) in cases {
            assert_eq!(error.code(), code);
            assert_eq!(error.status().as_u16(), status, "{code}");
        }
    }

    #[tokio::test]
    async fn the_response_is_json_uncached_and_hides_internal_detail() {
        let server = OAuthError::Server(OkfError::Refused {
            reason: "a detail the client must not see".into(),
        });

        let response = server.into_response();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let body = axum::body::to_bytes(response.into_body(), 1 << 16).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"error": "server_error", "error_description": "internal error"})
        );
    }

    #[tokio::test]
    async fn slow_down_says_when_to_retry() {
        let response = OAuthError::SlowDown.into_response();

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()["retry-after"], "10");
    }
}
```

Create `src/oauth/expiring.rs`:

```rust
//! A bounded map whose entries expire: the in-memory pending
//! authorizations and authorization codes (spec "In memory"). The server is
//! single-replica, so a restart costs at most a consent page in progress.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// One value and when it stops being returned.
struct Entry<V> {
    expires: Instant,
    value: V,
}

/// A map from string keys to values that expire `ttl` after insertion,
/// holding at most `capacity` entries. Expired entries are dropped when
/// touched, and all of them at once when the map is full, so a full prune
/// runs only when needed rather than on every insert.
// `<V>`: generic over the value type, like `dict[str, V]` with a type variable.
pub struct ExpiringMap<V> {
    ttl: Duration,
    capacity: usize,
    // A `std` mutex: held only for a lookup or an insert, never across an
    // `.await` (the `SessionKeys` pattern).
    entries: Mutex<HashMap<String, Entry<V>>>,
}

impl<V> ExpiringMap<V> {
    /// An empty map.
    ///
    /// # Arguments
    /// * `ttl` - how long an entry lives.
    /// * `capacity` - the most live entries it holds.
    ///
    /// # Returns
    /// The map.
    pub fn new(ttl: Duration, capacity: usize) -> Self {
        Self {
            ttl,
            capacity,
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Entry<V>>> {
        self.entries.lock().expect("expiring map lock poisoned")
    }

    /// Stores `value` under `key` until `now + ttl`, replacing any entry of
    /// that key.
    ///
    /// # Arguments
    /// * `key` - its key (a random id or a hash).
    /// * `value` - the value.
    /// * `now` - the current instant.
    ///
    /// # Returns
    /// `false`, storing nothing, when the map holds `capacity` live entries.
    pub fn insert(&self, key: String, value: V, now: Instant) -> bool {
        let mut entries = self.lock();
        if entries.len() >= self.capacity && !entries.contains_key(&key) {
            // `retain` keeps the entries the closure accepts, in place.
            entries.retain(|_, entry| entry.expires > now);
            if entries.len() >= self.capacity {
                return false;
            }
        }
        entries.insert(
            key,
            Entry {
                expires: now + self.ttl,
                value,
            },
        );
        true
    }

    /// Removes and returns a live entry: a single-use read.
    ///
    /// # Arguments
    /// * `key` - its key.
    /// * `now` - the current instant.
    ///
    /// # Returns
    /// The value, or `None` if absent or expired (an expired one is dropped).
    pub fn take(&self, key: &str, now: Instant) -> Option<V> {
        let entry = self.lock().remove(key)?;
        // `then_some` is `Some(value) if condition else None`.
        (entry.expires > now).then_some(entry.value)
    }

    /// Changes a live entry in place.
    ///
    /// # Arguments
    /// * `key` - its key.
    /// * `now` - the current instant.
    /// * `change` - what to do with it; its result is returned.
    ///
    /// # Returns
    /// `change`'s result, or `None` if the entry is absent or expired (an
    /// expired one is dropped).
    // `impl FnOnce(&mut V) -> R`: any closure called once with a mutable
    // borrow of the value, like a Python callback that mutates its argument.
    pub fn update<R>(&self, key: &str, now: Instant, change: impl FnOnce(&mut V) -> R) -> Option<R> {
        let mut entries = self.lock();
        let live = entries.get(key).map(|entry| entry.expires > now)?;
        if !live {
            entries.remove(key);
            return None;
        }
        entries.get_mut(key).map(|entry| change(&mut entry.value))
    }

    /// Drops an entry, live or not.
    ///
    /// # Arguments
    /// * `key` - its key.
    pub fn remove(&self, key: &str) {
        self.lock().remove(key);
    }

    /// How many entries it holds, expired ones included.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TTL: Duration = Duration::from_secs(60);

    #[test]
    fn take_returns_a_live_entry_once() {
        let map = ExpiringMap::new(TTL, 4);
        let start = Instant::now();
        assert!(map.insert("a".into(), 1, start));

        let first = map.take("a", start + Duration::from_secs(59));
        let second = map.take("a", start + Duration::from_secs(59));

        assert_eq!((first, second), (Some(1), None));
    }

    #[test]
    fn expired_entries_are_not_returned_and_are_dropped() {
        let map = ExpiringMap::new(TTL, 4);
        let start = Instant::now();
        map.insert("a".into(), 1, start);
        map.insert("b".into(), 2, start);

        assert_eq!(map.take("a", start + TTL), None);
        assert_eq!(map.update("b", start + TTL, |value| *value), None);
        assert_eq!(map.len(), 0);
    }

    #[test]
    fn update_changes_a_live_entry_in_place() {
        let map = ExpiringMap::new(TTL, 4);
        let start = Instant::now();
        map.insert("a".into(), 1, start);

        let seen = map.update("a", start, |value| {
            *value += 1;
            *value
        });

        assert_eq!(seen, Some(2));
        assert_eq!(map.take("a", start), Some(2));
        assert_eq!(map.update("missing", start, |value: &mut i32| *value), None);
    }

    #[test]
    fn a_full_map_prunes_expired_entries_then_refuses() {
        let map = ExpiringMap::new(TTL, 2);
        let start = Instant::now();
        assert!(map.insert("a".into(), 1, start));
        assert!(map.insert("b".into(), 2, start + Duration::from_secs(30)));

        // Full of live entries: refused, and nothing is dropped.
        assert!(!map.insert("c".into(), 3, start + Duration::from_secs(31)));
        // Replacing an existing key is always allowed.
        assert!(map.insert("b".into(), 4, start + Duration::from_secs(31)));
        // "a" has expired: the prune makes room.
        assert!(map.insert("c".into(), 3, start + TTL));

        assert_eq!(map.len(), 2);
        assert_eq!(map.take("c", start + TTL), Some(3));
        assert_eq!(map.take("b", start + TTL), Some(4));
    }

    #[test]
    fn remove_drops_an_entry() {
        let map = ExpiringMap::new(TTL, 2);
        let start = Instant::now();
        map.insert("a".into(), 1, start);

        map.remove("a");

        assert_eq!(map.take("a", start), None);
    }
}
```

Create `src/oauth/ratelimit.rs`:

```rust
//! Global token buckets for the unauthenticated OAuth endpoints (Decision
//! 8): registration, token exchange and consent posts. Global, not per
//! address: behind an ingress the address is the proxy's.

use std::sync::Mutex;
use std::time::Instant;

/// A token bucket: `capacity` requests at once, refilled at
/// `refill_per_sec`.
#[derive(Debug)]
pub struct TokenBucket {
    capacity: f64,
    refill_per_sec: f64,
    // Tokens left and when they were counted; `None` until the first
    // request, which finds the bucket full.
    state: Mutex<Option<(f64, Instant)>>,
}

impl TokenBucket {
    /// A full bucket.
    ///
    /// # Arguments
    /// * `capacity` - the burst size.
    /// * `refill_per_sec` - the sustained rate.
    ///
    /// # Returns
    /// The bucket.
    pub fn new(capacity: u32, refill_per_sec: f64) -> Self {
        Self {
            capacity: f64::from(capacity),
            refill_per_sec,
            state: Mutex::new(None),
        }
    }

    /// Takes one token if there is one.
    ///
    /// # Arguments
    /// * `now` - the current instant (passed in, so tests control time).
    ///
    /// # Returns
    /// `true` if the request may go ahead.
    pub fn try_take(&self, now: Instant) -> bool {
        let mut state = self.state.lock().expect("token bucket lock poisoned");
        // `unwrap_or` on an `Option` of a `Copy` tuple: a full bucket the
        // first time.
        let (tokens, counted) = state.unwrap_or((self.capacity, now));
        // `saturating_duration_since` is zero, not a panic, when another
        // thread counted with a later instant than ours.
        let elapsed = now.saturating_duration_since(counted).as_secs_f64();
        let available = (tokens + elapsed * self.refill_per_sec).min(self.capacity);
        let taken = available >= 1.0;
        let left = if taken { available - 1.0 } else { available };
        *state = Some((left, now.max(counted)));
        taken
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_burst_is_allowed_then_the_rate_applies() {
        let bucket = TokenBucket::new(3, 1.0);
        let start = Instant::now();

        let burst: Vec<bool> = (0..4).map(|_| bucket.try_take(start)).collect();
        let too_soon = bucket.try_take(start + Duration::from_millis(500));
        let refilled = bucket.try_take(start + Duration::from_millis(1500));

        assert_eq!(burst, [true, true, true, false]);
        assert!(!too_soon);
        assert!(refilled);
    }

    #[test]
    fn refill_stops_at_capacity_and_tolerates_earlier_instants() {
        let bucket = TokenBucket::new(2, 10.0);
        let start = Instant::now();
        bucket.try_take(start);

        let later = start + Duration::from_secs(60);
        let taken: Vec<bool> = (0..3).map(|_| bucket.try_take(later)).collect();
        // An instant before the last count: no refill, no panic.
        let earlier = bucket.try_take(start);

        assert_eq!(taken, [true, true, false]);
        assert!(!earlier);
    }
}
```

Create `src/oauth/state.rs`:

```rust
//! What the authorization server keeps in memory between requests (spec
//! "In memory"): pending authorizations (a consent page in progress) and
//! authorization codes (minted on Approve, exchanged once at `/oauth/token`).

use std::time::Duration;

use crate::store::oauth::clients::ClientRecord;

/// How long a consent page may stay open.
pub const PENDING_TTL: Duration = Duration::from_secs(10 * 60);

/// How long an authorization code may wait to be exchanged.
pub const AUTHORIZATION_CODE_TTL: Duration = Duration::from_secs(60);

/// Wrong sign-in codes a pending authorization survives.
pub const MAX_FAILED_ATTEMPTS: u8 = 5;

/// The most pending authorizations held at once (they are made by an
/// unauthenticated GET, so the map is bounded).
pub const MAX_PENDING: usize = 10_000;

/// The most unexchanged authorization codes held at once.
pub const MAX_AUTHORIZATION_CODES: usize = 10_000;

/// A validated `GET /oauth/authorize`, waiting for the person's decision.
/// Keyed in `OAuth::pending` by a random id the consent form carries.
#[derive(Clone)]
pub struct PendingAuthorization {
    /// The CSRF token every consent form must echo.
    pub csrf: String,
    /// The client, resolved (CIMD or DCR).
    pub client: ClientRecord,
    /// The redirect URI, already matched exactly against the client's.
    pub redirect_uri: String,
    /// The client's `state`, returned unchanged.
    pub state: Option<String>,
    /// The PKCE S256 challenge.
    pub code_challenge: String,
    /// The resource (always this server's `/mcp`).
    pub resource: String,
    /// The sign-in code accepted at step 1, awaiting Approve (its id, never
    /// the code).
    pub sign_in_code_id: Option<String>,
    /// Wrong codes entered so far.
    pub failed_attempts: u8,
}

// Written by hand, not derived, so a stray `{:?}` never prints the CSRF token.
impl std::fmt::Debug for PendingAuthorization {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingAuthorization")
            .field("csrf", &"<redacted>")
            .field("client_id", &self.client.client_id)
            .field("redirect_uri", &self.redirect_uri)
            .field("sign_in_code_id", &self.sign_in_code_id)
            .field("failed_attempts", &self.failed_attempts)
            .finish_non_exhaustive()
    }
}

/// An authorization code's binding, keyed in `OAuth::codes` by the hex
/// SHA-256 of the `okfac_` code (the code itself is never stored).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationCode {
    /// The grant the approval created.
    pub grant_id: String,
    /// The client it was issued to.
    pub client_id: String,
    /// The redirect URI it was issued with.
    pub redirect_uri: String,
    /// The PKCE S256 challenge the verifier must match.
    pub code_challenge: String,
    /// The resource the tokens will be issued for.
    pub resource: String,
    /// The token family its tokens join.
    pub family_id: String,
    /// Whether it was exchanged. A used code stays in the map until it
    /// expires, so presenting it again is seen as a replay (RFC 6749
    /// section 4.1.2: Task 6 then revokes its family).
    pub used: bool,
}

/// The key of `OAuth::codes` for an authorization code: the hex SHA-256 of
/// the `okfac_` code, so the map never holds a usable code.
///
/// # Arguments
/// * `code` - the authorization code, as minted or presented.
///
/// # Returns
/// 64 lowercase hex characters.
pub fn code_key(code: &str) -> String {
    // `fold` with `write!` appends each byte's two hex digits to one
    // preallocated `String` (Python: `hashlib.sha256(...).hexdigest()`),
    // with no `String` per byte.
    use std::fmt::Write as _;
    crate::keys::hash(code)
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            // Writing to a `String` cannot fail.
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::oauth::clients::ClientKind;

    #[test]
    fn code_key_is_the_hex_sha256() {
        // The FIPS 180-2 test vector for "abc" (as in `keys::tests`).
        assert_eq!(
            code_key("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn debug_never_prints_the_csrf_token() {
        let pending = PendingAuthorization {
            csrf: "csrf-value-that-must-not-leak".into(),
            client: ClientRecord {
                client_id: "https://client.example.com/client.json".into(),
                kind: ClientKind::Cimd,
                client_name: None,
                redirect_uris: vec!["https://client.example.com/callback".into()],
                metadata: serde_json::json!({}),
                fetched_at: None,
                cache_expires_at: None,
                created_at: chrono::Utc::now(),
            },
            redirect_uri: "https://client.example.com/callback".into(),
            state: None,
            code_challenge: "c".repeat(43),
            resource: "https://okf.example.com/mcp".into(),
            sign_in_code_id: None,
            failed_attempts: 0,
        };

        let printed = format!("{pending:?}");

        assert!(!printed.contains("csrf-value-that-must-not-leak"));
        assert!(printed.contains("<redacted>"));
    }
}
```

Create the stubs, each with only what is shown.

`src/oauth/metadata.rs`:

```rust
//! The well-known documents: Protected Resource Metadata (RFC 9728) and
//! Authorization Server Metadata (RFC 8414). Filled in by Task 3.

use std::sync::Arc;

use axum::Router;

use crate::app::App;

/// This area's routes (none yet; Task 3).
///
/// # Returns
/// An empty router.
pub fn routes() -> Router<Arc<App>> {
    Router::new()
}
```

`src/oauth/clients.rs`:

```rust
//! Client resolution: dynamic registration (RFC 7591) and the Client ID
//! Metadata Document fetcher. Filled in by Task 3.

use std::sync::Arc;

use axum::Router;

use crate::app::App;
use crate::error::OkfError;

/// Fetches and caches Client ID Metadata Documents (Task 3 gives it its
/// HTTP client and in-flight set).
#[derive(Debug)]
pub struct CimdFetcher {
    // Read by Task 3's fetch; stored now so `OAuth::new` is final.
    #[allow(dead_code)] // Task 3 removes this.
    allow_private: bool,
    #[allow(dead_code)] // Task 3 removes this.
    allow_http: bool,
}

impl CimdFetcher {
    /// A fetcher.
    ///
    /// # Arguments
    /// * `allow_private` - fetch documents on private and loopback addresses
    ///   (`oauth.cimd_allow_private`; test rigs only).
    /// * `allow_http` - also fetch `http://` document URLs (test rigs only:
    ///   set when private addresses are allowed and the issuer is `http://`).
    ///
    /// # Returns
    /// The fetcher.
    ///
    /// # Errors
    /// None yet; Task 3's HTTP client build can fail ([`OkfError::Config`]).
    pub fn new(allow_private: bool, allow_http: bool) -> Result<Self, OkfError> {
        Ok(Self {
            allow_private,
            allow_http,
        })
    }
}

/// This area's routes (none yet; Task 3 adds `POST /oauth/register`).
///
/// # Returns
/// An empty router.
pub fn routes() -> Router<Arc<App>> {
    Router::new()
}
```

`src/oauth/authorize.rs` gets the same shape as `metadata.rs`, with the module doc `//! GET and POST /oauth/authorize: request validation and the consent flow. Filled in by Task 5.` and `/// This area's routes (none yet; Task 5).`.

`src/oauth/token.rs` gets the same shape, with `//! POST /oauth/token and POST /oauth/revoke. Filled in by Task 6.` and `/// This area's routes (none yet; Task 6).`.

`src/oauth/pkce.rs`: `//! PKCE (RFC 7636), S256 only. Filled in by Task 5.`

`src/oauth/consent.rs`: `//! The consent page's HTML and its escaping. Filled in by Task 5.`

The access-token verifier, `src/oauth/access.rs`, is in Step 12, because it implements the trait that step adds. Create `src/oauth/mod.rs` now without it, and finish it in Step 12:

```rust
//! The built-in OAuth 2.1 authorization server and the OAuth side of the
//! resource server (M10; spec `docs/superpowers/specs/2026-11-11-okf-oauth-design.md`).
//! On only when `public_url` is set ([`OAuth::new`]).

pub mod authorize;
pub mod clients;
pub mod consent;
pub mod error;
pub mod expiring;
pub mod metadata;
pub mod pkce;
pub mod ratelimit;
pub mod state;
pub mod token;
```

Add `pub mod oauth;` to `src/lib.rs` after `pub mod ntp;`, and add "the OAuth authorization server (`oauth`)" to its module doc.

Run: `cargo test --lib oauth:: -- --test-threads=4`
Expected: PASS (error 3, expiring 5, ratelimit 2, state 1).

- [ ] **Step 12: Principals of two kinds, `BearerVerifier`, the access-token verifier, `OAuth`**

**`src/auth.rs`.** Add to the imports:

```rust
use std::future::Future;

use axum::http::HeaderValue;

use crate::app::App;
use crate::http::AppError;
use crate::oauth::access::OAuthVerifier;
```

and change `use crate::store::keys::{self as key_rows, Access, KeyRecord};` to `use crate::store::keys::{self as key_rows, Access, KeyKind, KeyRecord};`.

Add before `Principal`:

```rust
/// What credential a request came with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialKind {
    /// An `okf_` API key.
    ApiKey,
    /// An OAuth access token of a grant.
    OAuth {
        /// The grant's client id (`None` once the client row is gone).
        client_id: Option<String>,
        /// The grant's client, as it names itself.
        client_name: Option<String>,
    },
}
```

Add a field at the end of `Principal`:

```rust
    /// An API key or an OAuth grant.
    pub kind: CredentialKind,
```

Update the module doc's first line to "API-key and OAuth-token authentication". In `From<KeyRecord> for Principal`, build `kind` before `Self { .. }` and add it:

```rust
        let kind = match record.kind {
            KeyKind::Key => CredentialKind::ApiKey,
            // Moving the two fields out of `record` is fine: the struct
            // literal below moves only its other fields.
            KeyKind::OAuth => CredentialKind::OAuth {
                client_id: record.client_id,
                client_name: record.client_name,
            },
        };
```

Replace `KeyCache` and its `impl`:

```rust
/// Every live key, by SHA-256, and every live OAuth grant, by id.
/// Immutable: a reload builds a new one.
#[derive(Debug, Default)]
pub struct KeyCache {
    by_hash: HashMap<[u8; 32], Arc<Principal>>,
    // Grants have no secret of their own: an access token names one by id.
    by_id: HashMap<String, Arc<Principal>>,
}

impl KeyCache {
    /// Builds the cache from stored rows, leaving out revoked ones.
    ///
    /// # Arguments
    /// * `records` - the rows, as [`key_rows::list`] returns them.
    ///
    /// # Returns
    /// The cache.
    pub fn from_records(records: Vec<KeyRecord>) -> Self {
        let mut cache = Self::default();
        for record in records.into_iter().filter(|record| record.revoked_at.is_none()) {
            match record.kind {
                KeyKind::Key => {
                    // The schema checks `length(sha256) = 32`, so this fails
                    // only on a row edited by hand; such a key cannot
                    // authenticate. `let ... else` skips it.
                    let Some(hash) = record
                        .sha256
                        .as_deref()
                        .and_then(|sha256| <[u8; 32]>::try_from(sha256).ok())
                    else {
                        continue;
                    };
                    cache.by_hash.insert(hash, Arc::new(Principal::from(record)));
                }
                KeyKind::OAuth => {
                    let id = record.id.clone();
                    cache.by_id.insert(id, Arc::new(Principal::from(record)));
                }
            }
        }
        cache
    }

    /// The principal of a presented key.
    ///
    /// # Arguments
    /// * `presented` - the key from the request.
    ///
    /// # Returns
    /// Its principal, or `None` for an unknown or revoked key.
    pub fn find(&self, presented: &str) -> Option<Arc<Principal>> {
        self.by_hash.get(&keys::hash(presented)).cloned()
    }

    /// A live OAuth grant's principal.
    ///
    /// # Arguments
    /// * `grant_id` - the grant's id (from its access token).
    ///
    /// # Returns
    /// Its principal, or `None` for an unknown or revoked grant.
    pub fn find_grant(&self, grant_id: &str) -> Option<Arc<Principal>> {
        self.by_id.get(grant_id).cloned()
    }
}
```

Add to `impl Keys`:

```rust
    /// A live OAuth grant's principal, from the current cache.
    ///
    /// # Arguments
    /// * `grant_id` - the grant's id.
    ///
    /// # Returns
    /// Its principal, or `None` for an unknown or revoked grant (a revoke
    /// takes effect at the next reload, within `poll_interval`).
    pub fn find_grant(&self, grant_id: &str) -> Option<Arc<Principal>> {
        self.cache
            .read()
            .expect("key cache lock poisoned")
            .find_grant(grant_id)
    }
```

Add after `impl Keys`:

```rust
/// Turns a presented bearer token into the principal it acts as. The seam
/// for credential kinds: API keys ([`Keys`]), OAuth access tokens
/// ([`OAuthVerifier`]) and, in a later round, an external issuer.
// `-> impl Future<...> + Send`: the method returns some future that can move
// between threads (tokio's worker threads need that), without naming its
// type; an implementation may simply be an `async fn`.
pub trait BearerVerifier {
    /// The principal of `token`.
    ///
    /// # Arguments
    /// * `token` - the bearer token, as presented.
    ///
    /// # Returns
    /// The principal, or `None` for an unknown, expired, revoked or
    /// wrong-audience token.
    ///
    /// # Errors
    /// [`OkfError::Database`] when the lookup needs the database and it fails.
    fn verify(
        &self,
        token: &str,
    ) -> impl Future<Output = Result<Option<Arc<Principal>>, OkfError>> + Send;
}

impl BearerVerifier for Keys {
    async fn verify(&self, token: &str) -> Result<Option<Arc<Principal>>, OkfError> {
        Ok(self.find(token))
    }
}

/// The principal of a bearer token. Dispatch is by prefix with no fallback
/// between kinds: an `okfat_` token is only ever an OAuth access token
/// (`None` with OAuth off), and anything else is looked up only among the
/// API keys, which all carry `okf_`. Refresh tokens, sign-in codes and
/// authorization codes are never bearer credentials and match nothing.
///
/// # Arguments
/// * `app` - the shared state.
/// * `token` - the bearer token.
///
/// # Returns
/// The principal, or `None`.
///
/// # Errors
/// [`OkfError::Database`] from an access-token lookup.
pub async fn authenticate(app: &App, token: &str) -> Result<Option<Arc<Principal>>, OkfError> {
    if token.starts_with(keys::ACCESS_TOKEN_PREFIX) {
        return match app.oauth() {
            Some(oauth) => OAuthVerifier { app, oauth }.verify(token).await,
            None => Ok(None),
        };
    }
    app.keys().verify(token).await
}

/// The `WWW-Authenticate` value of a 401: with OAuth on, it names the
/// Protected Resource Metadata (RFC 9728 section 5.1), so a client can
/// discover the authorization server.
///
/// # Arguments
/// * `app` - the shared state.
///
/// # Returns
/// `Bearer`, or `Bearer resource_metadata="<public_url>/.well-known/oauth-protected-resource"`.
pub fn challenge(app: &App) -> HeaderValue {
    app.oauth().map_or_else(
        || HeaderValue::from_static("Bearer"),
        |oauth| oauth.challenge.clone(),
    )
}
```

Replace `require_key`:

```rust
/// axum middleware: admits a request with a live API key or OAuth access
/// token, inserting its `Arc<Principal>` into the request's extensions, and
/// answers anything else with 401 and [`challenge`].
///
/// # Arguments
/// * `app` - the shared state (the layer's state).
/// * `request` - the incoming request.
/// * `next` - the rest of the stack.
///
/// # Returns
/// The route's response; 401 with the challenge; or 500 if an access-token
/// lookup fails (logged).
// Used as `axum::middleware::from_fn_with_state(app, require_key)`; axum
// fills each argument from the request (the extractor pattern).
pub async fn require_key(
    State(app): State<Arc<App>>,
    mut request: Request,
    next: Next,
) -> Response {
    // The borrowed token (`&str` into the headers) ends with this `match`,
    // before the request is changed below.
    let found = match bearer(request.headers()) {
        Some(token) => authenticate(&app, token).await,
        None => Ok(None),
    };
    match found {
        Ok(Some(principal)) => {
            tracing::debug!(key = %principal.key_id, "authenticated");
            request.extensions_mut().insert(principal);
            next.run(request).await
        }
        Ok(None) => (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, challenge(&app))],
            "missing or unknown API key\n",
        )
            .into_response(),
        // `AppError` logs it and answers 500 without detail.
        Err(error) => AppError(error).into_response(),
    }
}
```

Remove `use sqlx::PgPool;` only if nothing else uses it (`Keys::load` and `reload` do, so it stays).

**Principal literals.** Add `kind: CredentialKind::ApiKey,`:
- in `src/mcp/testing.rs`, `principal_with_reporting` (and import `CredentialKind` next to `Principal, Reach`);
- in the `src/selection.rs` test helper `principal` (and `use crate::auth::{CredentialKind, Reach};`).

**`src/oauth/access.rs`:**

```rust
//! OAuth access tokens on the resource side: an in-memory cache over
//! `oauth_tokens`, the audience check (RFC 8707), and the grant check
//! through the key cache, behind [`BearerVerifier`].
//!
//! As with API keys (`auth`), tokens are looked up by their SHA-256 in a
//! `HashMap`; a timing difference tells an attacker only how a guess's
//! *hash* compares with stored ones, which does not help find a token.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::{DateTime, Utc};

use crate::app::App;
use crate::auth::{BearerVerifier, Principal};
use crate::error::OkfError;
use crate::keys;
use crate::oauth::OAuth;
use crate::store::oauth::tokens::{self, LiveAccessToken};

/// The most access tokens cached at once. Past it, expired entries are
/// pruned; if it is still full, a token is checked against the database each
/// time instead of cached.
const MAX_CACHED: usize = 10_000;

/// The cache's contents: the tokens, and how many revocations it has seen.
#[derive(Debug, Default)]
struct Cache {
    tokens: HashMap<[u8; 32], LiveAccessToken>,
    // Bumped by every `forget`. A lookup records it before reading the
    // database and may cache its result only if it has not moved, so a
    // revocation that commits while the lookup is in flight cannot be undone
    // by the lookup re-caching the token it read before the revocation
    // (PR #243 review).
    generation: u64,
}

/// Live access tokens seen recently, by SHA-256.
#[derive(Debug, Default)]
pub struct AccessTokens {
    // A `std` mutex, held only to read or change the cache, never across an
    // `.await` (each use below is its own statement or block).
    cache: Mutex<Cache>,
}

impl AccessTokens {
    /// An empty cache.
    ///
    /// # Returns
    /// The cache.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Cache> {
        self.cache.lock().expect("access token cache lock poisoned")
    }

    /// The revocation generation. Read it before a database lookup (or, in
    /// Task 6, before minting) and pass it to [`Self::remember`].
    ///
    /// # Returns
    /// How many times [`Self::forget`] has run.
    pub fn generation(&self) -> u64 {
        self.lock().generation
    }

    /// Caches a live token, unless a revocation happened since
    /// `seen_generation` was read: then the token may be the one just
    /// revoked, and the next request rechecks the database instead.
    ///
    /// # Arguments
    /// * `sha256` - `keys::hash` of the token.
    /// * `token` - its facts.
    /// * `now` - the current time, to prune expired entries when full.
    /// * `seen_generation` - [`Self::generation`], read before the token was
    ///   looked up or minted.
    pub fn remember(
        &self,
        sha256: [u8; 32],
        token: LiveAccessToken,
        now: DateTime<Utc>,
        seen_generation: u64,
    ) {
        // The check and the insert happen under one lock, so no `forget` can
        // fall between them.
        let mut cache = self.lock();
        if cache.generation != seen_generation {
            return;
        }
        if cache.tokens.len() >= MAX_CACHED {
            cache.tokens.retain(|_, cached| cached.expires_at > now);
            if cache.tokens.len() >= MAX_CACHED {
                return;
            }
        }
        cache.tokens.insert(sha256, token);
    }

    /// Drops tokens from the cache and bumps the generation (Task 6 calls
    /// this after a revocation commits), so a revoked token stops working at
    /// once, and no lookup already in flight can cache it again.
    ///
    /// # Arguments
    /// * `hashes` - the revoked tokens' hashes.
    pub fn forget(&self, hashes: &[[u8; 32]]) {
        let mut cache = self.lock();
        // `wrapping_add`: a u64 that never realistically wraps, but if it
        // did, a wrapped value still differs from every recent reading.
        cache.generation = cache.generation.wrapping_add(1);
        for hash in hashes {
            cache.tokens.remove(hash);
        }
    }

    /// The principal of an access token.
    ///
    /// # Arguments
    /// * `app` - the shared state (database, clock, key cache).
    /// * `resource` - this server's resource identifier (`<public_url>/mcp`).
    /// * `token` - the presented `okfat_` token.
    ///
    /// # Returns
    /// The grant's principal; `None` for an unknown, revoked or expired
    /// token, one issued for another resource, or one whose grant is revoked.
    ///
    /// # Errors
    /// [`OkfError::Database`] from the lookup on a cache miss.
    pub async fn verify(
        &self,
        app: &App,
        resource: &str,
        token: &str,
    ) -> Result<Option<Arc<Principal>>, OkfError> {
        let sha256 = keys::hash(token);
        let now = app.clock().now();
        // The lock guard is a temporary of this statement, released before
        // the `.await` below.
        // Read the generation in the same lock as the cache miss, before the
        // database read: a revocation committed after this point bumps it.
        let (cached, generation) = {
            let cache = self.lock();
            (cache.tokens.get(&sha256).cloned(), cache.generation)
        };
        let live = match cached {
            Some(live) => live,
            None => match tokens::find_access(app.pool(), &sha256, now).await? {
                Some(live) => {
                    self.remember(sha256, live.clone(), now, generation);
                    live
                }
                None => return Ok(None),
            },
        };
        if live.expires_at <= now {
            // Expiry is not a revocation: drop the entry without bumping the
            // generation, so lookups of other tokens in flight still cache.
            self.lock().tokens.remove(&sha256);
            return Ok(None);
        }
        // The audience check (RFC 8707): a token issued for another
        // resource is not a credential here.
        if live.resource != resource {
            return Ok(None);
        }
        Ok(app.keys().find_grant(&live.grant_id))
    }
}

/// The OAuth implementation of [`BearerVerifier`]: borrows what the lookup
/// needs for one request.
pub struct OAuthVerifier<'a> {
    /// The shared state.
    pub app: &'a App,
    /// The OAuth state (its settings give the resource).
    pub oauth: &'a OAuth,
}

impl BearerVerifier for OAuthVerifier<'_> {
    async fn verify(&self, token: &str) -> Result<Option<Arc<Principal>>, OkfError> {
        self.oauth
            .access
            .verify(self.app, &self.oauth.settings.resource, token)
            .await
    }
}
```

**`src/oauth/mod.rs`, complete:**

```rust
//! The built-in OAuth 2.1 authorization server and the OAuth side of the
//! resource server (M10; spec `docs/superpowers/specs/2026-11-11-okf-oauth-design.md`).
//! On only when `public_url` is set ([`OAuth::new`]).

pub mod access;
pub mod authorize;
pub mod clients;
pub mod consent;
pub mod error;
pub mod expiring;
pub mod metadata;
pub mod pkce;
pub mod ratelimit;
pub mod state;
// Task 5 adds `#[cfg(test)] pub(crate) mod testing;` with its file.
pub mod token;

use std::sync::Arc;
use std::time::Duration;

use axum::http::{header, HeaderValue};
use axum::middleware;
use axum::response::Response;
use axum::Router;

use crate::app::App;
use crate::config::Config;
use crate::error::OkfError;

/// Where the Protected Resource Metadata is served (RFC 9728), also with
/// the resource's path appended (`.../mcp`).
pub const PROTECTED_RESOURCE_PATH: &str = "/.well-known/oauth-protected-resource";

/// Where the Authorization Server Metadata is served (RFC 8414).
pub const AUTHORIZATION_SERVER_PATH: &str = "/.well-known/oauth-authorization-server";

/// Registration: a burst of 10, then one every 6 seconds.
const REGISTER_BURST: u32 = 10;
const REGISTER_PER_SEC: f64 = 1.0 / 6.0;
/// Token exchange: a burst of 60, then 2 a second.
const TOKEN_BURST: u32 = 60;
const TOKEN_PER_SEC: f64 = 2.0;
/// Consent posts: a burst of 20, then 1 a second (a person and a browser
/// journey post a few times per sign-in).
const CONSENT_BURST: u32 = 20;
const CONSENT_PER_SEC: f64 = 1.0;

/// The OAuth settings, derived once from the configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthSettings {
    /// The issuer: `public_url` without a trailing slash.
    pub issuer: String,
    /// The resource tokens are issued for: `<issuer>/mcp`.
    pub resource: String,
    /// The Protected Resource Metadata URL a 401 names.
    pub resource_metadata_url: String,
    /// `oauth.access_token_ttl`.
    pub access_token_ttl: Duration,
    /// `oauth.refresh_token_ttl`.
    pub refresh_token_ttl: Duration,
    /// `oauth.code_max_expiry`.
    pub code_max_expiry: Duration,
    /// `oauth.dcr_enabled`.
    pub dcr_enabled: bool,
    /// `oauth.cimd_allow_private`.
    pub cimd_allow_private: bool,
}

/// The authorization server's state, held by `App` when OAuth is on.
pub struct OAuth {
    /// The settings.
    pub settings: OAuthSettings,
    /// The 401 `WWW-Authenticate` value, built once.
    pub challenge: HeaderValue,
    /// Live access tokens (the resource side).
    pub access: access::AccessTokens,
    /// Consent pages in progress, by pending id.
    pub pending: expiring::ExpiringMap<state::PendingAuthorization>,
    /// Unexchanged authorization codes, by the hex SHA-256 of the code.
    pub codes: expiring::ExpiringMap<state::AuthorizationCode>,
    /// The Client ID Metadata Document fetcher.
    pub cimd: clients::CimdFetcher,
    /// `POST /oauth/register`'s limit.
    pub register_limit: ratelimit::TokenBucket,
    /// `POST /oauth/token`'s (and `/oauth/revoke`'s) limit.
    pub token_limit: ratelimit::TokenBucket,
    /// `POST /oauth/authorize`'s limit.
    pub consent_limit: ratelimit::TokenBucket,
}

impl OAuth {
    /// The OAuth state for a configuration that `Config::validate` accepted.
    ///
    /// # Arguments
    /// * `config` - the server's settings.
    ///
    /// # Returns
    /// `None` without `public_url` (OAuth off), else the state.
    ///
    /// # Errors
    /// [`OkfError::Config`] if `public_url` cannot appear in a header
    /// (`Config::validate` refuses such a URL first, so only a caller that
    /// skipped validation sees this).
    pub fn new(config: &Config) -> Result<Option<Self>, OkfError> {
        let Some(public_url) = config.public_url.as_deref() else {
            return Ok(None);
        };
        let issuer = public_url.trim_end_matches('/').to_string();
        let allow_private = config.oauth.cimd_allow_private;
        // Plain-http documents only on a test rig: private addresses allowed
        // and the server itself on loopback http.
        let allow_http = allow_private && issuer.starts_with("http://");
        let settings = OAuthSettings {
            resource: format!("{issuer}/mcp"),
            resource_metadata_url: format!("{issuer}{PROTECTED_RESOURCE_PATH}"),
            issuer,
            access_token_ttl: config.oauth.access_token_ttl,
            refresh_token_ttl: config.oauth.refresh_token_ttl,
            code_max_expiry: config.oauth.code_max_expiry,
            dcr_enabled: config.oauth.dcr_enabled,
            cimd_allow_private: config.oauth.cimd_allow_private,
        };
        let challenge = HeaderValue::from_str(&format!(
            "Bearer resource_metadata=\"{}\"",
            settings.resource_metadata_url
        ))
        .map_err(|_| OkfError::Config {
            path: "public_url".to_string(),
            message: format!("{public_url:?} cannot appear in a header"),
        })?;
        Ok(Some(Self {
            settings,
            challenge,
            access: access::AccessTokens::new(),
            pending: expiring::ExpiringMap::new(state::PENDING_TTL, state::MAX_PENDING),
            codes: expiring::ExpiringMap::new(
                state::AUTHORIZATION_CODE_TTL,
                state::MAX_AUTHORIZATION_CODES,
            ),
            cimd: clients::CimdFetcher::new(allow_private, allow_http)?,
            register_limit: ratelimit::TokenBucket::new(REGISTER_BURST, REGISTER_PER_SEC),
            token_limit: ratelimit::TokenBucket::new(TOKEN_BURST, TOKEN_PER_SEC),
            consent_limit: ratelimit::TokenBucket::new(CONSENT_BURST, CONSENT_PER_SEC),
        }))
    }
}

/// Every OAuth route (`/.well-known/*`, `/oauth/*`), for `http::router` to
/// merge when OAuth is on. No route needs a bearer token.
///
/// # Returns
/// A router whose responses all carry `Cache-Control: no-store`.
pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .merge(metadata::routes())
        .merge(clients::routes())
        .merge(authorize::routes())
        .merge(token::routes())
        .layer(middleware::map_response(no_store))
}

/// Marks a response uncacheable: tokens, codes and consent pages must never
/// be stored by a cache.
async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_is_off_without_public_url() {
        assert!(OAuth::new(&Config::default()).unwrap().is_none());
    }

    #[test]
    fn settings_derive_from_public_url() {
        let config = crate::store::oauth::testing::oauth_config("https://okf.example.com/");

        let oauth = OAuth::new(&config).unwrap().expect("on");

        assert_eq!(oauth.settings.issuer, "https://okf.example.com");
        assert_eq!(oauth.settings.resource, "https://okf.example.com/mcp");
        assert_eq!(
            oauth.settings.resource_metadata_url,
            "https://okf.example.com/.well-known/oauth-protected-resource"
        );
        assert_eq!(
            oauth.challenge,
            "Bearer resource_metadata=\"https://okf.example.com/.well-known/oauth-protected-resource\""
        );
        assert_eq!(oauth.settings.access_token_ttl, config.oauth.access_token_ttl);
    }
}
```

**`src/store/oauth/testing.rs`, appended** (Tasks 3 to 6 use these too; Task 5's `src/oauth/testing.rs` should call them, not copy them). Add to its imports:

```rust
use chrono::TimeDelta;

use crate::app::App;
use crate::config::Config;
use crate::keys;
use crate::store::oauth::tokens::{self, NewToken, TokenKind};
```

and append:

```rust
/// The defaults with OAuth on at `public_url`, its host (and loopback) allowed.
///
/// # Arguments
/// * `public_url` - e.g. `https://okf.example.com`.
///
/// # Returns
/// A configuration `Config::validate` accepts.
///
/// # Panics
/// If `public_url` has no host, or the result does not validate.
pub(crate) fn oauth_config(public_url: &str) -> Config {
    let host = public_url
        .parse::<axum::http::Uri>()
        .ok()
        .and_then(|uri| uri.host().map(str::to_string))
        .expect("public_url has a host");
    let config = Config {
        public_url: Some(public_url.to_string()),
        allowed_hosts: vec![host, "localhost".into(), "127.0.0.1".into()],
        ..Config::default()
    };
    assert_eq!(config.validate(), Ok(()), "a valid OAuth test configuration");
    config
}

/// Stores a new access token for `grant_id`, issued for `resource`, valid
/// for `ttl` from the app's clock.
///
/// # Arguments
/// * `app` - the app (its pool and clock).
/// * `grant_id` - the grant.
/// * `resource` - the audience.
/// * `ttl` - how long it is valid; a negative one makes an already expired
///   token, created twice that long ago so `expires_at > created_at` holds.
///
/// # Returns
/// The `okfat_` token.
///
/// # Panics
/// If the insert fails.
pub(crate) async fn mint_access(
    app: &App,
    grant_id: &str,
    resource: &str,
    ttl: TimeDelta,
) -> String {
    let token = keys::generate_with(keys::ACCESS_TOKEN_PREFIX).expect("random token");
    let now = app.clock().now();
    let created = if ttl > TimeDelta::zero() { now } else { now + ttl * 2 };
    let mut conn = app.pool().acquire().await.expect("a connection");
    tokens::insert(
        &mut conn,
        &NewToken {
            sha256: keys::hash(&token),
            grant_id,
            kind: TokenKind::Access,
            family_id: "test-family",
            resource,
            expires_at: now + ttl,
        },
        created,
    )
    .await
    .expect("store a test access token");
    token
}
```

Run: `cargo build --all-targets`
Expected: errors only in `src/app.rs`, `src/http/mod.rs` and `src/http/api/mod.rs` (no `App::oauth`, and `require_key`'s state changed). Step 13 fixes them.

- [ ] **Step 13: Wire `OAuth` into the app and both routers**

**`src/app.rs`.** Import `crate::oauth::OAuth` and `crate::store::oauth::{clients as oauth_clients, codes as sign_in_codes, tokens as oauth_tokens}`. Add the constants after `EMPTY_CHANGESET_HOURS`:

```rust
/// How often the poll loop prunes the OAuth tables (they change slowly; a
/// pass every `poll_interval` would be three needless deletes).
const OAUTH_PRUNE_EVERY: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// How long expired tokens and unredeemed codes are kept (for diagnosis).
const OAUTH_RETENTION_DAYS: i64 = 7;

/// How long a registered client that no grant names is kept.
const UNBOUND_CLIENT_HOURS: i64 = 24;
```

Add fields to `App`, after `clock_status`:

```rust
    // The OAuth authorization server's state; `None` without `public_url`.
    oauth: Option<OAuth>,
    // When the OAuth tables were last pruned; `None` until the first pass.
    // A `std` mutex: held only to read or set the instant.
    oauth_pruned: std::sync::Mutex<Option<std::time::Instant>>,
```

In `App::load`, before `let sessions = ...`:

```rust
        let oauth = OAuth::new(&config)?;
```

and in the `Self { .. }`, `oauth,` and `oauth_pruned: std::sync::Mutex::new(None),`. Add to the doc's `# Errors`: "[`OkfError::Config`] from [`OAuth::new`]".

Add the accessor after `sessions()`:

```rust
    /// The OAuth state.
    ///
    /// # Returns
    /// `None` when OAuth is off (no `public_url`).
    pub fn oauth(&self) -> Option<&OAuth> {
        self.oauth.as_ref()
    }
```

In `poll`, after `self.prune_empty_changesets().await;`:

```rust
        self.prune_oauth().await;
```

Add after `prune_empty_changesets`:

```rust
    /// At most once per [`OAUTH_PRUNE_EVERY`], deletes expired OAuth tokens
    /// and unredeemed codes kept [`OAUTH_RETENTION_DAYS`] past their expiry,
    /// and registered clients no grant names after [`UNBOUND_CLIENT_HOURS`].
    /// Runs whether or not OAuth is on, so rows left from when it was are
    /// cleared too. Called only under `polling`.
    ///
    /// A failure is logged, not returned, and waits for the next hourly pass.
    async fn prune_oauth(&self) {
        let now_instant = std::time::Instant::now();
        {
            // A block, so the guard is released before the `.await`s below.
            let mut pruned = self.oauth_pruned.lock().expect("oauth prune lock poisoned");
            if pruned.is_some_and(|at| now_instant.duration_since(at) < OAUTH_PRUNE_EVERY) {
                return;
            }
            *pruned = Some(now_instant);
        }
        let now = self.clock.now();
        let retained = now - TimeDelta::days(OAUTH_RETENTION_DAYS);
        log_pruned("expired OAuth tokens", oauth_tokens::prune(&self.pool, retained).await);
        log_pruned("expired sign-in codes", sign_in_codes::prune(&self.pool, retained).await);
        log_pruned(
            "unused registered OAuth clients",
            oauth_clients::prune_unbound_dcr(&self.pool, now - TimeDelta::hours(UNBOUND_CLIENT_HOURS))
                .await,
        );
    }
```

and a free function near `note_failure`:

```rust
/// Logs one OAuth prune's outcome: silent for nothing deleted, `info` for
/// some, `error` for a failure.
fn log_pruned(what: &'static str, result: Result<u64, OkfError>) {
    match result {
        Ok(0) => {}
        Ok(pruned) => tracing::info!(pruned, "deleted {what}"),
        Err(error) => tracing::error!(%error, "deleting {what} failed; retrying in an hour"),
    }
}
```

**`src/http/mod.rs`.**
- Add `use crate::oauth;`.
- In `router`, delete `let keys = Arc::clone(app.keys());`, and change the keyed layer to `.route_layer(middleware::from_fn_with_state(Arc::clone(&app), require_key));`. Update the comment above `mcp` to say "`require_key` (on `keyed`) runs before `bind_session`".
- Replace the final `Router::new().merge(health).merge(ui::routes())` chain's start with:

```rust
    // The OAuth routes exist only with OAuth on; without it `/oauth/*` and
    // `/.well-known/*` stay 404, as before M10. They take no bearer token.
    let open = if app.oauth().is_some() {
        Router::new()
            .merge(health)
            .merge(oauth::routes().layer(timeout_layer()))
    } else {
        Router::new().merge(health)
    };
    open.merge(ui::routes())
```

  The rest of the chain (`.nest("/api/v1", ...)`, `.merge(keyed)`, the trace layer and `.with_state(app)`) is unchanged.
- Update the module doc to name "the OAuth routes (`crate::oauth`)".

**`src/http/api/mod.rs`.** `routes(app: &App)` becomes `routes(app: &Arc<App>)`. Its doc says "`app` - the application (`require_key`'s state; the import route's body limit comes from its config)". The layer becomes:

```rust
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(app),
            require_key,
        ))
```

`catalogs::routes(app)` still compiles: `&Arc<App>` coerces to `&App` (deref coercion: Rust follows `Deref` from `Arc<App>` to `App` automatically).

**`src/http/api/identity.rs`.** Add to `WhoAmI`:

```rust
    /// `key`, or `oauth` for a grant.
    pub kind: &'static str,
    /// A grant's client, as it names itself.
    pub client_name: Option<String>,
```

Import `CredentialKind`, and in `whoami` build them before `Ok(Json(..))`:

```rust
    // `match` on a reference: `client_name` binds a `&Option<String>`.
    let (kind, client_name) = match &principal.kind {
        CredentialKind::ApiKey => ("key", None),
        CredentialKind::OAuth { client_name, .. } => ("oauth", client_name.clone()),
    };
```

Add `kind,` and `client_name,` to the struct literal. In `whoami_names_the_key` (the test at :84), add `assert_eq!(body["kind"], "key");`.

Run: `cargo build --all-targets`
Expected: builds with no warnings.

- [ ] **Step 14: Tests for the cache, the dispatch, the challenge and the wiring**

In `src/auth.rs` `mod tests`:

1. Replace the router helper `app(cache)`, `status_and_body` and `the_middleware_admits_a_known_key_and_refuses_the_rest` with a database-backed version (the middleware's state is now the whole `App`):

```rust
    /// A router whose one route echoes the principal's key id, behind the
    /// real middleware over `app`.
    fn echo(app: &Arc<App>) -> Router {
        Router::new()
            .route(
                "/",
                get(|request: Request| async move {
                    request
                        .extensions()
                        .get::<Arc<Principal>>()
                        .map(|principal| principal.key_id.clone())
                        .unwrap_or_default()
                }),
            )
            .route_layer(axum::middleware::from_fn_with_state(
                Arc::clone(app),
                require_key,
            ))
            .with_state(Arc::clone(app))
    }

    /// The response's status, its `WWW-Authenticate` header and its body.
    async fn status_and_body(
        router: Router,
        authorization: Option<&str>,
    ) -> (StatusCode, Option<String>, String) {
        let mut request = axum::http::Request::builder().uri("/");
        if let Some(value) = authorization {
            request = request.header(header::AUTHORIZATION, value);
        }
        let response = router
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let challenge = response
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .map(|value| value.to_str().unwrap().to_string());
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 16)
            .await
            .unwrap();
        (status, challenge, String::from_utf8(bytes.to_vec()).unwrap())
    }

    /// A stored read key on `main`, polled into the app's cache.
    async fn stored_key(app: &App) -> (String, String) {
        let names = ["main".to_string()];
        let created = key_rows::create_with_id(
            app.pool(),
            &key_rows::NewKey {
                name: "reader",
                access: Access::Read,
                maintenance_report: false,
                grants: key_rows::Grants::Catalogs(&names),
                default_catalog: Some("main"),
                actor: "example-actor",
            },
            Utc::now(),
        )
        .await
        .unwrap();
        app.poll().await.unwrap();
        (created.id, created.key)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_middleware_admits_a_known_key_and_refuses_the_rest() {
        let test = crate::mcp::testing::app_with(&[("main", &[])], Config::default()).await;
        let (id, key) = stored_key(&test.app).await;
        let router = || echo(&test.app);

        let admitted = status_and_body(router(), Some(&format!("Bearer {key}"))).await;
        let refused = [
            status_and_body(router(), Some("Bearer okf_other-key")).await,
            status_and_body(router(), Some(&format!("Basic {key}"))).await,
            // With OAuth off, an access-token-shaped bearer is simply unknown.
            status_and_body(router(), Some("Bearer okfat_anything")).await,
            status_and_body(router(), None).await,
        ];

        assert_eq!(admitted, (StatusCode::OK, None, id));
        for (status, challenge, body) in refused {
            assert_eq!(status, StatusCode::UNAUTHORIZED);
            assert_eq!(challenge.as_deref(), Some("Bearer"));
            assert_eq!(body, "missing or unknown API key\n");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn with_oauth_on_a_401_names_the_resource_metadata() {
        let config = crate::store::oauth::testing::oauth_config("https://okf.example.com");
        let test = crate::mcp::testing::app_with(&[("main", &[])], config).await;
        let (_, key) = stored_key(&test.app).await;

        let refused = status_and_body(echo(&test.app), None).await;
        let admitted = status_and_body(echo(&test.app), Some(&format!("Bearer {key}"))).await;

        assert_eq!(refused.0, StatusCode::UNAUTHORIZED);
        assert_eq!(
            refused.1.as_deref(),
            Some(
                "Bearer resource_metadata=\"https://okf.example.com/.well-known/oauth-protected-resource\""
            )
        );
        // Static keys keep working beside OAuth.
        assert_eq!(admitted.0, StatusCode::OK);
    }
```

   Add `use crate::config::Config;` to the test module's imports.

2. Add cache tests:

```rust
    fn grant_record(id: &str, revoked: bool) -> KeyRecord {
        KeyRecord {
            sha256: None,
            kind: KeyKind::OAuth,
            client_id: Some("https://client.example.com/client.json".into()),
            client_name: Some("Example Client".into()),
            ..record(id, "unused", revoked)
        }
    }

    #[test]
    fn grants_are_found_by_id_only_and_carry_their_client() {
        let cache = KeyCache::from_records(vec![
            record("reader", "reader-key", false),
            grant_record("grant-1", false),
            grant_record("grant-old", true),
        ]);

        let grant = cache.find_grant("grant-1").expect("live grant");

        assert_eq!(
            grant.kind,
            CredentialKind::OAuth {
                client_id: Some("https://client.example.com/client.json".into()),
                client_name: Some("Example Client".into())
            }
        );
        assert_eq!(grant.actor, "actor-grant-1");
        assert!(cache.find_grant("grant-old").is_none());
        // A key is not a grant, and a grant has no key to present.
        assert!(cache.find_grant("reader").is_none());
        assert!(cache.find("unused").is_none());
        assert_eq!(cache.find("reader-key").unwrap().kind, CredentialKind::ApiKey);
    }
```

3. In `keys_load_from_the_database_and_reload_after_a_revoke`, nothing changes (it calls `Keys` directly).

In `src/oauth/access.rs`, add the end-to-end resource-side tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;

    use crate::auth::{authenticate, CredentialKind};
    use crate::mcp::testing::{app_with, TestApp};
    use crate::store::keys::{self as key_rows, Access};
    use crate::store::oauth::testing::{insert_client, insert_grant, mint_access, oauth_config};

    const RESOURCE: &str = "https://okf.example.com/mcp";

    /// An OAuth-on app with one grant on `main`, polled in.
    async fn with_grant() -> (TestApp, String) {
        let test = app_with(&[("main", &[])], oauth_config("https://okf.example.com")).await;
        let now = test.app.clock().now();
        let client = insert_client(
            test.app.pool(),
            "https://client.example.com/client.json",
            "Example Client",
            now,
        )
        .await;
        let grant = insert_grant(test.app.pool(), &client, Access::Write, &["main"], now).await;
        test.app.poll().await.unwrap();
        (test, grant)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_live_token_for_this_resource_acts_as_its_grant() {
        let (test, grant) = with_grant().await;
        let token = mint_access(&test.app, &grant, RESOURCE, TimeDelta::hours(1)).await;

        let principal = authenticate(&test.app, &token).await.unwrap().expect("admitted");
        // The second call is served from the cache: the same answer.
        let again = authenticate(&test.app, &token).await.unwrap().expect("admitted");

        assert_eq!(principal.key_id, grant);
        assert_eq!(principal.access, Access::Write);
        assert!(principal.reaches("main"));
        assert_eq!(principal.actor, "example-actor");
        assert_eq!(
            principal.kind,
            CredentialKind::OAuth {
                client_id: Some("https://client.example.com/client.json".into()),
                client_name: Some("Example Client".into())
            }
        );
        assert_eq!(again, principal);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn wrong_audience_expired_revoked_and_unknown_tokens_are_refused() {
        let (test, grant) = with_grant().await;
        let other = mint_access(
            &test.app,
            &grant,
            "https://other.example.com/mcp",
            TimeDelta::hours(1),
        )
        .await;
        let expired = mint_access(&test.app, &grant, RESOURCE, -TimeDelta::minutes(1)).await;
        let revoked = mint_access(&test.app, &grant, RESOURCE, TimeDelta::hours(1)).await;
        // Seen once (cached), then revoked: `forget` makes it stop at once.
        assert!(authenticate(&test.app, &revoked).await.unwrap().is_some());
        crate::store::oauth::tokens::revoke(
            test.app.pool(),
            &crate::keys::hash(&revoked),
            test.app.clock().now(),
        )
        .await
        .unwrap();
        test.app
            .oauth()
            .unwrap()
            .access
            .forget(&[crate::keys::hash(&revoked)]);
        let unknown = crate::keys::generate_with(crate::keys::ACCESS_TOKEN_PREFIX).unwrap();

        for token in [other, expired, revoked, unknown] {
            assert!(authenticate(&test.app, &token).await.unwrap().is_none());
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revoking_the_grant_refuses_its_cached_token_after_a_reload() {
        let (test, grant) = with_grant().await;
        let token = mint_access(&test.app, &grant, RESOURCE, TimeDelta::hours(1)).await;
        assert!(authenticate(&test.app, &token).await.unwrap().is_some());

        key_rows::revoke_by_id(test.app.pool(), &grant, test.app.clock().now())
            .await
            .unwrap();
        test.app.poll().await.unwrap();

        assert!(authenticate(&test.app, &token).await.unwrap().is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_access_token_reaches_the_api_through_the_router() {
        let (test, grant) = with_grant().await;
        let token = mint_access(&test.app, &grant, RESOURCE, TimeDelta::hours(1)).await;

        let (status, body) =
            crate::http::api::testing::send(&test, "GET", "/api/v1/whoami", Some(&token), None)
                .await;

        assert_eq!(status.as_u16(), 200);
        assert_eq!(body["key_id"], grant.as_str());
        assert_eq!(body["kind"], "oauth");
        assert_eq!(body["client_name"], "Example Client");
        assert_eq!(body["access"], "write");
    }

    #[test]
    fn remember_prunes_expired_entries_when_full() {
        let tokens = AccessTokens::new();
        let now = chrono::Utc::now();
        let live = |expires_at| LiveAccessToken {
            grant_id: "g".into(),
            resource: RESOURCE.into(),
            expires_at,
        };
        for index in 0..MAX_CACHED {
            let mut hash = [0u8; 32];
            hash[..8].copy_from_slice(&(index as u64).to_le_bytes());
            tokens.remember(hash, live(now - TimeDelta::seconds(1)), now, 0);
        }

        tokens.remember([0xff; 32], live(now + TimeDelta::hours(1)), now, 0);

        let cache = tokens.lock();
        assert_eq!(cache.tokens.len(), 1, "the expired entries were pruned");
        assert!(cache.tokens.contains_key(&[0xff; 32]));
    }

    #[test]
    fn a_lookup_that_straddles_a_revocation_does_not_re_cache_the_token() {
        // The interleaving from the PR #243 review, made deterministic by
        // calling the cache's steps in that order: a request misses and reads
        // the generation, the revocation commits and forgets, then the first
        // request tries to cache what it read before the revocation.
        let tokens = AccessTokens::new();
        let now = chrono::Utc::now();
        let hash = [0x11; 32];
        let token = LiveAccessToken {
            grant_id: "g".into(),
            resource: RESOURCE.into(),
            expires_at: now + TimeDelta::hours(1),
        };
        let seen = tokens.generation();

        tokens.forget(&[hash]);
        tokens.remember(hash, token.clone(), now, seen);

        assert!(!tokens.lock().tokens.contains_key(&hash), "not re-cached");
        // A lookup that starts after the revocation caches normally (the
        // database, not the cache, then says whether the token is live).
        tokens.remember(hash, token, now, tokens.generation());
        assert!(tokens.lock().tokens.contains_key(&hash));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_revoked_token_read_before_its_revocation_is_refused_next_time() {
        // The same race end to end: a lookup reads the token while it is
        // live, the revocation commits and forgets, then the stale lookup's
        // `remember` runs with the generation it read. The next verify goes
        // to the database, which says revoked.
        let (test, grant) = with_grant().await;
        let oauth = test.app.oauth().expect("OAuth is on");
        let token = mint_access(&test.app, &grant, RESOURCE, TimeDelta::hours(1)).await;
        let hash = crate::keys::hash(&token);
        let now = test.app.clock().now();
        let seen = oauth.access.generation();
        let stale = tokens::find_access(test.app.pool(), &hash, now)
            .await
            .unwrap()
            .expect("live before the revocation");

        tokens::revoke(test.app.pool(), &hash, now).await.unwrap();
        oauth.access.forget(&[hash]);
        oauth.access.remember(hash, stale, now, seen);

        let after = oauth.access.verify(&test.app, RESOURCE, &token).await.unwrap();
        assert!(after.is_none(), "the revoked token is not accepted from the cache");
    }
}
```

(`send` in `src/http/api/testing.rs` takes the key text and adds `Bearer `, so an access token goes through it unchanged.)

> **Corrected (issue #246, 2026-11-11, PR #254 review).** One more test, `a_cached_token_past_its_expiry_is_refused_and_dropped`: it `remember`s a `LiveAccessToken` for the live grant with `expires_at` one second before the app's clock (not stored in the database), then asserts `verify` refuses it, drops it from the cache, and leaves the generation unchanged. Unlike the expired token above, which the database lookup already refuses, it fails if the cache-expiry check in `verify` is removed. That check drops the entry without `forget`: expiry is not a revocation.

In `src/http/mod.rs` tests, add:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn oauth_routes_exist_only_with_oauth_on() {
        let off = app_with(&[("main", &[])], Config::default()).await;
        let on = app_with(
            &[("main", &[])],
            crate::store::oauth::testing::oauth_config("https://okf.example.com"),
        )
        .await;
        let get = |test: &crate::mcp::testing::TestApp| {
            router(Arc::clone(&test.app), &CancellationToken::new()).oneshot(
                Request::get("/mcp")
                    .header("host", "okf.example.com")
                    .body(Body::empty())
                    .expect("request"),
            )
        };

        let refused_off = get(&off).await.unwrap();
        let refused_on = get(&on).await.unwrap();

        assert_eq!(refused_off.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(refused_off.headers()["www-authenticate"], "Bearer");
        assert_eq!(refused_on.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            refused_on.headers()["www-authenticate"],
            "Bearer resource_metadata=\"https://okf.example.com/.well-known/oauth-protected-resource\""
        );
    }
```

(Task 3 extends this test to the well-known documents. Until then the OAuth router is empty, so there is no OAuth path to probe yet.)

Run: `cargo test --lib auth:: oauth:: http:: -- --test-threads=6`
Expected: PASS.

- [ ] **Step 15: The poll loop prunes OAuth rows hourly**

Add to `src/app.rs` `mod tests`:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn poll_prunes_oauth_rows_at_most_hourly() {
        use crate::store::keys::Access;
        use crate::store::oauth::testing::{insert_client, insert_grant};
        let test = app_with(&[("main", &[])], Config::default()).await;
        let pool = test.app.pool().clone();
        let now = Utc::now();
        let client =
            insert_client(&pool, "https://client.example.com/client.json", "Example", now).await;
        let grant = insert_grant(&pool, &client, Access::Read, &["main"], now).await;
        let stale_token = |byte: u8| {
            let pool = pool.clone();
            let grant = grant.clone();
            async move {
                sqlx::query(
                    "INSERT INTO oauth_tokens \
                     (sha256, grant_id, kind, family_id, resource, created_at, expires_at) \
                     VALUES ($1, $2, 'access', 'f', 'https://okf.example.com/mcp', $3, $4)",
                )
                .bind([byte; 32].as_slice())
                .bind(&grant)
                .bind(now - TimeDelta::days(9))
                .bind(now - TimeDelta::days(8))
                .execute(&pool)
                .await
                .unwrap();
            }
        };
        let count = |table: &'static str| {
            let pool = pool.clone();
            async move {
                // `AssertSqlSafe`: `table` is one of this test's literals.
                sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!(
                    "SELECT count(*) FROM {table}"
                )))
                    .fetch_one(&pool)
                    .await
                    .unwrap()
            }
        };
        stale_token(1).await;
        sqlx::query(
            "INSERT INTO oauth_clients (client_id, kind, redirect_uris, metadata, created_at) \
             VALUES ('unbound', 'dcr', ARRAY['https://client.example.com/callback'], '{}', $1)",
        )
        .bind(now - TimeDelta::days(2))
        .execute(&pool)
        .await
        .unwrap();

        // The first pass prunes.
        test.app.poll().await.unwrap();
        assert_eq!(count("oauth_tokens").await, 0);
        assert_eq!(count("oauth_clients").await, 1, "the bound client stays");

        // Within the hour, a new stale row waits for the next hourly pass.
        stale_token(2).await;
        test.app.poll().await.unwrap();
        assert_eq!(count("oauth_tokens").await, 1);
    }
```

Run: `cargo test --lib app::tests::poll_prunes_oauth`
Expected: PASS.

- [ ] **Step 15a: Property tests for the secret prefixes (spec "Testing")**

Append to the test module of `src/keys.rs`. hegel 0.50.3 is imported as `hegel` (dev-dependency `hegeltest`); `generators::text()` and `generators::sampled_from` exist (REG/hegeltest-0.50.3/src/generators/strings.rs:254, combinators.rs:51), and `#[hegel::test]` is used the same way in `src/okf/markdown.rs:768`.

```rust
    /// Every kind of secret, by prefix. `KEY_PREFIX` (`okf_`) differs from the
    /// others at its fourth byte, so no prefix is a prefix of another.
    const PREFIXES: [&str; 5] = [
        KEY_PREFIX,
        ACCESS_TOKEN_PREFIX,
        REFRESH_TOKEN_PREFIX,
        SIGN_IN_CODE_PREFIX,
        AUTHORIZATION_CODE_PREFIX,
    ];

    #[hegel::test]
    fn a_prefixed_secret_is_exactly_one_kind(tc: hegel::TestCase) {
        use hegel::generators;
        let prefix: &str = tc.draw(generators::sampled_from(PREFIXES.to_vec()));
        let rest: String = tc.draw(generators::text());
        let token = format!("{prefix}{rest}");

        // The bearer dispatch (`auth::authenticate`) routes on
        // `starts_with`, so a token must never look like two kinds.
        let kinds = PREFIXES.iter().filter(|p| token.starts_with(**p)).count();
        assert_eq!(kinds, 1, "{token:?}");
    }

    #[hegel::test]
    fn generated_secrets_keep_their_prefix_and_alphabet(tc: hegel::TestCase) {
        use hegel::generators;
        let prefix: &str = tc.draw(generators::sampled_from(PREFIXES.to_vec()));

        let secret = generate_with(prefix).expect("random source");

        assert!(secret.starts_with(prefix));
        assert_eq!(secret.len(), prefix.len() + 43);
        assert!(secret[prefix.len()..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
    }
```

Append to the test module of `src/auth.rs`:

```rust
    #[hegel::test]
    fn bearer_never_panics_and_returns_a_trimmed_nonempty_token(tc: hegel::TestCase) {
        use hegel::generators;
        let value: String = tc.draw(generators::text());
        let mut headers = HeaderMap::new();
        // Text that is not a valid header value is simply not a header.
        let Ok(parsed) = value.parse() else { return };
        headers.insert(header::AUTHORIZATION, parsed);

        if let Some(token) = bearer(&headers) {
            assert!(!token.is_empty());
            assert_eq!(token, token.trim());
        }
    }
```

Run: `cargo test --lib keys:: auth::tests::bearer_never`
Expected: PASS (hegel runs each property 100 times by default).

- [ ] **Step 16: Run every gate**

Run each command and paste the counts into the PR:

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
cargo test -- --test-threads=6
```

Expected: all pass, with no warnings. `tests/server.rs`, `tests/smoke.rs` and the `ui_e2e` journeys pass unchanged: they use `okf_` keys and OAuth is off by default. Fix any `rustfmt` reflow the long lines above need with `cargo fmt --all`.

- [ ] **Step 17: Check the plan's interfaces against the code**

The plan's "Interfaces shared across tasks" already includes corrections 1 to 15 above (folded in while the plan was written). If the implementation had to differ from any signature there, fix the plan in this PR with a dated `> **Corrected (issue #N).**` note, then grep Tasks 3 to 7 for the changed symbol:

Run: `rg -n "remember\(|ExpiringMap::new|tokens::revoke|prune_unbound_dcr|validate\(\)|CimdFetcher::new|CredentialKind::OAuth" docs/superpowers/plans/2026-11-11-okf-mcp-m10-oauth.md`
Expected: every match agrees with the code as merged.

- [ ] **Step 18: Changelog**

Create `changelog/$(date -u +%Y-%m-%dT%H%M%SZ)-m10-task-02-oauth-foundation.md`:

```markdown
# M10 Task 2: OAuth foundation

## Added
- Migration `0008_oauth.sql`: `oauth_clients`, `sign_in_codes`,
  `sign_in_code_catalogs` and `oauth_tokens`. `api_keys` gains `kind`
  (`key` or `oauth`) and `client_id`. A grant row has no hash, and the
  database refuses an admin or maintenance-reporting grant
  (`api_keys_oauth_caps`).
- Configuration: `public_url` (turns OAuth on; must be an `https` origin
  whose host is in `allowed_hosts`) and an `[oauth]` section
  (`access_token_ttl`, `refresh_token_ttl`, `code_max_expiry`, `dcr_enabled`,
  `cimd_allow_private`).
- `BearerVerifier`: the bearer middleware accepts `okfat_` OAuth access
  tokens beside `okf_` keys, checking each token's audience (RFC 8707) and
  its grant.
- With OAuth on, a 401 carries `WWW-Authenticate: Bearer resource_metadata="..."`
  (RFC 9728).
- `src/oauth/`: shared state for the authorization server (pending
  authorizations, authorization codes, rate limits, errors). Its endpoints
  come in later tasks.
- The poll loop prunes expired OAuth tokens, unredeemed codes and unused
  registered clients once an hour.

## Changed
- `GET /api/v1/whoami` and `GET /api/v1/keys` include `kind` and
  `client_name`.
- The live name/actor uniqueness rule applies to API keys only.

## Notes
- Nothing changes without `public_url`: keys authenticate as before and
  401s still say `Bearer`.
```

- [ ] **Step 19: Commit**

```bash
git add migrations/0008_oauth.sql src/keys.rs src/config.rs src/lib.rs src/store/mod.rs \
    src/store/keys.rs src/store/oauth src/auth.rs src/app.rs src/db.rs src/oauth \
    src/http/mod.rs src/http/api/mod.rs src/http/api/keys.rs src/http/api/identity.rs \
    src/mcp/testing.rs src/selection.rs src/cli.rs \
    docs/superpowers/plans/2026-11-11-okf-mcp-m10-oauth.md changelog/
git commit -m "feat(oauth): schema, configuration and bearer verification for OAuth

Migration 0008 adds OAuth clients, sign-in codes and tokens, and makes a
grant an api_keys row of kind 'oauth' with no hash. The bearer middleware
dispatches okfat_ access tokens to an audience-checked lookup beside the
key cache, and 401s name the protected resource metadata when public_url
is set. src/oauth holds the shared state later tasks fill in.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Well-known documents, DCR, the CIMD fetcher

Depends on Task 2 (the `src/oauth/` skeleton, `OAuth`, `OAuthSettings`, `OAuthError`, `TokenBucket`, `store::oauth::clients::{ClientKind, ClientRecord, find}`, the migration). Runs in parallel with Task 4.

**Facts this task relies on** (verified; `REG` is `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/`):

- **`url` is already in the tree** at 2.5.8 (`Cargo.lock:3835-3837`, through reqwest). `reqwest::Url` is a re-export of `url::Url` (REG/reqwest-0.13.5/src/lib.rs:282), so a parsed `url::Url` is passed to `reqwest` as is (`impl IntoUrl for Url`, REG/reqwest-0.13.5/src/into_url.rs:9).
- `url::Url` gives `scheme()` (lib.rs:903), `username()` (:1054), `password()` (:1087), `host_str()` (:1162), `host() -> Option<Host<&str>>` (:1201), `path()` (:1391) and `fragment()` (:1545). `Host` is `Domain(S) | Ipv4(Ipv4Addr) | Ipv6(Ipv6Addr)` (REG/url-2.5.8/src/host.rs:46-60).
- **reqwest 0.13.5 client controls:**
  - `ClientBuilder::redirect(Policy)` (REG/reqwest-0.13.5/src/async_impl/client.rs:1392). `redirect::Policy::none()` (src/redirect.rs:58) does not follow redirects: a 3xx comes back as an ordinary response.
  - `no_proxy()` (client.rs:1436) and `timeout(Duration)` (:1450), the latter a whole-request deadline.
  - `https_only(bool)` (client.rs:2262).
  - **`dns_resolver<R: IntoResolve>(R)`** (client.rs:2327) replaces the resolver for every connection the client opens. `IntoResolve` is implemented for any `R: Resolve + 'static` and for `Arc<R>` (src/dns/resolve.rs:166-178).
  - `reqwest::dns::{Resolve, Name, Addrs, Resolving}` are public (src/dns/mod.rs:3, lib.rs:378). `Resolve::resolve(&self, Name) -> Resolving`, where `Resolving = Pin<Box<dyn Future<Output = Result<Addrs, BoxError>> + Send>>` and `Addrs = Box<dyn Iterator<Item = SocketAddr> + Send>` (resolve.rs:15-34). `Name::as_str()` gives the host (resolve.rs:50).
  - **A literal IP host never reaches the resolver**, because hyper connects to it directly. The fetcher therefore classifies literal IP hosts itself before sending.
  - `Response::chunk() -> Result<Option<Bytes>>` (src/async_impl/response.rs:310) needs no feature. `bytes_stream` needs the `stream` feature, which the project does not enable (response.rs:345-351).
  - `Response::content_length()` (:90), `headers()` (:68), `status()` (:56).
- **Why the resolver is the rebinding defence:** reqwest connects to exactly the addresses the custom resolver returns. A resolver that refuses when *any* resolved address is non-public therefore checks the very addresses the socket uses, with no window between check and connect. Task 2's planning note said "pin the connection". `ClientBuilder::resolve` (client.rs:2308) would need a new client per fetch, while the resolver does the same check on one shared client.
- **IP classification in core** (toolchain 1.99.1):
  - `Ipv4Addr`: `is_unspecified`, `is_loopback`, `is_private`, `is_link_local`, `is_multicast`, `is_broadcast` and `is_documentation` are stable.
  - `Ipv6Addr`: `is_unspecified`, `is_loopback`, `is_multicast`, `is_unique_local`, `is_unicast_link_local` (stable since 1.84) and `to_ipv4_mapped` are stable.
  - `is_global` is unstable (REG-free: `rustc --print sysroot`/lib/rustlib/src/rust/library/core/src/net/ip_addr.rs:350, :1655 carry `#[unstable]`). The classifier below is therefore written out by hand.
- `axum::extract::DefaultBodyLimit::max(usize)` (REG/axum-core-0.5.6/src/extract/default_body_limit.rs:150) caps a route's body; a larger body is 413.
- `crate::keyed_lock::KeyedLocks::new()` and `lock(&self, key: i64) -> KeyedGuard<'_>` (`src/keyed_lock.rs:58`, `:71`): fair per-key async locks with no leftover entry after release. The fetcher reuses it for single-flight, keyed by the first 8 bytes of the URL's SHA-256.
- `crate::store::catalogs::is_unique_violation(&sqlx::Error)` (`src/store/catalogs.rs:318`).
- `crate::db::test_db() -> TestDb` with `.pool` (`src/db.rs:50`), and `crate::mcp::testing::app_with(&[(name, files)], Config) -> TestApp { app }` (`src/mcp/testing.rs:42`).
- `App::clock().now() -> DateTime<Utc>` (`src/app.rs:147`, `src/clock.rs:15`).
- `Config::default().allowed_hosts` is `["localhost", "127.0.0.1", "::1"]` (`src/config.rs:26`), so `public_url = "http://127.0.0.1:8080"` passes Task 2's `Config::validate` in tests.

**Interface corrections this task needs from Task 2** (the plan's shared interfaces are updated to match):

1. `oauth_clients` gains `cache_expires_at timestamptz` (CIMD only: when the cached document must be refetched), and `ClientRecord` gains `pub cache_expires_at: Option<DateTime<Utc>>`. Without it, the `Cache-Control` freshness the spec requires has nowhere to live.
2. `store::oauth::clients` (Task 2) defines a private `client_columns!()` macro (the select list), `#[derive(sqlx::FromRow)] struct ClientRow` and `impl From<ClientRow> for ClientRecord`. Task 3's queries reuse all three.
   > **Corrected (issue #246, 2026-11-11).** The select list is a macro, not `const CLIENT_COLUMNS: &str`: sqlx 0.9 refuses a `format!`ed `String` as SQL (`SqlSafeStr`), so queries splice it with `concat!(.., client_columns!(), ..)`. `insert_dcr` below is updated.
3. `insert_dcr` takes the new client's fields and lets Postgres pick the id: `insert_dcr(pool: &PgPool, new: &NewDcrClient<'_>, now: DateTime<Utc>) -> Result<ClientRecord, OkfError>`. The id is `gen_random_uuid()::text`, as `api_keys.id` already uses.
4. `CimdFetcher::new(allow_private: bool, allow_http: bool) -> Result<Self, OkfError>`. `OAuth::new` passes `allow_private = cimd_allow_private` and `allow_http = cimd_allow_private && issuer starts with "http://"`. A test rig with a loopback `http` `public_url` and `cimd_allow_private` may thus serve CIMD documents over plain HTTP; production (an `https` `public_url`) never can. Task 6's end-to-end CIMD host relies on this.

**Files:**
- Modify: `Cargo.toml` (`url`), `Cargo.lock`
- Replace (Task 2 stubs): `src/oauth/metadata.rs`, `src/oauth/clients.rs`
- Modify: `src/store/oauth/clients.rs` (append Task 3 functions and their tests)
- Create: `tests/cimd_no_proxy.rs` (added by issue #247: the no-proxy defence's test)
- Create: `changelog/<date -u +%Y-%m-%dT%H%M%SZ>-m10-task-03-metadata-dcr-cimd.md`

- [ ] **Step 1: Add the `url` dependency**

In `Cargo.toml`, after the `toml = "1.1"` line's block (keep the list alphabetical: put it after `tracing-subscriber`):

```toml
# Parsing and checking OAuth URLs: CIMD client ids and redirect URIs
# (`src/oauth/clients.rs`). Already in the tree through reqwest; pinned to
# the same version so there is one copy.
url = "2.5.8"
```

Run: `cargo check`
Expected: compiles; `Cargo.lock` gains `url` as a direct dependency of `okf_mcp` and no new package.

- [ ] **Step 2: Write the failing metadata tests**

Replace `src/oauth/metadata.rs` with the module below, without the two handler bodies yet. Write `todo!()` in `protected_resource` and `authorization_server` only for this step, and delete it in Step 4. The tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::json;
    // `oneshot` sends one request through a router without a socket.
    use tower::ServiceExt;

    use crate::config::Config;
    use crate::mcp::testing;

    fn settings(dcr_enabled: bool) -> OAuthSettings {
        OAuthSettings {
            issuer: "https://okf.example.com".into(),
            resource: "https://okf.example.com/mcp".into(),
            resource_metadata_url: "https://okf.example.com/.well-known/oauth-protected-resource"
                .into(),
            access_token_ttl: std::time::Duration::from_secs(3600),
            refresh_token_ttl: std::time::Duration::from_secs(30 * 86_400),
            code_max_expiry: std::time::Duration::from_secs(7 * 86_400),
            dcr_enabled,
            cimd_allow_private: false,
        }
    }

    #[test]
    fn protected_resource_metadata_matches_the_spec() {
        let document = serde_json::to_value(ProtectedResourceMetadata::new(&settings(true)))
            .expect("serializes");

        assert_eq!(
            document,
            json!({
                "resource": "https://okf.example.com/mcp",
                "authorization_servers": ["https://okf.example.com"],
                "bearer_methods_supported": ["header"],
                "resource_name": "okf-mcp"
            })
        );
    }

    #[test]
    fn authorization_server_metadata_matches_the_spec() {
        let document = serde_json::to_value(AuthorizationServerMetadata::new(&settings(true)))
            .expect("serializes");

        assert_eq!(
            document,
            json!({
                "issuer": "https://okf.example.com",
                "authorization_endpoint": "https://okf.example.com/oauth/authorize",
                "token_endpoint": "https://okf.example.com/oauth/token",
                "registration_endpoint": "https://okf.example.com/oauth/register",
                "revocation_endpoint": "https://okf.example.com/oauth/revoke",
                "response_types_supported": ["code"],
                "grant_types_supported": ["authorization_code", "refresh_token"],
                "code_challenge_methods_supported": ["S256"],
                "token_endpoint_auth_methods_supported": ["none"],
                "revocation_endpoint_auth_methods_supported": ["none"],
                "client_id_metadata_document_supported": true,
                "authorization_response_iss_parameter_supported": true
            })
        );
    }

    #[test]
    fn without_dcr_there_is_no_registration_endpoint() {
        let document = serde_json::to_value(AuthorizationServerMetadata::new(&settings(false)))
            .expect("serializes");

        assert!(document.get("registration_endpoint").is_none());
        assert_eq!(document["client_id_metadata_document_supported"], true);
    }

    // Task 2's shared helper: OAuth on at a loopback `public_url`.
    fn oauth_config() -> Config {
        crate::store::oauth::testing::oauth_config("http://127.0.0.1:8080")
    }

    async fn get(test: &testing::TestApp, uri: &str) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
        let response = routes()
            .with_state(Arc::clone(&test.app))
            .oneshot(Request::get(uri).body(Body::empty()).expect("request"))
            .await
            .expect("infallible");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 16)
            .await
            .expect("body");
        let body = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).expect("json")
        };
        (status, headers, body)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_routes_serve_both_documents_at_every_path() {
        let test = testing::app_with(&[("main", &[])], oauth_config()).await;

        for path in [
            "/.well-known/oauth-protected-resource",
            "/.well-known/oauth-protected-resource/mcp",
        ] {
            let (status, headers, body) = get(&test, path).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert_eq!(body["resource"], "http://127.0.0.1:8080/mcp", "{path}");
            assert_eq!(headers["access-control-allow-origin"], "*", "{path}");
        }
        let (status, _, body) = get(&test, "/.well-known/oauth-authorization-server").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["issuer"], "http://127.0.0.1:8080");
        assert_eq!(
            body["registration_endpoint"],
            "http://127.0.0.1:8080/oauth/register"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn without_oauth_the_documents_are_404() {
        let test = testing::app_with(&[("main", &[])], Config::default()).await;

        let (status, _, _) = get(&test, "/.well-known/oauth-authorization-server").await;

        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
```

Run: `cargo test --lib oauth::metadata`
Expected: the three pure tests FAIL to compile until Step 3 defines the structs. Once Step 3 is in, the two router tests FAIL at `todo!()`.

- [ ] **Step 3: Write the documents**

`src/oauth/metadata.rs` (whole file; the tests from Step 2 stay at the bottom):

> **Corrected (issue #247, 2026-11-11).** Task 2's `oauth::routes()` layers `Cache-Control: no-store` over every OAuth response, so the `public, max-age=300` below never reached a client: `public()` sets only `Access-Control-Allow-Origin: *`, and the documents are `no-store` like the rest. The routes use `PROTECTED_RESOURCE_PATH` and `AUTHORIZATION_SERVER_PATH`. The router tests send through `crate::oauth::routes()` (as `http::router` mounts it) and also check `no-store`, CORS on both documents, and 404 at all three paths without OAuth. Read `src/oauth/metadata.rs`.

```rust
//! The two discovery documents (spec "Endpoints"): Protected Resource
//! Metadata (RFC 9728), which tells a client where this resource's
//! authorization server is, and Authorization Server Metadata (RFC 8414),
//! which tells it the endpoints and what they support. Both are public: no
//! token, and `Access-Control-Allow-Origin: *` so a browser-based MCP client
//! can read them too.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::app::App;
use crate::oauth::OAuthSettings;

/// RFC 9728 Protected Resource Metadata for `<public_url>/mcp`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProtectedResourceMetadata {
    /// The resource identifier: `<public_url>/mcp`, the value a token's
    /// `resource` must equal.
    pub resource: String,
    /// The one authorization server: this server's issuer.
    pub authorization_servers: Vec<String>,
    /// Tokens travel only in the `Authorization` header.
    // `&'static [&'static str]`: a fixed list compiled into the binary;
    // serde writes it as a JSON array, with no allocation.
    pub bearer_methods_supported: &'static [&'static str],
    /// A human-readable name for the resource.
    pub resource_name: &'static str,
}

impl ProtectedResourceMetadata {
    /// The document for these settings.
    ///
    /// # Arguments
    /// * `settings` - the OAuth settings (issuer and resource).
    ///
    /// # Returns
    /// The document.
    pub fn new(settings: &OAuthSettings) -> Self {
        Self {
            resource: settings.resource.clone(),
            authorization_servers: vec![settings.issuer.clone()],
            bearer_methods_supported: &["header"],
            resource_name: "okf-mcp",
        }
    }
}

/// RFC 8414 Authorization Server Metadata for the built-in server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuthorizationServerMetadata {
    /// The issuer: `<public_url>`, also sent as `iss` on authorization responses.
    pub issuer: String,
    /// `<public_url>/oauth/authorize`.
    pub authorization_endpoint: String,
    /// `<public_url>/oauth/token`.
    pub token_endpoint: String,
    /// `<public_url>/oauth/register`, left out when DCR is off.
    // `skip_serializing_if`: serde omits the key entirely for `None`,
    // instead of writing `null`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registration_endpoint: Option<String>,
    /// `<public_url>/oauth/revoke`.
    pub revocation_endpoint: String,
    /// Only the authorization code flow.
    pub response_types_supported: &'static [&'static str],
    /// Authorization codes and refresh tokens.
    pub grant_types_supported: &'static [&'static str],
    /// PKCE with S256 only.
    pub code_challenge_methods_supported: &'static [&'static str],
    /// Public clients only.
    pub token_endpoint_auth_methods_supported: &'static [&'static str],
    /// Public clients only.
    pub revocation_endpoint_auth_methods_supported: &'static [&'static str],
    /// An `https://` `client_id` is fetched as a Client ID Metadata Document.
    pub client_id_metadata_document_supported: bool,
    /// Authorization responses carry `iss` (RFC 9207).
    pub authorization_response_iss_parameter_supported: bool,
}

impl AuthorizationServerMetadata {
    /// The document for these settings.
    ///
    /// # Arguments
    /// * `settings` - the OAuth settings (issuer and `dcr_enabled`).
    ///
    /// # Returns
    /// The document.
    pub fn new(settings: &OAuthSettings) -> Self {
        let issuer = &settings.issuer;
        Self {
            issuer: issuer.clone(),
            authorization_endpoint: format!("{issuer}/oauth/authorize"),
            token_endpoint: format!("{issuer}/oauth/token"),
            // `bool::then`: `Some(closure())` when true, `None` when false.
            registration_endpoint: settings
                .dcr_enabled
                .then(|| format!("{issuer}/oauth/register")),
            revocation_endpoint: format!("{issuer}/oauth/revoke"),
            response_types_supported: &["code"],
            grant_types_supported: &["authorization_code", "refresh_token"],
            code_challenge_methods_supported: &["S256"],
            token_endpoint_auth_methods_supported: &["none"],
            revocation_endpoint_auth_methods_supported: &["none"],
            client_id_metadata_document_supported: true,
            authorization_response_iss_parameter_supported: true,
        }
    }
}

/// The discovery routes. Mounted only when OAuth is on (Task 2); each
/// handler still answers 404 without it, so the router is safe to mount
/// unconditionally in tests.
///
/// # Returns
/// `GET /.well-known/oauth-protected-resource` (also with the `/mcp`
/// suffix, the path-suffixed form RFC 9728 section 3.1 clients try first) and
/// `GET /.well-known/oauth-authorization-server`.
pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get(protected_resource),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(protected_resource),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(authorization_server),
        )
}

/// `GET /.well-known/oauth-protected-resource[/mcp]`.
async fn protected_resource(State(app): State<Arc<App>>) -> Response {
    match app.oauth() {
        Some(oauth) => public(ProtectedResourceMetadata::new(&oauth.settings)),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `GET /.well-known/oauth-authorization-server`.
async fn authorization_server(State(app): State<Arc<App>>) -> Response {
    match app.oauth() {
        Some(oauth) => public(AuthorizationServerMetadata::new(&oauth.settings)),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// A public JSON document: readable cross-origin, cacheable for five minutes.
// `impl Serialize` in argument position: any serializable type, chosen at
// compile time (a generic parameter without a name).
fn public(document: impl Serialize) -> Response {
    let mut response = Json(document).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=300"),
    );
    response
}
```

- [ ] **Step 4: Run the metadata tests**

Run: `cargo test --lib oauth::metadata`
Expected: 5 passed.

- [ ] **Step 5: Write the failing store tests**

Append to the `#[cfg(test)] mod tests` of `src/store/oauth/clients.rs` (Task 2 created the module; add `use super::*;` and `use crate::db::test_db;` if absent):

> **Corrected (issue #247, 2026-11-11).** Task 2's test module already has a local `insert_dcr(pool, client_id, created_at)` helper, which would shadow the new `super::insert_dcr` (a local item beats a glob import); it is renamed `insert_dcr_row`. The tests use Task 2's `now()`, `insert_client` and `insert_grant` instead of `at(hour)` and a hand-written `api_keys` insert, `prune_unbound_dcr` keeps Task 2's own test, and two tests are added: `upsert_cimd_never_rewrites_a_dcr_row` and `only_dcr_clients_without_a_grant_are_counted`. Read the module.

```rust
    fn at(hour: u32) -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(2026, 11, 11, hour, 0, 0)
            .single()
            .expect("a valid time")
    }

    fn cimd(url: &str, name: &str, fetched: DateTime<Utc>) -> ClientRecord {
        ClientRecord {
            client_id: url.to_string(),
            kind: ClientKind::Cimd,
            client_name: Some(name.to_string()),
            redirect_uris: vec!["https://client.example.com/callback".to_string()],
            metadata: serde_json::json!({ "client_id": url, "client_name": name }),
            fetched_at: Some(fetched),
            cache_expires_at: Some(fetched + chrono::TimeDelta::minutes(5)),
            created_at: fetched,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_dcr_client_gets_a_fresh_uuid_and_is_found() {
        let db = test_db().await;
        let uris = vec!["https://client.example.com/callback".to_string()];
        let metadata = serde_json::json!({ "client_name": "Example" });

        let first = insert_dcr(
            &db.pool,
            &NewDcrClient {
                client_name: Some("Example"),
                redirect_uris: &uris,
                metadata: &metadata,
            },
            at(12),
        )
        .await
        .expect("insert");
        let second = insert_dcr(
            &db.pool,
            &NewDcrClient {
                client_name: None,
                redirect_uris: &uris,
                metadata: &metadata,
            },
            at(12),
        )
        .await
        .expect("insert");

        assert_eq!(first.kind, ClientKind::Dcr);
        assert_eq!(first.client_id.len(), 36, "a UUID: {}", first.client_id);
        assert_ne!(first.client_id, second.client_id);
        assert_eq!(first.fetched_at, None);
        assert_eq!(first.cache_expires_at, None);
        let found = find(&db.pool, &first.client_id).await.expect("find");
        assert_eq!(found, Some(first));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn upsert_cimd_inserts_then_refreshes_but_keeps_created_at() {
        let db = test_db().await;
        let url = "https://client.example.com/oauth/client.json";

        upsert_cimd(&db.pool, &cimd(url, "Old", at(10)))
            .await
            .expect("insert");
        upsert_cimd(&db.pool, &cimd(url, "New", at(11)))
            .await
            .expect("refresh");

        let found = find(&db.pool, url).await.expect("find").expect("row");
        assert_eq!(found.client_name.as_deref(), Some("New"));
        assert_eq!(found.fetched_at, Some(at(11)));
        assert_eq!(found.created_at, at(10), "the first sighting is kept");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unbound_dcr_clients_are_counted_and_pruned_by_age() {
        let db = test_db().await;
        let uris = vec!["https://client.example.com/callback".to_string()];
        let metadata = serde_json::json!({});
        let new = NewDcrClient {
            client_name: None,
            redirect_uris: &uris,
            metadata: &metadata,
        };
        let old = insert_dcr(&db.pool, &new, at(1)).await.expect("old");
        let bound = insert_dcr(&db.pool, &new, at(1)).await.expect("bound");
        insert_dcr(&db.pool, &new, at(20)).await.expect("young");
        upsert_cimd(&db.pool, &cimd("https://client.example.com/c.json", "C", at(1)))
            .await
            .expect("cimd");
        // A grant bound to `bound`: the smallest valid oauth key row.
        sqlx::query(
            "INSERT INTO api_keys (name, kind, client_id, access, all_catalogs, actor, created_at) \
             VALUES ('grant', 'oauth', $1, 'read', true, 'example-actor', $2)",
        )
        .bind(&bound.client_id)
        .bind(at(2))
        .execute(&db.pool)
        .await
        .expect("grant");

        assert_eq!(count_unbound_dcr(&db.pool).await.expect("count"), 2);
        let pruned = prune_unbound_dcr(&db.pool, at(12)).await.expect("prune");

        assert_eq!(pruned, 1, "only the old unbound DCR client");
        assert!(find(&db.pool, &old.client_id).await.expect("find").is_none());
        assert!(find(&db.pool, &bound.client_id).await.expect("find").is_some());
        assert_eq!(count_unbound_dcr(&db.pool).await.expect("count"), 1);
    }
```

Run: `cargo test --lib store::oauth::clients`
Expected: FAIL to compile (`insert_dcr`, `NewDcrClient`, `upsert_cimd`, `count_unbound_dcr` not found).

- [ ] **Step 6: Write the store functions**

> **Corrected (issue #247, 2026-11-11, PR #257 review).** `register` below counted the unbound clients and then inserted, so concurrent registrations at 499 all read 499 and all inserted (a test saw 6 or 7 of 8 succeed). What shipped: `insert_dcr(pool, new, now, max_unbound) -> Result<Option<ClientRecord>, OkfError>` takes `pg_advisory_xact_lock(0x6f6b666f /* "okfo" */, 1)` in a transaction and runs one `INSERT ... SELECT ... WHERE <unbound count> < $5 RETURNING ...`; `None` means the cap was reached. The count subquery is a private `unbound_dcr_count!()` macro shared with `count_unbound_dcr`. `register` calls it with `MAX_UNBOUND_DCR` and answers `slow_down` on `None`, with no separate count. The lock is in the database, so it holds for a CLI or a second process too. Tests: `insert_dcr_stops_at_the_cap_of_unbound_clients` (store) and `concurrent_registrations_never_pass_the_cap` (8 simultaneous registrations at 499: exactly one 201, the rest 429, 500 stored). Read `src/store/oauth/clients.rs` and `register` in `src/oauth/clients.rs`.

Append to `src/store/oauth/clients.rs`, above its test module. This reuses Task 2's `client_columns!()`, `ClientRow` and `From<ClientRow> for ClientRecord` (correction 2). `prune_unbound_dcr` is Task 2's (its correction 6); the test below exercises it beside this task's functions, and the code block below does not repeat it.

```rust
/// A client registering through RFC 7591, before it has an id.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NewDcrClient<'a> {
    /// Its `client_name`, if it sent one.
    pub client_name: Option<&'a str>,
    /// Its validated redirect URIs.
    pub redirect_uris: &'a [String],
    /// The registered metadata as the registration response echoes it
    /// (without `client_id`, which the database picks).
    pub metadata: &'a serde_json::Value,
}

/// Registers a DCR client under a fresh UUID.
///
/// # Arguments
/// * `pool` - the database.
/// * `new` - the validated registration.
/// * `now` - the registration time (server clock).
///
/// # Returns
/// The stored client.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn insert_dcr(
    pool: &PgPool,
    new: &NewDcrClient<'_>,
    now: DateTime<Utc>,
) -> Result<ClientRecord, OkfError> {
    // `concat!` splices the shared column list into the `RETURNING` clause
    // at compile time, so the row decodes through the same `ClientRow` as `find`.
    let row = sqlx::query_as::<_, ClientRow>(concat!(
        "INSERT INTO oauth_clients (client_id, kind, client_name, redirect_uris, metadata, created_at) \
         VALUES (gen_random_uuid()::text, 'dcr', $1, $2, $3, $4) RETURNING ",
        client_columns!()
    ))
    .bind(new.client_name)
    .bind(new.redirect_uris)
    .bind(new.metadata)
    .bind(now)
    .fetch_one(pool)
    .await?;
    Ok(row.into())
}

/// Stores a freshly fetched Client ID Metadata Document, or refreshes the
/// cached one. The row's `created_at` (its first sighting) is kept. A DCR
/// row can never share the id, since DCR ids are UUIDs and CIMD ids are URLs,
/// but the `WHERE` makes sure a refresh never rewrites one.
///
/// # Arguments
/// * `pool` - the database.
/// * `client` - the validated document, with `kind` `Cimd`.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn upsert_cimd(pool: &PgPool, client: &ClientRecord) -> Result<(), OkfError> {
    sqlx::query(
        "INSERT INTO oauth_clients \
         (client_id, kind, client_name, redirect_uris, metadata, fetched_at, cache_expires_at, created_at) \
         VALUES ($1, 'cimd', $2, $3, $4, $5, $6, $7) \
         ON CONFLICT (client_id) DO UPDATE SET \
           client_name = EXCLUDED.client_name, redirect_uris = EXCLUDED.redirect_uris, \
           metadata = EXCLUDED.metadata, fetched_at = EXCLUDED.fetched_at, \
           cache_expires_at = EXCLUDED.cache_expires_at \
         WHERE oauth_clients.kind = 'cimd'",
    )
    .bind(&client.client_id)
    .bind(&client.client_name)
    .bind(&client.redirect_uris)
    .bind(&client.metadata)
    .bind(client.fetched_at)
    .bind(client.cache_expires_at)
    .bind(client.created_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// How many DCR clients no grant points at: the number the registration cap
/// (spec "Abuse limits", 500) counts.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn count_unbound_dcr(pool: &PgPool) -> Result<i64, OkfError> {
    Ok(sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM oauth_clients c WHERE c.kind = 'dcr' \
         AND NOT EXISTS (SELECT 1 FROM api_keys k WHERE k.client_id = c.client_id)",
    )
    .fetch_one(pool)
    .await?)
}
```

Run: `cargo test --lib store::oauth::clients`
Expected: Task 2's tests plus these 3 pass.

- [ ] **Step 7: Write the failing pure-validation tests**

Replace Task 2's stub `src/oauth/clients.rs` with the module skeleton from Step 8, and put this test module at its bottom first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    use axum::http::HeaderMap;
    use serde_json::json;

    fn v4(text: &str) -> IpAddr {
        IpAddr::V4(text.parse::<Ipv4Addr>().expect("v4"))
    }

    fn v6(text: &str) -> IpAddr {
        IpAddr::V6(text.parse::<Ipv6Addr>().expect("v6"))
    }

    #[test]
    fn only_public_addresses_are_public() {
        // Documentation ranges are refused too, so these "public" examples
        // use ordinary unicast space that no test ever connects to.
        for public in ["8.8.4.4", "1.1.1.1", "2606:4700::1111"] {
            let ip = if public.contains(':') { v6(public) } else { v4(public) };
            assert!(is_public_address(ip), "{public}");
        }
        for refused in [
            "0.0.0.0", "0.1.2.3", "10.0.0.1", "100.64.0.1", "127.0.0.1", "169.254.169.254",
            "172.16.0.1", "192.0.0.8", "192.0.2.1", "192.168.1.1", "198.18.0.1",
            "198.51.100.1", "203.0.113.1", "224.0.0.1", "240.0.0.1", "255.255.255.255",
        ] {
            assert!(!is_public_address(v4(refused)), "{refused}");
        }
        for refused in [
            "::", "::1", "fc00::1", "fd12:3456::1", "fe80::1", "fec0::1", "ff02::1",
            "2001:db8::1", "100::1",
            // IPv4-mapped and NAT64 forms of private addresses.
            "::ffff:127.0.0.1", "::ffff:10.0.0.1", "64:ff9b::a00:1",
        ] {
            assert!(!is_public_address(v6(refused)), "{refused}");
        }
        assert!(is_public_address(v6("::ffff:8.8.4.4")), "mapped public v4");
    }

    #[test]
    fn redirect_uris_are_https_or_loopback_http() {
        for good in [
            "https://client.example.com/callback",
            "https://client.example.com:8443/cb?x=1",
            "http://127.0.0.1:33418/callback",
            "http://localhost/callback",
            "http://[::1]:9000/cb",
        ] {
            assert!(validate_redirect_uri(good).is_ok(), "{good}");
        }
        for bad in [
            "http://client.example.com/callback",
            "https://client.example.com/cb#frag",
            "https://user:pw@client.example.com/cb",
            "javascript:alert(1)",
            "cursor://callback",
            "/relative",
            "",
        ] {
            assert!(
                matches!(validate_redirect_uri(bad), Err(OAuthError::InvalidRedirectUri(_))),
                "{bad}"
            );
        }
        let long = format!("https://client.example.com/{}", "a".repeat(MAX_URI_CHARS));
        assert!(validate_redirect_uri(&long).is_err(), "too long");
    }

    #[test]
    fn max_age_is_clamped_to_five_minutes_and_a_day() {
        let header = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(header::CACHE_CONTROL, value.parse().expect("header"));
            headers
        };

        assert_eq!(cache_lifetime(&HeaderMap::new()), CACHE_FLOOR);
        assert_eq!(cache_lifetime(&header("max-age=3600")), Duration::from_secs(3600));
        assert_eq!(cache_lifetime(&header("public, MAX-AGE=7200")), Duration::from_secs(7200));
        assert_eq!(cache_lifetime(&header("max-age=10")), CACHE_FLOOR);
        assert_eq!(cache_lifetime(&header("max-age=999999")), CACHE_CEILING);
        assert_eq!(cache_lifetime(&header("no-store")), CACHE_FLOOR);
        assert_eq!(cache_lifetime(&header("no-cache, max-age=3600")), CACHE_FLOOR);
        assert_eq!(cache_lifetime(&header("max-age=junk")), CACHE_FLOOR);
    }

    #[test]
    fn metadata_validation_accepts_public_clients_only() {
        let valid = validate_metadata(&json!({
            "client_name": "Example",
            "redirect_uris": ["https://client.example.com/callback"],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
            "logo_uri": "https://client.example.com/logo.png"
        }))
        .expect("valid");
        assert_eq!(valid.client_name.as_deref(), Some("Example"));
        assert_eq!(valid.grant_types, ["authorization_code", "refresh_token"]);

        let defaults = validate_metadata(&json!({
            "redirect_uris": ["https://client.example.com/callback"]
        }))
        .expect("defaults");
        assert_eq!(defaults.grant_types, ["authorization_code"]);
        assert_eq!(defaults.response_types, ["code"]);
        assert_eq!(defaults.client_name, None);

        for (bad, why) in [
            (json!({}), "no redirect_uris"),
            (json!({ "redirect_uris": [] }), "empty redirect_uris"),
            (json!({ "redirect_uris": "https://client.example.com/cb" }), "not a list"),
            (json!({ "redirect_uris": ["https://client.example.com/cb"],
                     "token_endpoint_auth_method": "client_secret_basic" }), "confidential"),
            (json!({ "redirect_uris": ["https://client.example.com/cb"],
                     "grant_types": ["client_credentials"] }), "grant type"),
            (json!({ "redirect_uris": ["https://client.example.com/cb"],
                     "response_types": ["token"] }), "response type"),
            (json!({ "redirect_uris": ["https://client.example.com/cb"],
                     "client_name": "x".repeat(MAX_CLIENT_NAME_CHARS + 1) }), "long name"),
            (json!(["not", "an", "object"]), "not an object"),
        ] {
            assert!(
                matches!(validate_metadata(&bad), Err(OAuthError::InvalidClientMetadata(_))),
                "{why}"
            );
        }
        let bad_uri = validate_metadata(&json!({ "redirect_uris": ["http://client.example.com/cb"] }));
        assert!(matches!(bad_uri, Err(OAuthError::InvalidRedirectUri(_))));
        let too_many: Vec<String> = (0..=MAX_REDIRECT_URIS)
            .map(|n| format!("https://client.example.com/cb{n}"))
            .collect();
        assert!(validate_metadata(&json!({ "redirect_uris": too_many })).is_err());
    }

    #[test]
    fn cimd_urls_must_be_canonical_https_with_a_path() {
        assert!(cimd_url("https://client.example.com/oauth/client.json", false).is_ok());
        assert!(cimd_url("http://127.0.0.1:9000/client.json", true).is_ok());
        for bad in [
            "http://client.example.com/client.json",
            "https://client.example.com",
            "https://client.example.com/",
            "https://client.example.com/a/../client.json",
            "https://CLIENT.example.com/client.json",
            "https://client.example.com/client.json#x",
            "https://u:p@client.example.com/client.json",
        ] {
            assert!(
                matches!(cimd_url(bad, false), Err(OAuthError::InvalidClient(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_cimd_document_must_name_its_own_url() {
        let url = "https://client.example.com/oauth/client.json";
        let document = json!({
            "client_id": url,
            "client_name": "Example",
            "redirect_uris": ["https://client.example.com/callback"]
        });
        assert!(validate_cimd_document(url, &document).is_ok());

        let other = json!({
            "client_id": "https://client.example.com/other.json",
            "redirect_uris": ["https://client.example.com/callback"]
        });
        assert!(matches!(
            validate_cimd_document(url, &other),
            Err(OAuthError::InvalidClient(_))
        ));
        let missing = json!({ "redirect_uris": ["https://client.example.com/callback"] });
        assert!(validate_cimd_document(url, &missing).is_err());
    }
}
```

Run: `cargo test --lib oauth::clients`
Expected: FAIL to compile until Step 8 defines the functions.

- [ ] **Step 8: Write `src/oauth/clients.rs`**

Whole file (the Step 7 tests at the bottom, extended in Steps 9 and 11):

> **Corrected (issue #247, 2026-11-11).** What shipped differs from the block below in these points (read `src/oauth/clients.rs`):
> - `is_loopback_host` takes `&Host<&str>`: `url::Host<&str>` is not `Copy`, and a value bound in a match pattern cannot be moved into the guard (E0507).
> - The resolver reports "no address" with a typed `NoAddress` error, found in reqwest's source chain like `RefusedAddress`, instead of matching the text "no addresses".
> - The name lookup is a `Lookup` function pointer: `CimdFetcher::new` calls a private `with_lookup(allow_private, allow_http, system_lookup)`, and tests pass fixed answers to prove the connection goes only to the addresses the guard checked.
> - `is_public_v6` also judges 6to4 (`2002::/16`) by its embedded IPv4 address and refuses `::/96` (IPv4-compatible).
> - `resolve_cimd` truncates `now` to microseconds (`SubsecRound::trunc_subsecs(6)`), so the record it returns equals the row a later cache hit reads back.
> - `register` stores and echoes `client_name` only when the client sent one (no `null`), and relies on the `no-store` layer in `oauth::routes()` rather than setting the header itself.
> - `CimdError` derives `Clone, Copy, PartialEq, Eq`, so tests compare results with `assert_eq!`.
> - **Corrected (issue #247, 2026-11-11, PR #257 review).** `validate_metadata` refuses anything but a JSON object (`invalid_client_metadata`) before deserializing: serde's derived struct deserializer also accepts an array in field order, so `["Example", ["https://client.example.com/cb"], null, null, null]` registered a client. CIMD documents share the check. Tests: `metadata_must_be_a_json_object` and `registration_refuses_metadata_that_is_not_an_object`.

```rust
//! OAuth clients (spec "Client ID Metadata Documents", "`POST /oauth/register`"):
//! dynamic registration (RFC 7591), the CIMD fetcher, and [`resolve`], which
//! turns an authorize request's `client_id` into a [`ClientRecord`] either
//! way. Every client is public: no secret, PKCE required.
//!
//! The fetcher is the server's one outbound request driven by a stranger's
//! input (a `client_id` URL), so it is hardened against server-side request
//! forgery: HTTPS only, no redirects, a 5-second deadline, a 16 KiB body
//! cap, and a DNS resolver that refuses non-public addresses. reqwest connects
//! to exactly the addresses that resolver returns, so the check cannot be
//! raced by DNS rebinding.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use chrono::{DateTime, TimeDelta, Utc};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use serde::Deserialize;
use serde_json::{json, Value};
use url::{Host, Url};

use crate::app::App;
use crate::error::OkfError;
use crate::keyed_lock::KeyedLocks;
use crate::oauth::error::OAuthError;
use crate::oauth::OAuth;
use crate::store::oauth::clients::{self as client_rows, ClientKind, ClientRecord, NewDcrClient};

/// The whole-request deadline of a CIMD fetch.
pub const CIMD_TIMEOUT: Duration = Duration::from_secs(5);
/// The largest CIMD document read, in bytes.
pub const CIMD_MAX_BYTES: usize = 16 * 1024;
/// The shortest a fetched document is trusted before a refetch.
pub const CACHE_FLOOR: Duration = Duration::from_secs(5 * 60);
/// The longest a fetched document is trusted, and how long a stale one may
/// stand in for a failed refetch.
pub const CACHE_CEILING: Duration = Duration::from_secs(24 * 60 * 60);
/// Registration is refused while this many DCR clients have no grant.
pub const MAX_UNBOUND_DCR: i64 = 500;
/// The most redirect URIs one client may register.
pub const MAX_REDIRECT_URIS: usize = 10;
/// The longest redirect URI accepted, in characters.
pub const MAX_URI_CHARS: usize = 2000;
/// The longest `client_name` accepted, in characters.
pub const MAX_CLIENT_NAME_CHARS: usize = 200;
/// The largest registration request body.
const REGISTER_BODY_LIMIT: usize = 16 * 1024;

/// Is `ip` an ordinary public unicast address, safe for the server to
/// connect to on a stranger's behalf? Refuses loopback, private, link-local,
/// unique-local, shared (CGNAT), documentation, benchmarking, multicast,
/// broadcast, reserved and unspecified space, and judges IPv4-mapped and
/// NAT64 IPv6 forms by the IPv4 address inside.
///
/// # Arguments
/// * `ip` - a resolved or literal address.
///
/// # Returns
/// `true` only for an address the fetcher may connect to.
pub fn is_public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    // `octets()` is the address as four bytes; a slice pattern matches on them.
    let [a, b, c, _] = ip.octets();
    let special = ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_documentation()
        || a == 0                                   // 0.0.0.0/8 "this network"
        || (a == 100 && (64..=127).contains(&b))    // 100.64.0.0/10 shared (CGNAT)
        || (a == 192 && b == 0 && c == 0)           // 192.0.0.0/24 protocol assignments
        || (a == 198 && (b == 18 || b == 19))       // 198.18.0.0/15 benchmarking
        || a >= 240; // 240.0.0.0/4 reserved (and broadcast)
    !special
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    let segments = ip.segments();
    // 64:ff9b::/96, the NAT64 well-known prefix: the last 32 bits are IPv4.
    if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        let [.., high, low] = segments;
        return is_public_v4(Ipv4Addr::from((u32::from(high) << 16) | u32::from(low)));
    }
    let special = ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || ip.is_unique_local()                     // fc00::/7
        || ip.is_unicast_link_local()               // fe80::/10
        || (segments[0] & 0xffc0) == 0xfec0         // fec0::/10 site-local (deprecated)
        || (segments[0] == 0x2001 && segments[1] == 0x0db8) // 2001:db8::/32 documentation
        || segments[..4] == [0x100, 0, 0, 0]; // 100::/64 discard
    !special
}

/// Is this a loopback host, as written in a URL?
fn is_loopback_host(host: Host<&str>) -> bool {
    match host {
        Host::Domain(name) => name.eq_ignore_ascii_case("localhost"),
        Host::Ipv4(ip) => ip.is_loopback(),
        Host::Ipv6(ip) => ip.is_loopback(),
    }
}

/// Checks one redirect URI: `https` with a host, or `http` on a loopback
/// host (RFC 8252 native clients); no fragment, no user information, at
/// most [`MAX_URI_CHARS`] characters. OAuth 2.1 compares redirect URIs
/// exactly, so nothing here normalizes.
///
/// # Arguments
/// * `uri` - the URI as the client sent it.
///
/// # Errors
/// [`OAuthError::InvalidRedirectUri`] naming what is wrong.
pub fn validate_redirect_uri(uri: &str) -> Result<(), OAuthError> {
    let refuse = |why: &str| Err(OAuthError::InvalidRedirectUri(format!("{uri:?}: {why}")));
    if uri.chars().count() > MAX_URI_CHARS {
        return refuse("too long");
    }
    let Ok(parsed) = Url::parse(uri) else {
        return refuse("not an absolute URL");
    };
    if parsed.fragment().is_some() {
        return refuse("a redirect URI must not have a fragment");
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return refuse("a redirect URI must not carry user information");
    }
    match (parsed.scheme(), parsed.host()) {
        ("https", Some(_)) => Ok(()),
        ("http", Some(host)) if is_loopback_host(host) => Ok(()),
        ("http", Some(_)) => refuse("http is allowed only on a loopback host"),
        _ => refuse("must be https (or http on a loopback host)"),
    }
}

/// Client metadata after validation: what both registration routes keep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedMetadata {
    /// Its display name, if it gave one.
    pub client_name: Option<String>,
    /// Its redirect URIs, each checked by [`validate_redirect_uri`].
    pub redirect_uris: Vec<String>,
    /// A non-empty subset of `authorization_code` and `refresh_token`.
    pub grant_types: Vec<String>,
    /// `["code"]`.
    pub response_types: Vec<String>,
}

/// The fields of RFC 7591 client metadata this server reads. Unknown fields
/// (`logo_uri`, `scope`, ...) are ignored, as RFC 7591 section 2 allows.
#[derive(Debug, Deserialize)]
struct ClientMetadata {
    client_name: Option<String>,
    redirect_uris: Vec<String>,
    grant_types: Option<Vec<String>>,
    response_types: Option<Vec<String>>,
    token_endpoint_auth_method: Option<String>,
}

/// Validates client metadata, from a registration request or a CIMD
/// document.
///
/// # Arguments
/// * `document` - the JSON as received.
///
/// # Returns
/// The fields the server keeps, with RFC 7591's defaults filled in
/// (`grant_types` `["authorization_code"]`, `response_types` `["code"]`).
///
/// # Errors
/// [`OAuthError::InvalidClientMetadata`] for a shape, auth method, grant or
/// response type this server does not support;
/// [`OAuthError::InvalidRedirectUri`] for a bad redirect URI.
pub fn validate_metadata(document: &Value) -> Result<ValidatedMetadata, OAuthError> {
    let invalid = |why: String| OAuthError::InvalidClientMetadata(why);
    // `T::deserialize(&Value)` reads a `Value` already in memory, without
    // re-serializing it (serde_json implements `Deserializer` for `&Value`).
    let metadata = ClientMetadata::deserialize(document)
        .map_err(|error| invalid(format!("client metadata: {error}")))?;
    if metadata.redirect_uris.is_empty() || metadata.redirect_uris.len() > MAX_REDIRECT_URIS {
        return Err(invalid(format!(
            "redirect_uris must list 1 to {MAX_REDIRECT_URIS} URIs"
        )));
    }
    metadata
        .redirect_uris
        .iter()
        .try_for_each(|uri| validate_redirect_uri(uri))?;
    match metadata.token_endpoint_auth_method.as_deref() {
        None | Some("none") => {}
        Some(other) => {
            return Err(invalid(format!(
                "token_endpoint_auth_method {other:?} is not supported; only public clients (\"none\")"
            )))
        }
    }
    let grant_types = metadata
        .grant_types
        .unwrap_or_else(|| vec!["authorization_code".to_string()]);
    if grant_types.is_empty()
        || !grant_types
            .iter()
            .all(|grant| matches!(grant.as_str(), "authorization_code" | "refresh_token"))
    {
        return Err(invalid(
            "grant_types must be authorization_code and optionally refresh_token".to_string(),
        ));
    }
    let response_types = metadata
        .response_types
        .unwrap_or_else(|| vec!["code".to_string()]);
    if response_types != ["code"] {
        return Err(invalid("response_types must be [\"code\"]".to_string()));
    }
    if let Some(name) = &metadata.client_name {
        if name.chars().count() > MAX_CLIENT_NAME_CHARS || name.trim().is_empty() {
            return Err(invalid(format!(
                "client_name must be 1 to {MAX_CLIENT_NAME_CHARS} characters"
            )));
        }
    }
    Ok(ValidatedMetadata {
        client_name: metadata.client_name,
        redirect_uris: metadata.redirect_uris,
        grant_types,
        response_types,
    })
}

/// Parses a CIMD `client_id`: an `https` URL (or `http`, on a test rig that
/// allows it) in canonical form, with a path, without a fragment or user
/// information. Canonical form means the text equals the parser's own
/// serialization, which rules out dot segments, an upper-case host and
/// other spellings of the same URL; the document must name the identical
/// string.
///
/// # Arguments
/// * `client_id` - the `client_id` an authorize request carried.
/// * `allow_http` - also accept `http` (test rigs only).
///
/// # Errors
/// [`OAuthError::InvalidClient`].
pub fn cimd_url(client_id: &str, allow_http: bool) -> Result<Url, OAuthError> {
    let refuse = |why: &str| {
        Err(OAuthError::InvalidClient(format!(
            "client_id {client_id:?} is not a valid metadata document URL: {why}"
        )))
    };
    let Ok(url) = Url::parse(client_id) else {
        return refuse("not a URL");
    };
    if url.as_str() != client_id {
        return refuse("not in canonical form");
    }
    match url.scheme() {
        "https" => {}
        "http" if allow_http => {}
        _ => return refuse("must be https"),
    }
    if url.fragment().is_some() || !url.username().is_empty() || url.password().is_some() {
        return refuse("must not have a fragment or user information");
    }
    if url.path() == "/" || url.path().is_empty() {
        return refuse("must have a path");
    }
    Ok(url)
}

/// Validates a fetched Client ID Metadata Document.
///
/// # Arguments
/// * `url` - the URL it was fetched from (the `client_id`).
/// * `document` - its JSON.
///
/// # Returns
/// Its metadata.
///
/// # Errors
/// [`OAuthError::InvalidClient`] when its `client_id` is not exactly `url`,
/// or for any metadata problem (the authorize request names the client, so
/// a broken document is an invalid client, not a registration error).
pub fn validate_cimd_document(url: &str, document: &Value) -> Result<ValidatedMetadata, OAuthError> {
    if document.get("client_id").and_then(Value::as_str) != Some(url) {
        return Err(OAuthError::InvalidClient(format!(
            "the metadata document at {url:?} does not name itself as its client_id"
        )));
    }
    validate_metadata(document).map_err(|error| {
        OAuthError::InvalidClient(format!("the metadata document at {url:?}: {error}"))
    })
}

/// How long to trust a fetched document: its `Cache-Control: max-age`,
/// clamped to [[`CACHE_FLOOR`], [`CACHE_CEILING`]]. `no-store` or `no-cache`,
/// a missing header or an unreadable one means the floor.
///
/// # Arguments
/// * `headers` - the response headers.
///
/// # Returns
/// The lifetime.
pub fn cache_lifetime(headers: &HeaderMap) -> Duration {
    let mut max_age = None;
    // A response may carry several `Cache-Control` headers; directives are
    // comma-separated and case-insensitive (RFC 9111 section 5.2).
    for value in headers.get_all(header::CACHE_CONTROL) {
        let Ok(text) = value.to_str() else { continue };
        for directive in text.split(',').map(str::trim) {
            let lower = directive.to_ascii_lowercase();
            if lower == "no-store" || lower == "no-cache" {
                return CACHE_FLOOR;
            }
            if let Some(seconds) = lower.strip_prefix("max-age=") {
                max_age = seconds.trim_matches('"').parse::<u64>().ok();
            }
        }
    }
    max_age
        .map(Duration::from_secs)
        .unwrap_or(CACHE_FLOOR)
        .clamp(CACHE_FLOOR, CACHE_CEILING)
}

/// Why a CIMD fetch failed. Shown to the person on the authorize error page
/// (inside an [`OAuthError::InvalidClient`]), so no variant carries more
/// than the URL's own host and a status.
#[derive(Debug, thiserror::Error)]
pub enum CimdError {
    /// The host resolved to, or is, a non-public address.
    #[error("the metadata document's host is not a public address")]
    PrivateAddress,
    /// The host has no address.
    #[error("the metadata document's host does not resolve")]
    Unresolvable,
    /// Anything but 200 (redirects included: they are not followed).
    #[error("the metadata document answered HTTP {0}")]
    Status(u16),
    /// Over [`CIMD_MAX_BYTES`].
    #[error("the metadata document is larger than {CIMD_MAX_BYTES} bytes")]
    TooLarge,
    /// Not JSON.
    #[error("the metadata document is not JSON")]
    NotJson,
    /// The deadline passed.
    #[error("the metadata document took longer than {} seconds", CIMD_TIMEOUT.as_secs())]
    Timeout,
    /// Any other transport failure (TLS, connection refused).
    #[error("the metadata document could not be fetched")]
    Transport,
}

/// The refusal a resolver returns; recognized again in reqwest's error chain.
#[derive(Debug, thiserror::Error)]
#[error("refused a non-public address")]
struct RefusedAddress;

/// The fetcher's DNS resolver: the system resolver, then a refusal if any
/// address is non-public (unless private addresses are allowed). Refusing
/// on *any* bad address, rather than filtering, means a name that mixes
/// public and private addresses is never fetched at all.
#[derive(Debug, Clone, Copy)]
struct GuardedResolver {
    allow_private: bool,
}

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let allow_private = self.allow_private;
        let host = name.as_str().to_string();
        // `Box::pin(async move { ... })`: reqwest wants a boxed, pinned
        // future (a trait object), since a trait method cannot return
        // `impl Future` through `dyn Resolve`.
        Box::pin(async move {
            // Port 0: reqwest substitutes the URL's port.
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .collect();
            if addrs.is_empty() {
                return Err(std::io::Error::other("no addresses").into());
            }
            if !allow_private && addrs.iter().any(|addr| !is_public_address(addr.ip())) {
                return Err(Box::new(RefusedAddress) as Box<dyn std::error::Error + Send + Sync>);
            }
            let addrs: Addrs = Box::new(addrs.into_iter());
            Ok(addrs)
        })
    }
}

/// A fetched document and how long to trust it.
#[derive(Debug, Clone, PartialEq)]
pub struct Fetched {
    /// The parsed JSON.
    pub document: Value,
    /// From `Cache-Control`, clamped ([`cache_lifetime`]).
    pub lifetime: Duration,
}

/// Fetches Client ID Metadata Documents, one fetch per URL at a time.
pub struct CimdFetcher {
    client: reqwest::Client,
    allow_private: bool,
    allow_http: bool,
    // Single-flight: concurrent resolves of one URL queue here, and all but
    // the first find the fresh cache row when they get the lock.
    in_flight: KeyedLocks,
}

impl std::fmt::Debug for CimdFetcher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CimdFetcher")
            .field("allow_private", &self.allow_private)
            .field("allow_http", &self.allow_http)
            .finish_non_exhaustive()
    }
}

impl CimdFetcher {
    /// Builds the hardened HTTP client.
    ///
    /// # Arguments
    /// * `allow_private` - accept non-public addresses (`cimd_allow_private`,
    ///   test rigs only).
    /// * `allow_http` - accept `http` URLs (only with `cimd_allow_private` and
    ///   a loopback `http` `public_url`; see `OAuth::new`).
    ///
    /// # Returns
    /// The fetcher.
    ///
    /// # Errors
    /// [`OkfError::Config`] if the TLS backend cannot be initialized.
    pub fn new(allow_private: bool, allow_http: bool) -> Result<Self, OkfError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(CIMD_TIMEOUT)
            // A proxy would resolve the host itself, around the guard.
            .no_proxy()
            .https_only(!allow_http)
            .dns_resolver(GuardedResolver { allow_private })
            .user_agent(concat!("okf-mcp/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| OkfError::Config {
                path: "<oauth>".to_string(),
                message: format!("building the CIMD client: {error}"),
            })?;
        Ok(Self {
            client,
            allow_private,
            allow_http,
            in_flight: KeyedLocks::new(),
        })
    }

    /// Whether `http` CIMD URLs are accepted (test rigs only).
    pub fn allows_http(&self) -> bool {
        self.allow_http
    }

    /// Fetches one document. Does not validate it ([`validate_cimd_document`]).
    ///
    /// # Arguments
    /// * `url` - a URL [`cimd_url`] accepted.
    ///
    /// # Returns
    /// The JSON and its cache lifetime.
    ///
    /// # Errors
    /// A [`CimdError`] for each way the fetch can fail.
    pub async fn fetch(&self, url: &Url) -> Result<Fetched, CimdError> {
        // A literal IP host is connected to directly, never resolved, so the
        // guard in the resolver would not see it.
        let literal = match url.host() {
            Some(Host::Ipv4(ip)) => Some(IpAddr::V4(ip)),
            Some(Host::Ipv6(ip)) => Some(IpAddr::V6(ip)),
            Some(Host::Domain(_)) | None => None,
        };
        if let Some(ip) = literal {
            if !self.allow_private && !is_public_address(ip) {
                return Err(CimdError::PrivateAddress);
            }
        }
        let mut response = self
            .client
            .get(url.clone())
            .header(header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(classify_transport)?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(CimdError::Status(response.status().as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|length| length > CIMD_MAX_BYTES as u64)
        {
            return Err(CimdError::TooLarge);
        }
        let lifetime = cache_lifetime(response.headers());
        // Read chunk by chunk and stop at the cap, so a server that lies
        // about (or omits) its length still cannot make us buffer more.
        let mut body = Vec::with_capacity(CIMD_MAX_BYTES.min(4096));
        while let Some(chunk) = response.chunk().await.map_err(classify_transport)? {
            if body.len() + chunk.len() > CIMD_MAX_BYTES {
                return Err(CimdError::TooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        let document = serde_json::from_slice(&body).map_err(|_| CimdError::NotJson)?;
        Ok(Fetched { document, lifetime })
    }
}

/// Maps a reqwest failure to a [`CimdError`], finding the resolver's own
/// refusal in the error's source chain.
fn classify_transport(error: reqwest::Error) -> CimdError {
    if error.is_timeout() {
        return CimdError::Timeout;
    }
    // `std::iter::successors` walks `source()` links: Python's
    // `while e := e.__cause__`.
    let mut chain = std::iter::successors(
        Some(&error as &(dyn std::error::Error + 'static)),
        |current| current.source(),
    );
    if chain.any(|cause| cause.is::<RefusedAddress>()) {
        return CimdError::PrivateAddress;
    }
    if error.is_connect() {
        // Resolution failures surface as connect errors too.
        let mut chain = std::iter::successors(
            Some(&error as &(dyn std::error::Error + 'static)),
            |current| current.source(),
        );
        if chain.any(|cause| cause.to_string().contains("no addresses")) {
            return CimdError::Unresolvable;
        }
    }
    CimdError::Transport
}

/// The single-flight key for a URL: the first 8 bytes of its SHA-256. A
/// collision only makes two URLs take turns.
fn flight_key(url: &str) -> i64 {
    let digest = crate::keys::hash(url);
    let mut first = [0u8; 8];
    first.copy_from_slice(&digest[..8]);
    i64::from_be_bytes(first)
}

/// Turns an authorize request's `client_id` into its client: an `https`
/// URL is a CIMD client (fetched, or from the cache while fresh); anything
/// else must be a registered DCR client.
///
/// # Arguments
/// * `app` - the application (the database and clock).
/// * `oauth` - the OAuth state (the fetcher).
/// * `client_id` - as the request carried it.
///
/// # Returns
/// The client.
///
/// # Errors
/// [`OAuthError::InvalidClient`] for an unknown DCR id, a bad CIMD URL, a
/// failed fetch with no usable cached copy, or an invalid document;
/// [`OAuthError::Server`] for a database failure.
pub async fn resolve(app: &App, oauth: &OAuth, client_id: &str) -> Result<ClientRecord, OAuthError> {
    let looks_like_url = client_id.starts_with("https://")
        || (oauth.cimd.allows_http() && client_id.starts_with("http://"));
    if looks_like_url {
        return resolve_cimd(app, oauth, client_id).await;
    }
    match client_rows::find(app.pool(), client_id).await? {
        Some(client) if client.kind == ClientKind::Dcr => Ok(client),
        Some(_) | None => Err(OAuthError::InvalidClient(format!(
            "unknown client_id {client_id:?}"
        ))),
    }
}

/// Is a cached CIMD row still fresh at `now`?
fn is_fresh(client: &ClientRecord, now: DateTime<Utc>) -> bool {
    client.cache_expires_at.is_some_and(|expires| now < expires)
}

/// May a cached CIMD row stand in for a failed refetch at `now`? Up to
/// [`CACHE_CEILING`] after it was fetched.
fn is_usable_stale(client: &ClientRecord, now: DateTime<Utc>) -> bool {
    let ceiling = TimeDelta::from_std(CACHE_CEILING).unwrap_or(TimeDelta::MAX);
    client
        .fetched_at
        .is_some_and(|fetched| now < fetched + ceiling)
}

/// The CIMD half of [`resolve`].
async fn resolve_cimd(app: &App, oauth: &OAuth, client_id: &str) -> Result<ClientRecord, OAuthError> {
    let url = cimd_url(client_id, oauth.cimd.allows_http())?;
    let cached = client_rows::find(app.pool(), client_id).await?;
    if let Some(client) = &cached {
        if is_fresh(client, app.clock().now()) {
            return Ok(client.clone());
        }
    }
    // Held until this function returns: other resolves of the same URL wait.
    let _flight = oauth.cimd.in_flight.lock(flight_key(client_id)).await;
    // Another resolve may have refreshed it while this one waited.
    let cached = match client_rows::find(app.pool(), client_id).await? {
        Some(client) if is_fresh(&client, app.clock().now()) => return Ok(client),
        other => other.or(cached),
    };
    let now = app.clock().now();
    let fetched = match oauth.cimd.fetch(&url).await {
        Ok(fetched) => fetched,
        Err(error) => {
            return match cached {
                Some(client) if is_usable_stale(&client, now) => {
                    tracing::warn!(client = %client_id, %error, "CIMD refetch failed; using the cached document");
                    Ok(client)
                }
                Some(_) | None => Err(OAuthError::InvalidClient(error.to_string())),
            };
        }
    };
    let metadata = validate_cimd_document(client_id, &fetched.document)?;
    let lifetime = TimeDelta::from_std(fetched.lifetime).unwrap_or(TimeDelta::MAX);
    let client = ClientRecord {
        client_id: client_id.to_string(),
        kind: ClientKind::Cimd,
        client_name: metadata.client_name,
        redirect_uris: metadata.redirect_uris,
        metadata: fetched.document,
        fetched_at: Some(now),
        cache_expires_at: Some(now + lifetime),
        // `Option::map_or`: the cached row's first sighting, or now.
        created_at: cached.map_or(now, |client| client.created_at),
    };
    client_rows::upsert_cimd(app.pool(), &client).await?;
    Ok(client)
}

/// The registration route. Always mounted with the other OAuth routes; the
/// handler answers 404 when DCR is off (`dcr_enabled = false`), as if the
/// route did not exist.
///
/// # Returns
/// `POST /oauth/register`, with a 16 KiB body limit.
pub fn routes() -> Router<Arc<App>> {
    Router::new().route(
        "/oauth/register",
        post(register).layer(DefaultBodyLimit::max(REGISTER_BODY_LIMIT)),
    )
}

/// `POST /oauth/register` (RFC 7591).
///
/// The body is parsed by hand rather than with the `Json` extractor, so a
/// malformed body is an RFC 7591 `invalid_client_metadata` error, not
/// axum's plain-text 400 or 422.
async fn register(State(app): State<Arc<App>>, body: Bytes) -> Result<Response, OAuthError> {
    let Some(oauth) = app.oauth().filter(|oauth| oauth.settings.dcr_enabled) else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    if !oauth.register_limit.try_take(Instant::now()) {
        return Err(OAuthError::SlowDown);
    }
    let document: Value = serde_json::from_slice(&body).map_err(|_| {
        OAuthError::InvalidClientMetadata("the body must be a JSON object".to_string())
    })?;
    let metadata = validate_metadata(&document)?;
    if client_rows::count_unbound_dcr(app.pool()).await? >= MAX_UNBOUND_DCR {
        tracing::warn!("DCR refused: {MAX_UNBOUND_DCR} registered clients have no grant");
        return Err(OAuthError::SlowDown);
    }
    let now = app.clock().now();
    let stored_metadata = json!({
        "client_name": metadata.client_name,
        "redirect_uris": metadata.redirect_uris,
        "grant_types": metadata.grant_types,
        "response_types": metadata.response_types,
        "token_endpoint_auth_method": "none",
    });
    let client = client_rows::insert_dcr(
        app.pool(),
        &NewDcrClient {
            client_name: metadata.client_name.as_deref(),
            redirect_uris: &metadata.redirect_uris,
            metadata: &stored_metadata,
        },
        now,
    )
    .await?;
    let mut answer = stored_metadata;
    // `as_object_mut` borrows the JSON object's map mutably; it is always an
    // object here, since `json!({...})` built it.
    if let Some(fields) = answer.as_object_mut() {
        fields.insert("client_id".to_string(), Value::String(client.client_id));
        fields.insert("client_id_issued_at".to_string(), json!(now.timestamp()));
        if metadata.client_name.is_none() {
            fields.remove("client_name");
        }
    }
    let mut response = (StatusCode::CREATED, Json(answer)).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}
```

Run: `cargo test --lib oauth::clients`
Expected: the 6 Step 7 tests pass.

- [ ] **Step 9: Write the failing fetcher tests (local CIMD host)**

Add to the `tests` module of `src/oauth/clients.rs`. The local host is an axum server on `127.0.0.1:0` whose routes count their hits:

```rust
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use axum::response::Redirect;
    use axum::routing::get;

    /// A local CIMD host: its base URL, and counters the tests read.
    struct Host {
        base: String,
        hits: Arc<AtomicUsize>,
        failing: Arc<AtomicBool>,
    }

    /// Serves `/client.json` (a valid document naming its own URL, cached
    /// for an hour, counted, 500 while `failing`), `/mismatch.json` (names
    /// another URL), `/big.json` (over the cap), `/redirect` (302 to
    /// `/client.json`) and `/slow.json` (answers after one second).
    async fn host() -> Host {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("address"));
        let hits = Arc::new(AtomicUsize::new(0));
        let failing = Arc::new(AtomicBool::new(false));
        let client_url = format!("{base}/client.json");
        let (counter, failure) = (Arc::clone(&hits), Arc::clone(&failing));
        let router = Router::new()
            .route(
                "/client.json",
                // `move` closures own their captures; each request clones the
                // `Arc`s it needs, as Python closures would share them.
                get(move || {
                    let (counter, failure, client_url) =
                        (Arc::clone(&counter), Arc::clone(&failure), client_url.clone());
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        // Slow enough that concurrent resolves overlap.
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        if failure.load(Ordering::SeqCst) {
                            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                        }
                        (
                            [(header::CACHE_CONTROL, "max-age=3600")],
                            Json(json!({
                                "client_id": client_url,
                                "client_name": "Example Client",
                                "redirect_uris": ["https://client.example.com/callback"]
                            })),
                        )
                            .into_response()
                    }
                }),
            )
            .route(
                "/mismatch.json",
                get(|| async {
                    Json(json!({
                        "client_id": "https://client.example.com/other.json",
                        "redirect_uris": ["https://client.example.com/callback"]
                    }))
                }),
            )
            .route(
                "/big.json",
                get(|| async { format!("{{\"pad\": \"{}\"}}", "x".repeat(CIMD_MAX_BYTES)) }),
            )
            .route("/redirect", get(|| async { Redirect::temporary("/client.json") }));
        tokio::spawn(async move {
            axum::serve(listener, router).await.expect("serve");
        });
        Host {
            base,
            hits,
            failing,
        }
    }

    fn url(text: &str) -> Url {
        Url::parse(text).expect("url")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_fetcher_reads_a_document_and_its_lifetime() {
        let host = host().await;
        let fetcher = CimdFetcher::new(true, true).expect("fetcher");

        let fetched = fetcher
            .fetch(&url(&format!("{}/client.json", host.base)))
            .await
            .expect("fetched");

        assert_eq!(fetched.document["client_name"], "Example Client");
        assert_eq!(fetched.lifetime, Duration::from_secs(3600));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_fetcher_refuses_redirects_oversize_and_non_json() {
        let host = host().await;
        let fetcher = CimdFetcher::new(true, true).expect("fetcher");

        let redirected = fetcher.fetch(&url(&format!("{}/redirect", host.base))).await;
        let big = fetcher.fetch(&url(&format!("{}/big.json", host.base))).await;
        let missing = fetcher.fetch(&url(&format!("{}/nothing", host.base))).await;

        assert!(matches!(redirected, Err(CimdError::Status(307))), "{redirected:?}");
        assert!(matches!(big, Err(CimdError::TooLarge)), "{big:?}");
        assert!(matches!(missing, Err(CimdError::Status(404))), "{missing:?}");
        assert_eq!(host.hits.load(Ordering::SeqCst), 0, "the redirect was not followed");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_fetcher_refuses_private_addresses_by_name_and_literal() {
        let host = host().await;
        let port = host.base.rsplit(':').next().expect("port");
        let guarded = CimdFetcher::new(false, true).expect("fetcher");

        let by_name = guarded
            .fetch(&url(&format!("http://localhost:{port}/client.json")))
            .await;
        let by_literal = guarded
            .fetch(&url(&format!("http://127.0.0.1:{port}/client.json")))
            .await;
        let by_mapped = guarded
            .fetch(&url(&format!("http://[::ffff:127.0.0.1]:{port}/client.json")))
            .await;

        assert!(matches!(by_name, Err(CimdError::PrivateAddress)), "{by_name:?}");
        assert!(matches!(by_literal, Err(CimdError::PrivateAddress)), "{by_literal:?}");
        assert!(matches!(by_mapped, Err(CimdError::PrivateAddress)), "{by_mapped:?}");
        assert_eq!(host.hits.load(Ordering::SeqCst), 0, "nothing reached the host");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn without_allow_http_plain_http_is_refused() {
        let host = host().await;
        let fetcher = CimdFetcher::new(true, false).expect("fetcher");

        let fetched = fetcher.fetch(&url(&format!("{}/client.json", host.base))).await;

        assert!(matches!(fetched, Err(CimdError::Transport)), "{fetched:?}");
        assert!(cimd_url(&format!("{}/client.json", host.base), false).is_err());
    }
```

> **Corrected (issue #247, 2026-11-11).** Observed as the plan expected: `Redirect::temporary` answers 307, and `https_only(true)` refuses an `http` URL before sending (`CimdError::Transport`). The tests that shipped differ: the host type is `CimdHost` (`Host` collides with `url::Host`); a raw-socket `raw_host` serves a body with no `Content-Length`, so the 16 KiB cap is tested while streaming, and a body that arrives after the deadline (`the_fetcher_gives_up_after_its_deadline`); `/text` tests non-JSON; a mixed public-and-private lookup is refused; `the_fetcher_connects_only_to_the_addresses_its_resolver_checked` tests pinning (and `Unresolvable`); `tests/cimd_no_proxy.rs` tests `no_proxy`. Each defence was removed in turn and its test failed.

Run: `cargo test --lib oauth::clients`
Expected: these 4 tests pass with Step 8's code (they test it; there is no new production code). If `the_fetcher_refuses_redirects_oversize_and_non_json` reports a different redirect status, adjust the expected status to the one `Redirect::temporary` sends: axum 0.8.9 sends 307. If `without_allow_http_plain_http_is_refused` sees another variant, check how `https_only` reports itself (`reqwest::Error::is_builder`). Either way, record the observed behaviour in a `> **Corrected (issue #N).**` note.

- [ ] **Step 10: Run the fetcher tests**

Run: `cargo test --lib oauth::clients -- --test-threads=6`
Expected: 10 passed.

- [ ] **Step 11: Write the `resolve` and registration tests**

Add to the same `tests` module. They need an `App` with OAuth on: a loopback `http` `public_url` (passes `Config::validate`) with `cimd_allow_private`, which by correction 4 also allows `http` CIMD URLs.

> **Corrected (issue #247, 2026-11-11).** `rig_config` starts from Task 2's `store::oauth::testing::oauth_config`. `register_request` sends through `crate::oauth::routes()`, so `no-store` comes from the real layer. The cap test stores 499 clients and checks that the 500th registers and the 501st is refused; `registration_is_rate_limited` checks the burst of 10. Read the module.

```rust
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    use crate::config::{Config, OAuthConfig};
    use crate::mcp::testing::{self, TestApp};

    fn rig_config(dcr_enabled: bool) -> Config {
        Config {
            public_url: Some("http://127.0.0.1:8080".into()),
            oauth: OAuthConfig {
                cimd_allow_private: true,
                dcr_enabled,
                ..OAuthConfig::default()
            },
            ..Config::default()
        }
    }

    async fn rig(dcr_enabled: bool) -> TestApp {
        testing::app_with(&[("main", &[])], rig_config(dcr_enabled)).await
    }

    fn oauth(test: &TestApp) -> &OAuth {
        test.app.oauth().expect("OAuth is on")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_fetches_once_then_serves_the_cache() {
        let host = host().await;
        let test = rig(true).await;
        let client_id = format!("{}/client.json", host.base);

        let first = resolve(&test.app, oauth(&test), &client_id).await.expect("first");
        let second = resolve(&test.app, oauth(&test), &client_id).await.expect("second");

        assert_eq!(first.kind, ClientKind::Cimd);
        assert_eq!(first.client_name.as_deref(), Some("Example Client"));
        assert_eq!(first.redirect_uris, ["https://client.example.com/callback"]);
        let fetched = first.fetched_at.expect("fetched");
        assert_eq!(
            first.cache_expires_at,
            Some(fetched + TimeDelta::seconds(3600)),
            "max-age=3600"
        );
        assert_eq!(second, first);
        assert_eq!(host.hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_resolves_of_one_url_fetch_once() {
        let host = host().await;
        let test = rig(true).await;
        let client_id = format!("{}/client.json", host.base);

        // `tokio::join!` polls all four futures at once on this task.
        let (a, b, c, d) = tokio::join!(
            resolve(&test.app, oauth(&test), &client_id),
            resolve(&test.app, oauth(&test), &client_id),
            resolve(&test.app, oauth(&test), &client_id),
            resolve(&test.app, oauth(&test), &client_id),
        );

        for result in [a, b, c, d] {
            assert!(result.is_ok(), "{result:?}");
        }
        assert_eq!(host.hits.load(Ordering::SeqCst), 1, "single-flight");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_expired_cache_refetches_and_a_failed_refetch_serves_stale() {
        let host = host().await;
        let test = rig(true).await;
        let client_id = format!("{}/client.json", host.base);
        resolve(&test.app, oauth(&test), &client_id).await.expect("first");
        let expire = || {
            sqlx::query("UPDATE oauth_clients SET cache_expires_at = now() - interval '1 second'")
                .execute(test.app.pool())
        };

        expire().await.expect("expire");
        resolve(&test.app, oauth(&test), &client_id).await.expect("refetched");
        assert_eq!(host.hits.load(Ordering::SeqCst), 2);

        expire().await.expect("expire");
        host.failing.store(true, Ordering::SeqCst);
        let stale = resolve(&test.app, oauth(&test), &client_id).await;
        assert!(stale.is_ok(), "stale within a day: {stale:?}");
        assert_eq!(host.hits.load(Ordering::SeqCst), 3);

        sqlx::query("UPDATE oauth_clients SET fetched_at = now() - interval '25 hours'")
            .execute(test.app.pool())
            .await
            .expect("age");
        let too_old = resolve(&test.app, oauth(&test), &client_id).await;
        assert!(matches!(too_old, Err(OAuthError::InvalidClient(_))), "{too_old:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_refuses_mismatched_documents_and_unknown_ids() {
        let host = host().await;
        let test = rig(true).await;

        let mismatch =
            resolve(&test.app, oauth(&test), &format!("{}/mismatch.json", host.base)).await;
        let unknown = resolve(&test.app, oauth(&test), "no-such-client").await;

        assert!(matches!(mismatch, Err(OAuthError::InvalidClient(_))), "{mismatch:?}");
        assert!(matches!(unknown, Err(OAuthError::InvalidClient(_))), "{unknown:?}");
        assert!(
            client_rows::find(test.app.pool(), &format!("{}/mismatch.json", host.base))
                .await
                .expect("find")
                .is_none(),
            "an invalid document is not cached"
        );
    }

    async fn register_request(
        test: &TestApp,
        body: &str,
    ) -> (StatusCode, HeaderMap, Value) {
        let response = routes()
            .with_state(Arc::clone(&test.app))
            .oneshot(
                Request::post("/oauth/register")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .expect("request"),
            )
            .await
            .expect("infallible");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 16)
            .await
            .expect("body");
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, headers, json)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn registration_stores_a_public_client_and_resolve_finds_it() {
        let test = rig(true).await;

        let (status, headers, body) = register_request(
            &test,
            r#"{"client_name": "Example", "redirect_uris": ["https://client.example.com/cb"],
                "grant_types": ["authorization_code", "refresh_token"]}"#,
        )
        .await;

        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");
        assert_eq!(body["client_name"], "Example");
        assert_eq!(body["token_endpoint_auth_method"], "none");
        assert!(body.get("client_secret").is_none());
        assert!(body["client_id_issued_at"].is_i64());
        let client_id = body["client_id"].as_str().expect("client_id");
        let found = resolve(&test.app, oauth(&test), client_id).await.expect("found");
        assert_eq!(found.kind, ClientKind::Dcr);
        assert_eq!(found.redirect_uris, ["https://client.example.com/cb"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn registration_refuses_bad_metadata_with_rfc_7591_errors() {
        let test = rig(true).await;

        for (body, code) in [
            ("not json", "invalid_client_metadata"),
            (r#"{"redirect_uris": []}"#, "invalid_client_metadata"),
            (
                r#"{"redirect_uris": ["https://client.example.com/cb"],
                    "token_endpoint_auth_method": "client_secret_post"}"#,
                "invalid_client_metadata",
            ),
            (r#"{"redirect_uris": ["http://client.example.com/cb"]}"#, "invalid_redirect_uri"),
        ] {
            let (status, headers, json) = register_request(&test, body).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(json["error"], code, "{body}");
            assert_eq!(headers[header::CACHE_CONTROL], "no-store", "{body}");
        }
        assert_eq!(client_rows::count_unbound_dcr(test.app.pool()).await.expect("count"), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn registration_is_capped_at_500_unbound_clients() {
        let test = rig(true).await;
        sqlx::query(
            "INSERT INTO oauth_clients (client_id, kind, redirect_uris, metadata, created_at) \
             SELECT gen_random_uuid()::text, 'dcr', ARRAY['https://client.example.com/cb'], \
                    '{}'::jsonb, now() FROM generate_series(1, 500)",
        )
        .execute(test.app.pool())
        .await
        .expect("fill");

        let (status, _, json) =
            register_request(&test, r#"{"redirect_uris": ["https://client.example.com/cb"]}"#).await;

        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(json["error"], "slow_down");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn without_dcr_registration_is_404() {
        let test = rig(false).await;

        let (status, _, _) =
            register_request(&test, r#"{"redirect_uris": ["https://client.example.com/cb"]}"#).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
    }
```

Run: `cargo test --lib oauth::clients -- --test-threads=6`
Expected: 18 passed. If `registration_is_capped_at_500_unbound_clients` sees 429 without `"slow_down"`, Task 2's `OAuthError::SlowDown` serialization differs from the conventions: fix it there, not here.

- [ ] **Step 12: Gates**

Run each; all must pass:
- `cargo fmt --all --check`
- `cargo clippy --all-targets -- -D warnings`
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
- `cargo test -- --test-threads=6`

Expected: no warnings. Watch for two clippy lints and fix them if they fire. `clippy::cast_possible_truncation` is not on by default, but `CIMD_MAX_BYTES as u64` is lossless. `clippy::large_enum_variant` should not fire, since `CimdError` holds no `reqwest::Error`.

- [ ] **Step 13: Changelog**

Create `changelog/$(date -u +%Y-%m-%dT%H%M%SZ)-m10-task-03-metadata-dcr-cimd.md`:

```markdown
# M10 Task 3: discovery documents, dynamic client registration, CIMD

## Added

- `GET /.well-known/oauth-protected-resource` (also with the `/mcp` suffix):
  RFC 9728 Protected Resource Metadata naming `<public_url>/mcp` and this
  server as its authorization server.
- `GET /.well-known/oauth-authorization-server`: RFC 8414 metadata (S256
  PKCE, public clients, `client_id_metadata_document_supported`, RFC 9207
  `iss`). `registration_endpoint` is left out when `dcr_enabled = false`.
- `POST /oauth/register`: RFC 7591 registration for public clients
  (`token_endpoint_auth_method` `none`; `https` redirect URIs, or `http` on
  loopback). Refused with `slow_down` past the rate limit or while 500
  registered clients have no grant.
- Client ID Metadata Documents: an `https://` `client_id` is fetched over
  HTTPS without redirects, within 5 seconds and 16 KiB, from a public
  address only (checked in the DNS resolver, so rebinding cannot slip past
  it). The document must name its own URL. It is cached for its
  `Cache-Control` max-age, clamped to 5 minutes and 24 hours. A failed
  refetch serves the cached copy for up to a day. Concurrent requests for
  one URL fetch it once.

## Changed

- `url` 2.5.8 is now a direct dependency (it was already in the tree
  through reqwest).

## Notes

- The discovery documents carry `Access-Control-Allow-Origin: *` and
  `Cache-Control: public, max-age=300`. They are public by design.
- With `cimd_allow_private` and a loopback `http` `public_url` (test rigs
  only), CIMD documents may also be fetched over `http`.
```

- [ ] **Step 14: Commit**

```bash
git add Cargo.toml Cargo.lock src/oauth/metadata.rs src/oauth/clients.rs \
        src/store/oauth/clients.rs changelog/*-m10-task-03-metadata-dcr-cimd.md
git commit -m "feat(oauth): discovery documents, dynamic client registration and CIMD

RFC 9728 and RFC 8414 metadata, RFC 7591 registration for public
clients, and a hardened Client ID Metadata Document fetcher (https only,
no redirects, 5 s, 16 KiB, public addresses only via a guarded DNS
resolver, single-flight, Cache-Control caching clamped to 5 min..24 h).

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Sign-in codes: store, CLI, JSON API

Depends on Task 2. Parallel with Task 3. Tasks 5 and 7 build on it.

> **Corrected (issue #248, 2026-11-11).** What shipped differs from the steps below in these ways (the code is in `src/store/oauth/codes.rs`, `src/store/keys.rs`, `src/cli.rs`, `src/http/api/{keys,oauth}.rs`):
> - **Added scope:** `store::keys::resolve_active` (the `*_target` functions behind `okf-mcp key revoke|grant|ungrant|maintenance-report`) considers only `kind = 'key'` rows, by exact id and by name, including the "is revoked" diagnosis. A key name never matches, collides with or is made ambiguous by a grant, and a grant's id is "no active key" to `key ...`. Exact-id paths (`Selector::Exact`: `oauth grant revoke`, `/api/v1/keys/{id}`) still reach grants. Tests: `store::keys::tests::key_commands_by_name_act_on_the_key_and_never_on_a_grant_of_that_name`, and `cli::tests::oauth_subcommands_run_against_the_database` (a key with the grant's own name and actor).
> - `redeem` has no unique-violation branch: migration 0008's `api_keys_live_name_actor` covers `kind = 'key'` only, so a grant insert cannot violate it. `codes.rs` does not import `is_unique_violation`. The grant name's code-id suffix (Decision 1) now only tells grants apart in lists.
> - `insert_test_client` sets `fetched_at` and `cache_expires_at` for a CIMD row (Task 2's `oauth_clients_fetched_cimd` check refuses one without), binds `kind.as_str()`, and returns the row read back with `clients::find`.
> - The tests reuse Task 2's `now()` (2026-11-11 12:00) rather than an `at()`, compare `admin_generation` with its value before the change (not a literal `2`), and the grant-name test's `ClientRecord` has `cache_expires_at: None`.
> - `a_racing_double_redeem_yields_one_grant` is deterministic: a third connection holds the code's row lock, both redeems run as `tokio::spawn`ed tasks until `pg_stat_activity` shows two backends waiting on that lock, then the blocker rolls back and exactly one redeem wins.
> - `expiry` adds with `checked_add_signed`, so a huge `requested` is a refusal, not a panic.
> - More tests: `redeem_refuses_expired_revoked_and_unknown_codes_and_changes_nothing`, `code_states_have_their_spellings`, `api::keys::tests::a_grant_is_managed_by_id_but_never_given_maintenance_reporting`, and more 409/404 rows in `create_refuses_admin_access_all_from_a_scoped_admin_and_long_expiries`.
> - The "First run" paragraph does not link a "Connecting over OAuth" section (none exists until Task 8's "OAuth connectors").

**Facts this task relies on** (verified at `c24fef1`; Task 2's additions are taken from "Interfaces shared across tasks"):
- `src/store/keys.rs`:
  - `create_with_id` (166-262) runs its checks in this order: the name against `is_valid_name` (131, private), no empty `Grants::Catalogs([])`, a default that is among the granted names (a position computed before any query). Then, in one transaction, `catalog_ids` (294; Task 2 makes it `pub(crate)`) and `catalogs::find` for an all-catalogs key's default, the insert, the `key_catalogs` insert through `unnest($2::bigint[])`, and `bump_admin_generation`.
  - A taken live name and actor is detected as a unique violation on the constraint `api_keys_live_name_actor` (migration `0005_key_names.sql`).
  - `set_maintenance_report_with` (385-428) selects the row (`select`, 625, `FOR UPDATE` for an exact id), then updates `maintenance_report`.
  - `revoke_by_id(pool, id, now) -> Result<String>` (447) refuses an unknown or revoked id with `Refused`, and bumps the admin generation.
  - The `api_keys.name` check is `^[A-Za-z0-9._-]+$`. **`@` is not allowed**, so the spec's grant name `<code name>@<client host>` cannot be stored (see "Decisions" below).
- `bump_admin_generation(&mut PgConnection)` and `is_unique_violation(&sqlx::Error)` live in **`src/store/catalogs.rs`** (286, 318), already `pub(crate)`, not in `store::keys`. `store::keys` imports them (`src/store/keys.rs:8`). `catalogs::find(executor, name) -> Result<Catalog>` (171) is `CatalogNotSelected` for an unknown name. `catalogs::delete(pool, name, force)` (207).
- `src/http/api/keys.rs`:
  - `manages(principal, record)` (169) is the reach rule (M9 Deviation 8);
  - `create_key` (237-301) checks `all` against `catalogs`, `Reach::All` for `all`, `principal.require(Access::Admin, c)` for each catalog and the default, and parses `access` into `InvalidPath { path: "access", .. }` (400);
  - `changed` (205) polls after a key change (M9 Decision 12).
- `src/http/api/mod.rs:40-60` merges each area's `routes()` above the `require_key` route layer.
- `src/http/api/testing.rs`: `app()`, `keys(&TestApp) -> Keys { read, write, admin, stranger }`, `send(...)`, `Row`, `assert_matrix` (callers run read, write, stranger, admin).
- `src/cli.rs`:
  - `Command` (54) holds `#[command(subcommand)] Key(KeyCommand)` (63-65);
  - `run` (219) connects per subcommand and dispatches to `run_key(&pool, command, clock)` (482), which prints the secret to stdout and its id to stderr;
  - `format_key_record` (532-551) is tab-separated: name, actor, id, access, state, catalogs (`*` for all), `default X`, `maintenance_report B`;
  - `KeyCreateArgs` (191-217) uses `--catalogs a,b` (comma-delimited) or `--all`, `--default`, `--actor`;
  - the tests (`mod tests`, 563) use `crate::db::pg_support::TestDb::start()` and `run(cli(Some(db.url.clone()), command), fixed_now())`.
- clap derives a subcommand's name from its variant in kebab case, so `OAuth` would become `o-auth`. The variant is therefore named `Oauth` (which becomes `oauth`).
- `humantime_serde` re-exports humantime as `humantime_serde::re::humantime` (REG/humantime-serde-1.1.1/src/lib.rs:39-41), and `humantime::parse_duration(&str) -> Result<std::time::Duration, humantime::DurationError>` (REG/humantime-2.4.0/src/duration.rs:364). No new dependency is needed.
- chrono 0.4.45: `TimeDelta::from_std(std::time::Duration) -> Result<TimeDelta, OutOfRangeError>`.
- `crate::db::test_db()` (`src/db.rs:50`, `#[cfg(test)]`) is a migrated throwaway database.
- `clock::format_timestamp` (`src/clock.rs:56`).
- `docs/operations.md`:
  - "Web console" (1058) has "API reference" (1097), whose count is "31 endpoints", and "API keys" (1115);
  - "First run" (404) shows `okf-mcp key create`.

**Decisions (this task):**
1. **A grant's name is `<code name>.<client host>-<first 8 characters of the code id>`**, for example `laptop.client.example.com-1a2b3c4d`. `@` fails the `api_keys` name check, and the suffix tells apart the grants of two codes with one name, actor and client (corrected issue #248: `api_keys_live_name_actor` covers keys only, so they could not collide anyway). The client host is the host of the CIMD `client_id` URL, or of a DCR client's first redirect URI. It is lowercased and mapped to the name charset, and is `client` when nothing is left. Task 8 updates the spec ("Consent flow", step 3).
2. **The CLI takes `--catalogs a,b` or `--all`, exactly as `key create` does,** not the spec's repeated `--catalog`. Task 8 updates the spec.
3. **`--actor` is required for a code** (the spec's CLI line). A code has no meaningful name-as-actor default: the code's name is a label for the hand-over.
4. **Sign-in codes are read from the database on every use, never cached,** so minting or revoking one needs no `app.poll()`. Redeeming creates a key row and does bump the admin generation, so the key cache picks up the grant on the next poll (Task 5 polls right after redeeming).
5. **Enabling maintenance reporting on an OAuth grant is a 409, not a 500.** Without this, `set_maintenance_report_with` would run into Task 2's `api_keys_oauth_caps` CHECK and surface a database error. The store refuses it first.
6. **Redeeming a code whose named catalogs have all been deleted is refused,** rather than creating a grant that reaches nothing.
7. **DRY with the key paths.**
   - `store::keys` gains `resolve_grants`: the grants and default checks plus the catalog-id lookup, shared by `create_with_id` and `codes::create`.
   - `api::keys` gains `manages_grants`, `requested_grants` and `parse_access`, shared by the keys and codes areas.
   - `codes::expiry` is the one expiry rule for the CLI and the API.

**Files:**
- Modify: `src/store/keys.rs` (`is_valid_name` becomes `pub(crate)`, new `resolve_grants`, the reporting refusal for grants)
- Modify: `src/store/oauth/codes.rs` (everything but Task 2's `prune`)
- Modify: `src/http/api/keys.rs` (shared helpers), `src/http/api/mod.rs` (`mod oauth;` and its routes)
- Create: `src/http/api/oauth.rs`
- Modify: `src/cli.rs` (`okf-mcp oauth ...`)
- Modify: `docs/operations.md`, `README.md`
- Create: `changelog/<ts>-m10-task-04-sign-in-codes.md`

- [ ] **Step 1: Share the grant checks in `store::keys`**

In `src/store/keys.rs`, make the name check reachable from `codes.rs`:

```rust
/// Is `name` valid under the schema's `api_keys_name_check` constraint? Sign-in
/// codes use the same rule for their names (`store::oauth::codes`).
pub(crate) fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}
```

Add `resolve_grants` below `catalog_ids`:

```rust
/// Checks a new credential's grants and default catalog, then looks up their
/// ids: the rules `okf-mcp key create` and `okf-mcp oauth code create` share.
///
/// # Arguments
/// * `conn` - the creating transaction's connection.
/// * `noun` - `"key"` or `"sign-in code"`, for the messages.
/// * `name` - the credential's name, for the messages.
/// * `grants` - the catalogs it reaches.
/// * `default_catalog` - its default catalog, if any.
///
/// # Returns
/// The granted catalog ids, in the caller's order (empty for [`Grants::All`]),
/// and the default catalog's id.
///
/// # Errors
/// [`OkfError::Refused`] for no catalogs or an ungranted default (checked
/// before any query); [`OkfError::CatalogNotSelected`] for an unknown catalog;
/// [`OkfError::Database`].
pub(crate) async fn resolve_grants(
    conn: &mut PgConnection,
    noun: &str,
    name: &str,
    grants: Grants<'_>,
    default_catalog: Option<&str>,
) -> Result<(Vec<i64>, Option<i64>), OkfError> {
    if matches!(grants, Grants::Catalogs([])) {
        // A slice pattern: `[]` matches an empty slice.
        return Err(OkfError::Refused {
            reason: format!("{noun} {name:?} must be granted a catalog, or every catalog"),
        });
    }
    // Where the default sits among the granted names, checked before any query.
    let default_position = match (default_catalog, grants) {
        (Some(default), Grants::Catalogs(names)) => Some(
            names
                .iter()
                .position(|granted| granted == default)
                .ok_or_else(|| OkfError::Refused {
                    reason: format!("default catalog {default:?} is not granted to the {noun}"),
                })?,
        ),
        (Some(_), Grants::All) | (None, _) => None,
    };
    let granted = match grants {
        Grants::All => Vec::new(),
        Grants::Catalogs(names) => catalog_ids(conn, names).await?,
    };
    let default_id = match (default_catalog, default_position) {
        (_, Some(position)) => Some(granted[position]),
        // An unknown catalog is `CatalogNotSelected`; any other error propagates.
        (Some(default), None) => Some(catalogs::find(&mut *conn, default).await?.id),
        (None, None) => None,
    };
    Ok((granted, default_id))
}
```

In `create_with_id`, delete the empty-grants check, the `default_position` block, the `granted` match and the `default_catalog_id` match. Put this one call after `let mut tx = pool.begin().await?;` in their place:

```rust
    let (granted, default_catalog_id) =
        resolve_grants(&mut tx, "key", new.name, new.grants, new.default_catalog).await?;
```

The name check stays first. The messages are unchanged for keys: "key \"x\" must be granted a catalog, or every catalog" and "default catalog \"y\" is not granted to the key".

In `set_maintenance_report_with`, right after `let id = select(&mut tx, selector).await?;`, add the refusal (Decision 5):

```rust
    if enabled {
        // An OAuth grant never reports (the schema's `api_keys_oauth_caps`):
        // refuse it here as a 409 instead of letting the CHECK fail as a 500.
        let kind: Option<String> = sqlx::query_scalar("SELECT kind FROM api_keys WHERE id = $1")
            .bind(&id)
            .fetch_optional(&mut *tx)
            .await?;
        if kind.as_deref() == Some(KeyKind::OAuth.as_str()) {
            return Err(OkfError::Refused {
                reason: format!(
                    "key {id:?} is an OAuth grant, which cannot submit maintenance reports"
                ),
            });
        }
    }
```

> **Corrected (issue #248, 2026-11-11): added scope.** `resolve_active` adds `AND kind = 'key'` to its three queries (the exact-id lookup, the live-name lookup and the revoked-name check) and gains a doc comment saying why; `Selector`'s docs say `Resolve` is API keys only and `Exact` reaches grants too. Add `key_commands_by_name_act_on_the_key_and_never_on_a_grant_of_that_name` to `store::keys`' tests: a grant from `testing::insert_grant` (name `laptop`, actor `example-actor`) and a key with the same name and actor (creatable: the index covers keys only); `set_maintenance_report_target`, `grant_target`, `ungrant_target` and `revoke_target` by name all return the key's id and leave the grant alone; afterwards the name reports "is revoked" (the key), and the grant's id is "no active key" to `revoke_target`, `grant_target` and `set_maintenance_report_target`.

Run `cargo test --lib store::keys` and expect the existing tests to pass. The refactor changes no behaviour; Step 2's tests cover the refusal.

- [ ] **Step 2: Write the failing store tests**

`src/store/oauth/codes.rs` holds Task 2's `prune` and its test. Add a test-only client helper above the test module. Several test modules use it: this file's, the CLI's and the API's. Task 3's `clients::insert_dcr` is not available to a parallel Task 4, which is why this helper exists.

```rust
/// Inserts an OAuth client row straight into the database, for tests that
/// need a grant's `client_id` to exist.
///
/// # Arguments
/// * `pool` - the database.
/// * `client_id` - the id (a URL for CIMD).
/// * `kind` - DCR or CIMD.
/// * `redirect_uri` - its one redirect URI.
///
/// # Returns
/// The stored record.
#[cfg(test)]
pub(crate) async fn insert_test_client(
    pool: &PgPool,
    client_id: &str,
    kind: ClientKind,
    redirect_uri: &str,
) -> ClientRecord {
    // A CIMD row must carry its fetch times (`oauth_clients_fetched_cimd`);
    // a DCR row must not.
    sqlx::query(
        "INSERT INTO oauth_clients \
         (client_id, kind, client_name, redirect_uris, metadata, fetched_at, cache_expires_at, \
          created_at) \
         VALUES ($1, $2, 'Example Client', ARRAY[$3], '{}'::jsonb, \
                 CASE WHEN $2 = 'cimd' THEN now() END, \
                 CASE WHEN $2 = 'cimd' THEN now() + interval '1 hour' END, now())",
    )
    .bind(client_id)
    .bind(kind.as_str())
    .bind(redirect_uri)
    .execute(pool)
    .await
    .expect("insert a test client");
    crate::store::oauth::clients::find(pool, client_id)
        .await
        .expect("read the test client")
        .expect("the test client exists")
}
```

Add these tests to the module's `#[cfg(test)] mod tests` (beside Task 2's `prune` test; keep its `use` lines and add the ones below that are missing):

```rust
    use chrono::{TimeDelta, TimeZone};

    use crate::db::test_db;
    use crate::store::catalogs;
    use crate::store::keys::{self as key_rows, KeyKind};

    fn at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 2, 12, 0, 0).unwrap()
    }

    fn new_code<'a>(
        name: &'a str,
        grants: Grants<'a>,
        default: Option<&'a str>,
    ) -> NewSignInCode<'a> {
        NewSignInCode {
            name,
            access: Access::Write,
            grants,
            default_catalog: default,
            actor: "example-actor",
            expires_at: at() + TimeDelta::hours(24),
        }
    }

    async fn with_catalogs(names: &[&str]) -> crate::db::pg_support::TestDb {
        let db = test_db().await;
        for name in names {
            catalogs::create(&db.pool, name, "", at()).await.unwrap();
        }
        db
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_stores_only_the_hash_and_the_template() {
        let db = with_catalogs(&["main", "other"]).await;
        let names = ["other".to_string(), "main".to_string()];

        let created = create(
            &db.pool,
            &new_code("laptop", Grants::Catalogs(&names), Some("main")),
            at(),
        )
        .await
        .unwrap();

        assert!(created.code.starts_with(crate::keys::SIGN_IN_CODE_PREFIX));
        assert!(!format!("{created:?}").contains(&created.code), "Debug redacts the code");
        let codes = list(&db.pool).await.unwrap();
        assert_eq!(codes.len(), 1);
        let code = &codes[0];
        assert_eq!(code.id, created.id);
        assert_eq!(code.name, "laptop");
        assert_eq!(code.access, Access::Write);
        assert!(!code.all_catalogs);
        assert_eq!(code.catalogs, ["main", "other"]);
        assert_eq!(code.default_catalog.as_deref(), Some("main"));
        assert_eq!(code.actor, "example-actor");
        assert_eq!(code.state(at()), CodeState::Live);
        let stored: Vec<u8> = sqlx::query_scalar("SELECT sha256 FROM sign_in_codes")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert!(crate::keys::matches(&stored, &created.code));
        // Minting a code changes no key, so the key cache is not reloaded.
        assert_eq!(catalogs::admin_generation(&db.pool).await.unwrap(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_refuses_admin_bad_names_ungranted_defaults_and_the_past() {
        let db = with_catalogs(&["main"]).await;
        let main = ["main".to_string()];
        let refused = |result: Result<CreatedSignInCode, OkfError>| {
            matches!(result, Err(OkfError::Refused { .. }))
        };

        let admin = NewSignInCode {
            access: Access::Admin,
            ..new_code("a", Grants::Catalogs(&main), None)
        };
        let past = NewSignInCode {
            expires_at: at(),
            ..new_code("p", Grants::Catalogs(&main), None)
        };
        let no_actor = NewSignInCode {
            actor: " ",
            ..new_code("n", Grants::Catalogs(&main), None)
        };

        assert!(refused(create(&db.pool, &admin, at()).await));
        assert!(refused(create(&db.pool, &past, at()).await));
        assert!(refused(create(&db.pool, &no_actor, at()).await));
        assert!(refused(
            create(&db.pool, &new_code("bad name", Grants::Catalogs(&main), None), at()).await
        ));
        assert!(refused(
            create(&db.pool, &new_code("x", Grants::Catalogs(&[]), None), at()).await
        ));
        assert!(refused(
            create(&db.pool, &new_code("x", Grants::Catalogs(&main), Some("other")), at()).await
        ));
        assert!(matches!(
            create(&db.pool, &new_code("x", Grants::Catalogs(&["nope".to_string()]), None), at())
                .await,
            Err(OkfError::CatalogNotSelected { .. })
        ));
        assert!(list(&db.pool).await.unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn find_live_skips_expired_revoked_and_redeemed_codes() {
        let db = with_catalogs(&["main"]).await;
        let main = ["main".to_string()];
        let client = insert_test_client(
            &db.pool,
            "https://client.example.com/client.json",
            ClientKind::Cimd,
            "https://client.example.com/callback",
        )
        .await;
        let live = create(&db.pool, &new_code("live", Grants::Catalogs(&main), None), at())
            .await
            .unwrap();
        let revoked = create(&db.pool, &new_code("gone", Grants::Catalogs(&main), None), at())
            .await
            .unwrap();
        let redeemed = create(&db.pool, &new_code("used", Grants::Catalogs(&main), None), at())
            .await
            .unwrap();
        revoke(&db.pool, &revoked.id, at()).await.unwrap();
        redeem(&db.pool, &redeemed.id, &client, at()).await.unwrap();
        let later = at() + TimeDelta::hours(25);

        let found = find_live(&db.pool, &live.code, at()).await.unwrap();

        assert_eq!(found.map(|code| code.id), Some(live.id.clone()));
        assert!(find_live(&db.pool, &live.code, later).await.unwrap().is_none());
        assert!(find_live(&db.pool, &revoked.code, at()).await.unwrap().is_none());
        assert!(find_live(&db.pool, &redeemed.code, at()).await.unwrap().is_none());
        assert!(find_live(&db.pool, "okf_not-a-code", at()).await.unwrap().is_none());
        let states: Vec<(String, CodeState)> = list(&db.pool)
            .await
            .unwrap()
            .into_iter()
            .map(|code| (code.name.clone(), code.state(later)))
            .collect();
        assert!(states.contains(&("live".to_string(), CodeState::Expired)));
        assert!(states.contains(&("gone".to_string(), CodeState::Revoked)));
        assert!(states.contains(&("used".to_string(), CodeState::Redeemed)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revoke_refuses_redeemed_revoked_and_unknown_codes() {
        let db = with_catalogs(&["main"]).await;
        let main = ["main".to_string()];
        let client = insert_test_client(
            &db.pool,
            "dcr-client",
            ClientKind::Dcr,
            "https://client.example.com/callback",
        )
        .await;
        let code = create(&db.pool, &new_code("c", Grants::Catalogs(&main), None), at())
            .await
            .unwrap();
        let used = create(&db.pool, &new_code("u", Grants::Catalogs(&main), None), at())
            .await
            .unwrap();
        redeem(&db.pool, &used.id, &client, at()).await.unwrap();

        revoke(&db.pool, &code.id, at()).await.unwrap();

        assert!(matches!(revoke(&db.pool, &code.id, at()).await, Err(OkfError::Refused { .. })));
        assert!(matches!(revoke(&db.pool, &used.id, at()).await, Err(OkfError::Refused { .. })));
        assert!(matches!(revoke(&db.pool, "no-such-code", at()).await, Err(OkfError::Refused { .. })));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn redeem_turns_the_code_into_an_oauth_grant_once() {
        let db = with_catalogs(&["main", "other"]).await;
        let names = ["main".to_string(), "other".to_string()];
        let client = insert_test_client(
            &db.pool,
            "https://client.example.com/client.json",
            ClientKind::Cimd,
            "https://callback.example.com/cb",
        )
        .await;
        let code = create(
            &db.pool,
            &new_code("laptop", Grants::Catalogs(&names), Some("other")),
            at(),
        )
        .await
        .unwrap();
        let generation = catalogs::admin_generation(&db.pool).await.unwrap();

        let grant_id = redeem(&db.pool, &code.id, &client, at()).await.unwrap();

        let grant = key_rows::list(&db.pool)
            .await
            .unwrap()
            .into_iter()
            .find(|key| key.id == grant_id)
            .expect("the grant is a key row");
        assert_eq!(grant.kind, KeyKind::OAuth);
        assert_eq!(grant.sha256, None);
        assert_eq!(grant.access, Access::Write);
        assert!(!grant.maintenance_report);
        assert_eq!(grant.catalogs, ["main", "other"]);
        assert_eq!(grant.default_catalog.as_deref(), Some("other"));
        assert_eq!(grant.actor, "example-actor");
        assert_eq!(grant.client_id.as_deref(), Some(client.client_id.as_str()));
        // The CIMD client's host, not the redirect URI's (Decision 1).
        let suffix = &code.id[..8];
        assert_eq!(grant.name, format!("laptop.client.example.com-{suffix}"));
        assert_eq!(
            catalogs::admin_generation(&db.pool).await.unwrap(),
            generation + 1,
            "redeeming reloads the key cache"
        );
        let redeemed = list(&db.pool).await.unwrap().remove(0);
        assert_eq!(redeemed.state(at()), CodeState::Redeemed);
        assert_eq!(redeemed.grant_id.as_deref(), Some(grant_id.as_str()));
        assert!(matches!(
            redeem(&db.pool, &code.id, &client, at()).await,
            Err(OkfError::Refused { .. })
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_racing_double_redeem_yields_one_grant() {
        let db = with_catalogs(&["main"]).await;
        let main = ["main".to_string()];
        let client = insert_test_client(
            &db.pool,
            "dcr-client",
            ClientKind::Dcr,
            "https://client.example.com/callback",
        )
        .await;
        let code = create(&db.pool, &new_code("race", Grants::Catalogs(&main), None), at())
            .await
            .unwrap();

        // Corrected (issue #248, 2026-11-11): what shipped holds the row lock
        // on a third connection, `tokio::spawn`s both redeems, waits until
        // `pg_stat_activity` shows both blocked on it, then releases it; see
        // `src/store/oauth/codes.rs`.
        let (first, second) = tokio::join!(
            redeem(&db.pool, &code.id, &client, at()),
            redeem(&db.pool, &code.id, &client, at())
        );

        assert_eq!(
            [first.is_ok(), second.is_ok()].iter().filter(|ok| **ok).count(),
            1,
            "{first:?} {second:?}"
        );
        assert!(matches!(first.or(second), Ok(_)));
        let grants = key_rows::list(&db.pool)
            .await
            .unwrap()
            .into_iter()
            .filter(|key| key.kind == KeyKind::OAuth)
            .count();
        assert_eq!(grants, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn two_codes_with_one_name_for_one_client_make_two_grants() {
        let db = with_catalogs(&["main"]).await;
        let main = ["main".to_string()];
        let client = insert_test_client(
            &db.pool,
            "dcr-client",
            ClientKind::Dcr,
            "https://client.example.com/callback",
        )
        .await;
        let first = create(&db.pool, &new_code("phone", Grants::Catalogs(&main), None), at())
            .await
            .unwrap();
        let second = create(&db.pool, &new_code("phone", Grants::Catalogs(&main), None), at())
            .await
            .unwrap();

        let one = redeem(&db.pool, &first.id, &client, at()).await.unwrap();
        let two = redeem(&db.pool, &second.id, &client, at()).await.unwrap();

        assert_ne!(one, two);
        let names: Vec<String> = key_rows::list(&db.pool)
            .await
            .unwrap()
            .into_iter()
            .filter(|key| key.kind == KeyKind::OAuth)
            .map(|key| key.name)
            .collect();
        assert_eq!(names.len(), 2);
        assert!(names.iter().all(|name| name.starts_with("phone.client.example.com-")));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn redeem_refuses_a_code_whose_catalogs_are_gone() {
        let db = with_catalogs(&["main"]).await;
        let main = ["main".to_string()];
        let client = insert_test_client(
            &db.pool,
            "dcr-client",
            ClientKind::Dcr,
            "https://client.example.com/callback",
        )
        .await;
        let code = create(&db.pool, &new_code("c", Grants::Catalogs(&main), Some("main")), at())
            .await
            .unwrap();

        catalogs::delete(&db.pool, "main", true).await.unwrap();

        // Deleting the catalog cascades to the code's grants and clears its default.
        let listed = list(&db.pool).await.unwrap().remove(0);
        assert!(listed.catalogs.is_empty());
        assert_eq!(listed.default_catalog, None);
        assert!(matches!(
            redeem(&db.pool, &code.id, &client, at()).await,
            Err(OkfError::Refused { .. })
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_oauth_grant_cannot_be_given_maintenance_reporting() {
        let db = with_catalogs(&["main"]).await;
        let main = ["main".to_string()];
        let client = insert_test_client(
            &db.pool,
            "dcr-client",
            ClientKind::Dcr,
            "https://client.example.com/callback",
        )
        .await;
        let code = create(&db.pool, &new_code("c", Grants::Catalogs(&main), None), at())
            .await
            .unwrap();
        let grant = redeem(&db.pool, &code.id, &client, at()).await.unwrap();

        let enabled = key_rows::set_maintenance_report_by_id(&db.pool, &grant, true).await;
        let disabled = key_rows::set_maintenance_report_by_id(&db.pool, &grant, false).await;

        assert!(matches!(enabled, Err(OkfError::Refused { .. })), "{enabled:?}");
        assert!(disabled.is_ok(), "turning it off is a no-op: {disabled:?}");
    }

    #[test]
    fn the_grant_name_uses_the_client_host_in_the_key_name_charset() {
        let client = |client_id: &str, kind: ClientKind, redirect: &str| ClientRecord {
            client_id: client_id.to_string(),
            kind,
            client_name: None,
            redirect_uris: vec![redirect.to_string()],
            metadata: serde_json::json!({}),
            fetched_at: None,
            cache_expires_at: None,
            created_at: at(),
        };
        let id = "1a2b3c4d-0000-4000-8000-000000000000";

        let cimd = client(
            "https://Client.Example.com:8443/meta.json",
            ClientKind::Cimd,
            "https://cb.example.com/x",
        );
        let dcr = client("abc", ClientKind::Dcr, "http://user@127.0.0.1:3000/callback?x=1");
        let ipv6 = client("abc", ClientKind::Dcr, "http://[::1]:3000/callback");
        let empty = client("abc", ClientKind::Dcr, "");

        assert_eq!(grant_name("laptop", &cimd, id), "laptop.client.example.com-1a2b3c4d");
        assert_eq!(grant_name("laptop", &dcr, id), "laptop.127.0.0.1-1a2b3c4d");
        // "[::1]" maps to "---1-", trimmed to "1".
        assert_eq!(grant_name("laptop", &ipv6, id), "laptop.1-1a2b3c4d");
        assert_eq!(grant_name("laptop", &empty, id), "laptop.client-1a2b3c4d");
        assert!(is_valid_name(&grant_name("laptop", &ipv6, id)));
    }

    #[test]
    fn expiry_defaults_to_a_day_and_is_capped() {
        let max = std::time::Duration::from_secs(7 * 24 * 60 * 60);

        assert_eq!(
            expiry(at(), DEFAULT_EXPIRY, max).unwrap(),
            at() + TimeDelta::hours(24)
        );
        assert_eq!(expiry(at(), max, max).unwrap(), at() + TimeDelta::days(7));
        assert!(matches!(
            expiry(at(), max + std::time::Duration::from_secs(1), max),
            Err(OkfError::Refused { .. })
        ));
        assert!(matches!(
            expiry(at(), std::time::Duration::ZERO, max),
            Err(OkfError::Refused { .. })
        ));
    }
```

`crate::db::pg_support::TestDb` is the type `test_db()` returns (`src/db.rs:50`).

- [ ] **Step 3: Run the store tests to make sure they fail**

Run: `cargo test --lib store::oauth::codes`
Expected: compile errors. `create`, `list`, `find_live`, `revoke`, `redeem`, `grant_name`, `expiry`, `DEFAULT_EXPIRY`, `CodeState` and `NewSignInCode` do not exist yet.

- [ ] **Step 4: Implement `store::oauth::codes`**

Above Task 2's `prune`, `src/store/oauth/codes.rs` becomes the following. Keep Task 2's module doc if it has one, merged into this one, and keep its `prune` and its test.

```rust
//! Sign-in codes (OAuth design "Sign-in codes"): one-time credentials an
//! admin mints and hands to a person, each bound to a grant template
//! (catalogs, access, default catalog, actor). Only a code's SHA-256 is
//! stored; the code itself is returned once, by [`create`]. [`redeem`] turns a
//! live code into an OAuth grant, an `api_keys` row with `kind = 'oauth'`, in
//! one transaction.

use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use sqlx::PgPool;

use crate::error::OkfError;
use crate::keys::{self, SIGN_IN_CODE_PREFIX};
use crate::store::catalogs::bump_admin_generation;
use crate::store::keys::{is_valid_name, resolve_grants, Access, Grants};
use crate::store::oauth::clients::{ClientKind, ClientRecord};

/// How long a code stays valid when its creator names no expiry: a day.
pub const DEFAULT_EXPIRY: Duration = Duration::from_secs(24 * 60 * 60);

/// A code to mint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewSignInCode<'a> {
    /// Its label, `[A-Za-z0-9._-]+`; the grant's name starts with it.
    pub name: &'a str,
    /// `Read` or `Write`; `Admin` is refused.
    pub access: Access,
    /// The catalogs the grant will reach.
    pub grants: Grants<'a>,
    /// The grant's default catalog (must be granted).
    pub default_catalog: Option<&'a str>,
    /// The actor the grant's writes are recorded as.
    pub actor: &'a str,
    /// When the code stops working if nobody redeems it.
    pub expires_at: DateTime<Utc>,
}

/// A minted code: its id and, this once, the code itself.
// `Debug` by hand, so a stray `{:?}` can never print the code.
#[derive(Clone, PartialEq, Eq)]
pub struct CreatedSignInCode {
    /// Its immutable id.
    pub id: String,
    /// The `okfsc_` code. Not stored; it cannot be shown again.
    pub code: String,
}

impl std::fmt::Debug for CreatedSignInCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CreatedSignInCode")
            .field("id", &self.id)
            .field("code", &"<redacted>")
            .finish()
    }
}

/// Where a code is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeState {
    /// Unexpired and unused: it can be redeemed.
    Live,
    /// Past `expires_at`, never redeemed or revoked.
    Expired,
    /// Turned into a grant.
    Redeemed,
    /// Revoked before anyone redeemed it.
    Revoked,
}

impl CodeState {
    /// The spelling the CLI and the API show.
    ///
    /// # Returns
    /// `"live"`, `"expired"`, `"redeemed"` or `"revoked"`.
    pub fn as_str(self) -> &'static str {
        match self {
            CodeState::Live => "live",
            CodeState::Expired => "expired",
            CodeState::Redeemed => "redeemed",
            CodeState::Revoked => "revoked",
        }
    }
}

/// A stored code (never the code or its hash).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInCodeRecord {
    /// Its immutable id.
    pub id: String,
    /// Its label.
    pub name: String,
    /// The grant's access.
    pub access: Access,
    /// The grant reaches every catalog.
    pub all_catalogs: bool,
    /// The grant's catalogs, sorted (empty with `all_catalogs`).
    pub catalogs: Vec<String>,
    /// The grant's default catalog.
    pub default_catalog: Option<String>,
    /// The grant's actor.
    pub actor: String,
    /// When it was minted.
    pub created_at: DateTime<Utc>,
    /// When it stops working unredeemed.
    pub expires_at: DateTime<Utc>,
    /// When it was redeemed.
    pub redeemed_at: Option<DateTime<Utc>>,
    /// The grant it became.
    pub grant_id: Option<String>,
    /// When it was revoked.
    pub revoked_at: Option<DateTime<Utc>>,
}

impl SignInCodeRecord {
    /// Its state at `now`.
    ///
    /// # Arguments
    /// * `now` - the server clock's time.
    ///
    /// # Returns
    /// Redeemed or revoked if it was, else expired once `expires_at` is
    /// reached, else live.
    pub fn state(&self, now: DateTime<Utc>) -> CodeState {
        if self.redeemed_at.is_some() {
            CodeState::Redeemed
        } else if self.revoked_at.is_some() {
            CodeState::Revoked
        } else if self.expires_at <= now {
            CodeState::Expired
        } else {
            CodeState::Live
        }
    }
}

#[derive(sqlx::FromRow)]
struct CodeRow {
    id: String,
    name: String,
    access: String,
    all_catalogs: bool,
    catalogs: Vec<String>,
    default_catalog: Option<String>,
    actor: String,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    redeemed_at: Option<DateTime<Utc>>,
    grant_id: Option<String>,
    revoked_at: Option<DateTime<Utc>>,
}

impl From<CodeRow> for SignInCodeRecord {
    fn from(row: CodeRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            access: Access::parse(&row.access).expect("the schema checks sign_in_codes.access"),
            all_catalogs: row.all_catalogs,
            catalogs: row.catalogs,
            default_catalog: row.default_catalog,
            actor: row.actor,
            created_at: row.created_at,
            expires_at: row.expires_at,
            redeemed_at: row.redeemed_at,
            grant_id: row.grant_id,
            revoked_at: row.revoked_at,
        }
    }
}

// A macro that expands to a string literal, so `concat!` can glue the shared
// SELECT and each query's tail together at compile time (a `const &str`
// cannot go into `concat!`).
macro_rules! select_codes {
    () => {
        "SELECT s.id, s.name, s.access, s.all_catalogs, d.name AS default_catalog, s.actor, \
                s.created_at, s.expires_at, s.redeemed_at, s.grant_id, s.revoked_at, \
                COALESCE(array_agg(c.name ORDER BY c.name) FILTER (WHERE c.name IS NOT NULL), \
                         '{}'::text[]) AS catalogs \
         FROM sign_in_codes s \
         LEFT JOIN catalogs d ON d.id = s.default_catalog_id \
         LEFT JOIN sign_in_code_catalogs g ON g.code_id = s.id \
         LEFT JOIN catalogs c ON c.id = g.catalog_id"
    };
}

/// When a code asked to last `requested` expires: the one rule the CLI and
/// the API share.
///
/// # Arguments
/// * `now` - the server clock's time.
/// * `requested` - how long it should last.
/// * `max` - `oauth.code_max_expiry`.
///
/// # Returns
/// `now + requested`.
///
/// # Errors
/// [`OkfError::Refused`] for zero, or more than `max`.
pub fn expiry(
    now: DateTime<Utc>,
    requested: Duration,
    max: Duration,
) -> Result<DateTime<Utc>, OkfError> {
    if requested.is_zero() || requested > max {
        return Err(OkfError::Refused {
            reason: format!(
                "a sign-in code must expire within {} (oauth.code_max_expiry), not {}",
                humantime_serde::re::humantime::format_duration(max),
                humantime_serde::re::humantime::format_duration(requested)
            ),
        });
    }
    // `checked_add_signed` is `None` past chrono's range instead of panicking.
    TimeDelta::from_std(requested)
        .ok()
        .and_then(|delta| now.checked_add_signed(delta))
        .ok_or_else(|| OkfError::Refused {
            reason: "the expiry is out of range".to_string(),
        })
}

/// Mints a code and its catalog grants in one transaction. It changes no
/// key, so the admin generation is left alone.
///
/// # Arguments
/// * `pool` - the database.
/// * `new` - the code's template and expiry.
/// * `now` - the creation time (server clock).
///
/// # Returns
/// The code itself: hand it over once; it is not stored and cannot be shown again.
///
/// # Errors
/// [`OkfError::Refused`] for an invalid name, `admin` access, an empty
/// actor, an expiry not after `now`, no catalogs or an ungranted default;
/// [`OkfError::CatalogNotSelected`] for an unknown catalog; [`OkfError::Io`]
/// if the OS random source fails; [`OkfError::Database`] otherwise.
pub async fn create(
    pool: &PgPool,
    new: &NewSignInCode<'_>,
    now: DateTime<Utc>,
) -> Result<CreatedSignInCode, OkfError> {
    let refuse = |reason: String| Err(OkfError::Refused { reason });
    if !is_valid_name(new.name) {
        return refuse(format!(
            "sign-in code name {:?} must be letters, digits, '.', '_' and '-'",
            new.name
        ));
    }
    if new.access == Access::Admin {
        return refuse("a sign-in code grants read or write access, never admin".to_string());
    }
    if new.actor.trim().is_empty() {
        return refuse(format!("sign-in code {:?} needs an actor", new.name));
    }
    if new.expires_at <= now {
        return refuse(format!("sign-in code {:?} must expire in the future", new.name));
    }
    let mut tx = pool.begin().await?;
    let (granted, default_catalog_id) = resolve_grants(
        &mut tx,
        "sign-in code",
        new.name,
        new.grants,
        new.default_catalog,
    )
    .await?;
    let code = keys::generate_with(SIGN_IN_CODE_PREFIX)?;
    let id: String = sqlx::query_scalar(
        "INSERT INTO sign_in_codes \
         (name, sha256, access, all_catalogs, default_catalog_id, actor, created_at, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(new.name)
    .bind(keys::hash(&code).as_slice())
    .bind(new.access.as_str())
    .bind(matches!(new.grants, Grants::All))
    .bind(default_catalog_id)
    .bind(new.actor.trim())
    .bind(now)
    .bind(new.expires_at)
    .fetch_one(&mut *tx)
    .await?;
    // `unnest` turns the bound array into rows; a name given twice is one grant.
    sqlx::query(
        "INSERT INTO sign_in_code_catalogs (code_id, catalog_id) \
         SELECT $1, unnest($2::bigint[]) ON CONFLICT DO NOTHING",
    )
    .bind(&id)
    .bind(&granted)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(CreatedSignInCode { id, code })
}

/// Every code, newest first, in every state.
///
/// # Arguments
/// * `pool` - the database.
///
/// # Returns
/// The records.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn list(pool: &PgPool) -> Result<Vec<SignInCodeRecord>, OkfError> {
    let rows = sqlx::query_as::<_, CodeRow>(concat!(
        select_codes!(),
        " GROUP BY s.id, d.name ORDER BY s.created_at DESC, s.id"
    ))
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(SignInCodeRecord::from).collect())
}

/// The live code a person typed, if it is one.
///
/// # Arguments
/// * `pool` - the database.
/// * `presented` - the text entered on the consent page.
/// * `now` - the server clock's time.
///
/// # Returns
/// Its record when it exists, is unexpired, and is neither redeemed nor
/// revoked; `None` otherwise (one answer for every reason, so the consent
/// page cannot tell them apart).
///
/// # Errors
/// [`OkfError::Database`].
pub async fn find_live(
    pool: &PgPool,
    presented: &str,
    now: DateTime<Utc>,
) -> Result<Option<SignInCodeRecord>, OkfError> {
    let presented = presented.trim();
    if !presented.starts_with(SIGN_IN_CODE_PREFIX) {
        // Not a sign-in code at all: no query.
        return Ok(None);
    }
    let row = sqlx::query_as::<_, CodeRow>(concat!(
        select_codes!(),
        " WHERE s.sha256 = $1 AND s.redeemed_at IS NULL AND s.revoked_at IS NULL \
          AND s.expires_at > $2 \
          GROUP BY s.id, d.name"
    ))
    .bind(keys::hash(presented).as_slice())
    .bind(now)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(SignInCodeRecord::from))
}

/// Revokes a code nobody has redeemed (an expired one too).
///
/// # Arguments
/// * `pool` - the database.
/// * `id` - the code's id.
/// * `now` - the revocation time.
///
/// # Errors
/// [`OkfError::Refused`] for an unknown, redeemed or already revoked code;
/// [`OkfError::Database`].
pub async fn revoke(pool: &PgPool, id: &str, now: DateTime<Utc>) -> Result<(), OkfError> {
    let revoked = sqlx::query(
        "UPDATE sign_in_codes SET revoked_at = $2 \
         WHERE id = $1 AND revoked_at IS NULL AND redeemed_at IS NULL",
    )
    .bind(id)
    .bind(now)
    .execute(pool)
    .await?;
    if revoked.rows_affected() == 0 {
        return Err(OkfError::Refused {
            reason: format!("no unredeemed, unrevoked sign-in code {id:?}"),
        });
    }
    Ok(())
}

/// The locked row [`redeem`] reads.
#[derive(sqlx::FromRow)]
struct LockedCode {
    name: String,
    access: String,
    all_catalogs: bool,
    default_catalog_id: Option<i64>,
    actor: String,
    expires_at: DateTime<Utc>,
    used: bool,
}

/// Turns a live code into an OAuth grant for `client`, in one transaction:
/// lock the code, re-check it, insert the grant and its catalogs, mark the
/// code redeemed, and bump the admin generation so the key cache loads the
/// grant.
///
/// # Arguments
/// * `pool` - the database.
/// * `code_id` - the code (from [`find_live`]).
/// * `client` - the client being authorized.
/// * `now` - the redemption time.
///
/// # Returns
/// The grant's id (its `api_keys.id`).
///
/// # Errors
/// [`OkfError::Refused`] when the code is unknown, expired, redeemed or
/// revoked (one message for all four), or its named catalogs are all gone;
/// [`OkfError::Database`].
pub async fn redeem(
    pool: &PgPool,
    code_id: &str,
    client: &ClientRecord,
    now: DateTime<Utc>,
) -> Result<String, OkfError> {
    let not_live = || OkfError::Refused {
        reason: "the sign-in code is unknown, expired, redeemed or revoked".to_string(),
    };
    let mut tx = pool.begin().await?;
    // `FOR UPDATE` holds the row until commit: a racing redeem of the same
    // code waits here, then sees `used` and is refused.
    let code: Option<LockedCode> = sqlx::query_as(
        "SELECT name, access, all_catalogs, default_catalog_id, actor, expires_at, \
                (redeemed_at IS NOT NULL OR revoked_at IS NOT NULL) AS used \
         FROM sign_in_codes WHERE id = $1 FOR UPDATE",
    )
    .bind(code_id)
    .fetch_optional(&mut *tx)
    .await?;
    let code = code.ok_or_else(not_live)?;
    if code.used || code.expires_at <= now {
        return Err(not_live());
    }
    let catalog_ids: Vec<i64> = sqlx::query_scalar(
        "SELECT catalog_id FROM sign_in_code_catalogs WHERE code_id = $1 ORDER BY catalog_id",
    )
    .bind(code_id)
    .fetch_all(&mut *tx)
    .await?;
    if !code.all_catalogs && catalog_ids.is_empty() {
        return Err(OkfError::Refused {
            reason: "every catalog this sign-in code granted has been deleted".to_string(),
        });
    }
    // The live name/actor unique index covers `kind = 'key'` only (migration
    // 0008), so a grant's name can never collide with a key or another grant.
    let grant_id: String = sqlx::query_scalar(
        "INSERT INTO api_keys \
         (name, sha256, kind, client_id, access, maintenance_report, all_catalogs, \
          default_catalog_id, actor, created_at) \
         VALUES ($1, NULL, 'oauth', $2, $3, false, $4, $5, $6, $7) RETURNING id",
    )
    .bind(grant_name(&code.name, client, code_id))
    .bind(&client.client_id)
    .bind(&code.access)
    .bind(code.all_catalogs)
    .bind(code.default_catalog_id)
    .bind(&code.actor)
    .bind(now)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO key_catalogs (key_id, catalog_id) SELECT $1, unnest($2::bigint[])",
    )
    .bind(&grant_id)
    .bind(&catalog_ids)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE sign_in_codes SET redeemed_at = $2, grant_id = $3 WHERE id = $1")
        .bind(code_id)
        .bind(now)
        .bind(&grant_id)
        .execute(&mut *tx)
        .await?;
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(grant_id)
}

/// A grant's key name (Decision 1): `<code name>.<client host>-<code id prefix>`.
///
/// # Arguments
/// * `code_name` - the code's label (already in the name charset).
/// * `client` - the client being authorized.
/// * `code_id` - the code's id.
///
/// # Returns
/// A name that passes the `api_keys` name check.
fn grant_name(code_name: &str, client: &ClientRecord, code_id: &str) -> String {
    // `get(..8)` is `None` for a shorter id instead of panicking.
    let suffix = code_id.get(..8).unwrap_or(code_id);
    format!("{code_name}.{}-{suffix}", client_host(client))
}

/// The client's host for a grant name: the CIMD document URL's host, or a
/// DCR client's first redirect URI's, lowercased and mapped to
/// `[a-z0-9._-]`; `client` when nothing is left.
fn client_host(client: &ClientRecord) -> String {
    let source = match client.kind {
        ClientKind::Cimd => client.client_id.as_str(),
        ClientKind::Dcr => client.redirect_uris.first().map_or("", String::as_str),
    };
    // `split_once` is Python's `str.partition` without the separator.
    let rest = source.split_once("://").map_or(source, |(_, rest)| rest);
    // The first `split` item always exists (an empty string at worst).
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let host = match host_port.rsplit_once(':') {
        // A port follows the last ':' unless that ':' is inside "[...]" (IPv6).
        Some((host, port)) if !port.contains(']') => host,
        _ => host_port,
    };
    let mapped: String = host
        .chars()
        .map(|c| match c {
            'A'..='Z' => c.to_ascii_lowercase(),
            'a'..='z' | '0'..='9' | '.' | '_' | '-' => c,
            _ => '-',
        })
        .collect();
    let trimmed = mapped.trim_matches(|c| c == '.' || c == '-');
    if trimmed.is_empty() {
        "client".to_string()
    } else {
        trimmed.to_string()
    }
}
```

- [ ] **Step 5: Run the store tests**

Run: `cargo test --lib store::oauth::codes store::keys`
Expected: PASS. The tests need Docker for testcontainers.

- [ ] **Step 6: Write the failing CLI tests**

Add to `src/cli.rs`'s `mod tests`:

```rust
    #[test]
    fn oauth_code_create_parses_like_key_create() {
        let parsed = Cli::try_parse_from([
            "okf-mcp", "oauth", "code", "create", "laptop", "--actor", "example-actor",
            "--access", "write", "--catalogs", "main,other", "--default", "main",
            "--expires", "3d",
        ])
        .unwrap();
        let Some(Command::Oauth(OauthCommand::Code(CodeCommand::Create(args)))) = parsed.command
        else {
            panic!("expected oauth code create");
        };

        assert_eq!(args.name, "laptop");
        assert_eq!(args.actor, "example-actor");
        assert_eq!(args.access, Access::Write);
        assert_eq!(args.catalogs, ["main", "other"]);
        assert_eq!(args.default.as_deref(), Some("main"));
        assert_eq!(args.expires, std::time::Duration::from_secs(3 * 24 * 60 * 60));
        let defaulted = Cli::try_parse_from([
            "okf-mcp", "oauth", "code", "create", "c", "--actor", "a", "--access", "read", "--all",
        ])
        .unwrap();
        let Some(Command::Oauth(OauthCommand::Code(CodeCommand::Create(args)))) =
            defaulted.command
        else {
            panic!("expected oauth code create");
        };
        assert_eq!(args.expires, crate::store::oauth::codes::DEFAULT_EXPIRY);
        // `--actor` is required, and `--expires` must parse.
        assert!(Cli::try_parse_from([
            "okf-mcp", "oauth", "code", "create", "c", "--access", "read", "--all",
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "okf-mcp", "oauth", "code", "create", "c", "--actor", "a", "--access", "read",
            "--all", "--expires", "soon",
        ])
        .is_err());
        assert!(matches!(
            Cli::try_parse_from(["okf-mcp", "oauth", "grant", "revoke", "g"]).unwrap().command,
            Some(Command::Oauth(OauthCommand::Grant(GrantCommand::Revoke { ref id }))) if id == "g"
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn oauth_subcommands_run_against_the_database() {
        use crate::store::oauth::clients::ClientKind;
        use crate::store::oauth::codes::{self, insert_test_client};

        let db = crate::db::pg_support::TestDb::start().await;
        let clock = fixed_now();
        let url = Some(db.url.clone());
        let parse = |args: &[&str]| {
            let mut full = vec!["okf-mcp"];
            full.extend_from_slice(args);
            Cli::try_parse_from(full).unwrap().command.unwrap()
        };
        run(
            cli(url.clone(), parse(&["catalog", "create", "main"])),
            Arc::clone(&clock),
        )
        .await
        .unwrap();

        run(
            cli(
                url.clone(),
                parse(&[
                    "oauth", "code", "create", "laptop", "--actor", "example-actor",
                    "--access", "read", "--catalogs", "main",
                ]),
            ),
            Arc::clone(&clock),
        )
        .await
        .unwrap();
        let too_long = run(
            cli(
                url.clone(),
                parse(&[
                    "oauth", "code", "create", "late", "--actor", "example-actor",
                    "--access", "read", "--all", "--expires", "8d",
                ]),
            ),
            Arc::clone(&clock),
        )
        .await;
        run(
            cli(url.clone(), parse(&["oauth", "code", "list"])),
            Arc::clone(&clock),
        )
        .await
        .unwrap();
        let minted = codes::list(&db.pool).await.unwrap();
        assert_eq!(minted.len(), 1, "the 8-day code was refused: {too_long:?}");
        assert!(too_long.is_err());
        run(
            cli(url.clone(), parse(&["oauth", "code", "revoke", &minted[0].id])),
            Arc::clone(&clock),
        )
        .await
        .unwrap();
        assert!(codes::list(&db.pool).await.unwrap()[0].revoked_at.is_some());

        // A grant, made the way the consent page makes one.
        let main = ["main".to_string()];
        let code = codes::create(
            &db.pool,
            &codes::NewSignInCode {
                name: "phone",
                access: Access::Write,
                grants: Grants::Catalogs(&main),
                default_catalog: None,
                actor: "example-actor",
                expires_at: chrono::Utc::now() + chrono::TimeDelta::hours(1),
            },
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        let client = insert_test_client(
            &db.pool,
            "dcr-client",
            ClientKind::Dcr,
            "https://client.example.com/callback",
        )
        .await;
        let grant = codes::redeem(&db.pool, &code.id, &client, chrono::Utc::now())
            .await
            .unwrap();
        let plain = keys::create_with_id(
            &db.pool,
            &NewKey {
                name: "plain",
                access: Access::Read,
                maintenance_report: false,
                grants: Grants::All,
                default_catalog: None,
                actor: "plain",
            },
            chrono::Utc::now(),
        )
        .await
        .unwrap();

        run(
            cli(url.clone(), parse(&["oauth", "grant", "list"])),
            Arc::clone(&clock),
        )
        .await
        .unwrap();
        // A plain key is not an OAuth grant: `grant revoke` refuses it.
        assert!(run(
            cli(url.clone(), parse(&["oauth", "grant", "revoke", &plain.id])),
            Arc::clone(&clock),
        )
        .await
        .is_err());
        run(
            cli(url, parse(&["oauth", "grant", "revoke", &grant])),
            Arc::clone(&clock),
        )
        .await
        .unwrap();

        let revoked: Vec<(String, bool)> =
            sqlx::query_as("SELECT id, revoked_at IS NOT NULL FROM api_keys ORDER BY name")
                .fetch_all(&db.pool)
                .await
                .unwrap();
        assert!(revoked.contains(&(grant, true)));
        assert!(revoked.contains(&(plain.id, false)));
    }

    #[test]
    fn code_list_columns_have_the_documented_order() {
        use crate::store::oauth::codes::SignInCodeRecord;

        let now = chrono::Utc::now();
        let record = SignInCodeRecord {
            id: "code-id".to_string(),
            name: "laptop".to_string(),
            access: Access::Read,
            all_catalogs: false,
            catalogs: vec!["main".to_string(), "other".to_string()],
            default_catalog: None,
            actor: "example-actor".to_string(),
            created_at: now,
            expires_at: chrono::DateTime::parse_from_rfc3339("2026-11-12T12:00:00Z")
                .unwrap()
                .to_utc(),
            redeemed_at: None,
            grant_id: None,
            revoked_at: None,
        };
        let before = chrono::DateTime::parse_from_rfc3339("2026-11-11T12:00:00Z")
            .unwrap()
            .to_utc();

        assert_eq!(
            format_code_record(&record, before),
            "laptop\texample-actor\tcode-id\tread\tlive\tmain,other\tdefault -\t\
             expires 2026-11-12T12:00:00Z\tgrant -"
        );
    }
```

`DateTime<FixedOffset>::to_utc()` is `pub const fn` in chrono 0.4.45 (REG/chrono-0.4.45/src/datetime/mod.rs:428). `TimeDelta::from_std` is at REG/chrono-0.4.45/src/time_delta.rs:481, and `humantime::format_duration` at REG/humantime-2.4.0/src/duration.rs:392.

Run: `cargo test --lib cli::tests::oauth cli::tests::code_list`
Expected: compile errors. `Command::Oauth`, `OauthCommand`, `CodeCommand`, `GrantCommand` and `format_code_record` do not exist yet.

- [ ] **Step 7: Implement `okf-mcp oauth ...`**

In `src/cli.rs`, add to the imports:

```rust
use std::time::Duration;

use crate::clock::format_timestamp;
use crate::store::keys::KeyKind;
use crate::store::oauth::codes::{self, NewSignInCode, SignInCodeRecord};
```

Add the variant to `Command`, after `Key`:

```rust
    /// Mint, list and revoke OAuth sign-in codes; list and revoke OAuth grants.
    // clap names a subcommand after its variant in kebab case: `Oauth` is
    // `oauth` (an `OAuth` variant would be `o-auth`).
    #[command(subcommand)]
    Oauth(OauthCommand),
```

Add the new types after `KeyCreateArgs`:

```rust
/// `okf-mcp oauth ...`.
#[derive(Debug, Subcommand)]
pub enum OauthCommand {
    /// Sign-in codes: one-time codes a person enters on the consent page.
    #[command(subcommand)]
    Code(CodeCommand),
    /// OAuth grants: the keys sign-in codes became.
    #[command(subcommand)]
    Grant(GrantCommand),
}

/// `okf-mcp oauth code ...`.
#[derive(Debug, Subcommand)]
pub enum CodeCommand {
    /// Mint a code and print it once.
    Create(CodeCreateArgs),
    /// List codes in every state (never the codes themselves).
    List,
    /// Revoke a code nobody has redeemed.
    Revoke {
        /// The code's id (from `oauth code list`).
        id: String,
    },
}

/// `okf-mcp oauth grant ...`.
#[derive(Debug, Subcommand)]
pub enum GrantCommand {
    /// List OAuth grants, revoked ones included.
    List,
    /// Revoke an OAuth grant: its tokens stop working within `poll_interval`.
    Revoke {
        /// The grant's id (from `oauth grant list`).
        id: String,
    },
}

/// `okf-mcp oauth code create`'s options.
#[derive(Debug, Args)]
pub struct CodeCreateArgs {
    /// A label (`[A-Za-z0-9._-]+`); the grant's name starts with it.
    pub name: String,
    /// The actor the grant's writes are recorded as.
    #[arg(long)]
    pub actor: String,
    /// Access level: `read` or `write` (`admin` is refused).
    #[arg(long, value_enum)]
    pub access: Access,
    /// Granted catalogs, comma-separated.
    #[arg(
        long,
        value_delimiter = ',',
        required_unless_present = "all",
        conflicts_with = "all"
    )]
    pub catalogs: Vec<String>,
    /// Grant every catalog, present and future.
    #[arg(long)]
    pub all: bool,
    /// The grant's default catalog (must be granted).
    #[arg(long)]
    pub default: Option<String>,
    /// How long the code stays valid unredeemed (`24h`, `3d`; at most
    /// `oauth.code_max_expiry`).
    // `value_parser = fn` runs the function on the text; its `Err(String)`
    // becomes clap's usage error.
    #[arg(long, default_value = "24h", value_parser = parse_expiry)]
    pub expires: Duration,
}

/// Parses `--expires` (`humantime` syntax: `90m`, `24h`, `3d`).
fn parse_expiry(text: &str) -> Result<Duration, String> {
    humantime_serde::re::humantime::parse_duration(text).map_err(|error| error.to_string())
}
```

In `run`, add the arm after `Command::Key`:

```rust
        Command::Oauth(command) => {
            let pool = connect(cli.database_url.as_deref(), &config).await?;
            run_oauth(&pool, command, &config, clock.as_ref()).await?;
        }
```

Add after `format_key_record`:

```rust
/// Runs one `okf-mcp oauth ...` subcommand.
///
/// # Arguments
/// * `pool` - the database (migrated).
/// * `command` - the subcommand.
/// * `config` - for `oauth.code_max_expiry`.
/// * `clock` - the source of every timestamp.
///
/// # Errors
/// The store's refusal or database error; an id that is no OAuth grant.
async fn run_oauth(
    pool: &PgPool,
    command: OauthCommand,
    config: &Config,
    clock: &dyn Clock,
) -> anyhow::Result<()> {
    let now = clock.now();
    match command {
        OauthCommand::Code(CodeCommand::Create(args)) => {
            let expires_at = codes::expiry(now, args.expires, config.oauth.code_max_expiry)?;
            let grants = if args.all {
                Grants::All
            } else {
                Grants::Catalogs(&args.catalogs)
            };
            let new = NewSignInCode {
                name: &args.name,
                access: args.access,
                grants,
                default_catalog: args.default.as_deref(),
                actor: &args.actor,
                expires_at,
            };
            let created = codes::create(pool, &new, now).await?;
            println!("{}", created.code);
            eprintln!(
                "code id: {}\nexpires: {}\nhand this code over now: it is not shown again",
                created.id,
                format_timestamp(expires_at)
            );
        }
        OauthCommand::Code(CodeCommand::List) => {
            for code in codes::list(pool).await? {
                println!("{}", format_code_record(&code, now));
            }
        }
        OauthCommand::Code(CodeCommand::Revoke { id }) => {
            codes::revoke(pool, &id, now).await?;
            println!("revoked sign-in code {id}");
        }
        OauthCommand::Grant(GrantCommand::List) => {
            for grant in keys::list(pool).await? {
                if grant.kind == KeyKind::OAuth {
                    println!(
                        "{}\tclient {}",
                        format_key_record(&grant),
                        grant.client_name.as_deref().unwrap_or("-")
                    );
                }
            }
        }
        OauthCommand::Grant(GrantCommand::Revoke { id }) => {
            let is_grant = keys::list(pool)
                .await?
                .iter()
                .any(|key| key.id == id && key.kind == KeyKind::OAuth);
            if !is_grant {
                anyhow::bail!("no OAuth grant {id:?} (a plain key is revoked with `key revoke`)");
            }
            keys::revoke_by_id(pool, &id, now).await?;
            println!("revoked OAuth grant {id}");
        }
    }
    Ok(())
}

/// One `oauth code list` line, tab-separated: name, actor, id, access,
/// state, catalogs (`*` for all), `default X`, `expires T`, `grant G`.
fn format_code_record(code: &SignInCodeRecord, now: chrono::DateTime<chrono::Utc>) -> String {
    let catalogs = if code.all_catalogs {
        "*".to_string()
    } else {
        code.catalogs.join(",")
    };
    format!(
        "{}\t{}\t{}\t{}\t{}\t{catalogs}\tdefault {}\texpires {}\tgrant {}",
        code.name,
        code.actor,
        code.id,
        code.access.as_str(),
        code.state(now).as_str(),
        code.default_catalog.as_deref().unwrap_or("-"),
        format_timestamp(code.expires_at),
        code.grant_id.as_deref().unwrap_or("-")
    )
}
```

The `oauth_code_create_parses_like_key_create` test compares `args.expires` with `DEFAULT_EXPIRY`, which keeps `default_value = "24h"` and the constant in step.

Run: `cargo test --lib cli::tests`
Expected: PASS.

- [ ] **Step 8: Write the failing API tests**

Create `src/http/api/oauth.rs` with only its test module for now. Add `mod oauth;` to `src/http/api/mod.rs` (alphabetical, after `mod maintenance;`). Leave `routes()` unchanged until Step 10.

```rust
#[cfg(test)]
mod tests {
    use chrono::{TimeDelta, Utc};
    use serde_json::json;

    use crate::http::api::testing::{app, assert_matrix, keys, send, Row};
    use crate::mcp::testing::TestApp;
    use crate::store::keys::{Access, Grants};
    use crate::store::oauth::codes::{self, CreatedSignInCode, NewSignInCode};

    /// Mints a code straight in the store, as `okf-mcp oauth code create` would.
    async fn stored(test: &TestApp, name: &str, catalog: &str) -> CreatedSignInCode {
        let grants = [catalog.to_string()];
        let new = NewSignInCode {
            name,
            access: Access::Read,
            grants: Grants::Catalogs(&grants),
            default_catalog: None,
            actor: "example-actor",
            expires_at: Utc::now() + TimeDelta::hours(1),
        };
        codes::create(test.app.pool(), &new, Utc::now())
            .await
            .expect("mint a code")
    }

    #[tokio::test]
    async fn code_routes_need_an_admin_that_reaches_the_code() {
        let test = app().await;
        let code = stored(&test, "target", "main").await.id;
        assert_matrix(
            &test,
            &[
                Row {
                    method: "GET",
                    uri: "/api/v1/oauth/codes".into(),
                    body: None,
                    read: 403,
                    write: 403,
                    admin: 200,
                    stranger: 200,
                },
                Row {
                    method: "POST",
                    uri: "/api/v1/oauth/codes".into(),
                    body: Some(json!({"name": "made", "actor": "example-actor",
                                        "access": "read", "catalogs": ["main"]})),
                    read: 403,
                    write: 403,
                    admin: 200,
                    stranger: 404,
                },
                Row {
                    method: "DELETE",
                    uri: format!("/api/v1/oauth/codes/{code}"),
                    body: None,
                    read: 403,
                    write: 403,
                    admin: 200,
                    stranger: 404,
                },
            ],
        )
        .await;
    }

    #[tokio::test]
    async fn create_returns_the_code_once_and_the_list_never_shows_it() {
        let test = app().await;
        let keys = keys(&test).await;
        let body = json!({"name": "laptop", "actor": "example-actor", "access": "write",
                          "catalogs": ["main"], "default_catalog": "main", "expires_in": "2h"});

        let (status, created) =
            send(&test, "POST", "/api/v1/oauth/codes", Some(&keys.admin), Some(body)).await;

        assert_eq!(status.as_u16(), 200, "{created}");
        let secret = created["secret"].as_str().expect("a secret").to_string();
        assert!(secret.starts_with(crate::keys::SIGN_IN_CODE_PREFIX));
        let code = &created["code"];
        assert_eq!(code["name"], "laptop");
        assert_eq!(code["actor"], "example-actor");
        assert_eq!(code["access"], "write");
        assert_eq!(code["catalogs"], json!(["main"]));
        assert_eq!(code["default_catalog"], "main");
        assert_eq!(code["state"], "live");
        assert!(code["redeemed_at"].is_null() && code["grant_id"].is_null());
        let (status, listed) =
            send(&test, "GET", "/api/v1/oauth/codes", Some(&keys.admin), None).await;
        assert_eq!(status.as_u16(), 200);
        let text = listed.to_string();
        assert!(!text.contains(&secret));
        assert!(!text.contains("sha256"));
        assert_eq!(listed["codes"][0]["id"], code["id"]);
        // The code works on the consent page's lookup.
        let live = codes::find_live(test.app.pool(), &secret, Utc::now())
            .await
            .unwrap();
        assert_eq!(live.map(|record| record.name), Some("laptop".to_string()));
    }

    #[tokio::test]
    async fn create_refuses_admin_access_all_from_a_scoped_admin_and_long_expiries() {
        let test = app().await;
        let keys = keys(&test).await;
        let post = |body: serde_json::Value| {
            let test = &test;
            let key = keys.admin.clone();
            async move {
                send(test, "POST", "/api/v1/oauth/codes", Some(&key), Some(body))
                    .await
                    .0
                    .as_u16()
            }
        };
        let base = json!({"name": "c", "actor": "example-actor", "access": "read",
                          "catalogs": ["main"]});
        let with = |field: &str, value: serde_json::Value| {
            let mut body = base.clone();
            body[field] = value;
            body
        };

        assert_eq!(post(with("access", json!("admin"))).await, 409);
        assert_eq!(post(with("access", json!("root"))).await, 400);
        assert_eq!(post(with("expires_in", json!("8d"))).await, 409);
        assert_eq!(post(with("expires_in", json!("soon"))).await, 400);
        assert_eq!(post(with("actor", json!(" "))).await, 409);
        assert_eq!(
            post(json!({"name": "c", "actor": "example-actor", "access": "read", "all": true}))
                .await,
            403,
            "a scoped admin cannot mint an all-catalogs code"
        );
        assert_eq!(post(with("default_catalog", json!("other"))).await, 404);
        assert_eq!(post(with("surprise", json!(1))).await, 422);
    }

    #[tokio::test]
    async fn a_scoped_admin_sees_and_revokes_only_codes_inside_its_reach() {
        let test = app().await;
        let keys = keys(&test).await;
        let mine = stored(&test, "mine", "main").await.id;
        let theirs = stored(&test, "theirs", "other").await.id;

        let (_, listed) = send(&test, "GET", "/api/v1/oauth/codes", Some(&keys.admin), None).await;
        let (status, _) = send(
            &test,
            "DELETE",
            &format!("/api/v1/oauth/codes/{theirs}"),
            Some(&keys.admin),
            None,
        )
        .await;
        let (revoked, view) = send(
            &test,
            "DELETE",
            &format!("/api/v1/oauth/codes/{mine}"),
            Some(&keys.admin),
            None,
        )
        .await;
        let (again, _) = send(
            &test,
            "DELETE",
            &format!("/api/v1/oauth/codes/{mine}"),
            Some(&keys.admin),
            None,
        )
        .await;

        let ids: Vec<&str> = listed["codes"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|code| code["id"].as_str())
            .collect();
        assert_eq!(ids, [mine.as_str()]);
        assert_eq!(status.as_u16(), 404);
        assert_eq!(revoked.as_u16(), 200);
        assert_eq!(view["state"], "revoked");
        assert_eq!(again.as_u16(), 409);
    }
}
```

Run: `cargo test --lib http::api::oauth`
Expected: FAIL. Every request is 404, because the routes are not merged yet (the matrix's first assertion fails).

- [ ] **Step 9: Share the key area's rules**

In `src/http/api/keys.rs`, replace `manages` with a pair, so the codes area applies the same rule:

```rust
/// Whether `principal` manages a credential with these grants (module doc,
/// "Access"); keys and sign-in codes share this rule.
///
/// # Arguments
/// * `principal` - the caller.
/// * `all_catalogs` - the credential reaches every catalog.
/// * `catalogs` - its named catalogs.
///
/// # Returns
/// `true` for a caller reaching every catalog; otherwise `true` only for a
/// credential that is not all-catalogs and whose every catalog the caller reaches.
pub(crate) fn manages_grants(principal: &Principal, all_catalogs: bool, catalogs: &[String]) -> bool {
    match &principal.reach {
        Reach::All => true,
        Reach::Catalogs(_) => {
            !all_catalogs && catalogs.iter().all(|catalog| principal.reaches(catalog))
        }
    }
}

/// Whether `principal` may see and change `record` ([`manages_grants`]).
fn manages(principal: &Principal, record: &KeyRecord) -> bool {
    manages_grants(principal, record.all_catalogs, &record.catalogs)
}

/// The grants a caller asks a new credential to have, checked against its
/// reach: shared by `POST /keys` and `POST /oauth/codes`.
///
/// # Arguments
/// * `principal` - the caller (already checked to be admin).
/// * `all` - the body's `all`.
/// * `catalogs` - the body's `catalogs`.
/// * `default_catalog` - the body's default catalog, empty already mapped to `None`.
///
/// # Returns
/// The grants, borrowing `catalogs`.
///
/// # Errors
/// [`OkfError::Refused`] for `all` together with `catalogs`;
/// [`OkfError::Forbidden`] for `all` from a caller that does not reach every
/// catalog; [`OkfError::CatalogNotSelected`] for a catalog or default the
/// caller does not reach.
pub(crate) fn requested_grants<'a>(
    principal: &Principal,
    all: bool,
    catalogs: &'a [String],
    default_catalog: Option<&str>,
) -> Result<Grants<'a>, OkfError> {
    let grants = if all {
        if !catalogs.is_empty() {
            return Err(OkfError::Refused {
                reason: "give `all` or `catalogs`, not both".to_string(),
            });
        }
        if !matches!(principal.reach, Reach::All) {
            return Err(OkfError::Forbidden {
                reason: "only a key that reaches every catalog may grant every catalog"
                    .to_string(),
            });
        }
        Grants::All
    } else {
        for catalog in catalogs {
            principal.require(Access::Admin, catalog)?;
        }
        Grants::Catalogs(catalogs)
    };
    if let Some(name) = default_catalog {
        principal.require(Access::Admin, name)?;
    }
    Ok(grants)
}

/// Parses a body's `access` field.
///
/// # Errors
/// [`OkfError::InvalidPath`] (400) naming the `access` field.
pub(crate) fn parse_access(text: &str) -> Result<Access, OkfError> {
    Access::parse(text).ok_or_else(|| OkfError::InvalidPath {
        path: "access".to_string(),
        reason: format!("must be read, write or admin, not {text:?}"),
    })
}
```

`create_key`'s checks before `let actor = ...` become:

```rust
    require_admin(&principal)?;
    let access = parse_access(&body.access)?;
    // `filter` turns `Some("")` (an empty form field) into `None`.
    let default_catalog = body
        .default_catalog
        .as_deref()
        .filter(|name| !name.is_empty());
    let grants = requested_grants(&principal, body.all, &body.catalogs, default_catalog)?;
```

The Forbidden message changes from "...may create one that does" to "...may grant every catalog". No test asserts that text (`grep -rn "may create one that does" src tests` finds only the old line).

Run: `cargo test --lib http::api::keys`
Expected: PASS (unchanged behaviour).

- [ ] **Step 10: Implement the codes area**

Put this above the test module in `src/http/api/oauth.rs`:

```rust
//! `/api/v1/oauth/codes` (OAuth design "Sign-in codes"): what
//! `okf-mcp oauth code ...` does, over HTTP. Grants are listed and revoked
//! through `/api/v1/keys`, whose views carry `kind` and `client_name`.
//!
//! Access is the key area's (`api::keys`, M9 Task 2 "Security decisions"):
//! - every route needs an admin key;
//! - an admin manages only codes inside its reach (`manages_grants`); any
//!   other code is a 404, like an unknown id;
//! - a new code may name only catalogs the caller reaches, and `all: true`
//!   needs a caller that reaches every catalog;
//! - only `POST /oauth/codes` answers a code, once; nothing answers its hash.
//!
//! Codes are read from the database on every use, never cached, so these
//! handlers do not poll (Task 4, Decision 4).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::{delete, get};
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::auth::Principal;
use crate::clock::format_timestamp;
use crate::error::OkfError;
use crate::http::api::keys::{manages_grants, parse_access, requested_grants};
use crate::http::api::require_admin;
use crate::http::AppError;
use crate::store::oauth::codes::{self, NewSignInCode, SignInCodeRecord};

/// One sign-in code as the API shows it: never the code or its hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SignInCodeView {
    /// Its immutable id (the `{id}` of `DELETE`).
    pub id: String,
    /// Its label.
    pub name: String,
    /// The grant's actor.
    pub actor: String,
    /// `read` or `write`.
    pub access: &'static str,
    /// The grant reaches every catalog.
    pub all_catalogs: bool,
    /// The grant's catalogs, sorted.
    pub catalogs: Vec<String>,
    /// The grant's default catalog.
    pub default_catalog: Option<String>,
    /// `live`, `expired`, `redeemed` or `revoked`, as of the request.
    pub state: &'static str,
    /// When it was minted.
    pub created_at: String,
    /// When it stops working unredeemed.
    pub expires_at: String,
    /// When it was redeemed.
    pub redeemed_at: Option<String>,
    /// The grant (key id) it became.
    pub grant_id: Option<String>,
    /// When it was revoked.
    pub revoked_at: Option<String>,
}

impl SignInCodeView {
    /// The view of `record` at `now` (its state depends on the time).
    fn new(record: SignInCodeRecord, now: DateTime<Utc>) -> Self {
        let state = record.state(now).as_str();
        // Destructuring names every field: a field added to the record later
        // fails to compile here until someone decides whether the API shows it.
        let SignInCodeRecord {
            id,
            name,
            access,
            all_catalogs,
            catalogs,
            default_catalog,
            actor,
            created_at,
            expires_at,
            redeemed_at,
            grant_id,
            revoked_at,
        } = record;
        Self {
            id,
            name,
            actor,
            access: access.as_str(),
            all_catalogs,
            catalogs,
            default_catalog,
            state,
            created_at: format_timestamp(created_at),
            expires_at: format_timestamp(expires_at),
            redeemed_at: redeemed_at.map(format_timestamp),
            grant_id,
            revoked_at: revoked_at.map(format_timestamp),
        }
    }
}

/// `GET /oauth/codes`' answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SignInCodeList {
    /// The codes the caller manages, newest first, in every state.
    pub codes: Vec<SignInCodeView>,
}

/// `POST /oauth/codes`' answer: the new code's view and, this once, the code.
// `Debug` by hand, so a stray `{:?}` can never print the code.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CreatedSignInCodeView {
    /// The new code.
    pub code: SignInCodeView,
    /// The `okfsc_` code. Not stored; it cannot be shown again.
    pub secret: String,
}

impl std::fmt::Debug for CreatedSignInCodeView {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CreatedSignInCodeView")
            .field("code", &self.code)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// `POST /oauth/codes`' body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateCodeBody {
    /// Label, `[A-Za-z0-9._-]+`.
    name: String,
    /// The grant's actor (required).
    actor: String,
    /// `read` or `write`.
    access: String,
    /// Grant every catalog.
    #[serde(default)]
    all: bool,
    /// The catalogs to grant (without `all`).
    #[serde(default)]
    catalogs: Vec<String>,
    /// The default catalog (empty or absent: none).
    default_catalog: Option<String>,
    /// How long it stays valid (`humantime`: `2h`, `3d`; absent or empty: 24h).
    expires_in: Option<String>,
}

/// This area's routes.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/oauth/codes", get(list_codes).post(create_code))
        .route("/oauth/codes/{id}", delete(revoke_code))
}

/// The code with exactly this id, if `principal` is an admin that manages it.
///
/// # Errors
/// [`OkfError::Forbidden`] below admin; [`OkfError::NotFound`] for an unknown
/// id or a code the caller does not manage (worded alike); [`OkfError::Database`].
async fn managed_code(
    app: &App,
    principal: &Principal,
    id: &str,
) -> Result<SignInCodeRecord, OkfError> {
    require_admin(principal)?;
    codes::list(app.pool())
        .await?
        .into_iter()
        .find(|code| {
            code.id == id && manages_grants(principal, code.all_catalogs, &code.catalogs)
        })
        .ok_or_else(|| OkfError::NotFound {
            path: format!("oauth/codes/{id}"),
        })
}

/// `GET /oauth/codes`: the codes the caller manages.
///
/// # Errors
/// 403 below admin; 500 if the codes cannot be read.
async fn list_codes(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
) -> Result<Json<SignInCodeList>, AppError> {
    require_admin(&principal)?;
    let now = app.clock().now();
    let codes = codes::list(app.pool())
        .await?
        .into_iter()
        .filter(|code| manages_grants(&principal, code.all_catalogs, &code.catalogs))
        .map(|code| SignInCodeView::new(code, now))
        .collect();
    Ok(Json(SignInCodeList { codes }))
}

/// `POST /oauth/codes`: mints a code and returns it, once.
///
/// # Errors
/// 403 below admin, or `all` from a scoped caller; 404 for a catalog the
/// caller does not reach; 400 for an unknown `access` or an unparsable
/// `expires_in`; 409 for `admin` access, a bad name, an empty actor, no
/// catalogs, `all` with `catalogs`, an ungranted default, or an expiry past
/// `oauth.code_max_expiry`; 422 for an unknown field.
async fn create_code(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Json(body): Json<CreateCodeBody>,
) -> Result<Json<CreatedSignInCodeView>, AppError> {
    require_admin(&principal)?;
    let access = parse_access(&body.access)?;
    let default_catalog = body
        .default_catalog
        .as_deref()
        .filter(|name| !name.is_empty());
    let grants = requested_grants(&principal, body.all, &body.catalogs, default_catalog)?;
    let requested = match body
        .expires_in
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        None => codes::DEFAULT_EXPIRY,
        Some(text) => humantime_serde::re::humantime::parse_duration(text).map_err(|error| {
            OkfError::InvalidPath {
                path: "expires_in".to_string(),
                reason: error.to_string(),
            }
        })?,
    };
    let now = app.clock().now();
    let expires_at = codes::expiry(now, requested, app.config().oauth.code_max_expiry)?;
    let new = NewSignInCode {
        name: &body.name,
        access,
        grants,
        default_catalog,
        actor: &body.actor,
        expires_at,
    };
    let created = codes::create(app.pool(), &new, now).await?;
    let record = managed_code(&app, &principal, &created.id).await?;
    Ok(Json(CreatedSignInCodeView {
        code: SignInCodeView::new(record, now),
        secret: created.code,
    }))
}

/// `DELETE /oauth/codes/{id}`: revokes a code nobody has redeemed.
///
/// # Errors
/// 403 below admin; 404 for a code the caller does not manage; 409 if it is
/// redeemed or already revoked.
async fn revoke_code(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(id): Path<String>,
) -> Result<Json<SignInCodeView>, AppError> {
    let target = managed_code(&app, &principal, &id).await?;
    let now = app.clock().now();
    codes::revoke(app.pool(), &target.id, now).await?;
    let record = managed_code(&app, &principal, &target.id).await?;
    Ok(Json(SignInCodeView::new(record, now)))
}
```

In `src/http/api/mod.rs`, add `.merge(oauth::routes())` after `.merge(maintenance::routes())`.

Run: `cargo test --lib http::api`
Expected: PASS.

- [ ] **Step 11: Docs and changelog**

`docs/operations.md`, section "6. First run": after the "Create keys" block, add:

````markdown
**Mint a sign-in code** for a person connecting an OAuth client (ChatGPT,
Claude.ai). It works only once OAuth is on (`public_url`; see "Connecting
over OAuth"). The code is printed once. It is single-use and expires unused
after 24 hours unless `--expires` says otherwise, up to
`oauth.code_max_expiry` (7 days by default). Access is `read` or `write`;
`admin` is refused. The person enters the code on the consent page, which
turns it into an OAuth grant: a key named
`<code name>.<client host>-<8 characters>`, with the code's catalogs, access,
default and actor.

```sh
kubectl -n okf-mcp exec deploy/okf-mcp -- \
    okf-mcp oauth code create laptop --actor example-actor --access write --catalogs notes --default notes
```
````

In section "Web console", subsection "API reference", change "The API has 31 endpoints" to "The API has 34 endpoints". Add `[OAuth sign-in codes](#oauth-sign-in-codes) (3)` to the list after `[API keys](#api-keys) (6)`. After the "API keys" subsection, add:

````markdown
### OAuth sign-in codes

`okf-mcp oauth ...` and `/api/v1/oauth/codes` mint and manage the one-time
codes the OAuth consent page asks for. The reach rule is the one for keys:
a scoped admin manages only codes whose every catalog it reaches. The code
itself is in the create response (and the CLI's stdout) only. Grants, the
keys codes become, are listed and revoked with the other keys: their views
carry `kind: "oauth"` and `client_name`.

| Command | Does |
|---|---|
| `okf-mcp oauth code create <name> --actor <actor> --access read\|write (--catalogs a,b \| --all) [--default <c>] [--expires 24h]` | Prints the `okfsc_` code once (stdout) and its id and expiry (stderr). Refuses `admin`, and an expiry above `oauth.code_max_expiry` |
| `okf-mcp oauth code list` | Tab-separated: name, actor, id, access, state (`live`, `expired`, `redeemed`, `revoked`), catalogs (`*` for all), `default`, `expires`, `grant` |
| `okf-mcp oauth code revoke <id>` | Revokes an unredeemed code |
| `okf-mcp oauth grant list` | `key list`'s columns for OAuth grants only, plus `client <name>` |
| `okf-mcp oauth grant revoke <id>` | Revokes a grant: its tokens stop working within `poll_interval`. Refuses a plain key's id |

| Method and path | Key | Does |
|---|---|---|
| `GET /api/v1/oauth/codes` | admin | The codes the caller manages, newest first: `id`, `name`, `actor`, `access`, `all_catalogs`, `catalogs`, `default_catalog`, `state`, `created_at`, `expires_at`, `redeemed_at`, `grant_id`, `revoked_at`. Never a code or hash |
| `POST /api/v1/oauth/codes` | admin | Body `{name, actor, access: "read"\|"write", all, catalogs, default_catalog, expires_in}` (`expires_in` like `2h` or `3d`, default 24h). Answers `{code, secret}`. 400 for a bad `access` or `expires_in`, 403 for `all` from a scoped caller, 404 for an unreached catalog, 409 for `admin`, a bad name, an empty actor or too long an expiry, 422 for an unknown field |
| `DELETE /api/v1/oauth/codes/{id}` | admin | Revokes an unredeemed code; answers its view. 404 for an unmanaged code, 409 if redeemed or already revoked |

```sh
curl -fsS -H "Authorization: Bearer $OKF_MCP_KEY" -H 'Content-Type: application/json' \
    -d '{"name":"laptop","actor":"example-actor","access":"read","catalogs":["main"]}' \
    https://okf-mcp.example.com/api/v1/oauth/codes
```
````

`README.md`: in the section that lists the subcommands (`okf-mcp key ...`), add one line: `okf-mcp oauth code create|list|revoke` and `okf-mcp oauth grant list|revoke` manage OAuth sign-in codes and grants (see `docs/operations.md`).

`changelog/<date -u +%Y-%m-%dT%H%M%SZ>-m10-task-04-sign-in-codes.md`:
- **Added:** the sign-in code store (`create`, `list`, `find_live`, `revoke`, `redeem`, `expiry`); `okf-mcp oauth code create|list|revoke` and `oauth grant list|revoke`; `GET/POST /api/v1/oauth/codes` and `DELETE /api/v1/oauth/codes/{id}`.
- **Changed:**
  - `store::keys::resolve_grants` is shared by key and code creation;
  - `api::keys` shares `manages_grants`, `requested_grants` and `parse_access`;
  - enabling maintenance reporting on an OAuth grant is a 409.
- **Notes:**
  - a grant is named `<code name>.<client host>-<8 characters of the code id>`, because `@` is outside the key name charset;
  - the CLI takes `--catalogs` like `key create`;
  - codes are never cached, so minting and revoking need no poll.

- [ ] **Step 12: Gates and commit**

Run every gate in "Conventions". Then:

```bash
git add src/store/keys.rs src/store/oauth/codes.rs src/http/api/keys.rs src/http/api/mod.rs \
    src/http/api/oauth.rs src/cli.rs docs/operations.md README.md changelog/
git commit -m "feat(oauth): sign-in codes in the store, the CLI and the API

Admins mint single-use sign-in codes (okf-mcp oauth code create, or
POST /api/v1/oauth/codes) bound to a grant template; redeeming one creates
an OAuth grant key row in one locked transaction. Grants are listed and
revoked with okf-mcp oauth grant. Key and code creation share their grant
checks and reach rules.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: Authorize and the consent page

**Goal:** `GET /oauth/authorize` validates a client's request and shows the consent page. `POST /oauth/authorize` takes the sign-in code, shows what will be granted, and on Approve redeems the code into a grant and redirects to the client with a one-time `okfac_` authorization code.

**Facts this task relies on** (verified at `c24fef1` unless marked):

- axum 0.8.9:
  - `Redirect::to` answers **303 See Other** (REG/axum-0.8.9/src/response/redirect.rs:37-38). `Redirect::temporary` is 307, which would make the browser re-POST the consent form (sign-in code included) to the client, so it is never used here. OAuth 2.1 (draft-ietf-oauth-v2-1, "307 redirect" security consideration) recommends 303 for exactly this reason. This is from the draft, not re-checked in this repository.
  - `Form<T>` is in the default `form` feature (REG/axum-0.8.9/Cargo.toml:81-96, src/form.rs:71). Its rejection is `axum::extract::rejection::FormRejection` (src/extract/rejection.rs:102-112): 415 for the wrong content type, 422 for a body that does not deserialize.
  - `QueryRejection` (rejection.rs:92-99) is `Query`'s rejection. Taking `Result<Query<T>, QueryRejection>` (or `Result<Form<T>, FormRejection>`) as the extractor lets the handler answer with its own error page.
- url 2.5.8 (in `Cargo.lock`; Task 3 adds it as a direct dependency):
  - `Url::origin()` (REG/url-2.5.8/src/lib.rs:883);
  - `Origin::ascii_serialization()` (src/origin.rs:78) gives `https://client.example.com` or `http://127.0.0.1:43123`;
  - `Url::query_pairs_mut()` (lib.rs:1701) appends parameters and keeps an existing query.
- `subtle 2.6.1`'s `ConstantTimeEq::ct_eq` is already a dependency (`src/keys.rs:10`).
- The console's stylesheets are served by `src/http/ui.rs` at `/ui/vendor/pico.min.css` and `/ui/styles.css`. `build.rs` embeds every file under `src/http/ui/`, so a new `src/http/ui/consent.css` is served at `/ui/consent.css` with no Rust edit (M9 Decision 4). `/ui/*` responses carry the console CSP, `nosniff` and `no-referrer` (`src/http/ui.rs`).
- `styles.css` defines the colour tokens on `:root` and the dark set only under `[data-theme="dark"]`, which the console's `main.js` sets (styles.css:5-29). The consent page runs no script, so `consent.css` repeats the dark tokens under `@media (prefers-color-scheme: dark)`. The console's `.login-card`, `.brand`, `.muted` and `.error` classes are reused.
- The console loads Google Fonts (index.html:7-9). The console CSP allows `style-src https://fonts.googleapis.com` and `font-src https://fonts.gstatic.com` (`src/http/ui.rs` `CONTENT_SECURITY_POLICY`). The consent CSP allows the same two hosts (see "Deviation" below).
- Interfaces from Task 2:
  - `App::oauth() -> Option<&OAuth>`;
  - `OAuth.{settings, pending, codes, consent_limit}`;
  - `ExpiringMap::{insert, take, update, remove}`;
  - `PendingAuthorization`, `AuthorizationCode`, `PENDING_TTL`, `AUTHORIZATION_CODE_TTL`, `MAX_FAILED_ATTEMPTS`;
  - `TokenBucket::try_take`;
  - `OAuthError` with `code()` and `status()`;
  - `keys::{generate_with, hash, AUTHORIZATION_CODE_PREFIX}`;
  - `crate::maintenance::types::new_id()`.
- Interfaces from Task 3: `oauth::clients::resolve(app, oauth, client_id) -> Result<ClientRecord, OAuthError>` and `store::oauth::clients::insert_dcr`.
- Interfaces from Task 4:
  - `store::oauth::codes::{create, find_live, redeem, NewSignInCode, SignInCodeRecord}`;
  - `redeem` creates the grant row and **bumps `admin_generation`** in its transaction, as `create_with_id` does. That makes the `app.poll()` after it load the grant into the key cache. If Task 4's `redeem` does not bump it, add the bump there.
  - `redeem` answers `OkfError::Refused` for a code that is no longer live.
- Task 2's `consent_limit` bucket must allow at least 20 POSTs in a burst: each unit test gets a fresh app (so a fresh bucket) and sends up to 8. If Task 2 chose less, raise it there.
- `crate::http::router(app, &CancellationToken)` and `crate::mcp::testing::app_with(&[...], Config)` drive the whole router in unit tests (as `src/http/ui.rs` tests do).
- `tests/ui_e2e/harness.rs`: `drive(base, journey)` fails a journey on any console `error` record except Chromium's "Failed to load resource" lines. `Ui.page` is public, so a journey can `goto` any URL. **To verify while implementing (Step 12):** that Chromium reports a CSP `form-action` violation, including one on the redirect after a POST, as a console `error` record. Step 12 proves it with a probe.

**Interface corrections this task makes** (already folded into the plan's "Interfaces shared across tasks"):

- `PendingAuthorization` derives `Clone` (`Debug` stays by hand).
- `AuthorizationCode` derives `Debug, Clone` and gains `pub used: bool`. Task 6 marks a code used instead of removing it, so a replay within its 60 seconds is detected.
- New `pub fn code_key(code: &str) -> String` in `src/oauth/state.rs`: the lowercase hex SHA-256 of an `okfac_` code, the key of `OAuth.codes`.
  > **Corrected (issue #246, 2026-11-11).** Task 2 already ships both structs as below (`Clone`, `used`) and `code_key`, as `pub fn` with this task's `fold` body, plus `code_key_is_the_hex_sha256`. Step 4's `state.rs` changes are done: skip them (don't add a second `code_key` test).
- New `#[cfg(test)] pub(crate) mod testing;` in `src/oauth/mod.rs`. It holds the helpers Tasks 5 and 6 share.

**Deviation from the spec** (recorded in the plan's deviations list):

- The consent CSP adds `https://fonts.googleapis.com` to `style-src` and `font-src https://fonts.gstatic.com`, so the page uses the console's fonts. The spec says `style-src 'self'`.
- The page follows the system theme only (`prefers-color-scheme`), with no toggle: it has no script, and it is one step of a sign-in flow.

**Files:**
- Create: `src/oauth/pkce.rs` (replacing Task 2's stub), `src/oauth/consent.rs` (replacing the stub), `src/oauth/authorize.rs` (replacing the stub), `src/oauth/testing.rs`, `src/http/ui/consent.css`, `tests/ui_e2e/consent.rs`
- Modify: `src/oauth/mod.rs` (one `mod testing` line), `src/oauth/state.rs` (`Clone`, `used`, `code_key`), `tests/ui_e2e/main.rs` (one `mod consent;` line), `docs/superpowers/plans/2026-11-11-okf-mcp-m10-oauth.md` (the corrections above)
- Changelog: `changelog/<stamp>-m10-task-05-authorize-consent.md`

- [ ] **Step 1: Write the failing PKCE tests**

Replace `src/oauth/pkce.rs` (Task 2's stub) with the module below, with the implementation bodies left as `todo!()` for this step only:

```rust
//! PKCE (RFC 7636), S256 only: the authorize endpoint checks a challenge's
//! shape, the token endpoint checks a verifier against it.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// The length of an S256 challenge: 32 bytes of SHA-256 in unpadded base64url.
const CHALLENGE_LEN: usize = 43;

/// Is `byte` a base64url character (`A-Z a-z 0-9 - _`)?
fn is_base64url(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
}

/// Is `byte` an RFC 7636 "unreserved" character (`A-Z a-z 0-9 - . _ ~`)?
fn is_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

/// Could `challenge` be an S256 code challenge?
///
/// # Arguments
/// * `challenge` - the `code_challenge` parameter.
///
/// # Returns
/// `true` for exactly 43 base64url characters.
pub fn is_valid_challenge(challenge: &str) -> bool {
    challenge.len() == CHALLENGE_LEN && challenge.bytes().all(is_base64url)
}

/// Does `verifier` match `challenge` under S256?
///
/// # Arguments
/// * `challenge` - the `code_challenge` stored with the authorization code.
/// * `verifier` - the `code_verifier` the token request carries.
///
/// # Returns
/// `true` only if `verifier` is 43 to 128 unreserved characters and
/// `BASE64URL(SHA256(verifier))` equals `challenge`, compared in constant time.
pub fn verify(challenge: &str, verifier: &str) -> bool {
    let well_formed =
        (43..=128).contains(&verifier.len()) && verifier.bytes().all(is_unreserved);
    if !well_formed {
        return false;
    }
    let computed = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    // `ct_eq` on two byte slices of different lengths is simply false.
    computed.as_bytes().ct_eq(challenge.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 7636 appendix B: a published test vector, not a secret.
    const RFC_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    const RFC_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

    #[test]
    fn the_rfc_vector_verifies() {
        assert!(is_valid_challenge(RFC_CHALLENGE));
        assert!(verify(RFC_CHALLENGE, RFC_VERIFIER));
    }

    #[test]
    fn a_different_verifier_fails() {
        let other = format!("{}A", &RFC_VERIFIER[..42]);
        assert!(!verify(RFC_CHALLENGE, &other));
    }

    #[test]
    fn malformed_verifiers_fail_without_hashing() {
        assert!(!verify(RFC_CHALLENGE, &RFC_VERIFIER[..42]), "too short");
        assert!(!verify(RFC_CHALLENGE, &"a".repeat(129)), "too long");
        assert!(!verify(RFC_CHALLENGE, &format!("{}+", &RFC_VERIFIER[..42])), "'+' is reserved");
    }

    #[test]
    fn challenges_must_be_43_base64url_characters() {
        assert!(!is_valid_challenge(""));
        assert!(!is_valid_challenge(&RFC_CHALLENGE[..42]));
        assert!(!is_valid_challenge(&format!("{RFC_CHALLENGE}A")));
        assert!(!is_valid_challenge(&format!("{}=", &RFC_CHALLENGE[..42])));
        // A plain-method "challenge" is often the verifier itself: 43 chars
        // of the unreserved set, here with a '.', which base64url lacks.
        assert!(!is_valid_challenge(&format!("{}.", &RFC_CHALLENGE[..42])));
    }
}
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test --lib oauth::pkce`
Expected: 4 tests FAIL, each panicking at `not yet implemented`.

- [ ] **Step 3: Fill in the bodies**

Replace the two `todo!()`s with the bodies shown in Step 1.

Run: `cargo test --lib oauth::pkce`
Expected: `test result: ok. 4 passed`.

- [ ] **Step 3a: Property tests for PKCE (spec "Testing")**

Append to the test module of `src/oauth/pkce.rs`. `generators::from_regex` matches the whole string by default (REG/hegeltest-0.50.3/src/generators/strings.rs:446-450, `fullmatch: true`).

```rust
    /// The S256 challenge of a verifier, computed independently of `verify`.
    fn s256(verifier: &str) -> String {
        URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
    }

    #[hegel::test]
    fn verify_accepts_exactly_the_matching_verifier(tc: hegel::TestCase) {
        use hegel::generators;
        let verifier: String = tc.draw(generators::from_regex("[A-Za-z0-9._~-]{43,128}"));
        let other: String = tc.draw(generators::from_regex("[A-Za-z0-9._~-]{43,128}"));
        let challenge = s256(&verifier);

        assert!(is_valid_challenge(&challenge));
        assert!(verify(&challenge, &verifier));
        // SHA-256 collisions are out of reach, so a different verifier fails.
        assert_eq!(verify(&challenge, &other), verifier == other);
    }

    #[hegel::test]
    fn verify_never_panics_on_arbitrary_input(tc: hegel::TestCase) {
        use hegel::generators;
        let challenge: String = tc.draw(generators::text());
        let verifier: String = tc.draw(generators::text());

        // Any answer is fine; a malformed verifier must be `false`.
        let accepted = verify(&challenge, &verifier);
        if !(43..=128).contains(&verifier.len()) {
            assert!(!accepted);
        }
    }
```

Run: `cargo test --lib oauth::pkce`
Expected: PASS.

- [ ] **Step 4: State corrections and the shared test helpers**

In `src/oauth/state.rs`, change the two structs' derives and add `used` and `code_key`. Keep Task 2's hand-written `Debug` for `PendingAuthorization`.

> **Corrected (issue #246, 2026-11-11).** Already done by Task 2: the derives, `used`, and `pub fn code_key` with the body below, tested by `code_key_is_the_hex_sha256`. Skip this `state.rs` part of the step.

```rust
// PendingAuthorization: add `Clone` to its derive list (Debug stays by hand).
#[derive(Clone)]
pub struct PendingAuthorization { /* Task 2's fields, unchanged */ }

/// An authorization code waiting for the token endpoint (in `OAuth.codes`,
/// under [`code_key`] of the code).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationCode {
    /// The grant the code was minted for.
    pub grant_id: String,
    /// The client it was issued to.
    pub client_id: String,
    /// The redirect URI the authorization request named.
    pub redirect_uri: String,
    /// The PKCE S256 challenge.
    pub code_challenge: String,
    /// The `resource` the authorization request named.
    pub resource: String,
    /// The token family every token minted from it joins.
    pub family_id: String,
    /// Already exchanged: a second exchange is a replay (RFC 6749 4.1.2),
    /// and revokes the family.
    pub used: bool,
}

/// The `OAuth.codes` key of an authorization code: its SHA-256 in
/// lowercase hex, so the map never holds a usable code.
///
/// # Arguments
/// * `code` - the `okfac_` code as presented.
///
/// # Returns
/// 64 hex characters.
pub fn code_key(code: &str) -> String {
    // `fold` with `write!` appends each byte's two hex digits to one
    // preallocated `String` (Python: `hashlib.sha256(...).hexdigest()`).
    use std::fmt::Write as _;
    crate::keys::hash(code)
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            // Writing to a `String` cannot fail.
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}
```

Add to `state.rs`'s tests:

```rust
#[test]
fn code_key_is_lowercase_hex_sha256() {
    // SHA-256("abc"), the FIPS 180-2 vector `src/keys.rs` also tests.
    assert_eq!(
        code_key("abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}
```

In `src/oauth/mod.rs`, under the other `pub mod` lines, add:

```rust
#[cfg(test)]
pub(crate) mod testing;
```

Create `src/oauth/testing.rs`:

```rust
//! Test helpers for the OAuth routes: an app with OAuth on, a registered DCR
//! client, a live sign-in code, and a request helper over the whole router.

use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{header, Request, Response, StatusCode};
use chrono::{TimeDelta, Utc};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

use crate::config::Config;
use crate::http::api::testing::MAIN;
use crate::http::router;
use crate::mcp::testing::{app_with, TestApp};
use crate::store::keys::{Access, Grants};
use crate::store::oauth::codes::{self as code_rows, NewSignInCode};

/// The public URL the test apps run under.
pub(crate) const PUBLIC_URL: &str = "https://okf.example.com";
/// The resource every valid request names.
pub(crate) const RESOURCE: &str = "https://okf.example.com/mcp";
/// The test DCR client's id and its one redirect URI.
pub(crate) const CLIENT_ID: &str = "test-client";
pub(crate) const REDIRECT_URI: &str = "https://client.example.com/callback";
/// A valid S256 challenge (RFC 7636 appendix B) and its verifier.
pub(crate) const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
pub(crate) const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";

/// An app holding `main` and `other` with OAuth on at [`PUBLIC_URL`], and
/// the DCR client [`CLIENT_ID`] registered.
pub(crate) async fn oauth_app() -> TestApp {
    let config = Config {
        public_url: Some(PUBLIC_URL.to_string()),
        allowed_hosts: vec!["okf.example.com".to_string()],
        ..Config::default()
    };
    let test = app_with(&[("main", MAIN), ("other", &[("index.md", "# Other\n")])], config).await;
    // Corrected (issue #247, 2026-11-11): `insert_dcr` lets Postgres pick a
    // UUID, so the fixed test id is written with plain SQL.
    sqlx::query(
        "INSERT INTO oauth_clients (client_id, kind, client_name, redirect_uris, metadata, created_at) \
         VALUES ($1, 'dcr', 'Example Client', ARRAY[$2], jsonb_build_object('redirect_uris', ARRAY[$2]), now())",
    )
    .bind(CLIENT_ID)
    .bind(REDIRECT_URI)
    .execute(test.app.pool())
    .await
    .expect("register the test client");
    test
}

/// A live sign-in code: `access` on `main`, actor `example-actor`.
///
/// # Returns
/// The `okfsc_` code.
pub(crate) async fn sign_in_code(test: &TestApp, access: Access) -> String {
    let catalogs = ["main".to_string()];
    let new = NewSignInCode {
        name: "example",
        access,
        grants: Grants::Catalogs(&catalogs),
        default_catalog: Some("main"),
        actor: "example-actor",
        expires_at: Utc::now() + TimeDelta::hours(24),
    };
    code_rows::create(test.app.pool(), &new, Utc::now())
        .await
        .expect("create a sign-in code")
        .code
}

/// A valid authorization request's query string (the client's `state` is `xyz`).
pub(crate) fn authorize_query() -> String {
    form(&[
        ("response_type", "code"),
        ("client_id", CLIENT_ID),
        ("redirect_uri", REDIRECT_URI),
        ("state", "xyz"),
        ("code_challenge", CHALLENGE),
        ("code_challenge_method", "S256"),
        ("resource", RESOURCE),
    ])
}

/// `application/x-www-form-urlencoded` text of `pairs`.
pub(crate) fn form(pairs: &[(&str, &str)]) -> String {
    form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish()
}

/// Sends one request through the whole router.
///
/// # Arguments
/// * `method` - `GET` or `POST`.
/// * `uri` - the path and query.
/// * `body` - a form body (sent as `application/x-www-form-urlencoded`), if any.
///
/// # Returns
/// The status, the headers and the body as text.
pub(crate) async fn send(
    test: &TestApp,
    method: &str,
    uri: &str,
    body: Option<String>,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let mut request = Request::builder().method(method).uri(uri);
    if body.is_some() {
        request = request.header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    }
    let request = request
        .body(body.map_or_else(Body::empty, Body::from))
        .expect("request");
    let response: Response<Body> = router(Arc::clone(&test.app), &CancellationToken::new())
        .oneshot(request)
        .await
        .expect("response");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.expect("body");
    (status, headers, String::from_utf8_lossy(&bytes).into_owned())
}

/// The `value` of the hidden input `name` in a consent page.
///
/// # Panics
/// If the page has no such input.
pub(crate) fn hidden(page: &str, name: &str) -> String {
    let marker = format!("name=\"{name}\" value=\"");
    let start = page.find(&marker).expect("hidden input") + marker.len();
    let end = start + page[start..].find('"').expect("closing quote");
    page[start..end].to_string()
}
```

Run: `cargo test --lib oauth::state`
Expected: PASS, including `code_key_is_lowercase_hex_sha256`.

- [ ] **Step 5: Write the failing consent-page tests**

Create `src/oauth/consent.rs`. Its tests go first; the functions follow in Step 7.

```rust
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
    }

    #[test]
    fn the_csp_allows_self_and_exactly_the_redirect_origin() {
        let csp = content_security_policy(Some("https://client.example.com"));
        assert!(csp.contains("form-action 'self' https://client.example.com;"));
        assert!(csp.starts_with("default-src 'none';"));
        assert!(csp.contains("frame-ancestors 'none'"));
        assert!(!csp.contains("script-src"), "the page runs no script");
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
        let html = error_page_html("Unknown <client>.");
        assert!(html.contains("Unknown &lt;client&gt;."));
        assert!(!html.contains("<form"));
    }
}
```

Run: `cargo test --lib oauth::consent`
Expected: FAIL to compile (`PageView`, `escape`, ... not found).

- [ ] **Step 6: Add the page stylesheet**

Create `src/http/ui/consent.css`:

```css
/* The OAuth consent page (src/oauth/consent.rs), layered on pico.min.css and
 * styles.css. The page runs no script, so it follows the system theme here
 * instead of main.js's data-theme. The dark tokens repeat styles.css's. */

@media (prefers-color-scheme: dark) {
    :root {
        --bg: #0f0f11; --surface: #18181b; --surface-2: #1f1f23;
        --fg: #e4e4e7; --fg-muted: #a1a1aa;
        --border: #27272a; --border-strong: #3f3f46;
        --accent: #a78bfa; --accent-fg: #1a1a1c; --accent-soft: #2e1065;
        --ok: #34d399; --ok-soft: #064e3b;
        --warn: #fbbf24; --warn-soft: #451a03;
        --error: #f87171; --error-soft: #450a0a;
        --shadow: 0 1px 2px rgba(0,0,0,.3), 0 4px 16px rgba(0,0,0,.4);
        color-scheme: dark;
    }
}

.consent-card { max-width: 440px; }
.consent-card h1 { margin-bottom: .75rem; }
.consent-client { font-family: var(--font-display); font-size: 1.1rem; font-weight: 700; }
.consent-host { font-family: var(--font-mono); font-size: .85rem; }
.consent-badge { display: inline-block; margin-left: .4rem; padding: 0 .45rem; border-radius: 999px;
    font-size: .7rem; font-weight: 600; letter-spacing: .04em; text-transform: uppercase;
    background: var(--warn-soft); color: var(--warn); }
.consent-summary { padding: .9rem 1rem; margin: 1rem 0; border-radius: var(--radius-sm);
    background: var(--accent-soft); border: 1px solid var(--border); line-height: 1.6; }
.consent-summary strong { color: var(--accent); }
.consent-actions { display: flex; gap: .5rem; }
.consent-actions button { flex: 1; margin: 0; }
.consent-error { padding: .6rem .8rem; border-radius: var(--radius-sm); background: var(--error-soft); }
```

- [ ] **Step 7: Implement the consent pages**

Put this above the tests in `src/oauth/consent.rs`:

```rust
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
/// so the redirect URI's origin must be listed, or Approve would be blocked
/// on its way to the client.
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
    let html = document(title, body);
    let csp = content_security_policy(redirect_origin);
    // The origin came from a parsed URL, so it is always a valid header
    // value; a failure would be a bug, answered as an error page without one.
    let Ok(csp) = HeaderValue::from_str(&csp) else {
        tracing::error!("consent CSP is not a header value");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    (
        status,
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8")),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            (header::REFERRER_POLICY, HeaderValue::from_static("no-referrer")),
            (header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")),
            (header::CONTENT_SECURITY_POLICY, csp),
        ],
        html,
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
pub(crate) fn code_page_html(view: &PageView, error: Option<&str>) -> String {
    let error = error.map_or_else(String::new, |message| {
        format!(
            r#"        <p class="error consent-error" id="consent-error" role="alert">{}</p>
"#,
            escape(message)
        )
    });
    let body = format!(
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
    );
    body
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
pub(crate) fn summary_page_html(view: &PageView, grant: &GrantSummary) -> String {
    let default = grant.default_catalog.as_deref().map_or_else(String::new, |name| {
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
```

Run: `cargo test --lib oauth::consent`
Expected: `test result: ok. 6 passed`.

- [ ] **Step 8: Write the failing authorize tests**

Create `src/oauth/authorize.rs` with this test module (the handlers follow in Step 9):

```rust
#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use crate::oauth::state::code_key;
    use crate::oauth::testing::{
        authorize_query, form, hidden, oauth_app, send, sign_in_code, CLIENT_ID, PUBLIC_URL,
        REDIRECT_URI,
    };
    use crate::store::keys::{self as key_rows, Access, KeyKind};

    /// Replaces one parameter of the valid query.
    fn query_with(name: &str, value: &str) -> String {
        authorize_query()
            .split('&')
            .map(|pair| match pair.split_once('=') {
                Some((key, _)) if key == name => form(&[(name, value)]),
                _ => pair.to_string(),
            })
            .collect::<Vec<_>>()
            .join("&")
    }

    fn location(headers: &axum::http::HeaderMap) -> url::Url {
        url::Url::parse(headers["location"].to_str().unwrap()).unwrap()
    }

    fn param(url: &url::Url, name: &str) -> Option<String> {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_valid_request_shows_step_one_for_the_client() {
        let test = oauth_app().await;
        let (status, headers, page) =
            send(&test, "GET", &format!("/oauth/authorize?{}", authorize_query()), None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(page.contains("Example Client"));
        assert!(page.contains("returns to client.example.com"));
        assert!(page.contains("unverified"), "a DCR client");
        assert!(headers["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("form-action 'self' https://client.example.com;"));
        assert!(!hidden(&page, "pending").is_empty());
        assert!(!hidden(&page, "csrf").is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn before_the_redirect_uri_is_trusted_errors_never_redirect() {
        let test = oauth_app().await;
        for query in [
            query_with("client_id", "no-such-client"),
            query_with("redirect_uri", "https://attacker.example.com/callback"),
            query_with("redirect_uri", &format!("{REDIRECT_URI}/extra")),
            authorize_query().replace(&form(&[("client_id", CLIENT_ID)]), ""),
        ] {
            let (status, headers, page) =
                send(&test, "GET", &format!("/oauth/authorize?{query}"), None).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
            assert!(headers.get("location").is_none(), "{query}");
            assert!(page.contains("Cannot connect"), "{query}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn after_the_redirect_uri_is_trusted_errors_go_back_to_the_client() {
        let test = oauth_app().await;
        for (query, expected) in [
            (query_with("response_type", "token"), "unsupported_response_type"),
            (query_with("code_challenge_method", "plain"), "invalid_request"),
            (query_with("code_challenge", "short"), "invalid_request"),
            (query_with("resource", "https://other.example.com/mcp"), "invalid_target"),
            (authorize_query().replace(&form(&[("resource", "https://okf.example.com/mcp")]), ""), "invalid_target"),
        ] {
            let (status, headers, _) =
                send(&test, "GET", &format!("/oauth/authorize?{query}"), None).await;
            assert_eq!(status, StatusCode::SEE_OTHER, "{query}");
            let target = location(&headers);
            assert!(target.as_str().starts_with(REDIRECT_URI), "{query}");
            assert_eq!(param(&target, "error").as_deref(), Some(expected), "{query}");
            assert_eq!(param(&target, "state").as_deref(), Some("xyz"));
            assert_eq!(param(&target, "iss").as_deref(), Some(PUBLIC_URL));
        }
    }

    /// Runs step 1 and returns `(pending, csrf)`.
    async fn start(test: &crate::mcp::testing::TestApp) -> (String, String) {
        let (_, _, page) =
            send(test, "GET", &format!("/oauth/authorize?{}", authorize_query()), None).await;
        (hidden(&page, "pending"), hidden(&page, "csrf"))
    }

    async fn post(
        test: &crate::mcp::testing::TestApp,
        pairs: &[(&str, &str)],
    ) -> (StatusCode, axum::http::HeaderMap, String) {
        send(test, "POST", "/oauth/authorize", Some(form(pairs))).await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_code_then_approve_creates_a_grant_and_redirects_with_a_code() {
        let test = oauth_app().await;
        let code = sign_in_code(&test, Access::Write).await;
        let (pending, csrf) = start(&test).await;

        let (status, _, summary) = post(&test, &[
            ("pending", &pending), ("csrf", &csrf), ("action", "code"), ("sign_in_code", &code),
        ]).await;
        assert_eq!(status, StatusCode::OK);
        assert!(summary.contains("<strong>write</strong> access to <strong>main</strong>"));
        assert!(summary.contains("as actor <strong>example-actor</strong>"));

        let (status, headers, _) =
            post(&test, &[("pending", &pending), ("csrf", &csrf), ("action", "approve")]).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        let target = location(&headers);
        let issued = param(&target, "code").expect("a code");
        assert!(issued.starts_with("okfac_"));
        assert_eq!(param(&target, "state").as_deref(), Some("xyz"));
        assert_eq!(param(&target, "iss").as_deref(), Some(PUBLIC_URL));

        // The grant exists, is in the key cache, and the code is waiting.
        let oauth = test.app.oauth().expect("oauth on");
        let waiting = oauth
            .codes
            .update(&code_key(&issued), std::time::Instant::now(), |code| code.clone())
            .expect("the code is stored");
        assert!(!waiting.used);
        let principal = test.app.keys().find_grant(&waiting.grant_id).expect("cached grant");
        assert_eq!(principal.actor, "example-actor");
        assert_eq!(principal.access, Access::Write);
        let rows = key_rows::list(test.app.pool()).await.unwrap();
        let grant = rows.iter().find(|row| row.id == waiting.grant_id).unwrap();
        assert_eq!(grant.kind, KeyKind::OAuth);
        assert_eq!(grant.client_id.as_deref(), Some(CLIENT_ID));

        // The pending request is spent: a second approve finds nothing.
        let (status, _, _) =
            post(&test, &[("pending", &pending), ("csrf", &csrf), ("action", "approve")]).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deny_redirects_with_access_denied() {
        let test = oauth_app().await;
        let (pending, csrf) = start(&test).await;
        let (status, headers, _) =
            post(&test, &[("pending", &pending), ("csrf", &csrf), ("action", "deny")]).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(param(&location(&headers), "error").as_deref(), Some("access_denied"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_wrong_csrf_token_is_refused_and_keeps_the_request() {
        let test = oauth_app().await;
        let (pending, csrf) = start(&test).await;
        let (status, _, _) =
            post(&test, &[("pending", &pending), ("csrf", "forged"), ("action", "deny")]).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // The real form still works: a forger cannot cancel someone's request.
        let (status, _, _) =
            post(&test, &[("pending", &pending), ("csrf", &csrf), ("action", "deny")]).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn five_bad_codes_end_the_request() {
        let test = oauth_app().await;
        let (pending, csrf) = start(&test).await;
        for attempt in 1..=4 {
            let (status, _, page) = post(&test, &[
                ("pending", &pending), ("csrf", &csrf), ("action", "code"), ("sign_in_code", "okfsc_wrong"),
            ]).await;
            assert_eq!(status, StatusCode::OK, "attempt {attempt}");
            assert!(page.contains("That sign-in code is not valid"));
        }
        let (status, _, page) = post(&test, &[
            ("pending", &pending), ("csrf", &csrf), ("action", "code"), ("sign_in_code", "okfsc_wrong"),
        ]).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(page.contains("Too many"));
        // Even a good code no longer helps.
        let code = sign_in_code(&test, Access::Read).await;
        let (status, _, _) = post(&test, &[
            ("pending", &pending), ("csrf", &csrf), ("action", "code"), ("sign_in_code", &code),
        ]).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn approve_without_an_accepted_code_goes_back_to_step_one() {
        let test = oauth_app().await;
        let (pending, csrf) = start(&test).await;
        let (status, _, page) =
            post(&test, &[("pending", &pending), ("csrf", &csrf), ("action", "approve")]).await;
        assert_eq!(status, StatusCode::OK);
        assert!(page.contains(r#"id="sign-in-code""#));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_code_redeemed_elsewhere_meanwhile_is_refused_at_approve() {
        let test = oauth_app().await;
        let code = sign_in_code(&test, Access::Read).await;
        let (first, first_csrf) = start(&test).await;
        let (second, second_csrf) = start(&test).await;
        for (pending, csrf) in [(&first, &first_csrf), (&second, &second_csrf)] {
            post(&test, &[
                ("pending", pending), ("csrf", csrf), ("action", "code"), ("sign_in_code", &code),
            ]).await;
        }
        let (status, _, _) =
            post(&test, &[("pending", &first), ("csrf", &first_csrf), ("action", "approve")]).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        let (status, _, page) =
            post(&test, &[("pending", &second), ("csrf", &second_csrf), ("action", "approve")]).await;
        assert_eq!(status, StatusCode::OK);
        assert!(page.contains("That sign-in code is not valid"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_malformed_form_is_an_error_page() {
        let test = oauth_app().await;
        let (status, _, page) = send(&test, "POST", "/oauth/authorize", Some("action=code".into())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(page.contains("Cannot connect"));
    }
}
```

Run: `cargo test --lib oauth::authorize`
Expected: FAIL to compile (`routes` and the handlers not found).

- [ ] **Step 9: Implement the authorize endpoint**

Put this above the tests in `src/oauth/authorize.rs`:

```rust
//! `GET` and `POST /oauth/authorize` (spec "Endpoints" and "Consent flow").
//!
//! The GET validates the request in a fixed order. Until the client and its
//! exact redirect URI are known, a failure is an error page, never a
//! redirect (no open redirect). After that, failures go back to the client
//! as `error=...`. A valid request becomes a pending authorization in
//! memory, and the POSTs walk it through the sign-in code and consent.

use std::sync::Arc;
use std::time::Instant;

use axum::extract::rejection::{FormRejection, QueryRejection};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Router};
use serde::Deserialize;
use subtle::ConstantTimeEq;

use crate::app::App;
use crate::error::OkfError;
use crate::keys;
use crate::oauth::clients;
use crate::oauth::consent::{self, GrantSummary, PageView};
use crate::oauth::state::{code_key, AuthorizationCode, PendingAuthorization, MAX_FAILED_ATTEMPTS};
use crate::oauth::{pkce, OAuth};
use crate::store::oauth::clients::ClientKind;
use crate::store::oauth::codes::{self as code_rows, SignInCodeRecord};

/// The message for every unusable sign-in code, so the page never says
/// whether a code exists, expired, was redeemed or was revoked.
const BAD_CODE: &str = "That sign-in code is not valid. Check it, or ask for a new one.";
/// The message for an unknown, expired or forged pending authorization.
const EXPIRED: &str = "This sign-in request has expired or is not valid.";

/// The routes: `GET` and `POST /oauth/authorize`.
///
/// # Returns
/// A router for [`crate::oauth::routes`] to merge.
pub fn routes() -> Router<Arc<App>> {
    Router::new().route("/oauth/authorize", get(authorize).post(consent_post))
}

/// The authorization request's parameters. Every field is optional, so a
/// missing one is the handler's error, in the spec's order, not serde's.
#[derive(Debug, Default, Deserialize)]
struct AuthorizeQuery {
    response_type: Option<String>,
    client_id: Option<String>,
    redirect_uri: Option<String>,
    state: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    resource: Option<String>,
    // `scope` is accepted and ignored: the sign-in code decides the grant.
}

/// The consent form's fields.
// `Debug` by hand: the sign-in code and the CSRF token never reach a log.
#[derive(Deserialize)]
struct ConsentForm {
    pending: String,
    csrf: String,
    action: String,
    sign_in_code: Option<String>,
}

impl std::fmt::Debug for ConsentForm {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConsentForm")
            .field("pending", &self.pending)
            .field("action", &self.action)
            .finish_non_exhaustive()
    }
}

/// The OAuth state, or a 404 when OAuth is off (the routes are not mounted
/// then, so this is only a guard).
fn oauth_or_404(app: &App) -> Result<&OAuth, Response> {
    app.oauth().ok_or_else(|| StatusCode::NOT_FOUND.into_response())
}

/// A 303 to the client's redirect URI with `pairs`, `state` and `iss` added.
///
/// # Arguments
/// * `redirect_uri` - an exact match of one of the client's URIs.
/// * `pairs` - `code`, or `error` and `error_description`.
/// * `state` - the client's `state`, echoed if it sent one.
/// * `issuer` - this server's issuer (RFC 9207).
fn back_to_client(
    redirect_uri: &str,
    pairs: &[(&str, &str)],
    state: Option<&str>,
    issuer: &str,
) -> Response {
    // Registration (Task 3) parsed every redirect URI, so this cannot fail;
    // a failure is answered as an error page rather than a panic.
    let Ok(mut target) = url::Url::parse(redirect_uri) else {
        return consent::error_page(StatusCode::BAD_REQUEST, "The client's redirect URI is invalid.");
    };
    {
        // `query_pairs_mut` borrows `target` mutably until this block ends,
        // and keeps any query the registered URI already has.
        let mut query = target.query_pairs_mut();
        query.extend_pairs(pairs);
        if let Some(state) = state {
            query.append_pair("state", state);
        }
        query.append_pair("iss", issuer);
    }
    Redirect::to(target.as_str()).into_response()
}

/// An OAuth error sent back to the client.
fn error_to_client(
    redirect_uri: &str,
    code: &str,
    description: &str,
    state: Option<&str>,
    issuer: &str,
) -> Response {
    back_to_client(
        redirect_uri,
        &[("error", code), ("error_description", description)],
        state,
        issuer,
    )
}

/// The redirect URI's origin and host, for the CSP and the page.
fn origin_and_host(redirect_uri: &str) -> (String, String) {
    match url::Url::parse(redirect_uri) {
        Ok(url) => (
            url.origin().ascii_serialization(),
            url.host_str().unwrap_or_default().to_string(),
        ),
        Err(_) => (String::new(), String::new()),
    }
}

/// The page view of a pending authorization.
fn view(pending_id: &str, pending: &PendingAuthorization) -> PageView {
    let (redirect_origin, client_host) = origin_and_host(&pending.redirect_uri);
    PageView {
        pending_id: pending_id.to_string(),
        csrf: pending.csrf.clone(),
        client_name: pending
            .client
            .client_name
            .clone()
            .unwrap_or_else(|| pending.client.client_id.clone()),
        client_host,
        verified: pending.client.kind == ClientKind::Cimd,
        redirect_origin,
    }
}

/// What a sign-in code grants, as step 2 states it.
fn summary(record: &SignInCodeRecord) -> GrantSummary {
    GrantSummary {
        access: record.access.as_str(),
        catalogs: if record.all_catalogs {
            "every catalog".to_string()
        } else {
            record.catalogs.join(", ")
        },
        actor: record.actor.clone(),
        default_catalog: record.default_catalog.clone(),
    }
}

/// `GET /oauth/authorize`: validates the request and shows step 1.
async fn authorize(
    State(app): State<Arc<App>>,
    query: Result<Query<AuthorizeQuery>, QueryRejection>,
) -> Response {
    let oauth = match oauth_or_404(&app) {
        Ok(oauth) => oauth,
        Err(response) => return response,
    };
    // An unparsable query is treated like an empty one: the first missing
    // parameter, `client_id`, is then the error page.
    // `map` unwraps the `Query` tuple struct; a rejection becomes the
    // all-`None` default.
    let query = query.map(|Query(query)| query).unwrap_or_default();
    let bad = |message: &str| consent::error_page(StatusCode::BAD_REQUEST, message);

    // 1. The client.
    let Some(client_id) = query.client_id.as_deref() else {
        return bad("The request names no client.");
    };
    let client = match clients::resolve(&app, oauth, client_id).await {
        Ok(client) => client,
        Err(error) => {
            if matches!(error, crate::oauth::error::OAuthError::Server(_)) {
                tracing::error!(%error, "resolving an OAuth client failed");
                return consent::error_page(StatusCode::INTERNAL_SERVER_ERROR, "The server could not check this client.");
            }
            return bad("This client is not known to the server.");
        }
    };
    // 2. Its exact redirect URI.
    let Some(redirect_uri) = query
        .redirect_uri
        .as_deref()
        .filter(|uri| client.redirect_uris.iter().any(|known| known == uri))
    else {
        return bad("The request's redirect URI is not one the client registered.");
    };
    // From here on, errors go back to the client.
    let issuer = oauth.settings.issuer.as_str();
    let state = query.state.as_deref();
    let back = |code: &str, description: &str| {
        error_to_client(redirect_uri, code, description, state, issuer)
    };
    // 3. The rest, in the spec's order.
    if query.response_type.as_deref() != Some("code") {
        return back("unsupported_response_type", "only response_type=code is supported");
    }
    let Some(code_challenge) = query
        .code_challenge
        .as_deref()
        .filter(|challenge| pkce::is_valid_challenge(challenge))
    else {
        return back("invalid_request", "a PKCE S256 code_challenge is required");
    };
    if query.code_challenge_method.as_deref() != Some("S256") {
        return back("invalid_request", "code_challenge_method must be S256");
    }
    if query.resource.as_deref() != Some(oauth.settings.resource.as_str()) {
        return back("invalid_target", "resource must name this server's MCP endpoint");
    }
    // 4. A pending authorization, and step 1.
    let (pending_id, csrf) = match (keys::generate_with(""), keys::generate_with("")) {
        (Ok(id), Ok(csrf)) => (id, csrf),
        (Err(error), _) | (_, Err(error)) => {
            tracing::error!(%error, "the OS random source failed");
            return consent::error_page(StatusCode::INTERNAL_SERVER_ERROR, "The server could not start sign-in.");
        }
    };
    let pending = PendingAuthorization {
        csrf,
        client,
        redirect_uri: redirect_uri.to_string(),
        state: query.state.clone(),
        code_challenge: code_challenge.to_string(),
        resource: oauth.settings.resource.clone(),
        sign_in_code_id: None,
        failed_attempts: 0,
    };
    let page = consent::code_page(&view(&pending_id, &pending), None);
    // `insert` is `false` when the bounded map is full (Task 2's correction
    // 3): an unauthenticated GET must not grow memory without limit.
    if !oauth.pending.insert(pending_id, pending, Instant::now()) {
        return consent::error_page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Too many sign-ins are in progress. Try again in a few minutes.",
        );
    }
    page
}

/// `POST /oauth/authorize`: the consent form (spec "Consent flow").
async fn consent_post(
    State(app): State<Arc<App>>,
    form: Result<Form<ConsentForm>, FormRejection>,
) -> Response {
    let oauth = match oauth_or_404(&app) {
        Ok(oauth) => oauth,
        Err(response) => return response,
    };
    let now = Instant::now();
    if !oauth.consent_limit.try_take(now) {
        return consent::error_page(StatusCode::TOO_MANY_REQUESTS, "Too many sign-in attempts. Wait a minute.");
    }
    let Ok(Form(form)) = form else {
        return consent::error_page(StatusCode::BAD_REQUEST, EXPIRED);
    };
    // The CSRF token is checked before anything else, and a mismatch leaves
    // the pending authorization alone: someone who learned its id cannot
    // cancel it. `update` holds the map's lock only for the closure.
    let pending = oauth.pending.update(&form.pending, now, |pending| {
        let matches: bool = pending.csrf.as_bytes().ct_eq(form.csrf.as_bytes()).into();
        matches.then(|| pending.clone())
    });
    let Some(Some(pending)) = pending else {
        return consent::error_page(StatusCode::BAD_REQUEST, EXPIRED);
    };
    match form.action.as_str() {
        "deny" => {
            oauth.pending.remove(&form.pending);
            error_to_client(
                &pending.redirect_uri,
                "access_denied",
                "the person signing in denied access",
                pending.state.as_deref(),
                &oauth.settings.issuer,
            )
        }
        "code" => {
            let presented = form.sign_in_code.as_deref().unwrap_or_default().trim();
            check_code(&app, oauth, &form.pending, &pending, presented).await
        }
        "approve" => approve(&app, oauth, &form.pending, &pending).await,
        _ => consent::error_page(StatusCode::BAD_REQUEST, EXPIRED),
    }
}

/// Counts a failed attempt and answers step 1 again, or ends the request
/// at [`MAX_FAILED_ATTEMPTS`].
fn failed_attempt(oauth: &OAuth, pending_id: &str, pending: &PendingAuthorization) -> Response {
    let attempts = oauth.pending.update(pending_id, Instant::now(), |pending| {
        pending.failed_attempts = pending.failed_attempts.saturating_add(1);
        pending.sign_in_code_id = None;
        pending.failed_attempts
    });
    match attempts {
        Some(attempts) if attempts < MAX_FAILED_ATTEMPTS => {
            consent::code_page(&view(pending_id, pending), Some(BAD_CODE))
        }
        _ => {
            oauth.pending.remove(pending_id);
            consent::error_page(StatusCode::BAD_REQUEST, "Too many wrong sign-in codes.")
        }
    }
}

/// `action=code`: checks the code without consuming it, and shows step 2.
async fn check_code(
    app: &App,
    oauth: &OAuth,
    pending_id: &str,
    pending: &PendingAuthorization,
    presented: &str,
) -> Response {
    match code_rows::find_live(app.pool(), presented, app.clock().now()).await {
        Ok(Some(record)) => {
            let code_id = record.id.clone();
            let stored = oauth.pending.update(pending_id, Instant::now(), |pending| {
                pending.sign_in_code_id = Some(code_id);
            });
            if stored.is_none() {
                return consent::error_page(StatusCode::BAD_REQUEST, EXPIRED);
            }
            consent::summary_page(&view(pending_id, pending), &summary(&record))
        }
        Ok(None) => failed_attempt(oauth, pending_id, pending),
        Err(error) => server_error(&error),
    }
}

/// `action=approve`: redeems the code into a grant, mints an authorization
/// code and sends the browser back to the client.
async fn approve(app: &App, oauth: &OAuth, pending_id: &str, pending: &PendingAuthorization) -> Response {
    let Some(code_id) = pending.sign_in_code_id.as_deref() else {
        return consent::code_page(&view(pending_id, pending), None);
    };
    let grant_id = match code_rows::redeem(app.pool(), code_id, &pending.client, app.clock().now()).await {
        Ok(grant_id) => grant_id,
        // Expired, revoked or redeemed meanwhile (another approve won).
        Err(OkfError::Refused { .. }) => return failed_attempt(oauth, pending_id, pending),
        Err(error) => return server_error(&error),
    };
    // The request is spent once the grant exists, whatever happens next.
    oauth.pending.remove(pending_id);
    // `redeem` bumped admin_generation, so this loads the grant into the key
    // cache before the client's token request can arrive.
    if let Err(error) = app.poll().await {
        tracing::error!(%error, "poll after redeeming a sign-in code failed");
    }
    let (code, family_id) = match (keys::generate_with(keys::AUTHORIZATION_CODE_PREFIX), crate::maintenance::types::new_id()) {
        (Ok(code), Ok(family_id)) => (code, family_id),
        (Err(error), _) | (_, Err(error)) => return server_error(&error),
    };
    // A full map refuses the code; the grant stays and can be revoked, and
    // the person starts again from their client.
    let stored = oauth.codes.insert(
        code_key(&code),
        AuthorizationCode {
            grant_id,
            client_id: pending.client.client_id.clone(),
            redirect_uri: pending.redirect_uri.clone(),
            code_challenge: pending.code_challenge.clone(),
            resource: pending.resource.clone(),
            family_id,
            used: false,
        },
        Instant::now(),
    );
    if !stored {
        return consent::error_page(
            StatusCode::SERVICE_UNAVAILABLE,
            "The server is busy. Start again from your client in a minute.",
        );
    }
    back_to_client(
        &pending.redirect_uri,
        &[("code", &code)],
        pending.state.as_deref(),
        &oauth.settings.issuer,
    )
}

/// A 500 error page; the details go to the log only.
fn server_error(error: &OkfError) -> Response {
    tracing::error!(%error, "consent failed");
    consent::error_page(StatusCode::INTERNAL_SERVER_ERROR, "The server could not complete sign-in.")
}
```

Run: `cargo test --lib oauth::authorize`
Expected: `test result: ok. 10 passed`.

- [ ] **Step 10: The whole library, clippy and docs**

Run: `cargo test --lib oauth && cargo clippy --all-targets -- -D warnings && RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
Expected: all OAuth tests pass, with no warnings. If clippy flags `too_many_lines` or `needless_pass_by_value`, fix the code rather than allowing the lint.

- [ ] **Step 11: Write the browser journey**

Add `mod consent;` to `tests/ui_e2e/main.rs`, in alphabetical order after `mod changesets;`. The journey needs a server whose `public_url` is its own address, so it uses `TestServer::start_with`, which Task 6 adds to `tests/support/server.rs`. **If Task 6 has not landed, add `start_with` here exactly as Task 6, Step 9 shows** and note it in the PR.

Create `tests/ui_e2e/consent.rs`:

```rust
//! The OAuth consent page in a real browser: step 1, step 2, Approve, and
//! the redirect to the client, with no console errors or CSP violations.
//! The client's redirect URI is on another port, so the CSP's
//! `form-action` must name its origin for the hand-off to work.

use axum::response::Html;
use axum::routing::get;
use axum::Router;
use chrono::{TimeDelta, Utc};
use okf_mcp::config::Config;
use okf_mcp::store::keys::{Access, Grants};
use okf_mcp::store::oauth::clients::{self as client_rows, NewDcrClient};
use okf_mcp::store::oauth::codes::{self as code_rows, NewSignInCode};
use rustwright::GotoOptions;

use crate::harness::drive;
use crate::support::server::{files, TestServer};

/// A page at `/callback` standing in for the client.
async fn callback_host() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    let router = Router::new().route(
        "/callback",
        get(|| async { Html("<!DOCTYPE html><title>client</title><p id=\"done\">back at the client</p>") }),
    );
    tokio::spawn(async move { axum::serve(listener, router).await });
    base
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_person_enters_a_code_approves_and_lands_back_at_the_client() {
    let server = TestServer::start_with(
        vec![("main", files(&[("index.md", "# Main\n")]))],
        |base| Config {
            public_url: Some(base.to_string()),
            ..Config::default()
        },
    )
    .await;
    let client_base = callback_host().await;
    let redirect_uri = format!("{client_base}/callback");
    // Corrected (issue #247, 2026-11-11): `insert_dcr` takes a `NewDcrClient`
    // and the cap of unbound clients, and returns the stored client (whose id
    // Postgres picks), or `None` at the cap.
    let redirect_uris = [redirect_uri.clone()];
    let metadata = serde_json::json!({ "redirect_uris": [redirect_uri] });
    let client = client_rows::insert_dcr(
        &server.db.pool,
        &NewDcrClient {
            client_name: Some("Example Client"),
            redirect_uris: &redirect_uris,
            metadata: &metadata,
        },
        Utc::now(),
        okf_mcp::oauth::clients::MAX_UNBOUND_DCR,
    )
    .await
    .expect("register the client")
    .expect("under the cap");
    let catalogs = ["main".to_string()];
    let code = code_rows::create(
        &server.db.pool,
        &NewSignInCode {
            name: "journey",
            access: Access::Write,
            grants: Grants::Catalogs(&catalogs),
            default_catalog: Some("main"),
            actor: "example-actor",
            expires_at: Utc::now() + TimeDelta::hours(1),
        },
        Utc::now(),
    )
    .await
    .expect("create a sign-in code")
    .code;
    let query = form_urlencoded::Serializer::new(String::new())
        .extend_pairs([
            ("response_type", "code"),
            ("client_id", client.client_id.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("state", "xyz"),
            // RFC 7636 appendix B's challenge: a published vector.
            ("code_challenge", "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"),
            ("code_challenge_method", "S256"),
            ("resource", &format!("{}/mcp", server.base)),
        ])
        .finish();
    let authorize = format!("{}/oauth/authorize?{query}", server.base);

    drive(server.base.clone(), move |ui| {
        ui.page.goto(&authorize, GotoOptions::default()).expect("open the consent page");
        ui.wait_text("#consent-client", "Example Client");
        ui.fill("#sign-in-code", &code);
        ui.click("#consent-continue");
        ui.wait_text("#consent-summary", "write");
        assert!(ui.text("#consent-summary").contains("example-actor"));
        ui.click("#consent-approve");
        ui.wait_visible("#done");
        let href = ui.eval("() => location.href", serde_json::json!(null));
        let href = href.as_str().unwrap_or_default();
        assert!(href.starts_with(&redirect_uri), "back at the client: {href}");
        assert!(href.contains("code=okfac_"), "with a code: {href}");
        assert!(href.contains("state=xyz"));
    })
    .await;
}
```

Run: `cargo test --test ui_e2e consent -- --test-threads=2`
Expected: PASS with Chromium installed (`1 passed`), or the skip line without it. The PR must show it running.

- [ ] **Step 12: Prove the journey catches a CSP block (probe, not committed)**

This verifies the fact marked "to verify" above. Temporarily change `content_security_policy` to always write `form-action 'self';` and run the journey again:

Run: `cargo test --test ui_e2e consent -- --test-threads=2`
Expected: FAIL. Either `wait_visible("#done")` times out, or `assert_no_console_errors` lists a Chromium "Refused to send form data ... form-action" error.

Write which of the two happened into the PR description. Then restore the function and run the journey again. Expected: PASS.

If neither happens and the probe passes, Chromium is not enforcing `form-action` on the redirect. Record that in the PR and in the plan's verified facts, and keep the origin in the CSP anyway, since other browsers do enforce it.

- [ ] **Step 13: Gates**

Run:
```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
cargo test -- --test-threads=6
just test-ui
```
Expected: every command succeeds. Paste the test counts into the PR.

- [ ] **Step 14: Changelog and plan corrections**

Create `changelog/$(date -u +%Y-%m-%dT%H%M%SZ)-m10-task-05-authorize-consent.md`:

```markdown
# M10 Task 5: authorize and the consent page

## Added
- `GET /oauth/authorize`: validates the client, the exact redirect URI, PKCE S256 and `resource`, in that order. Errors before the redirect URI is trusted render an error page; later ones go back to the client with `error`, `state` and `iss`.
- `POST /oauth/authorize`: the consent form. Step 1 takes the sign-in code, step 2 states what will be granted, and Approve redeems the code into an OAuth grant and redirects with a one-time `okfac_` code (303). Five wrong codes end the request.
- The consent page: server-rendered, no script, the console's look, following the system theme (`src/http/ui/consent.css`).
- `oauth::pkce`: S256 challenge checks and constant-time verification.

## Changed
- `AuthorizationCode` gains `used`; `PendingAuthorization` is `Clone`; `oauth::state::code_key`.

## Notes
- The consent CSP's `form-action` names the redirect URI's origin, since browsers apply it to the redirect after the POST. The CSP also allows the console's Google Fonts hosts (a deviation from the spec's `style-src 'self'`).
```

In the plan, apply this task's "Interface corrections" to "Interfaces shared across tasks". Add the two consent deviations to "Simplifications and deviations".

- [ ] **Step 15: Commit**

```bash
git add src/oauth/pkce.rs src/oauth/consent.rs src/oauth/authorize.rs src/oauth/testing.rs \
        src/oauth/mod.rs src/oauth/state.rs src/http/ui/consent.css \
        tests/ui_e2e/consent.rs tests/ui_e2e/main.rs \
        docs/superpowers/plans/2026-11-11-okf-mcp-m10-oauth.md changelog/
git commit -m "feat(oauth): authorize endpoint and consent page

GET /oauth/authorize validates client, redirect URI, PKCE S256 and
resource (error page before the redirect URI is trusted, redirect after);
POST walks a sign-in code through consent, redeems it into a grant and
redirects with a one-time okfac_ code.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: Token, refresh and revoke; the end-to-end OAuth tests

**Goal:** `POST /oauth/token` exchanges an authorization code (PKCE-checked) for an `okfat_` access token and an `okfrt_` refresh token, and rotates refresh tokens. A replayed code or refresh token revokes its whole family. `POST /oauth/revoke` implements RFC 7009. End-to-end tests drive the full flow against the real server with a real rmcp client.

**Facts this task relies on** (verified at `c24fef1` unless marked):

- axum 0.8.9 `Form` and `FormRejection` as in Task 5. `Json<T>` with a headers array is an `IntoResponse` tuple (M9 verified facts).
- RFC 6749 section 5.1: a token response carries `Cache-Control: no-store` and `Pragma: no-cache`. Section 4.1.2: a code used twice SHOULD revoke the tokens issued from it. RFC 7009 section 2.2: the revocation endpoint answers 200 for an unknown token too. These are from the RFCs, not checked in this repository.
- reqwest 0.13.5 is built without its `form` feature (REG/reqwest-0.13.5/Cargo.toml `[features]`; the project enables only `json` and `rustls`). The tests write form bodies with `form_urlencoded`, a regular dependency, and set the content type themselves. `ClientBuilder::redirect(Policy)` (REG/reqwest-0.13.5/src/async_impl/client.rs:1392) with `reqwest::redirect::Policy::none()` lets a test read a `Location` instead of following it. `RequestBuilder::bearer_auth` is used by `tests/server.rs:24-29`.
- rmcp 3.8.0's `StreamableHttpClientTransportConfig::auth_header(token)` sends `Authorization: Bearer <token>` (REG/rmcp-3.8.0/src/transport/streamable_http_client.rs:2165-2172). `tests/support/server.rs` `TestServer::client(token, None)` already uses it, so an OAuth access token works through the same helper. A refused token fails the client's initialization, and `client` returns `Err`.
- `tests/support/server.rs`:
  - `serve_app(db, config)` loads the app **before** binding (`App::load` then `TcpListener::bind("127.0.0.1:0")`), so a `public_url` naming the server's own port cannot be configured today;
  - this task changes `serve_app` to bind first and adds `TestServer::start_with(catalogs, make_config: impl FnOnce(&str) -> Config)`;
  - `start` and `restart` keep their signatures.
- `Config::default().allowed_hosts` is `["localhost", "127.0.0.1", "::1"]` (`src/config.rs:26`), so a `public_url` of `http://127.0.0.1:<port>` passes Decision 7's host check. Task 2's `Config::validate` accepts `http` on a loopback host.
- `store::keys::revoke_by_id(pool, id, now)` (`src/store/keys.rs:447`) revokes one key or grant by id and bumps the admin generation, so `app.poll()` drops it from the cache.
- Interfaces from Task 2:
  - `store::oauth::tokens::{insert, NewToken, TokenKind, LiveAccessToken, find_access, revoke, prune}`;
  - `OAuth.{settings, access, codes, token_limit}`;
  - `AccessTokens::{remember, forget}`;
  - `Keys::find_grant`;
  - `keys::{generate_with, hash, ACCESS_TOKEN_PREFIX, REFRESH_TOKEN_PREFIX}`;
  - `OAuthError` and its `IntoResponse` (JSON `{error, error_description}`, `no-store`).
- Interfaces from Task 5: `AuthorizationCode { .., used }` (Clone), `state::code_key`, `pkce::verify`, and `src/oauth/testing.rs` (`oauth_app`, `sign_in_code`, `authorize_query`, `form`, `hidden`, `send`, the constants).
- **Interface corrections this task makes** (already folded into "Interfaces shared across tasks"):
  - `CredentialKind::OAuth { client_id: Option<String>, client_name: Option<String> }` gains `client_id`, filled from `KeyRecord.client_id` in `From<KeyRecord> for Principal`. The token endpoint checks that a refresh request's `client_id` is the grant's own. Task 2's `KeyRecord` already carries `client_id`, so this is one field in `auth.rs`.
  - **Task 3 correction:** with `cimd_allow_private = true` (test rigs only), `clients::resolve` also treats an `http://` `client_id` on a loopback host as a CIMD URL. Without that, the end-to-end CIMD test needs a TLS host and a trusted test CA. If Task 3 landed without it, add it there in this PR, with a unit test, and note it.
    (Landed in Task 3, issue #247: `allow_http` is `cimd_allow_private` with an `http://` issuer, and `resolve` takes any `http://` `client_id` as a CIMD URL then; covered by `resolve_fetches_once_then_serves_the_cache`.)
  - `tests/support/server.rs` gains `TestServer::start_with`. `serve_app` binds before loading.
- RFC 7009 says a revocation request's token should belong to the requesting client. Tokens here carry no client id, and the clients are public (no authentication), so this endpoint revokes whatever token it is given. Knowing a token already grants everything revoking it could take away. Recorded as a decision in the plan.

**Files:**
- Modify: `src/store/oauth/tokens.rs` (`RefreshUse`, `use_refresh`, `revoke_family`, tests), `src/auth.rs` (`CredentialKind::OAuth.client_id`), `tests/support/server.rs` (`start_with`, `serve_app`), `tests/support/mod.rs` (`pub mod oauth;`), `docs/superpowers/plans/2026-11-11-okf-mcp-m10-oauth.md` (corrections)
- Create: `src/oauth/token.rs` (replacing Task 2's stub), `tests/support/oauth.rs`, `tests/server_oauth.rs`
- Changelog: `changelog/<stamp>-m10-task-06-token-endpoint.md`

- [ ] **Step 1: Write the failing store tests for refresh rotation and families**

In `src/store/oauth/tokens.rs`, add to the test module (it has `use super::*;` and Task 2's imports of `Utc` and `TimeDelta`; add them if not). Two helpers:

```rust
/// One live, client-less OAuth grant (read on every catalog); its id.
async fn grant_on(pool: &sqlx::PgPool) -> String {
    sqlx::query_scalar::<_, String>(
        "INSERT INTO api_keys (name, kind, access, all_catalogs, actor, created_at) \
         VALUES ('example', 'oauth', 'read', true, 'example-actor', now()) RETURNING id",
    )
    .fetch_one(pool)
    .await
    .expect("insert a grant")
}

/// A token row to insert. The explicit `'a` says the result borrows both
/// strings (Rust cannot infer which of two borrowed inputs it borrows).
fn token<'a>(grant_id: &'a str, kind: TokenKind, family: &'a str, sha256: [u8; 32]) -> NewToken<'a> {
    NewToken {
        sha256,
        grant_id,
        kind,
        family_id: family,
        resource: "https://okf.example.com/mcp",
        expires_at: Utc::now() + TimeDelta::days(30),
    }
}
```

The tests:

```rust
#[tokio::test(flavor = "multi_thread")]
async fn a_refresh_token_rotates_once_then_counts_as_reused() {
    let test = crate::mcp::testing::app_with(&[], crate::config::Config::default()).await;
    let pool = test.app.pool();
    let grant_id = grant_on(pool).await;
    let hash = crate::keys::hash("okfrt_first");
    let mut conn = pool.acquire().await.unwrap();
    insert(&mut conn, &token(&grant_id, TokenKind::Refresh, "family-a", hash), Utc::now()).await.unwrap();
    drop(conn);

    let mut tx = pool.begin().await.unwrap();
    let first = use_refresh(&mut tx, &hash, Utc::now()).await.unwrap();
    tx.commit().await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    let second = use_refresh(&mut tx, &hash, Utc::now()).await.unwrap();
    tx.commit().await.unwrap();

    assert_eq!(
        first,
        RefreshUse::Rotated {
            grant_id: grant_id.clone(),
            family_id: "family-a".to_string(),
            resource: "https://okf.example.com/mcp".to_string(),
        }
    );
    assert_eq!(second, RefreshUse::Reused { family_id: "family-a".to_string() });
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_expired_access_and_revoked_grant_tokens_are_unknown() {
    let test = crate::mcp::testing::app_with(&[], crate::config::Config::default()).await;
    let pool = test.app.pool();
    let grant_id = grant_on(pool).await;
    let access = crate::keys::hash("okfat_x");
    let expired = crate::keys::hash("okfrt_expired");
    let live = crate::keys::hash("okfrt_live");
    let mut conn = pool.acquire().await.unwrap();
    insert(&mut conn, &token(&grant_id, TokenKind::Access, "f", access), Utc::now()).await.unwrap();
    insert(
        &mut conn,
        &NewToken { expires_at: Utc::now() - TimeDelta::seconds(1), ..token(&grant_id, TokenKind::Refresh, "f", expired) },
        Utc::now(),
    ).await.unwrap();
    insert(&mut conn, &token(&grant_id, TokenKind::Refresh, "f", live), Utc::now()).await.unwrap();
    drop(conn);
    crate::store::keys::revoke_by_id(pool, &grant_id, Utc::now()).await.unwrap();

    for hash in [crate::keys::hash("okfrt_none"), access, expired, live] {
        let mut tx = pool.begin().await.unwrap();
        assert_eq!(use_refresh(&mut tx, &hash, Utc::now()).await.unwrap(), RefreshUse::Unknown);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn revoke_family_revokes_every_live_member_and_returns_their_hashes() {
    let test = crate::mcp::testing::app_with(&[], crate::config::Config::default()).await;
    let pool = test.app.pool();
    let grant_id = grant_on(pool).await;
    let a = crate::keys::hash("okfat_a");
    let b = crate::keys::hash("okfrt_b");
    let other = crate::keys::hash("okfat_other");
    let mut conn = pool.acquire().await.unwrap();
    insert(&mut conn, &token(&grant_id, TokenKind::Access, "fam", a), Utc::now()).await.unwrap();
    insert(&mut conn, &token(&grant_id, TokenKind::Refresh, "fam", b), Utc::now()).await.unwrap();
    insert(&mut conn, &token(&grant_id, TokenKind::Access, "else", other), Utc::now()).await.unwrap();
    drop(conn);

    let mut revoked = revoke_family(pool, "fam", Utc::now()).await.unwrap();
    revoked.sort();
    let again = revoke_family(pool, "fam", Utc::now()).await.unwrap();

    let mut expected = vec![a, b];
    expected.sort();
    assert_eq!(revoked, expected);
    assert!(again.is_empty(), "already revoked");
    assert!(find_access(pool, &a, Utc::now()).await.unwrap().is_none());
    assert!(find_access(pool, &other, Utc::now()).await.unwrap().is_some());
}
```

Run: `cargo test --lib store::oauth::tokens`
Expected: FAIL to compile (`RefreshUse`, `use_refresh`, `revoke_family` not found).

- [ ] **Step 2: Implement `use_refresh` and `revoke_family`**

Add to `src/store/oauth/tokens.rs`:

```rust
/// What presenting a refresh token found ([`use_refresh`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshUse {
    /// Live and unused: now marked used. Mint its successor in the same
    /// transaction.
    Rotated {
        /// Its grant.
        grant_id: String,
        /// Its family, which the successor joins.
        family_id: String,
        /// The resource it was issued for.
        resource: String,
    },
    /// Already used once: a stolen copy, or the client replaying it. The
    /// caller revokes the family.
    Reused {
        /// The family to revoke.
        family_id: String,
    },
    /// Unknown, not a refresh token, expired, revoked, or its grant revoked.
    Unknown,
}

/// Presents a refresh token: locks its row and, if it is live and unused,
/// marks it used.
///
/// Reuse is checked first: a used token is `Reused` even after its family
/// was revoked, so replaying it again stays harmless (revoking twice).
///
/// # Arguments
/// * `tx` - the transaction the successor is minted in (commit it after).
/// * `sha256` - the presented token's hash.
/// * `now` - the server clock.
///
/// # Returns
/// What was found.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn use_refresh(
    tx: &mut PgConnection,
    sha256: &[u8; 32],
    now: DateTime<Utc>,
) -> Result<RefreshUse, OkfError> {
    // `FOR UPDATE OF t` locks only the token row, so two racing refreshes of
    // one token run one after the other: the second sees `used_at` set.
    let row: Option<(String, String, String, Option<DateTime<Utc>>, Option<DateTime<Utc>>, DateTime<Utc>, Option<DateTime<Utc>>)> =
        sqlx::query_as(
            "SELECT t.grant_id, t.family_id, t.resource, t.used_at, t.revoked_at, t.expires_at, \
                    k.revoked_at \
             FROM oauth_tokens t JOIN api_keys k ON k.id = t.grant_id \
             WHERE t.sha256 = $1 AND t.kind = 'refresh' \
             FOR UPDATE OF t",
        )
        .bind(sha256.as_slice())
        .fetch_optional(&mut *tx)
        .await?;
    // `let ... else`: return early when there is no row.
    let Some((grant_id, family_id, resource, used_at, revoked_at, expires_at, grant_revoked_at)) = row
    else {
        return Ok(RefreshUse::Unknown);
    };
    if used_at.is_some() {
        return Ok(RefreshUse::Reused { family_id });
    }
    if revoked_at.is_some() || grant_revoked_at.is_some() || expires_at <= now {
        return Ok(RefreshUse::Unknown);
    }
    sqlx::query("UPDATE oauth_tokens SET used_at = $2 WHERE sha256 = $1")
        .bind(sha256.as_slice())
        .bind(now)
        .execute(&mut *tx)
        .await?;
    Ok(RefreshUse::Rotated { grant_id, family_id, resource })
}

/// Revokes every live token of a family.
///
/// # Arguments
/// * `pool` - the database.
/// * `family_id` - the family.
/// * `now` - the server clock.
///
/// # Returns
/// The hashes it revoked (none if it was already revoked), for
/// `AccessTokens::forget`.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn revoke_family(
    pool: &PgPool,
    family_id: &str,
    now: DateTime<Utc>,
) -> Result<Vec<[u8; 32]>, OkfError> {
    let rows: Vec<Vec<u8>> = sqlx::query_scalar(
        "UPDATE oauth_tokens SET revoked_at = $2 \
         WHERE family_id = $1 AND revoked_at IS NULL RETURNING sha256",
    )
    .bind(family_id)
    .bind(now)
    .fetch_all(pool)
    .await?;
    // The schema checks `length(sha256) = 32`, so `try_into` cannot fail on a
    // row the server wrote; `filter_map` skips a hand-edited one.
    Ok(rows
        .into_iter()
        .filter_map(|hash| <[u8; 32]>::try_from(hash.as_slice()).ok())
        .collect())
}
```

Run: `cargo test --lib store::oauth::tokens`
Expected: `ok`. Task 2's tests plus these 3 pass.

- [ ] **Step 3: Check `CredentialKind::OAuth.client_id`**

Task 2 already defines `CredentialKind::OAuth { client_id: Option<String>, client_name: Option<String> }` and fills both from the `KeyRecord` (its correction 14). The token endpoint reads `client_id` from the grant's principal. Confirm it is there:

Run: `rg -n "client_id: record.client_id" src/auth.rs`
Expected: one match in `impl From<KeyRecord> for Principal`.

- [ ] **Step 4: Write the failing token-endpoint tests**

Create `src/oauth/token.rs` with this test module (the handlers follow in Step 5):

```rust
#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use crate::oauth::testing::{
        authorize_query, form, hidden, oauth_app, send, sign_in_code, CLIENT_ID, REDIRECT_URI,
        RESOURCE, VERIFIER,
    };
    use crate::mcp::testing::TestApp;
    use crate::store::keys::Access;

    /// Runs authorize and consent; returns the `okfac_` code.
    async fn authorization_code(test: &TestApp) -> String {
        let code = sign_in_code(test, Access::Read).await;
        let (_, _, page) =
            send(test, "GET", &format!("/oauth/authorize?{}", authorize_query()), None).await;
        let (pending, csrf) = (hidden(&page, "pending"), hidden(&page, "csrf"));
        send(test, "POST", "/oauth/authorize", Some(form(&[
            ("pending", &pending), ("csrf", &csrf), ("action", "code"), ("sign_in_code", &code),
        ]))).await;
        let (_, headers, _) = send(test, "POST", "/oauth/authorize", Some(form(&[
            ("pending", &pending), ("csrf", &csrf), ("action", "approve"),
        ]))).await;
        let target = url::Url::parse(headers["location"].to_str().unwrap()).unwrap();
        target
            .query_pairs()
            .find(|(key, _)| key == "code")
            .map(|(_, value)| value.into_owned())
            .expect("a code")
    }

    fn exchange(code: &str) -> String {
        form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("client_id", CLIENT_ID),
            ("redirect_uri", REDIRECT_URI),
            ("code_verifier", VERIFIER),
            ("resource", RESOURCE),
        ])
    }

    async fn token(test: &TestApp, body: String) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
        let (status, headers, text) = send(test, "POST", "/oauth/token", Some(body)).await;
        (status, headers, serde_json::from_str(&text).expect("JSON"))
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
        let access = body["access_token"].as_str().unwrap();
        assert!(access.starts_with("okfat_"));
        assert!(body["refresh_token"].as_str().unwrap().starts_with("okfrt_"));
        // The access token authenticates as the grant.
        let principal = test.app.oauth().unwrap().access.verify(&test.app, &test.app.oauth().unwrap().settings.resource, access).await.unwrap();
        assert_eq!(principal.expect("a principal").actor, "example-actor");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_replayed_code_is_refused_and_revokes_what_it_minted() {
        let test = oauth_app().await;
        let code = authorization_code(&test).await;
        let (_, _, first) = token(&test, exchange(&code)).await;
        let access = first["access_token"].as_str().unwrap().to_string();

        let (status, _, body) = token(&test, exchange(&code)).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "invalid_grant");
        let oauth = test.app.oauth().unwrap();
        assert!(oauth.access.verify(&test.app, &test.app.oauth().unwrap().settings.resource, &access).await.unwrap().is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn each_binding_of_the_code_is_checked() {
        for (name, value, expected) in [
            ("code_verifier", "x".repeat(43), "invalid_grant"),
            ("client_id", "another-client".to_string(), "invalid_grant"),
            ("redirect_uri", format!("{REDIRECT_URI}/other"), "invalid_grant"),
            ("resource", "https://other.example.com/mcp".to_string(), "invalid_target"),
        ] {
            let test = oauth_app().await;
            let code = authorization_code(&test).await;
            let body = exchange(&code)
                .split('&')
                .map(|pair| match pair.split_once('=') {
                    Some((key, _)) if key == name => form(&[(name, value.as_str())]),
                    _ => pair.to_string(),
                })
                .collect::<Vec<_>>()
                .join("&");
            let (status, _, json) = token(&test, body).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{name}");
            assert_eq!(json["error"], expected, "{name}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refresh_rotates_and_a_reused_refresh_token_revokes_the_family() {
        let test = oauth_app().await;
        let code = authorization_code(&test).await;
        let (_, _, first) = token(&test, exchange(&code)).await;
        let old_refresh = first["refresh_token"].as_str().unwrap().to_string();
        let refresh = |token: &str| form(&[
            ("grant_type", "refresh_token"), ("refresh_token", token), ("client_id", CLIENT_ID),
        ]);

        let (status, _, second) = token(&test, refresh(&old_refresh)).await;
        assert_eq!(status, StatusCode::OK, "{second}");
        let new_access = second["access_token"].as_str().unwrap().to_string();
        let new_refresh = second["refresh_token"].as_str().unwrap().to_string();
        assert_ne!(new_refresh, old_refresh);

        let (status, _, reused) = token(&test, refresh(&old_refresh)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(reused["error"], "invalid_grant");
        // The whole family is gone: the new pair too.
        let oauth = test.app.oauth().unwrap();
        assert!(oauth.access.verify(&test.app, &test.app.oauth().unwrap().settings.resource, &new_access).await.unwrap().is_none());
        let (status, _, _) = token(&test, refresh(&new_refresh)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refresh_by_another_client_is_refused_without_spending_the_token() {
        let test = oauth_app().await;
        let code = authorization_code(&test).await;
        let (_, _, first) = token(&test, exchange(&code)).await;
        let refresh = first["refresh_token"].as_str().unwrap().to_string();

        let (status, _, body) = token(&test, form(&[
            ("grant_type", "refresh_token"), ("refresh_token", &refresh), ("client_id", "another-client"),
        ])).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "invalid_grant");
        // Rolled back: the rightful client can still use it.
        let (status, _, _) = token(&test, form(&[
            ("grant_type", "refresh_token"), ("refresh_token", &refresh), ("client_id", CLIENT_ID),
        ])).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bad_requests_get_rfc_6749_errors() {
        let test = oauth_app().await;
        for (body, expected) in [
            (form(&[("grant_type", "password")]), "unsupported_grant_type"),
            (form(&[("code", "x")]), "invalid_request"),
            (form(&[("grant_type", "authorization_code"), ("code", "okfac_unknown")]), "invalid_request"),
            (exchange("okfac_unknown"), "invalid_grant"),
            (form(&[("grant_type", "refresh_token"), ("refresh_token", "okfrt_x"), ("client_id", CLIENT_ID)]), "invalid_grant"),
        ] {
            let (status, headers, json) = token(&test, body.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(json["error"], expected, "{body}");
            assert_eq!(headers["cache-control"], "no-store");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revoking_a_refresh_token_revokes_its_family_and_unknown_tokens_are_fine() {
        let test = oauth_app().await;
        let code = authorization_code(&test).await;
        let (_, _, pair) = token(&test, exchange(&code)).await;
        let access = pair["access_token"].as_str().unwrap().to_string();
        let refresh = pair["refresh_token"].as_str().unwrap().to_string();

        let (unknown, _, _) = send(&test, "POST", "/oauth/revoke", Some(form(&[("token", "okfat_nothing")]))).await;
        let (status, _, _) = send(&test, "POST", "/oauth/revoke", Some(form(&[
            ("token", &refresh), ("token_type_hint", "refresh_token"), ("client_id", CLIENT_ID),
        ]))).await;

        assert_eq!(unknown, StatusCode::OK);
        assert_eq!(status, StatusCode::OK);
        let oauth = test.app.oauth().unwrap();
        assert!(oauth.access.verify(&test.app, &test.app.oauth().unwrap().settings.resource, &access).await.unwrap().is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revoking_an_access_token_leaves_the_refresh_token() {
        let test = oauth_app().await;
        let code = authorization_code(&test).await;
        let (_, _, pair) = token(&test, exchange(&code)).await;
        let access = pair["access_token"].as_str().unwrap().to_string();
        let refresh = pair["refresh_token"].as_str().unwrap().to_string();

        send(&test, "POST", "/oauth/revoke", Some(form(&[("token", &access)]))).await;

        let oauth = test.app.oauth().unwrap();
        assert!(oauth.access.verify(&test.app, &test.app.oauth().unwrap().settings.resource, &access).await.unwrap().is_none());
        let (status, _, _) = token(&test, form(&[
            ("grant_type", "refresh_token"), ("refresh_token", &refresh), ("client_id", CLIENT_ID),
        ])).await;
        assert_eq!(status, StatusCode::OK);
    }
}
```

Run: `cargo test --lib oauth::token`
Expected: FAIL to compile (`routes` and the handlers not found).

- [ ] **Step 5: Implement the token and revoke endpoints**

Put this above the tests in `src/oauth/token.rs`:

```rust
//! `POST /oauth/token` and `POST /oauth/revoke` (spec "Endpoints").
//!
//! Tokens are opaque and stored as SHA-256 only. One authorization code
//! starts a token family; each refresh rotates the refresh token within the
//! family. Presenting a spent code or a used refresh token revokes the
//! family (RFC 6749 4.1.2; OAuth 2.1 refresh token rotation).

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
use crate::oauth::state::code_key;
use crate::oauth::{pkce, OAuth};
use crate::store::oauth::tokens::{self as token_rows, LiveAccessToken, NewToken, RefreshUse, TokenKind};

/// The routes: `POST /oauth/token` and `POST /oauth/revoke`.
///
/// # Returns
/// A router for [`crate::oauth::routes`] to merge.
pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/oauth/token", post(token))
        .route("/oauth/revoke", post(revoke))
}

/// A token request's fields (RFC 6749 4.1.3 and 6). All optional, so a
/// missing one is `invalid_request` from the handler, not a 422 from serde.
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
        formatter
            .debug_struct("TokenForm")
            .field("grant_type", &self.grant_type)
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

/// A revocation request (RFC 7009 2.1). `client_id` and `token_type_hint`
/// are accepted and not needed: the token's prefix says its kind.
#[derive(Default, Deserialize)]
struct RevokeForm {
    token: Option<String>,
}

/// A successful token response (RFC 6749 5.1).
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
            .field("expires_in", &self.expires_in)
            .finish_non_exhaustive()
    }
}

impl IntoResponse for TokenResponse {
    fn into_response(self) -> Response {
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

/// A required field, or `invalid_request` naming it.
fn required<'a>(value: &'a Option<String>, name: &str) -> Result<&'a str, OAuthError> {
    value
        .as_deref()
        .filter(|text| !text.is_empty())
        .ok_or_else(|| OAuthError::InvalidRequest(format!("{name} is required")))
}

/// A duration from the configuration as a `chrono` delta.
fn delta(duration: std::time::Duration) -> Result<TimeDelta, OkfError> {
    TimeDelta::from_std(duration).map_err(|_| OkfError::Config {
        path: "[oauth]".to_string(),
        message: "a token lifetime is too long".to_string(),
    })
}

/// A minted pair, before its access token is remembered.
struct Minted {
    response: TokenResponse,
    access_hash: [u8; 32],
    access: LiveAccessToken,
    /// When it was minted, for the cache's pruning.
    now: DateTime<Utc>,
    /// The cache's revocation generation, read before the tokens were
    /// written: a family revocation that commits before `remember` (a
    /// replayed code racing this exchange) keeps the token out of the cache.
    generation: u64,
}

/// Mints an access and a refresh token in `conn`'s transaction.
///
/// # Arguments
/// * `conn` - the transaction (the caller commits, then calls [`remember`]).
/// * `oauth` - the lifetimes.
/// * `grant_id`, `family_id`, `resource` - what the tokens are bound to.
/// * `now` - the server clock.
///
/// # Errors
/// [`OkfError::Io`] if the random source fails; [`OkfError::Database`].
async fn mint(
    conn: &mut PgConnection,
    oauth: &OAuth,
    bound: (&str, &str, &str),
    now: DateTime<Utc>,
) -> Result<Minted, OkfError> {
    let (grant_id, family_id, resource) = bound;
    let generation = oauth.access.generation();
    let access_token = keys::generate_with(keys::ACCESS_TOKEN_PREFIX)?;
    let refresh_token = keys::generate_with(keys::REFRESH_TOKEN_PREFIX)?;
    let access_expires = now + delta(oauth.settings.access_token_ttl)?;
    let refresh_expires = now + delta(oauth.settings.refresh_token_ttl)?;
    let access_hash = keys::hash(&access_token);
    for (sha256, kind, expires_at) in [
        (access_hash, TokenKind::Access, access_expires),
        (keys::hash(&refresh_token), TokenKind::Refresh, refresh_expires),
    ] {
        let new = NewToken { sha256, grant_id, kind, family_id, resource, expires_at };
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
            grant_id: grant_id.to_string(),
            resource: resource.to_string(),
            expires_at: access_expires,
        },
        now,
        generation,
    })
}

/// After commit: caches the new access token and hands back the response.
fn remember(oauth: &OAuth, minted: Minted) -> TokenResponse {
    oauth.access.remember(minted.access_hash, minted.access, minted.now, minted.generation);
    minted.response
}

/// Revokes a family and drops its access tokens from the cache.
async fn revoke_family(app: &App, oauth: &OAuth, family_id: &str) -> Result<(), OkfError> {
    let revoked = token_rows::revoke_family(app.pool(), family_id, app.clock().now()).await?;
    oauth.access.forget(&revoked);
    Ok(())
}

/// The grant's client, if the grant is live and still has one.
fn grant_client(app: &App, grant_id: &str) -> Option<String> {
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
        other => Err(OAuthError::UnsupportedGrantType(format!("{other:?} is not supported"))),
    }
}

/// `grant_type=authorization_code`.
async fn exchange_code(app: &App, oauth: &OAuth, form: &TokenForm) -> Result<TokenResponse, OAuthError> {
    let code = required(&form.code, "code")?;
    let client_id = required(&form.client_id, "client_id")?;
    let redirect_uri = required(&form.redirect_uri, "redirect_uri")?;
    let verifier = required(&form.code_verifier, "code_verifier")?;
    let resource = required(&form.resource, "resource")?;
    // Marking the code used under the map's lock makes the exchange
    // single-use even for two racing requests. A used code stays in the map
    // until it expires, so a replay is recognized and revokes its family.
    let taken = oauth.codes.update(&code_key(code), Instant::now(), |stored| {
        if stored.used {
            Err(stored.family_id.clone())
        } else {
            stored.used = true;
            Ok(stored.clone())
        }
    });
    let stored = match taken {
        None => return Err(OAuthError::InvalidGrant("unknown or expired code".to_string())),
        Some(Err(family_id)) => {
            revoke_family(app, oauth, &family_id).await?;
            return Err(OAuthError::InvalidGrant("the code was already used".to_string()));
        }
        Some(Ok(stored)) => stored,
    };
    if stored.client_id != client_id || stored.redirect_uri != redirect_uri {
        return Err(OAuthError::InvalidGrant("the code was issued to another client or redirect URI".to_string()));
    }
    if stored.resource != resource {
        return Err(OAuthError::InvalidTarget("resource differs from the authorization request".to_string()));
    }
    if !pkce::verify(&stored.code_challenge, verifier) {
        return Err(OAuthError::InvalidGrant("code_verifier does not match".to_string()));
    }
    if grant_client(app, &stored.grant_id).as_deref() != Some(client_id) {
        return Err(OAuthError::InvalidGrant("the grant was revoked".to_string()));
    }
    let now = app.clock().now();
    let mut tx = app.pool().begin().await.map_err(OkfError::from)?;
    let minted = mint(&mut tx, oauth, (&stored.grant_id, &stored.family_id, &stored.resource), now).await?;
    tx.commit().await.map_err(OkfError::from)?;
    Ok(remember(oauth, minted))
}

/// `grant_type=refresh_token`.
async fn refresh(app: &App, oauth: &OAuth, form: &TokenForm) -> Result<TokenResponse, OAuthError> {
    let presented = required(&form.refresh_token, "refresh_token")?;
    let client_id = required(&form.client_id, "client_id")?;
    let now = app.clock().now();
    let mut tx = app.pool().begin().await.map_err(OkfError::from)?;
    match token_rows::use_refresh(&mut tx, &keys::hash(presented), now).await? {
        RefreshUse::Unknown => Err(OAuthError::InvalidGrant("unknown, expired or revoked refresh token".to_string())),
        RefreshUse::Reused { family_id } => {
            // Nothing was written; dropping `tx` rolls back, then revoke.
            drop(tx);
            revoke_family(app, oauth, &family_id).await?;
            Err(OAuthError::InvalidGrant("the refresh token was already used".to_string()))
        }
        RefreshUse::Rotated { grant_id, family_id, resource } => {
            // Each refusal returns before `commit`, so dropping `tx` rolls
            // back the `used_at` mark and the rightful client can retry.
            if grant_client(app, &grant_id).as_deref() != Some(client_id) {
                return Err(OAuthError::InvalidGrant("the refresh token belongs to another client".to_string()));
            }
            if form.resource.as_deref().is_some_and(|asked| asked != resource) {
                return Err(OAuthError::InvalidTarget("resource differs from the original grant".to_string()));
            }
            let minted = mint(&mut tx, oauth, (&grant_id, &family_id, &resource), now).await?;
            tx.commit().await.map_err(OkfError::from)?;
            Ok(remember(oauth, minted))
        }
    }
}

/// `POST /oauth/revoke` (RFC 7009): always 200, an unknown token included.
async fn revoke(
    State(app): State<Arc<App>>,
    form: Result<Form<RevokeForm>, FormRejection>,
) -> Result<Response, OAuthError> {
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
    let family = token_rows::revoke(app.pool(), &hash, app.clock().now()).await?;
    oauth.access.forget(&[hash]);
    // A refresh token takes its family with it (spec "Endpoints").
    // The stored kind says which was presented; revoking twice is harmless.
    if let Some(token_rows::RevokedToken { family_id, kind: TokenKind::Refresh }) = family {
        revoke_family(&app, oauth, &family_id).await?;
    }
    Ok(([(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))], StatusCode::OK).into_response())
}
```

> `OAuthError` must convert from `OkfError` (Task 2's `Server(#[from] OkfError)`), so `?` on a store call works in these handlers. `map_err(OkfError::from)` turns a raw `sqlx::Error` into `OkfError` first. This relies on `src/error.rs`'s `From<sqlx::Error> for OkfError`, which already exists (it maps deadlocks to `Busy`).

Run: `cargo test --lib oauth::token`
Expected: `test result: ok. 8 passed`.

- [ ] **Step 6: The whole library**

Run: `cargo test --lib && cargo clippy --all-targets -- -D warnings`
Expected: PASS with no warnings.

- [ ] **Step 7: `TestServer::start_with` (support change)**

In `tests/support/server.rs`, split the catalog setup out of `start`, add `start_with`, and make `serve_app` bind first:

```rust
impl TestServer {
    /// Creates and imports each catalog, then starts the server.
    /// (Doc unchanged.)
    pub async fn start(catalogs_and_files: Vec<(&str, Files)>, config: Config) -> Self {
        Self::start_with(catalogs_and_files, move |_| config).await
    }

    /// As [`TestServer::start`], with settings computed from the server's
    /// own base URL, which is known only once its port is bound: OAuth's
    /// `public_url` must name it.
    ///
    /// # Arguments
    /// * `catalogs_and_files` - as for `start`.
    /// * `make_config` - builds the settings from `http://127.0.0.1:<port>`.
    ///
    /// # Returns
    /// The running server.
    ///
    /// # Panics
    /// As [`TestServer::start`].
    pub async fn start_with(
        catalogs_and_files: Vec<(&str, Files)>,
        make_config: impl FnOnce(&str) -> Config,
    ) -> Self {
        let db = TestDb::start().await;
        okf_mcp::db::migrate(&db.pool).await.expect("migrations");
        for (name, files) in catalogs_and_files {
            catalogs::create(&db.pool, name, "", Utc::now())
                .await
                .expect("create catalog");
            let options = ImportOptions {
                replace: false,
                scope: ImportScope::Everything,
                expect_generation: None,
                actor: "tester",
                now: Utc::now(),
            };
            import(&db.pool, name, files, &options).await.expect("import");
        }
        let (base, app, shutdown, served) = serve_app(&db, make_config).await;
        Self { base, app, shutdown, served, db }
    }
}
```

`restart` changes its one call to `serve_app(&self.db, move |_| config).await`. The new `serve_app`:

```rust
/// Binds a free port, loads the app on `db` with settings built from the
/// port's URL, and serves it.
///
/// # Arguments
/// * `db` - the database, already migrated and imported.
/// * `make_config` - builds the settings from the base URL.
///
/// # Returns
/// The base URL, the app, the token that stops the server, and the task
/// serving it.
///
/// # Panics
/// If the bind fails or the app does not load.
async fn serve_app(
    db: &TestDb,
    make_config: impl FnOnce(&str) -> Config,
) -> (String, Arc<App>, CancellationToken, Served) {
    // Port 0: the OS picks a free one. Bound first, so the settings can
    // name the server's own URL.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    let app = App::load(db.pool.clone(), make_config(&base), Arc::new(SystemClock))
        .await
        .expect("app loads");
    let shutdown = CancellationToken::new();
    let served = tokio::spawn(okf_mcp::http::serve(
        Arc::clone(&app),
        listener,
        shutdown.clone(),
    ));
    (base, app, shutdown, served)
}
```

Note on `restart`: the port changes on a restart, as it did before. A `start_with` server's `public_url` would then be stale after a restart. No OAuth test restarts, so `restart` keeps taking a plain `Config`.

Run: `cargo test --test server`
Expected: PASS, unchanged: every existing end-to-end test still starts through `start`.

- [ ] **Step 8: The OAuth test client and CIMD host**

Add `pub mod oauth;` to `tests/support/mod.rs`, and create `tests/support/oauth.rs`:

```rust
//! The client side of OAuth for end-to-end tests: a server with OAuth on at
//! its own URL, a local Client ID Metadata Document host, and a client that
//! walks discovery, consent (as form POSTs, no browser) and the token
//! endpoint the way ChatGPT or Claude would.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::{TimeDelta, Utc};
use okf_mcp::archive::Files;
use okf_mcp::config::{Config, OAuthConfig};
use okf_mcp::store::keys::{Access, Grants};
use okf_mcp::store::oauth::codes::{self as code_rows, NewSignInCode};
use sha2::{Digest, Sha256};

use super::server::TestServer;

/// A server with OAuth on at its own base URL, allowing loopback CIMD hosts.
pub async fn start_oauth_server(catalogs: Vec<(&str, Files)>) -> TestServer {
    TestServer::start_with(catalogs, |base| Config {
        public_url: Some(base.to_string()),
        oauth: OAuthConfig {
            cimd_allow_private: true,
            ..OAuthConfig::default()
        },
        ..Config::default()
    })
    .await
}

/// A local host serving a client metadata document at `/client.json`, whose
/// `client_id` is that URL and whose redirect URI is `/callback` on it.
pub struct CimdHost {
    /// `http://127.0.0.1:<port>/client.json`.
    pub client_id: String,
    /// `http://127.0.0.1:<port>/callback`.
    pub redirect_uri: String,
}

impl CimdHost {
    /// Starts the host (it runs until the test's runtime ends).
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("address"));
        let client_id = format!("{base}/client.json");
        let redirect_uri = format!("{base}/callback");
        let document = serde_json::json!({
            "client_id": client_id,
            "client_name": "Example CIMD Client",
            "redirect_uris": [redirect_uri],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
        });
        let router = axum::Router::new().route(
            "/client.json",
            axum::routing::get(move || {
                let document = document.clone();
                async move { axum::Json(document) }
            }),
        );
        tokio::spawn(async move { axum::serve(listener, router).await });
        Self { client_id, redirect_uri }
    }
}

/// A live sign-in code: `access` on `catalogs`, actor `example-actor`.
pub async fn sign_in_code(server: &TestServer, access: Access, catalogs: &[&str]) -> String {
    let names: Vec<String> = catalogs.iter().map(|name| name.to_string()).collect();
    code_rows::create(
        &server.db.pool,
        &NewSignInCode {
            name: "e2e",
            access,
            grants: Grants::Catalogs(&names),
            default_catalog: catalogs.first().copied(),
            actor: "example-actor",
            expires_at: Utc::now() + TimeDelta::hours(1),
        },
        Utc::now(),
    )
    .await
    .expect("create a sign-in code")
    .code
}

/// `application/x-www-form-urlencoded` text of `pairs`.
pub fn form(pairs: &[(&str, &str)]) -> String {
    form_urlencoded::Serializer::new(String::new()).extend_pairs(pairs).finish()
}

/// What discovery found.
#[derive(Debug)]
pub struct Discovered {
    /// The resource the protected resource metadata names.
    pub resource: String,
    /// The authorization server metadata.
    pub metadata: serde_json::Value,
}

/// A token endpoint answer.
#[derive(Debug)]
pub struct TokenAnswer {
    /// The status.
    pub status: reqwest::StatusCode,
    /// The JSON body.
    pub body: serde_json::Value,
}

impl TokenAnswer {
    /// The access token of a successful answer.
    pub fn access(&self) -> String {
        self.body["access_token"].as_str().expect("access_token").to_string()
    }
    /// The refresh token of a successful answer.
    pub fn refresh(&self) -> String {
        self.body["refresh_token"].as_str().expect("refresh_token").to_string()
    }
}

/// One OAuth client of the server, with its PKCE pair.
pub struct OAuthClient {
    http: reqwest::Client,
    base: String,
    /// Its `client_id`.
    pub client_id: String,
    /// Its redirect URI.
    pub redirect_uri: String,
    verifier: String,
    challenge: String,
}

impl OAuthClient {
    /// A client of `server` with `client_id` and `redirect_uri`.
    pub fn new(server: &TestServer, client_id: &str, redirect_uri: &str) -> Self {
        // 43 random base64url characters: a valid PKCE verifier.
        let verifier = okf_mcp::keys::generate_with("").expect("random verifier");
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("HTTP client");
        Self {
            http,
            base: server.base.clone(),
            client_id: client_id.to_string(),
            redirect_uri: redirect_uri.to_string(),
            verifier,
            challenge,
        }
    }

    /// Registers a new client by DCR and returns a client for it.
    pub async fn register(server: &TestServer, redirect_uri: &str) -> Self {
        let response = reqwest::Client::new()
            .post(format!("{}/oauth/register", server.base))
            .json(&serde_json::json!({
                "client_name": "Example DCR Client",
                "redirect_uris": [redirect_uri],
                "token_endpoint_auth_method": "none",
            }))
            .send()
            .await
            .expect("register");
        assert_eq!(response.status(), 201);
        let body: serde_json::Value = response.json().await.expect("JSON");
        Self::new(server, body["client_id"].as_str().expect("client_id"), redirect_uri)
    }

    /// Discovery as an MCP client does it: an unauthenticated `/mcp` request,
    /// the `resource_metadata` URL of its 401, that document, then the
    /// authorization server's metadata.
    pub async fn discover(&self) -> Discovered {
        let unauthorized = self.http.post(format!("{}/mcp", self.base)).send().await.expect("POST /mcp");
        assert_eq!(unauthorized.status(), 401);
        let challenge = unauthorized.headers()["www-authenticate"].to_str().expect("header").to_string();
        let url = challenge
            .split("resource_metadata=\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("resource_metadata in WWW-Authenticate")
            .to_string();
        let prm: serde_json::Value = self.http.get(url).send().await.expect("PRM").json().await.expect("JSON");
        let issuer = prm["authorization_servers"][0].as_str().expect("an authorization server");
        let metadata: serde_json::Value = self
            .http
            .get(format!("{issuer}/.well-known/oauth-authorization-server"))
            .send()
            .await
            .expect("AS metadata")
            .json()
            .await
            .expect("JSON");
        Discovered {
            resource: prm["resource"].as_str().expect("resource").to_string(),
            metadata,
        }
    }

    /// Authorize and consent with `sign_in_code`; returns the `okfac_` code.
    ///
    /// # Panics
    /// If any step answers something other than the happy path.
    pub async fn authorize(&self, sign_in_code: &str, resource: &str) -> String {
        let query = form(&[
            ("response_type", "code"),
            ("client_id", &self.client_id),
            ("redirect_uri", &self.redirect_uri),
            ("state", "e2e-state"),
            ("code_challenge", &self.challenge),
            ("code_challenge_method", "S256"),
            ("resource", resource),
        ]);
        let page = self
            .http
            .get(format!("{}/oauth/authorize?{query}", self.base))
            .send()
            .await
            .expect("authorize");
        assert_eq!(page.status(), 200);
        let page = page.text().await.expect("page");
        let pending = hidden(&page, "pending");
        let csrf = hidden(&page, "csrf");
        let summary = self
            .consent(&[("pending", &pending), ("csrf", &csrf), ("action", "code"), ("sign_in_code", sign_in_code)])
            .await;
        assert_eq!(summary.status(), 200);
        let approved = self
            .consent(&[("pending", &pending), ("csrf", &csrf), ("action", "approve")])
            .await;
        assert_eq!(approved.status(), 303);
        let location = url::Url::parse(approved.headers()["location"].to_str().expect("location")).expect("URL");
        assert!(location.as_str().starts_with(&self.redirect_uri));
        let param = |name: &str| {
            location
                .query_pairs()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
        };
        assert_eq!(param("state").as_deref(), Some("e2e-state"));
        assert_eq!(param("iss").as_deref(), Some(self.base.as_str()));
        param("code").expect("a code")
    }

    async fn consent(&self, pairs: &[(&str, &str)]) -> reqwest::Response {
        self.http
            .post(format!("{}/oauth/authorize", self.base))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(form(pairs))
            .send()
            .await
            .expect("consent POST")
    }

    async fn token(&self, pairs: &[(&str, &str)]) -> TokenAnswer {
        let response = self
            .http
            .post(format!("{}/oauth/token", self.base))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(form(pairs))
            .send()
            .await
            .expect("token POST");
        let status = response.status();
        TokenAnswer { status, body: response.json().await.expect("JSON") }
    }

    /// Exchanges an authorization code.
    pub async fn exchange(&self, code: &str, resource: &str) -> TokenAnswer {
        self.token(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("client_id", &self.client_id),
            ("redirect_uri", &self.redirect_uri),
            ("code_verifier", &self.verifier),
            ("resource", resource),
        ])
        .await
    }

    /// Refreshes.
    pub async fn refresh(&self, refresh_token: &str) -> TokenAnswer {
        self.token(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", &self.client_id),
        ])
        .await
    }
}

/// The `value` of the hidden input `name` in a consent page.
fn hidden(page: &str, name: &str) -> String {
    let marker = format!("name=\"{name}\" value=\"");
    let start = page.find(&marker).expect("hidden input") + marker.len();
    let end = start + page[start..].find('"').expect("closing quote");
    page[start..end].to_string()
}
```

Run: `cargo test --test server --no-run`
Expected: compiles. `support` is shared, and `#![allow(dead_code)]` in `server.rs` covers only that file, so add `#![allow(dead_code)]` at the top of `oauth.rs` if a test crate that does not use it warns.

- [ ] **Step 9: The end-to-end tests**

Create `tests/server_oauth.rs`:

```rust
//! End-to-end OAuth (spec "Testing"): discovery, consent, tokens and an MCP
//! tool call with an OAuth access token, over real HTTP with a real rmcp
//! client, beside a static key on the same server.

mod support;

use chrono::{TimeDelta, Utc};
use okf_mcp::keys::{self, ACCESS_TOKEN_PREFIX};
use okf_mcp::store::keys::{self as key_rows, Access};
use okf_mcp::store::oauth::tokens::{self as token_rows, NewToken, TokenKind};
use support::oauth::{sign_in_code, start_oauth_server, CimdHost, OAuthClient};
use support::server::{call, files, structured, TestServer};

async fn server() -> TestServer {
    start_oauth_server(vec![
        ("main", files(&[("index.md", "# Main\n")])),
        ("other", files(&[("index.md", "# Other\n")])),
    ])
    .await
}

/// The catalogs a token's connection reaches, by `list_catalogs`.
async fn catalogs_of(server: &TestServer, token: &str) -> Vec<String> {
    let client = server.client(token, None).await.expect("the token connects");
    let result = call(&client, "list_catalogs", serde_json::json!({})).await;
    structured(&result)["catalogs"]
        .as_array()
        .expect("catalogs")
        .iter()
        .map(|catalog| catalog["name"].as_str().expect("name").to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cimd_client_discovers_consents_and_calls_a_tool() {
    let server = server().await;
    let host = CimdHost::start().await;
    let client = OAuthClient::new(&server, &host.client_id, &host.redirect_uri);

    let found = client.discover().await;
    assert_eq!(found.resource, format!("{}/mcp", server.base));
    assert_eq!(found.metadata["issuer"], server.base.as_str());
    assert_eq!(found.metadata["client_id_metadata_document_supported"], true);
    assert_eq!(found.metadata["code_challenge_methods_supported"][0], "S256");

    let code = client.authorize(&sign_in_code(&server, Access::Read, &["main"]).await, &found.resource).await;
    let tokens = client.exchange(&code, &found.resource).await;
    assert_eq!(tokens.status, 200, "{}", tokens.body);

    assert_eq!(catalogs_of(&server, &tokens.access()).await, vec!["main".to_string()]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dcr_client_does_the_same() {
    let server = server().await;
    let client = OAuthClient::register(&server, "http://127.0.0.1:9/callback").await;
    let resource = client.discover().await.resource;

    let code = client.authorize(&sign_in_code(&server, Access::Write, &["main", "other"]).await, &resource).await;
    let tokens = client.exchange(&code, &resource).await;

    assert_eq!(tokens.status, 200, "{}", tokens.body);
    let mut reached = catalogs_of(&server, &tokens.access()).await;
    reached.sort();
    assert_eq!(reached, vec!["main".to_string(), "other".to_string()]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_token_for_another_resource_is_refused() {
    let server = server().await;
    let client = OAuthClient::register(&server, "http://127.0.0.1:9/callback").await;
    let resource = client.discover().await.resource;
    let code = client.authorize(&sign_in_code(&server, Access::Read, &["main"]).await, &resource).await;
    let tokens = client.exchange(&code, &resource).await;
    let grant_id = server
        .app
        .oauth()
        .expect("oauth on")
        .access
        .verify(&server.app, &tokens.access())
        .await
        .expect("verify")
        .expect("a principal")
        .key_id
        .clone();
    // A token of the same grant, but issued for another resource: what a
    // token stolen from another MCP server would look like here.
    let foreign = keys::generate_with(ACCESS_TOKEN_PREFIX).expect("token");
    let mut conn = server.db.pool.acquire().await.expect("connection");
    token_rows::insert(
        &mut conn,
        &NewToken {
            sha256: keys::hash(&foreign),
            grant_id: &grant_id,
            kind: TokenKind::Access,
            family_id: "foreign",
            resource: "https://other.example.com/mcp",
            expires_at: Utc::now() + TimeDelta::hours(1),
        },
        Utc::now(),
    )
    .await
    .expect("insert");
    drop(conn);

    assert!(server.client(&foreign, None).await.is_err(), "wrong audience");
    assert!(server.client(&tokens.access(), None).await.is_ok(), "right audience");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refresh_works_and_reusing_the_old_refresh_token_revokes_the_family() {
    let server = server().await;
    let client = OAuthClient::register(&server, "http://127.0.0.1:9/callback").await;
    let resource = client.discover().await.resource;
    let code = client.authorize(&sign_in_code(&server, Access::Read, &["main"]).await, &resource).await;
    let first = client.exchange(&code, &resource).await;

    let second = client.refresh(&first.refresh()).await;
    assert_eq!(second.status, 200, "{}", second.body);
    assert_eq!(catalogs_of(&server, &second.access()).await, vec!["main".to_string()]);

    let replay = client.refresh(&first.refresh()).await;
    assert_eq!(replay.status, 400);
    assert_eq!(replay.body["error"], "invalid_grant");
    assert!(server.client(&second.access(), None).await.is_err(), "family revoked");
    assert_eq!(client.refresh(&second.refresh()).await.status, 400);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_static_key_still_works_and_its_401_names_the_metadata() {
    let server = server().await;
    let key = server.key("example-key", Access::Read, &["main"], Some("main")).await;

    assert_eq!(catalogs_of(&server, &key).await, vec!["main".to_string()]);
    let refused = reqwest::Client::new()
        .post(server.mcp_url())
        .bearer_auth("okf_not-a-key")
        .send()
        .await
        .expect("POST /mcp");
    assert_eq!(refused.status(), 401);
    assert_eq!(
        refused.headers()["www-authenticate"],
        format!(
            "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource\"",
            server.base
        )
        .as_str()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revoked_grant_stops_its_tokens_at_the_next_poll() {
    let server = server().await;
    let client = OAuthClient::register(&server, "http://127.0.0.1:9/callback").await;
    let resource = client.discover().await.resource;
    let code = client.authorize(&sign_in_code(&server, Access::Read, &["main"]).await, &resource).await;
    let tokens = client.exchange(&code, &resource).await;
    let grant_id = server
        .app
        .oauth()
        .expect("oauth on")
        .access
        .verify(&server.app, &tokens.access())
        .await
        .expect("verify")
        .expect("a principal")
        .key_id
        .clone();

    key_rows::revoke_by_id(&server.db.pool, &grant_id, Utc::now()).await.expect("revoke");
    server.app.poll().await.expect("poll");

    assert!(server.client(&tokens.access(), None).await.is_err());
    assert_eq!(client.refresh(&tokens.refresh()).await.status, 400);
}
```

Notes for the implementer:
- `http://127.0.0.1:9/callback` is never fetched. The consent POST's 303 is only read, not followed, so the port is irrelevant. Task 3's `validate_redirect_uri` accepts `http` on a loopback host for registration.
- `list_catalogs`' result shape (`catalogs[].name`) is that of `src/mcp/catalogs.rs`. Check the field name there and adjust `catalogs_of` if it differs.

Run: `cargo test --test server_oauth -- --test-threads=4`
Expected: `test result: ok. 6 passed`.

- [ ] **Step 10: Gates**

Run:
```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
cargo test -- --test-threads=6
```
Expected: every command succeeds. Paste the test counts into the PR.

- [ ] **Step 11: Changelog and plan corrections**

Create `changelog/$(date -u +%Y-%m-%dT%H%M%SZ)-m10-task-06-token-endpoint.md`:

```markdown
# M10 Task 6: token, refresh and revoke

## Added
- `POST /oauth/token`:
  - `authorization_code` checks the client, the redirect URI, `resource` and PKCE S256, and mints a 1-hour `okfat_` access token and a 30-day `okfrt_` refresh token;
  - `refresh_token` rotates the refresh token.
  - A replayed code or a reused refresh token revokes the whole token family.
- `POST /oauth/revoke` (RFC 7009). Revoking a refresh token revokes its family. The answer is always 200.
- End-to-end OAuth tests (`tests/server_oauth.rs`):
  - CIMD and DCR flows through to an MCP tool call with a real rmcp client;
  - the audience check;
  - refresh-token reuse detection;
  - grant revocation;
  - a static key beside OAuth.

## Changed
- `CredentialKind::OAuth` carries the grant's `client_id`.
- `tests/support/server.rs`: `TestServer::start_with` builds the settings from the bound URL. `serve_app` binds before loading.

## Notes
- The revocation endpoint does not check that the token belongs to the requesting client: clients are public, and holding a token already allows everything revoking it takes away.
```

In the plan, apply this task's corrections to "Interfaces shared across tasks":
- `CredentialKind::OAuth.client_id`;
- `tests/support/oauth.rs`'s real API, replacing the sketch: `start_oauth_server`, `CimdHost`, `OAuthClient`, `sign_in_code`;
- the Task 3 note on loopback `http` CIMD under `cimd_allow_private`.

Add the revocation decision to "Decisions taken while planning".

- [ ] **Step 12: Commit**

```bash
git add src/store/oauth/tokens.rs src/oauth/token.rs src/auth.rs \
        tests/support/server.rs tests/support/mod.rs tests/support/oauth.rs tests/server_oauth.rs \
        docs/superpowers/plans/2026-11-11-okf-mcp-m10-oauth.md changelog/
git commit -m "feat(oauth): token, refresh and revoke endpoints

Authorization-code exchange with PKCE, client, redirect URI and resource
checks; rotating refresh tokens with reuse detection that revokes the
token family; RFC 7009 revocation; end-to-end tests from discovery to an
MCP tool call with a real rmcp client.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: The console's sign-in codes panel and grant display

Depends on Task 4 (the `/api/v1/oauth/codes` endpoints) and Task 2 (`KeyView.kind`, `client_name`). Parallel with Tasks 5 and 6.

> **Corrected (issue #251, 2026-11-11).** What shipped (`src/http/ui/js/pages/keys.js`) differs from Step 2 in these ways; the API is as Task 4 shipped it, so no request or response changed:
> - Every helper takes the page's `main` (`newKey(main, reload)`, `showKey(main, key, reload)`, `codesPanel(main)`, `newCode(main, reload)`, `showCode(main, code, reload)`), and each checks `main.isConnected` after an await before it touches the drawer, reloads or redraws (the M9 router convention). The one-time dialog is still shown after navigating away: it is the code's only appearance.
> - So the revoke handlers (key and code) toast first, then return if `main` is detached, and only then `closeDrawer()` and reload; the create handlers close the drawer only while `main` is connected.
> - `showOnce` holds the secret in a local it sets to `null` when "I have stored it" is clicked, so not even the copy handler keeps it after the dialog closes.
> - The codes table's empty text is "No sign-in codes to show." (it also shows with every state ticked).
> - The first journey also asserts the code is in neither `localStorage` nor `sessionStorage` after the dialog closes.
> - The first journey (PR #256 review) first submits the form with no catalog: it waits for the 409 toast ("must be granted a catalog"), asserts no `#code-secret-dialog` and that name and actor are still filled, then ticks `main` and retries. With the dialog open it presses Escape (`ui.page.press_key(None, "Escape")`) and asserts the dialog is still open.

**Facts this task relies on** (verified at `c24fef1`; Task 2 and Task 4 additions from their sections):
- `src/http/ui/js/pages/keys.js`:
  - It renders the keys table (`#keys-table`, rows `[data-test=key-<name>]`), the `#key-new` drawer form (`#key-form`), the one-time secret dialog (`#key-secret`, `#key-secret-copy`, `#key-secret-done`), and the per-key drawer (`#key-grant`, `#key-report-toggle`, `#key-revoke`).
  - It imports `h, pill` (`core/dom.js`), `api, enc` (`core/api.js`), `store` (`core/store.js`), `toast`, `openDrawer, closeDrawer`, `when` (`core/format.js`), `listTable`, `confirmDialog`.
- `listTable(columns, rows, { onRow, empty })` returns a `<table>` with `redraw(rows?)` and `setFilter(fn|null)` (`src/http/ui/js/components/list-table.js:4-38`).
- `confirmDialog({ title, message, action })` resolves `true` on `#confirm-ok` (`components/confirm.js:5-26`).
- `openDrawer(title, subtitle, ...content)` fills `#drawer`, whose title is `#drawer-title` (`core/drawer.js:4-17`).
- `h()` skips `null`, `false` and `undefined` children and flattens arrays (`core/dom.js:7-20`).
- `store.whoami` has `all_catalogs` and `catalogs`; `store.can('admin')` gates the page.
- After Task 2, `GET /api/v1/keys` rows carry `kind` (`"key"` or `"oauth"`) and `client_name` (string or `null`).
- After Task 4, `GET /api/v1/oauth/codes` answers `{codes: [SignInCodeView]}`, where `state` is one of `live`, `expired`, `redeemed`, `revoked`. `POST` answers `{code, secret}`, and `DELETE /api/v1/oauth/codes/{id}` answers the revoked view.
- `styles.css` has the "API keys (Task 2)" block (138-150): `.key-name`, `dl.facts`, `pre.secret`, `.warn-text`. `.page-head` is at 83-85.
- Browser harness (`tests/ui_e2e/harness.rs`):
  - `drive(base, journey) -> bool`; `false` when skipped;
  - `Ui::{login, open, wait_visible, wait_hidden, wait_text, text, click, fill, eval}`. `wait_hidden` also accepts an absent element (97-100);
  - `assert_no_console_errors` runs after every journey, inside `drive`.
- `tests/ui_e2e/keys.rs` has `server()` (one `main` catalog) and three journeys. `TestServer` has public `app` and `db` (`tests/support/server.rs:62-71`).
- CSS: an attribute value holding `.` must be quoted (`[data-test="key-a.b"]`). Grant names contain dots (Task 4, Decision 1).

**Decisions (this task):**
1. **One page.** Sign-in codes are a second section of the API keys page (`#codes-panel`), not a page of their own. They are the same admin task, and a new page would need a nav entry and its own manifest file.
2. **The one-time dialog is shared.** `showSecret` becomes `showOnce({ prefix, ... })`, which builds ids from the prefix: `key-secret*` as before, and `code-secret*` for codes. The existing journeys keep their selectors.
3. **The catalog fields are shared.** The key and code forms use one `catalogFields()` and one `readGrants(data)`.
4. **An OAuth grant's drawer has no reporting toggle.** The store refuses it (Task 4, Decision 5). Grant and ungrant stay, so an admin can narrow a grant.
5. **The codes list shows live codes by default.** A "Show redeemed, expired and revoked codes" checkbox (`#codes-show-all`) shows the rest, the way `#keys-show-revoked` works.

**Files:**
- Modify: `src/http/ui/js/pages/keys.js`
- Modify: `src/http/ui/styles.css` (one rule appended to the API keys block)
- Modify: `tests/ui_e2e/keys.rs` (two journeys)
- Modify: `docs/operations.md` (one paragraph in "OAuth sign-in codes")
- Create: `changelog/<ts>-m10-task-07-console-sign-in-codes.md`

- [ ] **Step 1: Write the failing browser journeys**

Append to `tests/ui_e2e/keys.rs`, and extend its `use` lines:

```rust
use chrono::{TimeDelta, Utc};
use okf_mcp::store::keys::{self as key_rows, Grants};
use okf_mcp::store::oauth::clients;
use okf_mcp::store::oauth::codes::{self, NewSignInCode};
```

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admin_mints_a_sign_in_code_sees_it_once_and_revokes_it() {
    let server = server().await;
    let admin = server
        .key("console-admin", Access::Admin, &["main"], Some("main"))
        .await;
    let ran = drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.open("/keys");
        ui.wait_visible("#code-new");
        ui.click("#code-new");
        ui.fill("#code-form input[name=name]", "console-code");
        ui.fill("#code-form input[name=actor]", "example-actor");
        ui.click("#code-form input[name=catalog][value=main]");
        ui.click("#code-form button[type=submit]");
        ui.wait_visible("#code-secret");
        let secret = ui.text("#code-secret");
        assert!(secret.starts_with("okfsc_"), "a sign-in code, not {secret:?}");
        ui.click("#code-secret-done");
        ui.wait_hidden("#code-secret");
        ui.wait_text("[data-test=code-state-console-code]", "live");
        // Once dismissed, the code is nowhere on the page.
        let page = ui.eval("() => document.body.textContent", serde_json::json!(null));
        assert!(!page.as_str().unwrap_or_default().contains(&secret));
        ui.click("[data-test=code-console-code]");
        ui.wait_visible("#code-revoke");
        ui.click("#code-revoke");
        ui.click("#confirm-ok");
        // A revoked code leaves the default (live-only) list...
        ui.wait_hidden("[data-test=code-console-code]");
        // ...and shows again, revoked, with every state shown.
        ui.click("#codes-show-all");
        ui.wait_text("[data-test=code-state-console-code]", "revoked");
    })
    .await;
    if !ran {
        return;
    }
    let minted = codes::list(&server.db.pool).await.expect("list codes");
    assert_eq!(minted.len(), 1);
    assert_eq!(minted[0].name, "console-code");
    assert_eq!(minted[0].actor, "example-actor");
    assert!(minted[0].revoked_at.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_oauth_grant_is_marked_and_cannot_be_given_reporting() {
    let server = server().await;
    let admin = server
        .key("console-admin", Access::Admin, &["main"], Some("main"))
        .await;
    // A client and a grant made the way the consent page makes them (Task 3
    // registers clients; a raw row keeps this journey independent of it).
    let client_id = "https://client.example.com/client.json";
    // Corrected (issue #248, 2026-11-11): a CIMD row needs `fetched_at` and
    // `cache_expires_at` (Task 2's `oauth_clients_fetched_cimd` check).
    sqlx::query(
        "INSERT INTO oauth_clients \
         (client_id, kind, client_name, redirect_uris, metadata, fetched_at, cache_expires_at, \
          created_at) \
         VALUES ($1, 'cimd', 'Example Client', ARRAY['https://client.example.com/callback'], \
                 '{}'::jsonb, now(), now() + interval '1 hour', now())",
    )
    .bind(client_id)
    .execute(&server.db.pool)
    .await
    .expect("insert a client");
    let client = clients::find(&server.db.pool, client_id)
        .await
        .expect("read the client")
        .expect("the client exists");
    let main = ["main".to_string()];
    let code = codes::create(
        &server.db.pool,
        &NewSignInCode {
            name: "laptop",
            access: Access::Write,
            grants: Grants::Catalogs(&main),
            default_catalog: Some("main"),
            actor: "example-actor",
            expires_at: Utc::now() + TimeDelta::hours(1),
        },
        Utc::now(),
    )
    .await
    .expect("mint a code");
    let grant_id = codes::redeem(&server.db.pool, &code.id, &client, Utc::now())
        .await
        .expect("redeem the code");
    server.app.poll().await.expect("poll the grant in");
    let name = key_rows::list(&server.db.pool)
        .await
        .expect("list keys")
        .into_iter()
        .find(|key| key.id == grant_id)
        .expect("the grant")
        .name;
    // Quoted: a grant's name has dots (Task 4, Decision 1).
    let row = format!("[data-test=\"key-{name}\"]");
    drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.open("/keys");
        let text = ui.wait_text(&row, "OAuth");
        assert!(text.contains("Example Client"), "the client is named: {text:?}");
        ui.click(&row);
        ui.wait_text("#drawer", "OAuth grant");
        ui.wait_visible("#key-revoke");
        assert!(!ui.page.is_visible("#key-report-toggle").expect("visible"));
    })
    .await;
}
```

Run: `just test-ui` (Chromium available)
Expected: both new journeys FAIL, with "timed out waiting for #code-new" and "timed out waiting for [data-test="key-..."] containing "OAuth"". The three existing journeys pass.

- [ ] **Step 2: The page**

Replace `src/http/ui/js/pages/keys.js` with:

```js
// API keys (admin): the keys this admin manages, a form to create one (its
// secret shown once), and per-key grant, ungrant, reporting and revoke.
// Below them, OAuth sign-in codes: mint one (shown once), list, revoke.
// OAuth grants (the keys codes become) are marked in the key list.
// The server enforces every rule; this page only hides what cannot work.
import { h, pill } from '../core/dom.js';
import { api, enc } from '../core/api.js';
import { store } from '../core/store.js';
import { toast } from '../core/toast.js';
import { openDrawer, closeDrawer } from '../core/drawer.js';
import { when } from '../core/format.js';
import { listTable } from '../components/list-table.js';
import { confirmDialog } from '../components/confirm.js';

const LEVELS = ['read', 'write', 'admin'];
const CODE_LEVELS = ['read', 'write'];
const ACCESS_KIND = { read: 'muted', write: 'info', admin: 'warn' };
const CODE_STATE_KIND = { live: 'ok', expired: 'muted', redeemed: 'info', revoked: 'error' };
// humantime spellings the server parses; the default is a day.
const EXPIRIES = [['1h', '1 hour'], ['24h', '24 hours'], ['3d', '3 days'], ['7d', '7 days']];

// Keys and codes both carry all_catalogs and catalogs.
function catalogsOf(grantee) {
    if (grantee.all_catalogs) return pill('all', 'info');
    return grantee.catalogs.length ? grantee.catalogs.join(', ') : '-';
}

function isMine(key) {
    return key.id === store.whoami?.key_id;
}

function isGrant(key) {
    return key.kind === 'oauth';
}

const COLUMNS = [
    { key: 'name', label: 'Name', sort: key => key.name,
      render: key => h('span', { class: 'key-name', 'data-test': `key-${key.name}` }, key.name,
          isMine(key) ? [' ', pill('you', 'info')] : null,
          isGrant(key)
              ? [' ', pill('OAuth', 'info'), ' ', h('span', { class: 'muted small client-name' }, key.client_name ?? 'unknown client')]
              : null) },
    { key: 'actor', label: 'Actor', class: 'mono', sort: key => key.actor },
    { key: 'access', label: 'Access', sort: key => LEVELS.indexOf(key.access),
      render: key => pill(key.access, ACCESS_KIND[key.access]) },
    { key: 'catalogs', label: 'Catalogs', render: catalogsOf },
    { key: 'default_catalog', label: 'Default', class: 'mono' },
    { key: 'maintenance_report', label: 'Reporting',
      render: key => key.maintenance_report ? pill('reports', 'ok') : '-' },
    { key: 'created_at', label: 'Created', sort: key => key.created_at, render: key => when(key.created_at) },
    { key: 'revoked_at', label: 'State', sort: key => key.revoked_at ?? '',
      render: key => key.revoked_at
          ? [pill('revoked', 'error'), ' ', h('span', { class: 'muted small' }, when(key.revoked_at))]
          : pill('active', 'ok') },
];

const CODE_COLUMNS = [
    { key: 'name', label: 'Name', sort: code => code.name,
      render: code => h('span', { class: 'key-name', 'data-test': `code-${code.name}` }, code.name) },
    { key: 'actor', label: 'Actor', class: 'mono', sort: code => code.actor },
    { key: 'access', label: 'Access', sort: code => LEVELS.indexOf(code.access),
      render: code => pill(code.access, ACCESS_KIND[code.access]) },
    { key: 'catalogs', label: 'Catalogs', render: catalogsOf },
    { key: 'default_catalog', label: 'Default', class: 'mono' },
    { key: 'expires_at', label: 'Expires', sort: code => code.expires_at, render: code => when(code.expires_at) },
    { key: 'state', label: 'State', sort: code => code.state,
      render: code => h('span', { 'data-test': `code-state-${code.name}` }, pill(code.state, CODE_STATE_KIND[code.state])) },
];

export async function render(main) {
    const head = h('header', { class: 'page-head' },
        h('div', {},
            h('h1', {}, 'API keys'),
            h('p', { class: 'muted' }, 'The keys inside your reach. A secret is shown once, when its key is created.')));
    main.append(head);
    if (!store.can('admin')) {
        main.append(h('p', { class: 'muted', id: 'keys-denied' }, 'Managing keys needs an admin key.'));
        return;
    }
    const showRevoked = h('input', { type: 'checkbox', id: 'keys-show-revoked' });
    const table = listTable(COLUMNS, [], { onRow: key => showKey(key, load), empty: 'No keys.' });
    table.id = 'keys-table';
    const applyFilter = () => table.setFilter(showRevoked.checked ? null : key => !key.revoked_at);
    showRevoked.addEventListener('change', applyFilter);
    head.append(h('div', { class: 'actions' },
        h('button', { type: 'button', id: 'key-new', onclick: () => newKey(load) }, 'New key')));
    const codes = codesPanel();
    main.append(h('label', { class: 'small muted' }, showRevoked, ' Show revoked keys'), table, codes.section);

    async function load() {
        try {
            table.redraw((await api.get('/keys')).keys);
        } catch { /* toasted; a 401 has already signed the browser out */ }
    }
    applyFilter();
    await Promise.all([load(), codes.load()]);
}

// The catalog checkboxes and default select both forms share.
function catalogFields() {
    const me = store.whoami;
    return [
        h('fieldset', {},
            h('legend', {}, 'Catalogs'),
            me.all_catalogs
                ? h('label', {}, h('input', { type: 'checkbox', name: 'all' }), ' Every catalog, present and future')
                : null,
            me.catalogs.map(name => h('label', {},
                h('input', { type: 'checkbox', name: 'catalog', value: name }), ' ', h('span', { class: 'mono' }, name)))),
        h('label', {}, 'Default catalog',
            h('select', { name: 'default_catalog' },
                h('option', { value: '' }, 'None'),
                me.catalogs.map(name => h('option', { value: name }, name)))),
    ];
}

// The grant fields of a submitted form, as both create bodies spell them.
function readGrants(data) {
    const all = data.get('all') === 'on';
    return {
        all,
        catalogs: all ? [] : data.getAll('catalog').map(String),
        default_catalog: String(data.get('default_catalog') ?? '') || null,
    };
}

// The create form, in the drawer.
function newKey(reload) {
    const form = h('form', { id: 'key-form' },
        h('label', {}, 'Name', h('input', { name: 'name', required: true, autocomplete: 'off', placeholder: 'letters, digits, . _ -' })),
        h('label', {}, 'Actor', h('input', { name: 'actor', autocomplete: 'off', placeholder: 'defaults to the name' })),
        h('label', {}, 'Access', h('select', { name: 'access' }, LEVELS.map(level => h('option', { value: level }, level)))),
        catalogFields(),
        h('label', {},
            h('input', { type: 'checkbox', name: 'maintenance_report', role: 'switch' }), ' May submit maintenance reports'),
        h('button', { type: 'submit' }, 'Create key'));
    form.addEventListener('submit', async event => {
        event.preventDefault();
        const data = new FormData(form);
        const body = {
            name: String(data.get('name') ?? '').trim(),
            access: String(data.get('access') ?? 'read'),
            ...readGrants(data),
            actor: String(data.get('actor') ?? '').trim() || null,
            maintenance_report: data.get('maintenance_report') === 'on',
        };
        try {
            const created = await api.post('/keys', body);
            closeDrawer();
            showOnce({
                prefix: 'key',
                title: `Key ${created.key.name} created`,
                warning: 'Copy this key now. It is not stored anywhere and cannot be shown again.',
                secret: created.secret,
            });
            await reload();
        } catch { /* the server's reason is toasted; the form stays filled */ }
    });
    openDrawer('New key', 'The secret is shown once, right after creation.', form);
}

// The one time a secret is on screen: a modal that only the explicit
// button closes, after which the secret is removed from the page. Element
// ids start with `prefix` (`key-secret`, `code-secret-done`, ...).
function showOnce({ prefix, title, warning, secret }) {
    const code = h('code', { id: `${prefix}-secret` }, secret);
    const copy = h('button', { type: 'button', id: `${prefix}-secret-copy`, class: 'secondary' }, 'Copy');
    const done = h('button', { type: 'button', id: `${prefix}-secret-done` }, 'I have stored it');
    const dialog = h('dialog', { id: `${prefix}-secret-dialog`, class: 'confirm' },
        h('article', {},
            h('h3', {}, title),
            h('p', { class: 'warn-text' }, warning),
            h('pre', { class: 'secret' }, code),
            h('footer', {}, copy, done)));
    copy.addEventListener('click', async () => {
        try {
            // navigator.clipboard exists only in secure contexts (https or localhost).
            await navigator.clipboard.writeText(secret);
            toast('Copied to the clipboard.', 'ok');
        } catch {
            toast('Copying failed: select the text and copy it by hand.', 'error');
        }
    });
    done.addEventListener('click', () => {
        code.textContent = '';
        dialog.close();
        dialog.remove();
    });
    dialog.addEventListener('cancel', event => event.preventDefault()); // Escape does not close it
    document.body.append(dialog);
    dialog.showModal();
}

function fact(label, value) {
    return [h('dt', {}, label), h('dd', {}, value)];
}

// One key's drawer: its facts, then grant, ungrant, reporting and revoke.
function showKey(key, reload) {
    const me = store.whoami;
    const mine = isMine(key);
    const grant = isGrant(key);
    const facts = h('dl', { class: 'facts' },
        fact('Id', h('code', {}, key.id)),
        fact('Kind', grant ? 'OAuth grant' : 'API key'),
        grant ? fact('Client', key.client_name ?? 'unknown client') : null,
        fact('Actor', h('code', {}, key.actor)),
        fact('Access', pill(key.access, ACCESS_KIND[key.access])),
        fact('Catalogs', catalogsOf(key)),
        fact('Default', key.default_catalog ?? '-'),
        fact('Reporting', key.maintenance_report ? 'may submit maintenance reports' : 'no'),
        fact('Created', when(key.created_at)),
        fact('Revoked', key.revoked_at ? when(key.revoked_at) : 'no'));
    const subtitle = mine
        ? 'The key this browser is signed in with'
        : grant ? `OAuth grant for ${key.client_name ?? 'an unknown client'}` : `${key.access} key`;
    if (key.revoked_at) {
        openDrawer(key.name, subtitle, facts,
            h('p', { class: 'muted' }, 'This key is revoked: it authenticates nothing and cannot be changed.'));
        return;
    }
    const change = async (request, message) => {
        try {
            const updated = await request();
            toast(message, 'ok');
            await reload();
            showKey(updated, reload);
        } catch { /* toasted */ }
    };
    const base = `/keys/${enc(key.id)}`;
    let grants;
    if (key.all_catalogs) {
        grants = h('p', { class: 'muted small' }, 'Granted every catalog: per-catalog grants do not apply.');
    } else {
        const grantable = me.catalogs.filter(name => !key.catalogs.includes(name));
        const select = h('select', { id: 'key-grant-select', 'aria-label': 'Catalog to grant' },
            grantable.map(name => h('option', { value: name }, name)));
        grants = h('section', {},
            h('h3', {}, 'Catalogs'),
            h('ul', { class: 'grant-list' }, key.catalogs.length
                ? key.catalogs.map(name => h('li', {},
                    h('span', { class: 'mono' }, name), ' ',
                    h('button', {
                        type: 'button', class: 'text-btn', dataset: { ungrant: name },
                        onclick: () => change(() => api.del(`${base}/catalogs/${enc(name)}`), `Removed ${name} from ${key.name}.`),
                    }, 'Remove')))
                : h('li', { class: 'muted' }, 'No catalogs: this key reaches nothing.')),
            grantable.length
                ? h('div', { class: 'inline-form' }, select,
                    h('button', {
                        type: 'button', id: 'key-grant',
                        onclick: () => change(() => api.put(`${base}/catalogs/${enc(select.value)}`), `Granted ${select.value} to ${key.name}.`),
                    }, 'Grant'))
                : null);
    }
    // An OAuth grant never reports (the server refuses it), so no toggle.
    const report = grant ? null : h('button', {
        type: 'button', id: 'key-report-toggle', class: 'secondary',
        onclick: () => change(() => api.patch(base, { maintenance_report: !key.maintenance_report }),
            key.maintenance_report ? `${key.name} may no longer report.` : `${key.name} may now report.`),
    }, key.maintenance_report ? 'Disable reporting' : 'Enable reporting');
    const revoke = h('button', {
        type: 'button', id: 'key-revoke', class: 'danger',
        onclick: async () => {
            const ok = await confirmDialog({
                title: `Revoke ${key.name}?`,
                message: mine
                    ? 'This is the key this browser is signed in with: revoking it signs you out at once. This cannot be undone.'
                    : grant
                        ? 'The client is refused from its next request, and its tokens stop working. This cannot be undone.'
                        : 'Anything using this key is refused from its next request. This cannot be undone.',
                action: 'Revoke',
            });
            if (!ok) return;
            try {
                await api.post(`${base}/revoke`);
                closeDrawer();
                toast(`Revoked ${key.name}.`, 'ok');
                await reload(); // for the signed-in key this is a 401: back to the login card
            } catch { /* toasted */ }
        },
    }, 'Revoke');
    openDrawer(key.name, subtitle, facts, grants, h('div', { class: 'drawer-actions' }, report, revoke));
}

// The sign-in codes section: its own head, filter and table. Returns the
// section and its loader, which `render` awaits beside the keys'.
function codesPanel() {
    const showAll = h('input', { type: 'checkbox', id: 'codes-show-all' });
    const table = listTable(CODE_COLUMNS, [], { onRow: code => showCode(code, load), empty: 'No live sign-in codes.' });
    table.id = 'codes-table';
    const applyFilter = () => table.setFilter(showAll.checked ? null : code => code.state === 'live');
    showAll.addEventListener('change', applyFilter);
    applyFilter();
    async function load() {
        try {
            table.redraw((await api.get('/oauth/codes')).codes);
        } catch { /* toasted */ }
    }
    const section = h('section', { id: 'codes-panel', class: 'codes-panel' },
        h('header', { class: 'page-head' },
            h('div', {},
                h('h2', {}, 'Sign-in codes'),
                h('p', { class: 'muted' },
                    'One-time codes a person enters when connecting an OAuth client such as ChatGPT. '
                    + 'Each becomes an OAuth grant with the code\'s catalogs, access and actor.')),
            h('div', { class: 'actions' },
                h('button', { type: 'button', id: 'code-new', onclick: () => newCode(load) }, 'New sign-in code'))),
        h('label', { class: 'small muted' }, showAll, ' Show redeemed, expired and revoked codes'),
        table);
    return { section, load };
}

// The mint form, in the drawer.
function newCode(reload) {
    const form = h('form', { id: 'code-form' },
        h('label', {}, 'Name', h('input', { name: 'name', required: true, autocomplete: 'off', placeholder: 'letters, digits, . _ -' })),
        h('label', {}, 'Actor', h('input', { name: 'actor', required: true, autocomplete: 'off', placeholder: 'who the grant writes as' })),
        h('label', {}, 'Access', h('select', { name: 'access' }, CODE_LEVELS.map(level => h('option', { value: level }, level)))),
        catalogFields(),
        h('label', {}, 'Expires unused after',
            h('select', { name: 'expires_in' },
                EXPIRIES.map(([value, label]) => h('option', { value, selected: value === '24h' }, label)))),
        h('button', { type: 'submit' }, 'Create sign-in code'));
    form.addEventListener('submit', async event => {
        event.preventDefault();
        const data = new FormData(form);
        const body = {
            name: String(data.get('name') ?? '').trim(),
            actor: String(data.get('actor') ?? '').trim(),
            access: String(data.get('access') ?? 'read'),
            ...readGrants(data),
            expires_in: String(data.get('expires_in') ?? '24h'),
        };
        try {
            const created = await api.post('/oauth/codes', body);
            closeDrawer();
            showOnce({
                prefix: 'code',
                title: `Sign-in code ${created.code.name} created`,
                warning: 'Give this code to the person connecting now. It works once, is not stored anywhere and cannot be shown again.',
                secret: created.secret,
            });
            await reload();
        } catch { /* the server's reason is toasted; the form stays filled */ }
    });
    openDrawer('New sign-in code', 'The code is shown once, right after creation.', form);
}

// One code's drawer: its facts, and Revoke while nobody has redeemed it.
function showCode(code, reload) {
    const facts = h('dl', { class: 'facts' },
        fact('Id', h('code', {}, code.id)),
        fact('Actor', h('code', {}, code.actor)),
        fact('Access', pill(code.access, ACCESS_KIND[code.access])),
        fact('Catalogs', catalogsOf(code)),
        fact('Default', code.default_catalog ?? '-'),
        fact('State', pill(code.state, CODE_STATE_KIND[code.state])),
        fact('Created', when(code.created_at)),
        fact('Expires', when(code.expires_at)),
        code.redeemed_at ? fact('Redeemed', when(code.redeemed_at)) : null,
        code.grant_id ? fact('Grant', h('code', {}, code.grant_id)) : null,
        code.revoked_at ? fact('Revoked', when(code.revoked_at)) : null);
    const revocable = code.state === 'live' || code.state === 'expired';
    const revoke = revocable ? h('button', {
        type: 'button', id: 'code-revoke', class: 'danger',
        onclick: async () => {
            const ok = await confirmDialog({
                title: `Revoke sign-in code ${code.name}?`,
                message: 'Nobody can redeem it afterwards. This cannot be undone.',
                action: 'Revoke',
            });
            if (!ok) return;
            try {
                await api.del(`/oauth/codes/${enc(code.id)}`);
                closeDrawer();
                toast(`Revoked sign-in code ${code.name}.`, 'ok');
                await reload();
            } catch { /* toasted */ }
        },
    }, 'Revoke') : null;
    openDrawer(code.name, 'Sign-in code', facts,
        code.state === 'redeemed'
            ? h('p', { class: 'muted small' }, 'Redeemed: revoke the grant it became in the key list above.')
            : null,
        h('div', { class: 'drawer-actions' }, revoke));
}
```

`selected: value === '24h'` relies on `h()` skipping a `false` attribute (`core/dom.js:10`).

- [ ] **Step 3: The styles**

Append to the "API keys" block of `src/http/ui/styles.css`:

```css
/* Sign-in codes (M10 Task 7): the second section of the keys page */
.codes-panel { margin-top: 2.5rem; }
.codes-panel .page-head { margin-bottom: .75rem; }
.client-name { white-space: nowrap; }
```

- [ ] **Step 4: Run the journeys**

Run: `just test-ui`
Expected: every keys journey runs (not skipped) and passes, the two new ones included, with no console errors. The other pages' journeys still pass.

- [ ] **Step 5: Docs and changelog**

`docs/operations.md`, subsection "OAuth sign-in codes" (Task 4): add after its first paragraph:

```markdown
The API keys page (`/ui/#/keys`) has a "Sign-in codes" section below the
keys: "New sign-in code" mints one and shows it once, the list shows live
codes (tick "Show redeemed, expired and revoked codes" for the rest), and a
code's drawer revokes it until someone redeems it. In the key list, an OAuth
grant is marked `OAuth` with its client's name. Its drawer revokes it or
changes its catalogs, and has no reporting switch: a grant never reports.
```

`changelog/<date -u +%Y-%m-%dT%H%M%SZ>-m10-task-07-console-sign-in-codes.md`:
- **Added:** the "Sign-in codes" section of the API keys page (mint, show once, list with state, revoke); OAuth grant marking in the key list and drawer; two browser journeys.
- **Changed:** the one-time secret dialog and the catalog fields are shared by keys and codes; an OAuth grant's drawer has no reporting toggle.
- **Notes:** none.

- [ ] **Step 6: Gates and commit**

Run every gate in "Conventions", `just test-ui` included (its journeys must run). Then:

```bash
git add src/http/ui/js/pages/keys.js src/http/ui/styles.css tests/ui_e2e/keys.rs \
    docs/operations.md changelog/
git commit -m "feat(console): sign-in codes panel and OAuth grant display

The API keys page mints sign-in codes (shown once), lists them by state
and revokes them; OAuth grants are marked with their client in the key
list, and their drawer has no reporting toggle.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8: Operations documentation, spec wrap-up, release v0.13.0 preparation

This task documents OAuth for self-hosters, brings the spec in line with what
shipped, writes the milestone changelog, and prepares v0.13.0. It comes in
two parts, like M9 Task 8:
- **Part one** (Steps 1-9) is a docs PR.
- **Part two** (Steps 10-11) runs `just release-prepare minor` on `main`
  after that PR merges, and stops. Shipping, tagging, publishing the image
  and rollout each wait for the owner's go-ahead.

**Facts this task relies on** (verified at `c24fef1`):

- `docs/operations.md` structure:
  - "## 3. Configuration" with a key table (lines 71-83) and a TOML example (95-107);
  - the Kubernetes `ConfigMap` with `config.toml` (lines 221-230);
  - "## Web console" (1058) with an area subsection per API area;
  - "## 7. Health and time" (1321);
  - "## 8. Upgrades" (1360) with per-version notes (the 0.12.0 note at 1377-1381);
  - "### Preparing and shipping a release" (1383);
  - "## 10. Troubleshooting" (1642), a two-column table.
- The guide's placeholders are the host `okf-mcp.example.com` and the catalog `notes` (operations.md:4-7, 229). This task uses them too.
- The ingress example routes every path (`path: /`, `pathType: Prefix`, operations.md:353-354), so `/.well-known/*` and `/oauth/*` reach the server with no ingress change.
- `scripts/release.py`'s prepare hook counts the old version preceded by `:` or `v` in `docs/operations.md` (`(?<=[:v])<old>\b`). It requires **exactly four** such mentions (the pinned image examples) and rewrites them (`scripts/release.py:102-107`). **Text added by this task must not write a version right after `:` or `v`** (no `v0.13.0`, no image tag), or the next release preparation fails. A version after `**` or a space, as in the upgrade notes, is fine.
- `just release-prepare <patchlevel>` (`justfile`, `scripts/release-prepare.sh`) needs a clean `main` equal to `origin/main`. cargo-release then:
  - bumps `Cargo.toml` and `Cargo.lock`;
  - writes `changelog/<UTC-timestamp>-release-v<version>.md`;
  - runs `just fmt`, `just lint` and `just test`;
  - creates one local `chore: Release v<version>` commit and an annotated tag.

  It pushes nothing (operations.md:1398-1409). `minor` takes 0.12.1 to 0.13.0.
- M9's wrap-up wrote `changelog/<ts>-m9-web-console.md`, one bullet per task, each with its issue and PR. It also wrote a "What M9 shipped differently" section at the end of the M9 spec, one item per dated "Corrected (issue #N)" note in the plan (`docs/superpowers/specs/2026-11-09-okf-webui-design.md:353-420`).
- README:
  - the status line (README.md:7-11);
  - "## Command line" (43);
  - "### Connecting a client" (173);
  - "### Tools" (199);
  - "### Routes" (239), a three-column table.
- Task 4 owns the README's "Command line" mention of `okf-mcp oauth ...`, and the `/api/v1/oauth/codes` rows under "Web console" in `docs/operations.md` (its "Sign-in codes" subsection). This task adds the operator-facing "OAuth connectors" section, which holds the CLI reference. It does not repeat the API rows; it links to them.

**Files:**
- Modify: `docs/operations.md`:
  - configuration table, TOML example and ConfigMap;
  - new section "## OAuth connectors" between the "Web console" subsections and "## 7. Health and time";
  - Upgrades note;
  - Troubleshooting rows.
- Modify: `README.md` (status line, Connecting a client, Tools, Routes)
- Modify: `docs/superpowers/specs/2026-11-11-okf-oauth-design.md` (status, deviations, "What M10 shipped differently")
- Modify: `docs/superpowers/plans/2026-11-11-okf-mcp-m10-oauth.md` (the deviations list, if tasks added any)
- Create: `changelog/<ts>-m10-task-08-docs.md`, `changelog/<ts>-m10-oauth.md` (the milestone entry)
- Part two: `Cargo.toml`, `Cargo.lock`, `changelog/<ts>-release-v0.13.0.md` (all written by `just release-prepare`)

- [ ] **Step 1: Collect what the earlier tasks changed**

Run:
```bash
grep -n "Corrected (issue" docs/superpowers/plans/2026-11-11-okf-mcp-m10-oauth.md
ls changelog | grep m10-task
gh pr list --state merged --search "M10 task" --json number,title --limit 20
gh issue list --state all --search "M10" --json number,title --limit 20
```
Expected:
- the dated corrections, one line each (possibly none);
- seven task changelogs;
- seven merged PRs and their issues.

Keep this output: Steps 6 and 7 cite every item in it. Read each task changelog's "Notes" for behaviour that differs from the spec (for example, a changed error code or a renamed CLI flag), and use the shipped behaviour, not the spec's, in Steps 2 to 4.

- [ ] **Step 2: Configuration reference**

In `docs/operations.md`, "### The configuration file", add these rows to
the key table after `overlay_cache_size`:

```markdown
| `public_url` | none | The server's public origin, `https://okf-mcp.example.com` (no path, no trailing slash). Setting it turns on OAuth ("OAuth connectors"); without it the OAuth routes are 404. It must be `https`, except on a loopback host for local testing, and its host must be in `allowed_hosts` |
| `oauth.access_token_ttl` | `"1h"` | How long an OAuth access token works |
| `oauth.refresh_token_ttl` | `"30d"` | How long an unused refresh token works. Each refresh issues a new one and restarts this |
| `oauth.code_max_expiry` | `"7d"` | The longest `--expires` a sign-in code may have |
| `oauth.dcr_enabled` | `true` | Accept dynamic client registration (RFC 7591). Clients that publish a Client ID Metadata Document work either way |
| `oauth.cimd_allow_private` | `false` | Fetch Client ID Metadata Documents from loopback and private addresses. For test rigs only |
```

After the paragraph that begins "The server refuses to start", add:

```markdown
It also refuses a `public_url` that is not an origin (a path, query or
fragment), that is `http` on a host other than `localhost`, `127.0.0.1` or
`::1`, or whose host is not in `allowed_hosts`.
```

In the TOML example after `overlay_cache_size = 64`, add:

```toml
public_url = "https://okf-mcp.example.com"   # turns on OAuth; see "OAuth connectors"

[oauth]
access_token_ttl = "1h"
refresh_token_ttl = "30d"
code_max_expiry = "7d"
dcr_enabled = true
```

`[oauth]` is a TOML table, so it must come after every top-level key. Keep
it last in the example.

In the Kubernetes `ConfigMap` (`config.toml: |`), after `time_server = "time.example.com"`, add:

```toml
    public_url = "https://okf-mcp.example.com"
```

- [ ] **Step 3: The "OAuth connectors" section**

In `docs/operations.md`, insert this section after the last "Web console"
subsection ("### Testing the console") and before "## 7. Health and time":

````markdown
## OAuth connectors

Some MCP clients can only sign in with OAuth: ChatGPT's custom connectors,
and the Claude and ChatGPT connector directories. For them the server is also
an OAuth 2.1 authorization server, following the MCP authorization
specification (2026-07-28). Clients that send an API key (`Authorization:
Bearer okf_...`) are not affected: keys and OAuth work side by side on the
same `/mcp` URL.

There are no user accounts. An admin mints a **sign-in code** that says what
the connection may do (catalogs, `read` or `write`, the default catalog and
the actor its writes are recorded as). The person connecting pastes the code
into the server's consent page. Redeeming a code creates an **OAuth grant**,
which works like a key with exactly the code's access: it appears in the key
list and is revoked the same way. A code works once and expires unused after
24 hours by default.

OAuth grants are never `admin` and never submit maintenance reports. The
console and the maintenance runner keep using API keys.

### Turning it on

1. Set `public_url` to the origin clients reach the server at, and make sure
   its host is in `allowed_hosts` (section 3):

   ```toml
   allowed_hosts = ["okf-mcp.example.com"]
   public_url = "https://okf-mcp.example.com"
   ```

2. Restart the server. These now answer, with no key:

   ```sh
   curl -s https://okf-mcp.example.com/.well-known/oauth-protected-resource
   curl -s https://okf-mcp.example.com/.well-known/oauth-authorization-server
   ```

   The first names the resource `https://okf-mcp.example.com/mcp` and its
   authorization server. A request to `/mcp` without a token now gets a 401
   whose `WWW-Authenticate` header points clients at that document.

The example ingress (section 5) already routes every path, `/.well-known/`
and `/oauth/` included. A proxy that routes only `/mcp` must also pass
`/.well-known/oauth-protected-resource`, `/.well-known/oauth-authorization-server`
and `/oauth/`.

### Connecting a client

1. **Mint a sign-in code** for the connection, on the server:

   ```sh
   okf-mcp oauth code create chatgpt --actor example-actor \
       --access write --catalogs notes --default notes
   ```

   The command prints the code (`okfsc_...`) once. Only its hash is stored.
   Send it to the person connecting over a private channel. You can also
   mint one in the web console, on the API keys page under "Sign-in codes".

2. **Add the connector** in the client. Use the URL
   `https://okf-mcp.example.com/mcp` and choose OAuth authentication. Leave
   any client ID and client secret fields empty: the server accepts clients
   that identify themselves by a Client ID Metadata Document, and clients
   that register themselves (dynamic client registration).

3. **Sign in.** The client opens the server's consent page in a browser. It
   shows the client's name (marked "unverified" when the client registered
   itself rather than publishing a metadata document) and where it will
   return to. Paste the sign-in code, check what it grants, and approve. The
   browser goes back to the client, which is now connected.

The client renews its access itself. Access tokens last `access_token_ttl`
and are refreshed with a refresh token that lasts `refresh_token_ttl` from
its last use. A connection used at least once a month stays connected.

Each connection needs its own code. Connecting ChatGPT and Claude.ai, or the
same client again after a revoke, takes a new code each time.

### Managing codes and grants

| Command | Does |
|---|---|
| `okf-mcp oauth code create NAME --actor ACTOR --access read\|write (--catalogs C,C... \| --all) [--default C] [--expires 24h]` | Mints a single-use sign-in code and prints it once. `--catalogs` is comma-separated, as for `key create`. `--access admin` is refused. `--expires` defaults to 24 hours and may not exceed `oauth.code_max_expiry` |
| `okf-mcp oauth code list` | Every code, never the code itself: its id, name, access, catalogs, actor, expiry and state (`live`, `expired`, `redeemed` or `revoked`) |
| `okf-mcp oauth code revoke ID` | Revokes an unredeemed code. It cannot be redeemed afterwards |
| `okf-mcp oauth grant list` | The OAuth grants (keys of kind `oauth`), with the client each was made for |
| `okf-mcp oauth grant revoke ID` | Revokes a grant and with it every token it holds |

The console's API keys page offers the same operations, and the JSON API
behind it is under "Web console" ("Sign-in codes"). A revoke made with the
CLI takes effect within `poll_interval`; one made in the console takes effect
at once. Revoking a code does not touch a grant already made from it: revoke
the grant for that.

### What the server checks

- **Clients.** A `client_id` that is an `https://` URL is fetched as a Client
  ID Metadata Document. The fetch is over `https`, with no redirects, a
  5-second timeout and a 16 KiB limit, and never to a loopback or private
  address (unless `cimd_allow_private`). The document's `client_id` must
  equal its URL. Any other `client_id` must have been registered at
  `/oauth/register`. Registered clients without a grant are deleted after
  24 hours.
- **Redirects.** The `redirect_uri` must match one the client declared
  exactly. An error found before that check is shown on the server's own
  error page, never sent to an unchecked address.
- **PKCE.** S256 only, on every authorization.
- **Audience.** Every token is issued for `<public_url>/mcp` (RFC 8707) and
  refused anywhere else.
- **Replay.**
  - An authorization code works once, within 60 seconds.
  - Presenting it again revokes the tokens issued from it.
  - A refresh token works once: presenting a used one revokes the whole
    connection's tokens, and the client must sign in again with a new code.
- **Rate limits.** Registration, the token endpoint and the consent form are
  rate-limited (429 with `slow_down`). At most 500 registered clients
  without a grant are kept.

Tokens and codes start with `okfat_` (access), `okfrt_` (refresh), `okfac_`
(authorization code) and `okfsc_` (sign-in code), so secret scanners can find
them. The server stores only their SHA-256 and never logs them.

### Routes

| Route | Key | Does |
|---|---|---|
| `GET /.well-known/oauth-protected-resource`, `GET /.well-known/oauth-protected-resource/mcp` | none | Protected Resource Metadata (RFC 9728) |
| `GET /.well-known/oauth-authorization-server` | none | Authorization server metadata (RFC 8414) |
| `POST /oauth/register` | none | Dynamic client registration (RFC 7591), public clients only. Absent when `dcr_enabled = false` |
| `GET /oauth/authorize`, `POST /oauth/authorize` | none (a sign-in code on the form) | The consent page |
| `POST /oauth/token` | none (PKCE or a refresh token) | Exchanges a code or a refresh token for tokens |
| `POST /oauth/revoke` | none | Revokes a token (RFC 7009); always 200 |

Every `/oauth/*` response is `Cache-Control: no-store`, and every JSON error
is `{"error": ..., "error_description": ...}` with the OAuth error code.
````

- [ ] **Step 4: Upgrade note and troubleshooting**

In "## 8. Upgrades", after the **0.12.0 (web console)** bullet, add:

```markdown
- **0.13.0 (OAuth)** adds migration `0008_oauth.sql`: existing keys become
  keys of kind `key` and work as before. It adds the `public_url` setting
  and the `[oauth]` table. Without `public_url` nothing else changes: the
  OAuth routes are 404 and a 401 still carries a plain `WWW-Authenticate:
  Bearer`. With it, see "OAuth connectors". Every tool now carries a title
  and behaviour hints (`readOnlyHint`, `destructiveHint`, ...), which
  clients may show; no tool's arguments or results changed.
```

In "## 10. Troubleshooting", add these rows to the table:

```markdown
| `/.well-known/oauth-protected-resource` is 404 | `public_url` is not set (OAuth is off), or a proxy does not route `/.well-known/`. Set it, restart, and check the proxy ("OAuth connectors") |
| The server exits naming `public_url` | It is not an `https` origin, or its host is not in `allowed_hosts`. Give the bare origin, and add its host to `allowed_hosts` |
| A client says it cannot register, or the consent page says the client is unknown | With `dcr_enabled = false`, the client must publish a Client ID Metadata Document. Otherwise the client's document could not be fetched (it must be `https`, public, under 16 KiB and answer within 5 s) |
| The consent page refuses a sign-in code | The code is mistyped, expired, already redeemed or revoked (the page does not say which, on purpose). `okf-mcp oauth code list` shows its state; mint a new one |
| The consent page says to start again from the client | The sign-in took longer than 10 minutes, or a code was refused 5 times. Start the connection again in the client |
| An OAuth client was connected but now gets 401 and asks to sign in | Its grant was revoked, it was unused for longer than `refresh_token_ttl`, or a used refresh token was presented again (a sign of a copied token), which revokes the connection. Mint a new code and reconnect |
| The browser shows a CSP error after Approve, and the client never connects | A proxy replaced the consent page's `Content-Security-Policy`. Its `form-action` must allow the client's redirect origin; let the server's header through on `/oauth/` |
```

- [ ] **Step 5: README**

Change the status line (README.md:7-11). After "...its JSON API at
`/api/v1` ("Web console" below; design:
`docs/superpowers/specs/2026-11-09-okf-webui-design.md`)." add:

```markdown
Milestone 10 added OAuth 2.1 for connectors that cannot send an API key
(`docs/operations.md`, "OAuth connectors"; design:
`docs/superpowers/specs/2026-11-11-okf-oauth-design.md`).
```

At the end of "### Connecting a client", add:

```markdown
A client that can only use OAuth (ChatGPT's custom connectors, the
connector directories) connects to the same `/mcp` URL once the server has
a `public_url`. An admin mints a single-use sign-in code with `okf-mcp oauth
code create`, and the person connecting pastes it into the server's consent
page (`docs/operations.md`, "OAuth connectors").
```

At the end of the paragraph that begins "The read tools below need a
`read` key" in "### Tools", add:

```markdown
Every tool also has a `title` and the hints `readOnlyHint`,
`destructiveHint`, `idempotentHint` and `openWorldHint` (always false). The
read tools are read-only. The write tools that only stage changes in a
changeset are not destructive. `publish`, `discard`, `delete_concept` and
`maintenance_close` are destructive.
```

In "### Routes", add these rows after the `/api/v1/*` row:

```markdown
| `GET /.well-known/oauth-protected-resource`, `GET /.well-known/oauth-authorization-server` | none | OAuth metadata; only with `public_url` set (`docs/operations.md`, "OAuth connectors") |
| `GET/POST /oauth/authorize`, `POST /oauth/register`, `POST /oauth/token`, `POST /oauth/revoke` | none | The OAuth authorization server; only with `public_url` set |
```

Change the line under the routes example that begins "A missing or unknown
key gets 401 (`WWW-Authenticate: Bearer`, a plain-text body)" to:

```markdown
A missing or unknown key or token gets 401 (`WWW-Authenticate: Bearer`, with
`resource_metadata="<public_url>/.well-known/oauth-protected-resource"` when
OAuth is on, and a plain-text body).
```

- [ ] **Step 6: Spec wrap-up**

In `docs/superpowers/specs/2026-11-11-okf-oauth-design.md`:

1. Replace the status line `Status: approved in brainstorming, 2026-11-11. Not yet planned.` with:

   ```markdown
   Status: approved in brainstorming, 2026-11-11; delivered in milestone 10
   (epic #E, plan `docs/superpowers/plans/2026-11-11-okf-mcp-m10-oauth.md`).
   Where the plan deviated, this document says so in place ("Plan deviation
   N", numbered as in the plan's "Simplifications and deviations"), and "What
   M10 shipped differently" at the end lists the rest.
   ```

   `E` is the epic's number, from Step 1's `gh issue list`.

2. In "Data model", replace `    family_id  uuid NOT NULL,                -- one per authorization code; rotation keeps it` with:

   ```sql
       family_id  text NOT NULL,                -- one per authorization code; rotation keeps it (Plan deviation 1)
   ```

3. In "Data model", "In memory", replace the line about authorization codes
   that begins `- **Authorization codes**, keyed by SHA-256:` so that it starts:

   ```markdown
   - **Authorization codes**, keyed by the SHA-256 of the `okfac_` code
     (Plan deviation 2):
   ```

   Keep the rest of that bullet as it is.

4. In "Tool annotations", move `revert` from the "Irreversible or deleting"
   row to the "Staged edits" row. After the table's following paragraph, add:

   ```markdown
   `revert` stages an earlier revision in the changeset for review and
   publish, so it is a staged edit, not destructive (Plan deviation 3).
   ```

5. Append a section at the end of the spec:

   ```markdown
   ## What M10 shipped differently

   The plan's deviations are recorded in place above. These are the plan's
   dated "Corrected (issue #N)" notes: places where the merged code differs
   from the plan as first written. The code is the reference; each item names
   the file to read.
   ```

   Then add one subsection per task with corrections, in task order. Each
   is headed `**Task N, <name> (issue #I, PR #P):**` and has one bullet per
   correction from Step 1's grep. A bullet states the shipped behaviour and
   names the file, as the M9 spec's section does
   (`docs/superpowers/specs/2026-11-09-okf-webui-design.md:353-420`). If the
   grep found no corrections, write one sentence after the paragraph
   instead: `The plan shipped as written; it has no dated corrections.`

In the plan, check that "Simplifications and deviations relative to the
design" lists Deviation 3 (`revert` is a staged edit, Task 1). If it is
missing, add:

```markdown
3. `revert` is a staged edit in the tool annotations (Task 1): it adds an
   earlier revision to the changeset for review and publish. The spec's
   table listed it as destructive; Task 8 updates the spec.
```

Add any further deviations a task recorded in its changelog "Notes",
numbered on from there, and reference each from the spec in place.

- [ ] **Step 7: Changelogs**

Create `changelog/<date -u +%Y-%m-%dT%H%M%SZ>-m10-oauth.md` (the
milestone entry), using the issue and PR numbers from Step 1:

```markdown
# Milestone 10: OAuth 2.1 and directory readiness (epic #E)

OAuth-only MCP clients (ChatGPT custom connectors, the Claude and ChatGPT
connector directories) can connect through a built-in OAuth 2.1
authorization server, beside the static API keys, which work as before. An
admin-issued single-use sign-in code, pasted on the server's consent page,
becomes an OAuth grant: a key row of kind `oauth` with the code's catalogs,
access (never admin) and actor. Every tool carries a title and behaviour
hints. Design: `docs/superpowers/specs/2026-11-11-okf-oauth-design.md`;
plan: `docs/superpowers/plans/2026-11-11-okf-mcp-m10-oauth.md`.

## Added

- **Task 1, tool annotations (#I, PR #P):** a title and `readOnlyHint`,
  `destructiveHint`, `idempotentHint` and `openWorldHint` on all 41 tools,
  and a classification test every new tool must pass.
- **Task 2, foundation (#I, PR #P):** migration `0008_oauth.sql`
  (`api_keys.kind`, `oauth_clients`, `sign_in_codes`, `oauth_tokens`, and
  checks that keep grants below admin); `public_url` and `[oauth]`; the
  `BearerVerifier` trait with prefix dispatch (`okf_` keys, `okfat_` access
  tokens) and the audience check; `resource_metadata` on every 401; the
  `src/oauth/` skeleton; cleanup in the poll loop.
- **Task 3, metadata and clients (#I, PR #P):** Protected Resource Metadata
  (RFC 9728) and authorization server metadata (RFC 8414); dynamic client
  registration (RFC 7591); the hardened Client ID Metadata Document fetcher
  and its cache.
- **Task 4, sign-in codes (#I, PR #P):** the sign-in code store,
  `okf-mcp oauth code create|list|revoke` and `oauth grant list|revoke`, and
  the `/api/v1/oauth/codes` endpoints.
- **Task 5, authorize and consent (#I, PR #P):** `/oauth/authorize` with
  exact redirect matching, required S256 PKCE and the resource check; the
  two-step consent page (code, then what will be granted), with CSRF
  protection and a CSP whose `form-action` admits the validated redirect;
  redemption in one transaction.
- **Task 6, tokens (#I, PR #P):** `/oauth/token` (authorization code and
  rotating refresh tokens with reuse detection) and `/oauth/revoke` (RFC
  7009); end-to-end tests of the whole flow with CIMD and with DCR, beside a
  static key.
- **Task 7, console (#I, PR #P):** the "Sign-in codes" panel on the API keys
  page, and OAuth grants marked in the key list with their client.
- **Task 8, documentation (#I, PR #P):** "OAuth connectors" in
  `docs/operations.md`, the configuration reference, the 0.13.0 upgrade
  note, troubleshooting rows, the README, and the spec's wrap-up.

## Changed

- A 401 carries `WWW-Authenticate: Bearer resource_metadata="..."` when
  OAuth is on. Without `public_url` it is unchanged.
- `require_key` admits OAuth access tokens as well as API keys.
- `/whoami` and the key list show a credential's kind.

## Notes

- Without `public_url` the server behaves as 0.12.1 apart from the
  migration and the tool annotations.
- The server stays single-replica. Pending consent pages and authorization
  codes are in memory, so a restart interrupts at most a sign-in in
  progress.
```

Replace every `#I` and `#P` with the numbers from Step 1, and `#E` with the
epic.

Create `changelog/<date -u +%Y-%m-%dT%H%M%SZ>-m10-task-08-docs.md`:

```markdown
# M10 Task 8: Wrap-up (issue #I, part one)

## Added

- `docs/operations.md`: "OAuth connectors" (turning it on, connecting a
  client, managing codes and grants, what the server checks, routes); the
  `public_url` and `[oauth]` rows, the TOML and ConfigMap examples; the
  0.13.0 upgrade note; seven troubleshooting rows.
- The milestone changelog, `changelog/*-m10-oauth.md`.

## Changed

- README: the status line, OAuth under "Connecting a client", the tool
  hints under "Tools", the OAuth routes, and the 401's header.
- The spec records the plan's deviations in place (`family_id` is text,
  authorization codes carry `okfac_`, `revert` is a staged edit) and ends
  with "What M10 shipped differently".

## Notes

- No version was written right after `:` or `v` in `docs/operations.md`:
  release preparation requires exactly four such pinned mentions.
- The release (v0.13.0) is the second part of issue #I.
```

- [ ] **Step 8: Check the docs against the code**

Run:
```bash
cargo run --quiet -- oauth --help
cargo run --quiet -- oauth code create --help
grep -cP '(?<=[:v])0\.12\.1\b' docs/operations.md
grep -n "TBD\|TODO\|#I\b\|#P\b\|#E\b" README.md docs/operations.md changelog/*-m10-*.md docs/superpowers/specs/2026-11-11-okf-oauth-design.md
```
Expected:
- The help lists exactly the subcommands and flags named in "Managing codes
  and grants". If a flag differs, fix the docs to match the code, not the
  other way round.
- The pinned-version count is `4` (unchanged by this task).
- The last grep prints nothing.

Then check each route in the new "Routes" table against `src/oauth/*::routes()`:

```bash
grep -rn '\.route(' src/oauth
```
Expected: exactly the paths documented, no more and no fewer.

- [ ] **Step 9: Gates and commit (part one)**

Run:
```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
cargo test -- --test-threads=6
```
Expected: all clean. No code changed, so the counts equal Task 7's.

```bash
git add README.md docs changelog
git commit -m "docs: M10 wrap-up, OAuth connectors guide and spec deviations (M10 task 8)

The operations guide gains OAuth connectors (turning it on, connecting a
client, codes and grants, the checks, the routes), the public_url and
[oauth] reference, the 0.13.0 upgrade note and troubleshooting rows. The
README names OAuth and the tool hints. The spec records the deviations and
what M10 shipped differently; the milestone changelog lists every task.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

Open the PR for part one and take it through review to merge, as for every
task.

- [ ] **Step 10: Prepare v0.13.0 (part two, after part one merges)**

```bash
git switch main
git pull --ff-only
git status --short          # must print nothing
just release-prepare minor
```
Expected:
- `Cargo.toml` and `Cargo.lock` say `0.13.0`;
- a new `changelog/<ts>-release-v0.13.0.md` lists the commits since v0.12.1;
- `just fmt`, `just lint` and `just test` pass inside the hook;
- one local commit `chore: Release v0.13.0` and a local annotated tag
  `v0.13.0` exist.

Nothing is pushed.

Check:
```bash
git log --oneline -1
git tag --points-at HEAD
grep -cP '(?<=[:v])0\.13\.0\b' docs/operations.md
```
Expected: `chore: Release v0.13.0`, then `v0.13.0`, then `4`.

If preparation fails, follow "Failures and retries" in
`docs/operations.md`. There is no destructive rollback: inspect before acting.

- [ ] **Step 11: Stop for the go-ahead**

Do not run `just release-ship`, push a tag or roll out. Report to the owner:
- the prepared commit and tag;
- the release changelog's path;
- the test counts from the hook;
- for the rollout, the note that it needs `public_url` (and the public host
  in `allowed_hosts`) in the deployment's configuration before OAuth works.

Run `just release-ship`, then the rollout, only after the owner says to, one
step at a time, as in M7 to M9.

---
