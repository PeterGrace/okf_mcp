# okf-mcp user login: design

Status: delivered in milestone 12 (epic #308; sub-issues #309-#327; PRs #328-#334, #336-#338 and #340-#349, and the PR for #327), 2026-11-13 (plan `docs/superpowers/plans/2026-11-12-okf-mcp-m12-user-login.md`). It closes #298 and #299. What differs from this design is under "What shipped differently" at the end; the operator's view is `docs/operations.md`, "Signing in and self-service".

## Goal

People sign in to the web console as themselves. They manage their own API keys and OAuth grants within limits an admin sets, and they approve OAuth consent as themselves.

Today a user (M11) is only a label on a credential. The console is signed in to by pasting an API key, and every credential and sign-in code comes from an admin.

## Non-goals (this round)

- **Team owners managing their catalogs' access.** Only admins set a catalog's member access for now. The permission check is one function, so team owners can be added later without a schema change.
- **OIDC providers configured in the database.** Providers live in the config file.
- **Multi-factor authentication beyond passkeys.**
- **Self-service account deletion.**
- **Capping admin-issued credentials.** Keys and grants an admin issues keep their explicit grants, as today.

## Decisions

| Question | Decision |
|---|---|
| How a person proves who they are | Any combination of three methods: a local password, a passkey (WebAuthn), and one or more linked OIDC identities. |
| First OIDC sign-in | Linked to the existing user whose email matches the provider's *verified* email. Afterwards matched by `(provider, subject)`. There is no self-signup. |
| First password or passkey | An admin sends a setup email with a single-use link. The person chooses a password and/or registers a passkey. No admin ever sees a password. |
| Forgot password | Self-service, by emailed single-use link. |
| Sending mail | A `Mailer` trait. The first providers are SMTP, Postmark (HTTP API) and `log`, selected in config. Adding a provider means one more implementation. |
| A user's ceiling | The catalogs owned by the user's teams, at each catalog's `member_access`, plus per-user entitlements (`user_catalogs`). Where both apply, the higher level wins. |
| Team member access | Set per catalog by an admin: `none`, `read` or `write`, default `write`. Team role (owner or member) still governs only the team's membership. |
| Admin users | `users.is_admin`, set by the CLI or another admin. A signed-in admin user gets the full console. Admin keys are minted only by admins, never through self-service. |
| What the ceiling caps | Only self-issued credentials: keys a user creates in the console, and grants from consent signed in as the user. Admin-issued credentials are unchanged. |
| OAuth consent | The person signs in on the consent page and chooses catalogs and access within their ceiling. Admin-minted sign-in codes remain as an alternative. |
| Browser authentication | Server-side sessions behind an `HttpOnly` cookie, with a CSRF header on writes. Bearer keys still work everywhere. |
| OIDC configuration | The config file, with client secrets in environment variables. The console shows the providers read-only. |
| Policy settings | A database settings table that the admin Settings page edits. The config file supplies the defaults. |
| Background jobs | apalis, with jobs stored in the server's Postgres database (`apalis-postgres`). Sending mail is the first job. |
| Pasted-key console sign-in | Kept, for automation and for admins without a user. |
| Web console | At parity with the CLI. Every admin action has a console control. |

## Data model

Migration `migrations/0010_user_login.sql`, applied in one transaction. It only adds; no existing row changes meaning. Timestamps come from the server clock, as elsewhere.

```sql
CREATE EXTENSION IF NOT EXISTS citext;

ALTER TABLE users
    ADD COLUMN email               citext UNIQUE,
    ADD COLUMN is_admin            boolean NOT NULL DEFAULT false,
    ADD COLUMN password_hash       text,           -- argon2id PHC string
    ADD COLUMN password_changed_at timestamptz;

-- External identities (OIDC). A user may have several.
CREATE TABLE user_identities (
    provider      text NOT NULL,                  -- a configured provider id
    subject       text NOT NULL,                  -- the provider's `sub`
    user_id       text NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    email_at_link citext NOT NULL,
    created_at    timestamptz NOT NULL,
    last_used_at  timestamptz,
    PRIMARY KEY (provider, subject)
);
CREATE INDEX user_identities_user_id ON user_identities (user_id);

-- WebAuthn credentials: public keys only.
CREATE TABLE user_passkeys (
    credential_id bytea PRIMARY KEY,
    user_id       text NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    passkey       jsonb NOT NULL,                 -- webauthn-rs serialized Passkey
    name          text NOT NULL,
    created_at    timestamptz NOT NULL,
    last_used_at  timestamptz
);
CREATE INDEX user_passkeys_user_id ON user_passkeys (user_id);

-- Console sessions. The cookie carries the id; only its SHA-256 is stored.
CREATE TABLE user_sessions (
    id_hash      bytea PRIMARY KEY,
    user_id      text NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    csrf_token   text NOT NULL,
    created_at   timestamptz NOT NULL,
    last_seen_at timestamptz NOT NULL,
    expires_at   timestamptz NOT NULL,            -- the absolute limit
    ip           inet,
    user_agent   text NOT NULL DEFAULT ''
);
CREATE INDEX user_sessions_user_id ON user_sessions (user_id);

-- Single-use emailed links. Only the SHA-256 of the token is stored.
CREATE TABLE email_tokens (
    hash       bytea PRIMARY KEY,
    user_id    text NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    purpose    text NOT NULL CHECK (purpose IN ('setup', 'reset')),
    created_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL,
    used_at    timestamptz
);

-- Sign-in audit, shown to the user and to admins.
CREATE TABLE sign_in_events (
    id         bigserial PRIMARY KEY,
    user_id    text REFERENCES users (id) ON DELETE CASCADE,
    method     text NOT NULL CHECK (method IN ('password', 'passkey', 'oidc', 'setup')),
    outcome    text NOT NULL CHECK (outcome IN ('ok', 'failed', 'locked')),
    ip         inet,
    at         timestamptz NOT NULL
);
CREATE INDEX sign_in_events_user_at ON sign_in_events (user_id, at DESC);

-- Per-user entitlements: catalogs beyond the user's teams' catalogs.
CREATE TABLE user_catalogs (
    user_id    text NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    catalog_id text NOT NULL REFERENCES catalogs (id) ON DELETE CASCADE,
    access     text NOT NULL CHECK (access IN ('read', 'write')),
    created_at timestamptz NOT NULL,
    PRIMARY KEY (user_id, catalog_id)
);

ALTER TABLE catalogs
    ADD COLUMN member_access text NOT NULL DEFAULT 'write'
        CHECK (member_access IN ('none', 'read', 'write'));

ALTER TABLE api_keys
    ADD COLUMN self_issued boolean NOT NULL DEFAULT false;

-- Policy settings edited in the console; absent keys fall back to config.
CREATE TABLE server_settings (
    key        text PRIMARY KEY,
    value      jsonb NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by text NOT NULL
);
```

The exact column names of `catalogs` and `api_keys` follow the tables as they are on `main` when the plan is written; the plan verifies them.

Deleting a user still follows M11's rule: refused while a credential names the user. Identities, passkeys, sessions, email tokens, entitlements and sign-in events cascade with the user.

### The ceiling

```
ceiling(user) = merge_max(
    { c: c.member_access  for c in catalogs owned by any team the user belongs to,
                          where c.member_access != 'none' },
    { e.catalog: e.access for e in user_catalogs(user) })
```

`merge_max` keeps the higher of `read` and `write`. An admin user's console session acts as an admin and has no ceiling. An admin user's self-issued keys are capped like anyone's.

### Effective access of a self-issued credential

For each catalog, a self-issued credential's access is the lower of its own grant and the ceiling. If either is missing, the catalog is not reachable. The key cache computes this when it loads. Every change that can alter a ceiling bumps `server_state.admin_generation`:
- team membership;
- a catalog's owner;
- a catalog's `member_access`;
- entitlements;
- a user's admin flag.

Running servers then reload within one poll interval. Admin-issued credentials (`self_issued = false`) keep their explicit grants unchanged.

## Signing in

All sign-in pages are rendered by the server, carry the existing CSP and use no inline script. Password, passkey and OIDC sign-in need `public_url` to be set (it supplies the cookie origin, the WebAuthn relying party id and the OIDC redirect URI). Without it, only pasted-key sign-in is offered.

### The sign-in page

`GET /signin` offers four options:
- **Email and password**, if password sign-in is enabled in Settings.
- **Sign in with a passkey.** This is a discoverable credential, so no email is typed first.
- **One button per configured OIDC provider.**
- **Use an API key instead.** Today's pasted-key console sign-in, unchanged.

It takes an optional `return_to`, limited to same-origin paths such as `/ui/...` and `/oauth/authorize?...`.

### Passwords

- Hashed with argon2id at OWASP's recommended parameters.
- At least 12 characters, and not in a bundled list of common passwords. There are no composition rules.
- Every failure, whether a wrong password, an unknown email or a locked account, answers 401 "Email or password is incorrect". The response also takes the same time: a dummy hash is checked for unknown emails.
- **Lockout:** an account locks for 15 minutes after 10 consecutive failures.
- **Per-IP limit:** 30 attempts per 15 minutes per IP, using the existing rate-limit machinery.

### Passkeys

- Use the `webauthn-rs` crate. The relying party id is `public_url`'s host and the origin is `public_url`. User verification is required.
- Registration happens on the setup page or the account page. Sign-in uses discoverable authentication.
- The challenge state is kept server-side in a short-lived row or cache entry, keyed by a cookie that is good for 5 minutes and single-use.
- The stored `sign_count` is checked and updated on every sign-in.

### OIDC

Providers are configured in the config file:

```toml
[[auth.oidc]]
id = "example"                         # stable; stored in user_identities.provider
label = "Example ID"                   # the button text
issuer = "https://id.example.com"
client_id = "okf-console"
client_secret_env = "OKF_OIDC_EXAMPLE_SECRET"
scopes = ["openid", "email", "profile"]
```

- **Flow:** the authorization code flow with PKCE (S256), `state` and `nonce`, using the `openidconnect` crate.
- **Redirect URI:** `{public_url}/auth/oidc/{id}/callback`.
- **Flow state:** `state`, `nonce`, the PKCE verifier and `return_to` sit in a 10-minute, single-use cookie bound to a server-side row.
- **Discovery:** cached for 1 hour. A failed refresh keeps the cached document.

On the callback, after the ID token has been validated (issuer, audience, nonce, expiry), the user is found as follows:
1. If `(provider, sub)` is in `user_identities`, that user signs in.
2. Otherwise, if the token's `email_verified` is true and `email` matches a user's `email` case-insensitively, the identity is linked and the user signs in. The user gets a notice email.
3. Otherwise sign-in is refused with "No account for this email. Ask an admin to invite you."

A signed-in user can also link a provider from the account page, which records the identity whatever its email. They can unlink one too, unless it is their last sign-in method.

### Sessions

- **Cookie:** `okf_session` holds 256 random bits, base64url, with `HttpOnly; Secure; SameSite=Lax; Path=/`. Only the SHA-256 of the id is stored.
- **Lifetime:** 12 hours idle and 7 days absolute by default; both are settings.
- **Activity:** `last_seen_at` is written at most once a minute per session.
- **Rotation:** a new session id is issued at sign-in and whenever the user's admin flag changes.
- **CSRF:** every state-changing request authenticated by the cookie must carry `X-OKF-CSRF` equal to the session's `csrf_token`; otherwise it gets 403. The console reads the token from `GET /api/v1/session`.
- **Bearer and cookie:** a request with a bearer key is authenticated by the key, and the cookie is ignored.
- **Revocation:** sign-out deletes the session. Changing a password, adding a passkey or linking an identity revokes the user's other sessions by default; the form has a checkbox to keep them. Admins can revoke any user's sessions.

### The principal

`Principal` gains a session-backed variant. It carries the user id, name, admin flag and, for non-admins, the ceiling, computed per request from the cache.

A non-admin session can:
- reach, through the API, exactly the catalogs in the ceiling, at the ceiling's level;
- manage its own keys and grants.

An admin session behaves as an admin key does today. MCP (`/mcp`) still accepts only bearer credentials.

## Mail

```rust
#[async_trait]
pub trait Mailer: Send + Sync {
    async fn send(&self, message: &Mail) -> Result<(), MailError>;
}
```

`Mail` holds `to`, `subject`, `text` and `html`. Three providers ship:

```toml
[mail]
provider = "smtp"            # "smtp" | "postmark" | "log"
from = "okf <okf@example.com>"

[mail.smtp]
host = "smtp.example.com"
port = 587
starttls = true
username = "okf"
password_env = "OKF_SMTP_PASSWORD"

[mail.postmark]
token_env = "OKF_POSTMARK_TOKEN"
message_stream = "outbound"
```

- **SMTP:** uses `lettre`.
- **Postmark:** calls its `/email` HTTP endpoint with `reqwest`.
- **`log`:** writes the message, link included, to the log at `info`. It is meant for development and tests.

Mail is sent by a background job, never inside the request; see "Background jobs".

The templates are plain text with a minimal HTML twin:
- `setup`: "Set up your okf account" (link valid 24 hours by default);
- `reset`: "Reset your password" (1 hour);
- `method_added`: "A sign-in method was added to your account", sent after a password change, a new passkey or a linked identity;
- `test`: the Settings page's test email.

## Background jobs

New asynchronous work runs on [apalis](https://crates.io/crates/apalis) 1.x, with `apalis-postgres` storing its jobs in the server's own Postgres database.

- **Schema.** `apalis-postgres` creates and migrates its own `apalis` schema. It records its migrations in `apalis._sqlx_migrations`, apart from okf's `_sqlx_migrations`, so the two migrators never see each other's rows. `db::migrate` runs okf's migrations, then apalis's `PostgresStorage::setup`.
- **Extensions.** Its migrations create `pgcrypto` and `hstore`; migration 0010 creates `citext`. All three are trusted extensions, so the database owner can create them; no superuser is needed.
- **Workers.** Workers run inside the server process, started with the HTTP server and stopped on shutdown. Several replicas may run workers, because apalis claims jobs with row locks.
- **The first job is `SendMail`.** Its payload is the rendered `Mail`. It retries with exponential backoff for up to 24 hours, then fails and keeps its last error. A request that sends mail enqueues the job, after its own transaction commits, and returns.
- **Secrets in payloads.** A setup or reset link is the only secret a mail job holds. It is single-use and short-lived. Finished mail jobs are deleted after 7 days, and failed ones after 30.
- **Visibility.** The Settings page lists mail jobs that are pending or failed (recipient, template, attempts, last error, but never the body), with Retry and Delete. `okf-mcp mail queue` shows the same list.
- **Existing loops stay.** The existing background loops (the poll loop and token and code cleanup) are unchanged in this milestone. Moving them onto apalis is a follow-up.

### Emailed links

The token is 256 random bits, and only its SHA-256 is stored. A link is single-use and works only until it expires. Issuing a new link of the same purpose for a user invalidates the older one.

- **Setup.** An admin sets a user's email and clicks Send setup email (`okf-mcp user invite NAME`). The link opens `/account/setup`. There the person chooses a password, registers a passkey, or both, and is signed in.
- **Reset.** On `/signin/forgot` anyone may enter an email. The response is always "If an account exists, we sent a link". It is limited to 3 requests per email per hour and 20 per IP per hour. The link opens a set-password page. Using it revokes all of the user's sessions.

## Self-service

A signed-in non-admin user sees these console pages:
- **My keys:** list, create, rename and revoke their own keys. Creating a key means choosing catalogs and read or write for each, all within the ceiling, plus an optional expiry. The secret is shown once.
- **My grants:** list and revoke their OAuth grants.
- **Browse, Search and Health** for catalogs in their ceiling.
- **Account:**
  - change password;
  - passkeys: add, rename, remove;
  - linked identities: link, unlink;
  - active sessions, with revoke;
  - recent sign-ins;
  - teams and ceiling, read-only.
  The page refuses to remove the last sign-in method (409).

These rules apply to self-issued keys:
- never admin, never a maintenance reporter;
- at most `max_keys_per_user` live keys (default 20);
- an expiry of at most `max_key_lifetime`, or none if that setting is unset;
- the key's `user_id` is the signed-in user, and `actor` defaults to its name, as today;
- creating one above the ceiling or past the key limit answers 422, naming the limit.

The API enforces every rule; the console only hides what the user can't use.

## OAuth consent as the user

1. **No session:** `GET /oauth/authorize` sends the person to `/signin?return_to=<the authorize URL>`.
2. **Signed in:** the consent page lists the client and the user's ceiling. The person ticks catalogs, chooses read or write within the ceiling, picks a default catalog and approves.
3. **The grant:** an `api_keys` row of kind `oauth` with `self_issued = true` and the user's id. Its actor comes from the client's name.
4. **Sign-in codes:** "Use a sign-in code instead" keeps the admin-minted code path. Those grants are admin-issued and uncapped, as in M10 and M11.
5. **Guards:** M10's consent-integrity guards (the summary is bound to what is approved, single use, CSRF) apply to the new form.

Lowering the ceiling later lowers the grant's access at the next cache reload.

## Admin surface

Every admin action has a CLI command, a REST route and a console control under **Administer**. `docs/operations.md`'s parity table and `tests/admin_parity.rs` gain these rows.

| Action | CLI | Console |
|---|---|---|
| Admin flag | `user set-admin NAME --on\|--off` | User drawer: Admin toggle |
| Email | `user set-email NAME EMAIL` (`--clear`) | User drawer: email field |
| Setup email | `user invite NAME` | User drawer: Send setup email |
| Entitlements | `user entitle NAME CATALOG --access read\|write`, `user unentitle NAME CATALOG` | User drawer: Entitlements section |
| Sessions | `user sessions NAME`, `user revoke-sessions NAME` | User drawer: Sessions list, revoke one or all |
| Sign-in history | `user sign-ins NAME` | User drawer: Recent sign-ins |
| Member access | `catalog set-member-access NAME --access none\|read\|write` | Catalog drawer: Team member access |
| Policy settings | `settings list`, `settings set KEY VALUE`, `settings reset KEY` | Settings page |
| Test email | `mail test ADDRESS` | Settings page: Send test email |
| Mail queue | `mail queue`, `mail retry ID`, `mail delete ID` | Settings page: Mail queue, with Retry and Delete |

- `user set-admin --off` is refused (409) for the last admin user, unless an admin key performs it.
- `may_set_member_access(principal, catalog)` is the single permission check. It allows admins today, and team owners later.
- Writes need an admin that reaches every catalog, as in M11.

### Settings

| Key | Default | Meaning |
|---|---|---|
| `password_sign_in` | `true` | Offer email and password sign-in |
| `max_keys_per_user` | `20` | Live self-issued keys per user |
| `max_key_lifetime` | unset | Longest self-issued key expiry; unset means no limit |
| `session_idle` | `12h` | Console session idle timeout |
| `session_absolute` | `7d` | Console session absolute timeout |
| `setup_link_ttl` | `24h` | Setup link lifetime |
| `reset_link_ttl` | `1h` | Reset link lifetime |

- The config file's `[auth]` table supplies the defaults, and `server_settings` overrides them.
- Each change bumps `admin_generation`.
- The Settings page shows the configured OIDC providers (id, label, issuer) and the mail provider and From address, read-only. Secrets are never displayed.

## Errors

| Case | REST | Console |
|---|---|---|
| Wrong password, unknown email, locked account | 401 | "Email or password is incorrect" |
| OIDC: no user for this identity or verified email | 403 | "No account for this email. Ask an admin to invite you." |
| OIDC: invalid state, nonce or token | 400 | "Sign-in failed; try again" |
| Expired, used or unknown emailed link | 410 | "This link has expired. Ask for a new one." |
| Cookie write without a matching `X-OKF-CSRF` | 403 | (a reload signs the console in again) |
| Self-issued key above the ceiling, past the key limit or over the expiry limit | 422 | Names the limit |
| Removing the last sign-in method; clearing the last admin user | 409 | Says why |
| Duplicate email | 409 | "Another user has this email" |
| A non-admin session on an admin route | 403 | (the route is hidden) |

## Logging and audit

- Every sign-in attempt writes a `sign_in_events` row and an `info` log line naming the user id, method, outcome and IP. Secrets, passwords and tokens are never logged.
- Admin changes are logged at `info`, naming the acting principal: admin flag, email, entitlements, member access, settings, sessions revoked.
- Events older than 90 days are pruned with the existing cleanup task.

## Testing

- **Store unit tests** for every new function, including:
  - the ceiling: team union, `none`, the higher level winning, entitlements;
  - capping on cache reload after each kind of ceiling change.
- **The migration:** existing users, keys and catalogs survive unchanged. `member_access` is `write`, and every existing key has `self_issued = false`.
- **HTTP tests** for every route and every error in the table above.
- **Sessions:**
  - idle and absolute expiry;
  - rotation at sign-in and on an admin-flag change;
  - revoke;
  - the CSRF rule;
  - a bearer key winning over a cookie.
- **Passwords:**
  - hashing and verification;
  - the length rule and the common-password list;
  - lockout and per-IP limits;
  - the same error for an unknown email.
- **Passkeys:** registration and sign-in with `webauthn-rs`'s software authenticator, including sign-count handling.
- **OIDC,** against an in-process mock provider (discovery, JWKS, token endpoint):
  - linking by verified email;
  - refusing an unverified email;
  - a returning `(provider, subject)` after an email change;
  - a bad nonce or state.
- **Mail:**
  - each template sent through the `log` provider, with its link usable;
  - the `SendMail` job through apalis on the test database: retry after a provider error, failure after the retry budget, Retry from the queue;
  - the migration test: both migrators apply cleanly to an empty database and to one at schema 0009, and re-running is a no-op;
  - a Postmark request shape against a mock HTTP server.
- **Consent as a signed-in user:**
  - the grant is capped;
  - lowering the ceiling lowers its access;
  - the sign-in code path is unchanged.
- **Console journeys** (rustwright):
  - setup link, then password, then sign-in, then create a key within the ceiling, then refused above it;
  - an admin entitles a catalog and the user sees it;
  - member access `read` makes a team catalog read-only;
  - account page: change password, add and remove a passkey, revoke a session;
  - Settings change and test email;
  - a non-admin console shows no Administer section;
  - an admin user signs in and sees the full console.
- **The admin parity test,** extended to every new command.

## Rollout

- Migration 0010 only adds. Until an admin acts, nothing changes for existing users, keys, grants, codes or catalogs, and pasted-key sign-in keeps working.
- First steps after deploying:
  1. `okf-mcp user set-email <you> <address>`
  2. `okf-mcp user set-admin <you> --on`
  3. `okf-mcp user invite <you>`
  4. Configure `[mail]` and, if wanted, `[[auth.oidc]]`.
- Team catalogs default to `member_access = write`. As members start minting self-issued keys, review shared catalogs and set `read` where intended.

## Future work (to file as issues)

1. **Team owners manage member access and entitlements** for catalogs their team owns, through `may_set_member_access`.
2. **OIDC providers configured in the database**, with encrypted client secrets.
3. **Multi-factor authentication** for password sign-in (TOTP or passkey as a second factor).
4. **Self-service account deletion**, once credential history no longer blocks deletion (#297).
5. **Move the existing background loops onto apalis**: token, code and session cleanup, and sign-in event pruning.

## What shipped differently

Read the plan's "Decisions taken while planning" and its `Corrected (issue
#N)` and `Superseded (issue #N)` notes for the detail; the per-task
changelogs (`changelog/*-m12-task-*`) give the reasons.

### From the plan's decisions

- **Per-catalog levels on keys** (plan Decision 1). `key_catalogs` gained a
  nullable `access` (NULL: the key's own level), and `Principal`'s reach
  became a level per catalog, so a self-issued key holds read on one catalog
  and write on another, and capping lowers each catalog separately.
- **`user_catalogs.catalog_id` is `bigint`** (Decision 2), as `catalogs.id`
  is; the data model above wrote `text`.
- **Self-issued keys name their catalogs** (Decision 3): "all catalogs" is
  refused (422), and a CHECK keeps self-issued keys from `admin`,
  maintenance reporting and all catalogs.
- **Key expiry is a column** (Decision 4): `api_keys.expires_at`, checked at
  lookup as well as on cache load. Only self-issued keys get one.
- **A console session is a principal** (Decision 5):
  `CredentialKind::Session` with `key_id = "session:<user>"`, which can
  never be a key id. A session owns no changesets (`open_changeset` refuses
  it). `whoami` answers for it with `kind: "session"`.
- **Three error variants** (Decision 6): `Unauthenticated` (401), `Gone`
  (410) and `LimitExceeded` (422, naming the limit).
- **Client addresses behind a proxy** (Decision 7): a new
  `[auth] trusted_proxy_header`; without it the peer address is used and no
  header is read.
- **Limits** (Decision 8): per-address and per-email limits are in memory,
  per process, as M10's are; account lockout is durable
  (`users.failed_sign_ins`, `users.locked_until`).
- **Setting names** (Decision 9): `console_session_idle` and
  `console_session_absolute`, not `session_idle` and `session_absolute`,
  which would read as MCP's `session_idle_timeout`.
- **Mail retry** (Decision 11): 30 s doubling to 1 h for 24 hours from when
  the mail was queued; a permanent refusal fails at once; the worker polls
  every 30 s as well as listening, so manual retries are picked up.
- **OIDC over the existing HTTP client** (Decision 12): `openidconnect`
  without default features, through an adapter over the crate's `reqwest`
  0.13, so no second `reqwest` is linked.
- **Passkey ceremony state and OIDC flow state are in memory** (Decision
  13), each keyed by a cookie (5 minutes and 10 minutes, single use), not a
  database row. Passkeys are off when `public_url`'s host is an address.
- **Sign-in pages may run script** (Decision 14): they share `http::pages`
  with consent, but use the console's CSP (`script-src 'self'`), since the
  passkey button needs WebAuthn; consent keeps its no-script CSP.
- **The `Secure` cookie flag follows `public_url`'s scheme** (Decision 15),
  so a loopback `http` test server works; the design said `Secure` always.
- **The last admin user** (Decision 16): the API refuses clearing it for any
  principal, admin keys included; the CLI may, as operator access. The
  design let an admin key do it.
- **One route per admin action** (Decision 17): entitle and unentitle,
  retry and delete, set and reset each have their own route, so the parity
  test keeps one route per row.
- **An admin flag change signs the user out everywhere** (Decision 20,
  `users::set_admin`): instead of rotating the session id, every session of
  the user is deleted, so the next request signs in again with the new flag.
- **Linking an account from the console** goes through
  `POST /api/v1/me/identities/{provider}/link`, which answers the
  provider's URL, so the start of the link flow carries the CSRF token.
- **The console reads `GET /api/v1/session` beside `whoami`** (Decision
  22): the session gives the CSRF token, admin flag and ceiling
  (`{catalog: level}`), `whoami` the catalogs, as for keys. Sign-out is
  `DELETE /api/v1/session`, behind the CSRF check, not a separate sign-out
  page.
- **Passkey registration from the Account page** is
  `POST /api/v1/me/passkeys/register/start` and `/finish` (Decision 23),
  beside the setup page's `POST /auth/passkey/setup/start` and `/finish`.
- **No "sign out everywhere else" for oneself**: the Account page signs
  sessions out one at a time (`DELETE /api/v1/me/sessions/{id_prefix}`),
  this browser's included; admins still sign a user out everywhere.
- **Mail is queued inside the transaction it reports**, not after it
  commits: a setup link, and the "method added" notice, are queued in the
  same transaction as the change, so nothing that can block or fail runs
  after a sign-in or change commits.

### From the plan's corrections, by issue

- **#309 (schema).** apalis resolved to 1.1.1 and apalis-postgres to 1.1.0
  (API unchanged). Test databases get apalis' schema from the per-server
  template in `tests/support/pg.rs`, not from `db::migrate`. `apalis_error`
  maps every variant, with no catch-all.
- **#310 (key levels).** The self-issued rules hold after creation too:
  turning maintenance reporting on for a self-issued key is refused (422,
  `limit: "maintenance_report"`).
- **#311 (ceiling).** Every existing write to a ceiling's inputs already
  bumped `admin_generation`; one test walks each of them, and later writes
  to those tables must join it.
- **#312 (passwords and links).** The dummy hash is a constant PHC string,
  so the first unknown email costs no more than the rest. The success and
  failure updates re-check the stored hash and the lock under the row, so
  concurrent failures all count and a lock is never lost. Issuing a link
  locks the user `FOR NO KEY UPDATE`, so two issues leave one live link.
- **#313 (settings and sessions).** The CSRF token is derived from the
  session id and only its hash is stored, so `user_sessions` holds no
  secret. `touch` cannot revive an ended or revoked session, and writes at
  most once per `min(1 minute, idle / 4)`, not once a minute, so a short
  idle limit still works (an idle session ends between `idle` and `idle`
  plus that interval). Every `[auth]` and `[oauth]` lifetime is capped at
  ten years. `User-Agent` is kept to 256 characters.
- **#314 (mail).** The worker runs on its own pool (at most 4 connections,
  `application_name` `okf-mail-worker`) beside `database_max_connections`,
  and restarts after a backend error under a new worker name. `[mail]`
  gained `worker` (default `true`), and `from` defaults to a valid
  placeholder (`okf <okf@localhost>`) so a server without `[mail]` starts.
- **#315 (console sessions).** A session's admin level and ceiling come
  from the key cache on every request, never from the sign-in. The limiter
  is a token bucket whose find-or-create is one step, and IPv6 addresses
  share a bucket per `/64`. `users::set_email` bumps the generation, since
  the cache holds every user's email. The `/api/v1` copies of export and
  import take a session; `/mcp` and the archive routes stay bearer-only.
- **#316 (sign-in pages).** A malformed `POST /signin` is the same 401 as a
  wrong password. Every form POST refuses another site (`Sec-Fetch-Site`,
  403), because `Origin` is `null` under `no-referrer`. A password sign-in
  carries the hash it matched into the session insert, so no session
  outlives a reset; a reset, a setup and its session commit in one
  transaction, user row first. Email text Postgres would refuse is treated
  as an unknown email. The request span records no query (later, no literal
  path: see #322). Without `public_url`, `POST /signin` is 404.
- **#317 (passkeys).** Passkey sign-in verifies and starts the session in
  one transaction holding the user and the passkey rows, so the counter
  never goes back and no removal or reset slips between. Every sign-in path
  starts its session through one function in its own transaction, and
  records the success event and queues the notice there too. Ceremonies are
  bounded per browser and per session, and the open routes refuse other
  sites and take the per-address limit.
- **#318 (OIDC).** An OIDC sign-in is one transaction: find or link, end
  the old session, start the new one, stamp, record. A link callback needs
  the live session that started it. Each browser holds one flow and each
  session one link flow, and link starts are limited per user (a burst of
  10, one a minute). A user links at most one account per provider. A
  malformed `sub` is a bad token (400).
- **#319 (self-service keys).** `create_self_issued` gets every check
  `create_with_id` has, and counts live keys under the user's row, so
  concurrent creates never pass the limit.
- **#320 (self-service account).** A password change checks, sets, signs
  other sessions out and queues the notice in one transaction, and a wrong
  current password is 403, not 401. An ambiguous session prefix is 409
  (nothing ended). A passkey rename refuses a bad name with 422
  (`limit: "passkey_name"`), not 400.
- **#321 (consent).** The grant insert locks the user and the approving
  session and re-reads the ceiling. The POST is checked against the
  ceiling the page offered, each page approves once, and another site's
  POST is refused. A changed ceiling shows "Your access changed while this
  page was open".
- **#322 (admin REST).** The user view keeps `is_admin` (not `admin`). The
  setup link is issued and queued in one transaction under the user's row.
  "Sign out everywhere" locks the user first, so an uncommitted session is
  ended too. A malformed name or id in a path is 404, not 500, on the new
  routes and the user lookups they share (#339 tracks the older team,
  catalog and key routes). An invalid setting or test address is
  422. The request span records the matched route template, never the
  literal path (`MatchedPath`).
- **#323 (admin CLI).** `settings set|reset` take the key as a clap value
  list (an unknown key lists the valid ones). `settings` and `mail` stay
  out of the users-and-teams command table; the parity table covers them.
  `user invite` and `mail test` only queue, so they need no mail secret.
- **#324 (console, My keys).** `authHeaders` was already shared from Task
  7b, so the console has one place that picks a request's credentials. The
  stray `null` on four pages (#302) was fixed here.
- **#325 (console, My grants and Account).** The ceiling is read from
  `GET /api/v1/me` on every load, never from the sign-in snapshot. A
  passkey credential that does not verify at registration is **422**
  (`limit: "passkey"`), not 401, so the console does not read it as signed
  out; passkey sign-in keeps its 401. Refusals show the server's reason.
- **#326 (console, admin side).** Every write control is disabled while its
  write runs, tracked by a stable key (`core/pending.js`), so a redrawn
  control cannot send it twice. A refusal's reason stays beside its control,
  even when the follow-up read fails. Overlapping reads draw only the
  newest answer, and a settled write redraws the drawer showing its object
  now. The mail queue's statuses are `pending`, `sending`, `retrying` and
  `failed`. Issue #351 tracks the same overlapping-read fix for the users,
  teams and catalogs tables.
