# okf-mcp user login: design

Status: approved in brainstorming, 2026-11-12. This is milestone 12, and it closes issues #298 (login) and #299 (access through team membership).

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

-- Outgoing mail, retried until sent or given up.
CREATE TABLE mail_outbox (
    id            bigserial PRIMARY KEY,
    to_address    citext NOT NULL,
    template      text NOT NULL,
    payload       jsonb NOT NULL,                 -- never a secret other than the link
    created_at    timestamptz NOT NULL,
    attempts      integer NOT NULL DEFAULT 0,
    next_try_at   timestamptz NOT NULL,
    sent_at       timestamptz,
    last_error    text
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

A request that sends mail writes a `mail_outbox` row in the same transaction and returns. A background task delivers outbox rows, retrying with backoff for 24 hours, then gives up and records `last_error`. The Settings page shows undelivered mail.

The templates are plain text with a minimal HTML twin:
- `setup`: "Set up your okf account" (link valid 24 hours by default);
- `reset`: "Reset your password" (1 hour);
- `method_added`: "A sign-in method was added to your account", sent after a password change, a new passkey or a linked identity;
- `test`: the Settings page's test email.

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
  - outbox retry and give-up;
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
