# okf-mcp OAuth 2.1 and directory readiness: design

Status: approved in brainstorming, 2026-11-11; delivered in milestone 10
(epic #244, plan `docs/superpowers/plans/2026-11-11-okf-mcp-m10-oauth.md`).
Where the plan deviated, this document says so in place ("Plan deviation
N", numbered as in the plan's "Simplifications and deviations"; "Plan
decision N" for its "Decisions taken while planning"), and "What M10 shipped
differently" at the end lists the rest. The code is the reference.

## Goal

Let OAuth-only MCP clients (ChatGPT custom connectors, the ChatGPT and Claude
connector directories) connect to `okf-mcp`, while static `okf_` API keys keep
working unchanged. At the same time, give every tool the metadata the
connector directories ask for.

Two independent pieces, designed together and delivered as separate PRs:

1. **Tool annotations**: a `title` and explicit hints on every tool.
2. **OAuth 2.1**: `okf-mcp` becomes an OAuth protected resource with a
   built-in authorization server, following the MCP authorization
   specification 2026-07-28.

## Non-goals (this round)

- An external identity provider. The resource side validates tokens behind a
  `BearerVerifier` trait so a JWT issuer (JWKS) can be added later as another
  implementation; nothing else about it is designed here.
- Users, passwords, OIDC, `openid` scope or ID tokens.
- Confidential clients (`client_secret_*`, `private_key_jwt`). Only public
  clients (`token_endpoint_auth_method: none`) with PKCE.
- OAuth grants with `admin` access or the maintenance-report flag. The
  console and the maintenance runner keep using static keys.
- Narrowing a grant on the consent page. The admin sets the grant when minting
  the sign-in code.
- Multiple replicas. The server stays single-replica (`docs/operations.md`),
  which lets short-lived OAuth state live in memory.

## Decisions

| Question | Decision |
|---|---|
| Authorization server | Built in to the `okf-mcp` binary; token validation behind a `BearerVerifier` trait so an external issuer can be added later. |
| Sign-in on the consent page | An admin-issued sign-in code (`okfsc_...`) bound to a grant template: catalogs, access (`read` or `write`), default catalog, actor. |
| Sign-in code lifetime | Single use; expires unused after 24 hours by default, at most 7 days. One code yields one grant (one client connection). |
| Grant storage | A grant is an `api_keys` row with `kind = 'oauth'`, so `Principal`, `key_catalogs`, revocation, session binding, `changesets.key_id` and the console's key list all apply unchanged. |
| Token format | Opaque random tokens, stored only as SHA-256: `okfat_` (access, 1 hour), `okfrt_` (refresh, 30 days sliding, rotated on use). No JWTs. |
| Client registration | Client ID Metadata Documents (preferred by the spec) and RFC 7591 dynamic client registration (deprecated MAY, still used by some clients). |
| When OAuth is on | Only when `public_url` (HTTPS) is configured: issuer and resource URLs must be the externally visible origin, not the listen address. Its host must be in `allowed_hosts` (Plan decision 7). |
| Consent page | Server-rendered HTML with Pico CSS and a plain form; no JavaScript. |

## Spec conformance

What the MCP authorization specification (2026-07-28) and the clients need,
and where this design provides it:

| Requirement | Where |
|---|---|
| Protected Resource Metadata (RFC 9728) | `GET /.well-known/oauth-protected-resource` |
| `WWW-Authenticate: Bearer resource_metadata="..."` on 401 | the bearer middleware |
| Authorization Server Metadata (RFC 8414) | `GET /.well-known/oauth-authorization-server` |
| PKCE, S256 only | `/oauth/authorize` requires it; `/oauth/token` verifies it |
| Resource indicators (RFC 8707), audience check | `resource` required at authorize and token; stored on every token; checked on every request |
| Client ID Metadata Documents | `client_id_metadata_document_supported: true`; HTTPS `client_id` fetched and validated |
| Dynamic client registration (RFC 7591) | `POST /oauth/register` |
| Authorization server issuer identification (RFC 9207) | `iss` on the authorization response |
| Token revocation (RFC 7009) | `POST /oauth/revoke` |
| Exact redirect URI matching (OAuth 2.1) | `/oauth/authorize` and `/oauth/token` |

## Architecture

```
MCP client ── POST /mcp (no token) ────────────► 401 + resource_metadata URL
           ── GET /.well-known/oauth-protected-resource ─► { resource, authorization_servers }
           ── GET /.well-known/oauth-authorization-server ─► AS metadata
           ── POST /oauth/register (DCR clients only) ────► client_id
browser    ── GET /oauth/authorize ─► consent page (step 1: sign-in code)
           ── POST /oauth/authorize ─► step 2: what will be granted ─► Approve
           ◄── 303 redirect_uri?code&state&iss
MCP client ── POST /oauth/token ───────────────► okfat_ + okfrt_
           ── POST /mcp (Bearer okfat_...) ─► BearerVerifier ─► Principal ─► tools
```

New module `src/oauth/` (as shipped):

- `metadata.rs`: the two well-known documents.
- `clients.rs`: DCR registration, the CIMD fetcher and cache, client lookup.
- `authorize.rs`: the authorize endpoint and the consent form's steps;
  `consent.rs`: the consent and error pages, their CSP and the one escaping
  helper (Plan decision 9); `pkce.rs`: S256.
- `token.rs`: the token and revoke endpoints, minting and rotation.
- `access.rs`: the access-token cache and `OAuthVerifier`.
- `state.rs`, `expiring.rs`, `ratelimit.rs`: the in-memory pending
  authorizations and authorization codes (bounded expiring maps) and the
  token buckets.
- `error.rs`: `OAuthError` (`thiserror`) with its own `IntoResponse`.

Sign-in codes have no `src/oauth/codes.rs`: their logic is in the store.
`src/auth.rs` gains the `BearerVerifier` trait and its dispatch;
`src/store/oauth/` (`clients.rs`, `codes.rs`, `tokens.rs`) holds the
queries; the CLI gains `okf-mcp oauth ...`; the console's Keys page gains a
sign-in codes panel.

## Endpoints

All are served by the existing binary on its existing port, and are mounted
only when OAuth is on (Plan decision 6). `<public_url>` below is the
configured origin; the resource identifier is `<public_url>/mcp`. Every
response under these routes carries `Cache-Control: no-store`, the discovery
documents included (Plan deviation 12), and the two discovery documents also
carry `Access-Control-Allow-Origin: *`.

### `GET /.well-known/oauth-protected-resource`

Also served at `/.well-known/oauth-protected-resource/mcp` (the
path-suffixed form RFC 9728 clients try first). No authentication.

```json
{
  "resource": "<public_url>/mcp",
  "authorization_servers": ["<public_url>"],
  "bearer_methods_supported": ["header"],
  "resource_name": "okf-mcp"
}
```

### `GET /.well-known/oauth-authorization-server`

No authentication.

```json
{
  "issuer": "<public_url>",
  "authorization_endpoint": "<public_url>/oauth/authorize",
  "token_endpoint": "<public_url>/oauth/token",
  "registration_endpoint": "<public_url>/oauth/register",
  "revocation_endpoint": "<public_url>/oauth/revoke",
  "response_types_supported": ["code"],
  "grant_types_supported": ["authorization_code", "refresh_token"],
  "code_challenge_methods_supported": ["S256"],
  "token_endpoint_auth_methods_supported": ["none"],
  "revocation_endpoint_auth_methods_supported": ["none"],
  "client_id_metadata_document_supported": true,
  "authorization_response_iss_parameter_supported": true
}
```

`registration_endpoint` is left out when `dcr_enabled = false`.

### `POST /oauth/register` (RFC 7591)

- Accepts JSON client metadata, which must be a JSON object (an array is
  refused before deserializing, since serde would read one in field order).
  Requires `redirect_uris` (1 to 10, each at most 2,000 characters, `https`,
  or `http` on a loopback host for native clients). `token_endpoint_auth_method`
  must be absent or `none`. `grant_types` must be a subset of
  `authorization_code` and `refresh_token`; `response_types` of `code`.
  `client_name`, if sent, is 1 to 200 characters. The body is capped at 16
  KiB. Errors are `invalid_client_metadata` or `invalid_redirect_uri`.
- Stores an `oauth_clients` row (`kind = 'dcr'`, `client_id` a fresh UUID
  that Postgres picks) and answers 201 with the registered metadata and
  `client_id`. No `client_secret`; `client_name` is echoed only when sent.
- Refused with `slow_down` (429) beyond the rate limit, or when 500 DCR
  clients without a grant already exist. The cap check and the insert are
  one statement in a transaction holding an advisory lock
  (`pg_advisory_xact_lock(0x6f6b666f, 1)`), so concurrent registrations, from
  any process, cannot pass it.
- With `dcr_enabled = false` the route answers 404.

### `GET /oauth/authorize`

Parameters: `response_type=code`, `client_id`, `redirect_uri`, `state`,
`code_challenge`, `code_challenge_method=S256`, `resource`, optional `scope`
(accepted and ignored: the grant is set by the sign-in code).

1. Resolve the client: an `https://` `client_id` is a CIMD URL (see "Client
   ID Metadata Documents"); anything else must be a DCR `client_id`.
2. `redirect_uri` must exactly equal one of the client's `redirect_uris`.
3. **Until steps 1 and 2 pass, errors render an error page; never redirect.**
   This prevents an open redirect.
4. Once they pass, errors redirect to `redirect_uri` with `error`,
   `error_description`, `state` and `iss`: a missing or non-S256
   `code_challenge` is `invalid_request`; a `resource` other than
   `<public_url>/mcp` is `invalid_target`; any other `response_type` is
   `unsupported_response_type`.
   The shipped order is `response_type`, then `code_challenge`, then
   `code_challenge_method`, then `resource`; the redirect is a 303 (Plan
   deviation 17).
5. Store a pending authorization in memory and render consent step 1. A
   full pending map (10,000 entries) answers a 503 error page instead (Plan
   deviation 8).

### `POST /oauth/authorize`

The consent form's target. Fields: `pending` (the pending authorization id),
`csrf`, `action` (`code`, `approve` or `deny`), `sign_in_code` (step 1 only),
`summary` (Approve only: the nonce of the step-2 page it came from; Plan
deviation 18). See
"Consent flow".

### `POST /oauth/token`

Form-encoded, per RFC 6749. Responses are `Cache-Control: no-store`, and a
success also `Pragma: no-cache`. Shares a rate limit with `/oauth/revoke`.

- `grant_type=authorization_code`: requires `code`, `client_id`,
  `redirect_uri`, `code_verifier` and `resource`. The code must be live,
  unused and issued to that client with that redirect URI and resource;
  `BASE64URL(SHA256(code_verifier))` must equal its `code_challenge`
  (compared in constant time). A code presented twice revokes every token
  minted from it (RFC 6749 section 4.1.2), for any interleaving: the code is
  marked used under the map's lock, a replay sets its `replayed` flag, and
  the exchange checks that flag after its tokens commit, revoking them if it
  is set or the code has left the map (Plan deviation 14). The code is
  marked used before its bindings are checked, so a failed exchange (wrong
  verifier, client, redirect URI or resource) spends it, and presenting it
  again is a replay (Plan deviation 15). Answers `access_token` (`okfat_`),
  `token_type: Bearer`, `expires_in`, `refresh_token` (`okfrt_`).
- `grant_type=refresh_token`: requires `refresh_token` and `client_id`;
  `resource`, if given, must equal the token's. The refresh token must be live
  and belong to that client's grant. It is marked used and a new access and
  refresh token are minted in the same token family. **A refresh token that
  has already been used revokes the whole family** (reuse detection). A
  refresh by another client (`invalid_grant`) or for another `resource`
  (`invalid_target`) is refused and rolled back, so the rightful client can
  still use the token.
- Every transaction that adds tokens to a family (a rotation, before it
  reads its token; the code exchange, before it mints) or revokes one takes
  a transaction-scoped advisory lock on the family
  (`store::oauth::tokens::lock_family`: namespace `0x6f6b6666`, "okff", key
  `hashtext(family_id)`). Under read committed, a revocation's `UPDATE`
  cannot see a concurrent rotation's uncommitted successors; with the lock
  it runs wholly before or after the rotation (Plan deviation 16).
- A revoked grant, or one whose client no longer exists, is `invalid_grant`.

### `POST /oauth/revoke` (RFC 7009)

Form-encoded `token` (access or refresh). `client_id` and `token_type_hint`
may be sent and are ignored. Revokes that token; revoking a refresh token
revokes its family, while revoking an access token leaves the refresh token
working. Always 200, including for an unknown token; 400 `invalid_request`
without `token`.

The endpoint does not check that the token belongs to the requesting client
(Plan decision 11). RFC 7009 says it should, but clients are public (they do
not authenticate) and a token carries no client id: holding a token already
allows everything revoking it takes away.

## Client ID Metadata Documents

A `client_id` that is an `https://` URL is fetched as a JSON client metadata
document.

- **Fetch hardening:**
  - `https` only;
  - no redirects;
  - a 5-second timeout and a 16 KiB body cap;
  - addresses resolved first, and loopback, private, link-local and
    unique-local addresses refused (unless `cimd_allow_private`, for test
    rigs), with the connection pinned to the resolved address so DNS
    rebinding cannot swap it. A name with any non-public address is refused
    outright, and NAT64, 6to4 and IPv4-compatible forms of a non-public IPv4
    address count as non-public. A literal IP host is checked before sending;
    names are checked in the resolver;
  - no proxy: the fetcher ignores `HTTP_PROXY`, `ALL_PROXY` and the rest;
  - the body cap enforced while streaming, with or without
    `Content-Length`, and the timeout covering the body;
  - one fetch per URL at a time.
  - With `cimd_allow_private` and a loopback `http` `public_url` (a test rig
    only), documents may also be fetched over `http`.
- **Validation:** the document must be a JSON object, and its `client_id`
  must exactly equal the URL it was fetched from. It needs `redirect_uris`.
  `token_endpoint_auth_method` must be absent or `none`. `client_name` is
  optional. The other rules are DCR's.
- **Cache:** the result is an `oauth_clients` row (`kind = 'cimd'`), refetched
  when its `Cache-Control` max-age lapses, with a floor of 5 minutes and a
  ceiling of 24 hours. The freshness is stored in
  `oauth_clients.cache_expires_at` (Plan deviation 6). A failed refetch keeps
  a cached document for up to 24 hours after it was fetched, then fails the
  authorization.

## Sign-in codes

Admin only. A code is a one-time credential bound to a grant template.

- CLI:
  - `okf-mcp oauth code create NAME --actor <actor> --access read|write
    (--catalogs <c>,<c>... | --all) [--default <c>] [--expires 24h]` prints
    `okfsc_` plus 43 base64url characters once (stdout; its id and expiry on
    stderr). The name is positional and `--catalogs` comma-separated, as for
    `key create` (Plan deviation 5). Only its SHA-256 is stored.
  - `--access admin` is refused. `--expires` defaults to 24 hours and is
    refused above `code_max_expiry`.
  - `okf-mcp oauth code list` shows codes with their state (live, expired,
    redeemed, revoked).
  - `okf-mcp oauth code revoke <id>`.
  - `okf-mcp oauth grant list` and `okf-mcp oauth grant revoke <id>`: the key
    list and revoke, limited to `kind = 'oauth'`. A grant is revoked by its
    exact id only.
  - `okf-mcp key revoke|grant|ungrant|maintenance-report` resolve API keys
    only (`kind = 'key'`), by id and by name, including the "is revoked"
    diagnosis: a key name never matches, collides with or is made ambiguous
    by a grant, and a grant's id is "no active key" to them. Grants are
    reached by exact id: `okf-mcp oauth grant revoke` and the
    `/api/v1/keys/{id}` routes (Plan deviation 11).
  - Codes are never cached, so minting and revoking take effect at once.
- JSON API (admin access, behind the existing `require_key`):
  `GET /api/v1/oauth/codes`, `POST /api/v1/oauth/codes` (answers the code once,
  `no-store`), `DELETE /api/v1/oauth/codes/{id}`. Grants are listed and revoked
  through the existing keys endpoints, which show `kind` and `client_name`.
- The console's Keys page gets a "Sign-in codes" panel: create (showing the
  code once, with a copy button), list and revoke. The key list marks OAuth
  grants and shows their client.

Codes are generated like keys (`keys::generate_with(prefix)`, Plan decision
3), so the generator and hashing are shared. Enabling maintenance reporting
on a grant is a 409 from the API, not a 500 from the database check.

## Consent flow

The page is server-rendered HTML (Pico CSS, the console's colour tokens,
adaptive light and dark), a plain form, no JavaScript. It follows the
system's light or dark theme with no toggle (it has no script to run one;
Plan deviation 7).

1. **Step 1 (from `GET /oauth/authorize`).** It shows the client's name (from
   its metadata) and the host of its redirect URI. When the client came from
   DCR rather than a CIMD URL, the name is marked "unverified". It has one
   field for the sign-in code, plus Continue and Deny.
2. **Step 2 (`action=code`).** The code must be live, unexpired, unrevoked and
   unredeemed. The page then states what will be granted, without consuming
   the code: "**ChatGPT** (chatgpt.com) will get **write** access to
   **notes, work** as actor **example-actor**, default catalog **notes**."
   It has Approve and Deny. Each step-2 page carries a fresh `summary`
   nonce, stored with the code it shows (`PendingAuthorization.accepted`,
   replacing any earlier pair).
3. **Approve (`action=approve`).** Approve redeems only the code of the
   summary it came from: its `summary` must match the stored nonce (compared
   in constant time, under the same lock as the CSRF check). Once a code is
   accepted, an Approve with an older summary's nonce, or with none,
   re-renders step 1 with "That approval page was out of date..." and changes
   nothing, so the latest summary still approves; it is not a failed attempt.
   An Approve before any code was accepted re-renders step 1 with no message.
   Otherwise, one transaction:
   1. lock the code row and re-check that it is live;
   2. mark it redeemed;
   3. insert the grant: an `api_keys` row with `kind = 'oauth'`, the code's
      access, actor and default catalog, `client_id`, and the name
      `<code name>.<client host>-<first 8 characters of the code id>`, with
      its `key_catalogs` rows copied from the code. `@` fails
      `api_keys_name_check`, and the suffix tells apart grants made from
      codes of one name (Plan deviation 4). The client host is the CIMD
      URL's host, or a DCR client's first redirect URI's;
   4. set the code's `grant_id`.

   Then drop the pending authorization, reload the key cache (so the grant
   is known before the token request arrives), mint the in-memory
   authorization code (`okfac_`) and redirect (303) to `redirect_uri` with
   `code`, `state` and `iss`. A full code map (10,000 entries) answers a 503
   page instead; the grant stays and can be revoked.
4. **Deny** redirects with `error=access_denied`, `state` and `iss`.
5. **Failures.** An unknown, expired, redeemed or revoked code re-renders step
   1 with one generic message, so the page does not say which. After 5 failed
   attempts the pending authorization is dropped and the page tells the person
   to start again from their client. An expired pending authorization (10
   minutes) shows the same message.

**Browser security:**

- Every form carries the pending id and a CSRF token stored with the pending
  authorization; a mismatch is refused.
- Responses are `Cache-Control: no-store`, `Referrer-Policy: no-referrer`,
  `X-Content-Type-Options: nosniff`.
- CSP: `default-src 'none'; style-src 'self' https://fonts.googleapis.com;
  font-src https://fonts.gstatic.com; img-src 'self' data:;
  frame-ancestors 'none'; base-uri 'none'; form-action 'self' <origin of this
  request's redirect URI>`. The font hosts are the console's (Plan deviation
  7). Browsers apply `form-action` to the redirect that follows a form POST,
  so `'self'` alone would block the hand-off to the client (verified with
  Chromium).
- The page's stylesheets are served from the existing `/ui/` assets: Pico,
  the console's `styles.css`, and `consent.css`, which maps the tokens onto
  Pico's variables under `:root:not([data-theme])`.
- Consent POSTs are rate-limited (429 page).

## Data model

### Migration `0008_oauth.sql`

As shipped (`migrations/0008_oauth.sql` is the reference, with its comments):

```sql
CREATE TABLE oauth_clients (
    client_id     text PRIMARY KEY CHECK (client_id <> ''), -- a UUID (dcr) or the document URL (cimd)
    kind          text NOT NULL CHECK (kind IN ('dcr', 'cimd')),
    client_name   text,
    redirect_uris text[] NOT NULL CHECK (cardinality(redirect_uris) > 0),
    metadata      jsonb NOT NULL,
    fetched_at       timestamptz,             -- cimd only: when the document was last fetched
    cache_expires_at timestamptz,             -- cimd only: its freshness (Plan deviation 6)
    created_at    timestamptz NOT NULL,
    CONSTRAINT oauth_clients_fetched_cimd CHECK (
        (kind = 'cimd') = (fetched_at IS NOT NULL)
        AND (kind = 'cimd') = (cache_expires_at IS NOT NULL)
    )
);

ALTER TABLE api_keys
    ADD COLUMN kind text NOT NULL DEFAULT 'key' CHECK (kind IN ('key', 'oauth')),
    ADD COLUMN client_id text REFERENCES oauth_clients (client_id) ON DELETE SET NULL,
    ALTER COLUMN sha256 DROP NOT NULL,
    ADD CONSTRAINT api_keys_kind_hash CHECK ((kind = 'key') = (sha256 IS NOT NULL)),
    ADD CONSTRAINT api_keys_oauth_caps
        CHECK (kind = 'key' OR (access <> 'admin' AND NOT maintenance_report));
CREATE INDEX api_keys_client_id ON api_keys (client_id) WHERE client_id IS NOT NULL;

-- The live name/actor rule covers keys only (Plan deviation 4).
DROP INDEX api_keys_live_name_actor;
CREATE UNIQUE INDEX api_keys_live_name_actor
    ON api_keys (name, actor) WHERE revoked_at IS NULL AND kind = 'key';

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
    -- A grant id only on a redeemed code; a redeemed code may outlive its
    -- grant row (Plan deviation 10).
    CONSTRAINT sign_in_codes_redeemed_grant CHECK (grant_id IS NULL OR redeemed_at IS NOT NULL)
);
CREATE INDEX sign_in_codes_default_catalog_id ON sign_in_codes (default_catalog_id)
    WHERE default_catalog_id IS NOT NULL;
CREATE INDEX sign_in_codes_unredeemed_expires_at ON sign_in_codes (expires_at)
    WHERE redeemed_at IS NULL;

CREATE TABLE sign_in_code_catalogs (
    code_id    text NOT NULL REFERENCES sign_in_codes (id) ON DELETE CASCADE,
    catalog_id bigint NOT NULL REFERENCES catalogs (id) ON DELETE CASCADE,
    PRIMARY KEY (code_id, catalog_id)
);
CREATE INDEX sign_in_code_catalogs_catalog_id ON sign_in_code_catalogs (catalog_id);

CREATE TABLE oauth_tokens (
    sha256     bytea PRIMARY KEY CHECK (length(sha256) = 32),
    grant_id   text NOT NULL REFERENCES api_keys (id) ON DELETE CASCADE,
    kind       text NOT NULL CHECK (kind IN ('access', 'refresh')),
    family_id  text NOT NULL CHECK (family_id <> ''), -- one per authorization code; rotation keeps it (Plan deviation 1)
    resource   text NOT NULL,
    created_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL CHECK (expires_at > created_at),
    used_at    timestamptz,                  -- refresh tokens: when rotated
    revoked_at timestamptz
);
CREATE INDEX oauth_tokens_family_id ON oauth_tokens (family_id);
CREATE INDEX oauth_tokens_grant_id ON oauth_tokens (grant_id);
CREATE INDEX oauth_tokens_expires_at ON oauth_tokens (expires_at);
```

`family_id` is `text`, not `uuid`: the project has no `uuid` crate, and
families use `maintenance::types::new_id()` (32 random hex characters), as
maintenance ids do (Plan decision 2, Plan deviation 1).

The `api_keys_oauth_caps` check is the database's guarantee that no code path
can make an admin or maintenance-reporting grant.

### In memory

Both are an `ExpiringMap`: a `std` `Mutex<HashMap>` pruned of expired
entries on insert, held only for a lookup or insert and never across an
`.await` (the `SessionKeys` pattern). Each has a capacity of 10,000 entries;
a full map refuses an insert, which the authorize GET answers with a 503
page (Plan deviation 8). A restart drops them, which costs at most a consent
page in progress.

- **Pending authorizations**, keyed by a random id: client, redirect URI,
  state, PKCE challenge, resource, CSRF token, the accepted code and its
  summary nonce (`accepted`), failed-attempt count, expiry (10 minutes, not
  extended by use).
- **Authorization codes**, keyed by the hex SHA-256 of the `okfac_` code
  (Plan deviation 2): grant id, client, redirect URI, PKCE challenge,
  resource, family id, expiry (60 seconds), used flag, and the `replayed`
  flag (Plan deviation 14). A used code stays until it expires, so a replay
  is recognized.

## Token validation

- `src/auth.rs` gains a trait:

  ```rust
  /// Turns a presented bearer token into the principal it acts as.
  pub trait BearerVerifier {
      /// `None` for an unknown, expired, revoked or wrong-audience token.
      fn verify(
          &self,
          token: &str,
      ) -> impl Future<Output = Result<Option<Arc<Principal>>, OkfError>> + Send;
  }
  ```

  Return-position `impl Future` (Plan decision 5), not `async-trait`; an
  implementation may be an `async fn`.

  The middleware dispatches on prefix, with no fallback between kinds:
  `okf_` goes to `Keys` (today's `KeyCache`, unchanged behaviour) and
  `okfat_` goes to `OAuthVerifier` (`src/oauth/access.rs`); anything else is
  401. `require_key` takes `State<Arc<App>>` (Plan decision 4). The external-issuer
  round adds a third implementation and its own prefix rule. Dispatch is a
  `match`, not a `dyn` list, because the set is fixed at compile time.
- `OAuthVerifier::verify` (through `AccessTokens::verify(app, resource,
  token)`):
  1. Hash the token and look it up in an in-memory map from hash to
     `{grant_id, resource, expires_at}`, of at most 10,000 entries. The map
     is filled on a miss from one indexed query (live, unrevoked, access
     kind). A revocation generation, read before that query, keeps a lookup
     that straddles a revocation from caching the revoked token.
  2. Check that `resource` equals `<public_url>/mcp`. This is the audience
     check.
  3. Check that the token has not expired. An entry that expired while
     cached is dropped (without bumping the generation: expiry is not a
     revocation).
  4. Look up the grant's `Principal` by id in the key cache.

  Step 4 makes revoking a grant take effect within `poll_interval`, exactly as
  revoking a key does today. RFC 7009 revocation and family revocation also
  evict the affected entries from the map at once. The map is pruned of
  expired entries on insert.
- `KeyCache` gains a `by_id` map holding the grants, which have no hash.
- `Principal` gains `kind: CredentialKind` (`ApiKey` or
  `OAuth { client_id, client_name }`)
  for `/whoami` and the console. `key_id` is the grant's id, so session
  binding (`src/http/sessions.rs`) and the `changesets.key_id` foreign key are
  unchanged.
- Every 401 from the bearer middleware carries
  `WWW-Authenticate: Bearer resource_metadata="<public_url>/.well-known/oauth-protected-resource"`
  when OAuth is on, and plain `Bearer` when it is off.

## Cleanup and abuse limits

The existing poll loop also, at most once an hour, and whether or not OAuth
is on:

- deletes tokens 7 days after they expire;
- deletes sign-in codes 7 days after they expire unredeemed;
- deletes DCR clients more than 24 hours old with no grant.

Rate limits are three global in-memory token buckets (`src/oauth/ratelimit.rs`;
global because behind an ingress the client address is the proxy's; Plan
decision 8 and deviation 9):

| Bucket | Routes | Burst, then |
|---|---|---|
| register | `POST /oauth/register` | 10, then one every 6 seconds |
| token | `POST /oauth/token` and `POST /oauth/revoke` | 60, then 2 a second |
| consent | `POST /oauth/authorize` | 20, then 1 a second |

The JSON endpoints answer 429 `slow_down`, the consent form a 429 page. All
tokens and codes carry prefixes (`okfat_`, `okfrt_`, `okfac_`, `okfsc_`)
that secret scanners can match; property tests check that no token matches
two prefixes. Only their SHA-256 is stored, and none are ever logged; every
type holding one prints `<redacted>` in `Debug`.

## Tool annotations

rmcp 3.8's `#[tool(annotations(...))]` sets them. Every tool gets:

- a `title` in Title Case;
- `readOnlyHint`, `destructiveHint` and `idempotentHint`;
- `openWorldHint: false`, because every tool acts only on this server's
  catalogs.

| Class | Tools | readOnly | destructive | idempotent |
|---|---|---|---|---|
| Reads | `search`, `get_concept`, `read_document`, `list_catalogs`, `list_changesets`, `pending_changes`, `history`, `lint`, `sprawl`, `links_report`, `oldest`, `open_questions`, `maintenance_list_runs`, `maintenance_get_run`, `maintenance_list_findings`, `maintenance_get_finding`, `maintenance_read_revision` | true | false | true |
| Non-destructive writes: changeset staging edits (unpublished, undone by `discard`) plus maintenance lease, report and disposition operations (`maintenance_claim`, `maintenance_renew`, `maintenance_submit`, `maintenance_dispose`) | `open_changeset`, `create_concept`, `edit_document`, `replace_body`, `set_frontmatter`, `stamp`, `review`, `question`, `unquestion`, `add_source`, `remove_source`, `log`, `index`, `move_concept`, `split_concept`, `revert`, `maintenance_claim`, `maintenance_renew`, `maintenance_submit`, `maintenance_dispose` | false | false | false |
| Irreversible or deleting | `publish`, `discard`, `delete_concept`, `maintenance_close` | false | true | false |

`revert` stages an earlier revision in the changeset for review and
publish, so it is a staged edit, not destructive (Plan deviation 3). The
classes are 17 reads, 20 non-destructive writes (16 changeset staging
edits and 4 maintenance lease, report and disposition operations) and 4
destructive tools, 41 in all.

A unit test (`src/mcp/annotations.rs`) lists every registered tool and fails
if any tool lacks a title or any of the four hints. The test also checks
that no tool appears in more than one class, so a new tool must be
classified. The investigator's pinned tool fixture
(`src/investigator/contract_tools.json`) was recaptured to carry the titles
and annotations; its contract check compares only schemas.

## Errors

- OAuth endpoints answer RFC 6749 JSON errors (`error`,
  `error_description`) from `OAuthError`: `invalid_request`,
  `invalid_client`, `invalid_grant`, `unauthorized_client`,
  `unsupported_grant_type`, `unsupported_response_type`, `invalid_target`,
  `invalid_client_metadata`, `invalid_redirect_uri` (RFC 7591), `access_denied`,
  and `slow_down` with 429. `invalid_client` is 401, the rest are 400.
- Authorize errors before the client and redirect URI are validated render an
  error page (see "Endpoints").
- Database failures are 500 `server_error`, logged with `tracing::error!`
  without request secrets, never echoed in detail.
- On `/mcp` and `/api/v1`, a token that is valid but lacks access keeps
  today's 403 or `CatalogNotSelected`.

## Configuration

```toml
public_url = "https://okf.example.com"   # OAuth is on only when this is set (https)

[oauth]
access_token_ttl   = "1h"
refresh_token_ttl  = "30d"    # sliding: each rotation restarts it
code_max_expiry    = "7d"     # cap on `oauth code create --expires`
dcr_enabled        = true
cimd_allow_private = false    # test rigs only
```

A `public_url` that is not `https` is refused at start-up, except for a
loopback host (local testing), as is one with a path, query or user info, or
whose host no `allowed_hosts` entry admits (Plan decision 7; an entry with a
port must match the URL's port). A trailing `/` is dropped. A zero lifetime
is refused too.

## Testing

- **Unit tests:**
  - PKCE S256 with the RFC 7636 appendix B vector;
  - exact redirect URI matching;
  - DCR and CIMD metadata validation (including a `client_id` mismatch);
  - refusal of private addresses;
  - the authorize error page versus redirect split;
  - refresh rotation and reuse detection;
  - authorization code replay;
  - verifier dispatch by prefix;
  - the well-known documents against fixed JSON;
  - the tool annotation coverage test.
- **Property tests (hegel):**
  - token and code prefix parsing never panics and never crosses kinds;
  - PKCE verification accepts exactly the matching verifier.
- **Postgres tests (testcontainers):**
  - the migration on a database with existing keys (they become `kind = 'key'`
    and keep working);
  - the `api_keys_oauth_caps` and `api_keys_kind_hash` checks;
  - a racing double redemption of one code yields one grant;
  - revoking a grant makes its tokens fail within one reload;
  - cleanup deletes what it should and nothing more;
  - deleting a catalog clears it from codes as it does from keys.
- **End-to-end** (the rmcp client, with a local CIMD host and
  `cimd_allow_private`):
  - the full flow: 401, then discovery, then authorize, then the consent
    form with a code, then token, then a tool call;
  - the same flow with DCR;
  - a token for another resource refused;
  - a refreshed token works and the old refresh token then revokes the family;
  - a static key still works on the same server.
- **Browser (rustwright):**
  - the consent page journey with no console errors or CSP violations,
    including the redirect hand-off;
  - the console's sign-in codes panel journey.

## Documentation

- `docs/operations.md`:
  - a how-to, shipped as "OAuth connectors" (turning it on, connecting a
    client, managing codes and grants, what the console shows, what the
    server checks, the routes): set `public_url`, mint a code, paste
    `<public_url>/mcp` into the connector, enter the code;
  - reference rows for the `[oauth]` configuration and the `okf-mcp oauth`
    commands;
  - a note that grants are revoked like keys.
- A dated `changelog/` entry for each PR.

## Delivery order

Each step is one PR; each leaves static keys working.

1. Tool annotations and their coverage test.
2. Migration, `BearerVerifier`, `KeyCache.by_id`, `Principal.kind`; the
   `resource_metadata` 401 header. No OAuth endpoints yet.
3. Well-known documents, DCR and the CIMD fetcher.
4. Sign-in codes: store, CLI, JSON API.
5. Authorize and the consent page.
6. Token, refresh and revoke; the end-to-end tests.
7. The console's sign-in codes panel and grant display.
8. Operations documentation.

## What M10 shipped differently

The plan's deviations and decisions are recorded in place above. These are
the plan's dated "Corrected (issue #N)" notes and the task changelogs'
notes: places where the merged code differs from the plan as first written.
The code is the reference; each item names the file to read.

**Task 1, tool annotations (issue #245, PR #253):**

- `revert` is a staged edit, not destructive: 17 reads, 20 non-destructive
  writes (changeset staging edits plus maintenance lease, report and
  disposition operations), 4 destructive (`src/mcp/annotations.rs`; Plan
  deviation 3).
- The plan missed one file: the investigator's pinned tool fixture,
  `src/investigator/contract_tools.json`, is recaptured with its ignored
  capture test so its nine tools carry `title` and `annotations`. Its
  contract check compares only schemas, so the runner is unchanged.

**Task 2, foundation (issue #246, PR #254):**

- `sign_in_codes_redeemed_grant` is `grant_id IS NULL OR redeemed_at IS NOT
  NULL`, not `(redeemed_at IS NULL) = (grant_id IS NULL)`, which the
  `ON DELETE SET NULL` on `grant_id` would violate when a grant row is
  deleted (`migrations/0008_oauth.sql`; Plan deviation 10).
- Migration 0008 limits `api_keys_live_name_actor` to `kind = 'key'`, so
  grants of one code name never collide (Plan deviation 4).
- The client select list is a private `client_columns!()` macro spliced with
  `concat!`: sqlx 0.9 refuses a `format!`ed `String` as SQL
  (`src/store/oauth/clients.rs`).
- `public_url` validation also refuses user info (`https://user@host`)
  (`src/config.rs`).
- An access token that expires while cached is refused and dropped from the
  cache without bumping the revocation generation; a regression test fails
  if that check is removed (`src/oauth/access.rs`).
- `CimdFetcher::new(allow_private, allow_http)`: plain-http documents only
  with `cimd_allow_private` and a loopback `http` `public_url`, so production
  never fetches over `http` (`src/oauth/mod.rs`).
- `oauth::state::code_key` is `pub` from the start and already has Task 5's
  single-allocation body (`src/oauth/state.rs`).

**Task 3, discovery, DCR and CIMD (issue #247, PR #257):**

- The discovery documents are `Cache-Control: no-store`, not
  `public, max-age=300`: Task 2's layer sets it on every OAuth route
  (`src/oauth/metadata.rs`; Plan deviation 12).
- The DCR cap holds under concurrency: `insert_dcr(pool, new, now,
  max_unbound)` takes a transaction-scoped advisory lock (`0x6f6b666f`,
  "okfo", key 1) and runs one `INSERT ... SELECT ... WHERE <unbound count> <
  cap`; `None` means the cap was reached. The plan's count-then-insert let 6
  or 7 of 8 concurrent registrations at 499 through
  (`src/store/oauth/clients.rs`).
- Client metadata, registered or fetched, must be a JSON object: serde's
  derived deserializer would also read an array in field order
  (`validate_metadata` in `src/oauth/clients.rs`).
- `insert_dcr` lets Postgres pick the id (`gen_random_uuid()::text`).
- The fetcher's resolver reports "no address" with a typed error, looks up
  names through a `Lookup` function pointer so tests prove the connection
  goes only to checked addresses, judges 6to4 by its embedded IPv4 address
  and refuses IPv4-compatible `::/96`. `resolve_cimd` truncates `now` to
  microseconds so a returned record equals the row read back later.
  `register` echoes `client_name` only when sent.
- The fetcher ignores proxy variables, tested in its own binary
  (`tests/cimd_no_proxy.rs`); the 16 KiB cap is tested on a body with no
  `Content-Length`. Each defence was removed in turn and its test failed.

**Task 4, sign-in codes (issue #248, PR #255):**

- The CLI follows `key create`: a positional name and comma-separated
  `--catalogs` (`src/cli.rs`; Plan deviation 5).
- Grant names are `<code name>.<client host>-<code id prefix>`
  (`src/store/oauth/codes.rs`; Plan deviation 4).
- Added scope: `store::keys::resolve_active`, behind `okf-mcp key
  revoke|grant|ungrant|maintenance-report`, considers only `kind = 'key'`
  rows, by id and by name; grants are reached by exact id
  (`src/store/keys.rs`; Plan deviation 11).
- `redeem` has no unique-violation branch: the live name/actor index covers
  keys only, so a grant insert cannot violate it.
- `expiry` adds with `checked_add_signed`, so a huge `--expires` is a
  refusal, not a panic.
- The racing-redeem test is deterministic: a third connection holds the
  code's row lock until `pg_stat_activity` shows both redeems waiting on it.
- Enabling maintenance reporting on a grant is a 409, not a 500 from the
  database check (`src/http/api/keys.rs`).

**Task 5, authorize and consent (issue #249, PR #259):**

- A stale summary cannot approve a different code. `PendingAuthorization`
  holds `accepted: Option<AcceptedCode { code_id, summary_nonce }>` instead
  of `sign_in_code_id`; each step-2 page carries its own `summary` nonce, and
  Approve redeems only the code of the summary it came from, deciding under
  the CSRF check's lock. An out-of-date summary re-renders step 1 and is not
  a failed attempt (`src/oauth/authorize.rs`, `src/oauth/consent.rs`).
- Each handler opens with its own `let Some(oauth) = app.oauth() else { ...
  404 }` (clippy's `result_large_err` refused `Result<_, Response>`), and the
  client and redirect URI checks are one function, `trusted_client`, whose
  errors are always the error page.
- The consent page follows the system theme with no toggle, and its CSP
  allows the console's Google Fonts hosts; `consent.css` maps the tokens onto
  Pico's variables under `:root:not([data-theme])`, since `styles.css`'s plain
  `:root` mapping loses to Pico when no `data-theme` is set
  (`src/http/ui/consent.css`; Plan deviation 7).
- Verified with Chromium: with `form-action 'self'` alone, the 303 to the
  client after Approve is blocked (`tests/ui_e2e/consent.rs`).
- `TestServer::start_with` and the bind-first `serve_app` landed here, not in
  Task 6, because the consent journey needed them first
  (`tests/support/server.rs`; Plan deviation 13).

**Task 6, tokens (issue #250, PR #262):**

- A replay racing the first exchange still revokes what it mints:
  `AuthorizationCode.replayed` (`src/oauth/token.rs`, `src/oauth/state.rs`;
  Plan deviation 14).
- A failed exchange spends the code (Plan deviation 15).
- Family operations take a per-family advisory lock,
  `store::oauth::tokens::lock_family`, so a revocation racing a rotation
  cannot leave a live lineage (`src/store/oauth/tokens.rs`; Plan deviation
  16, from the PR #262 review).
- `/oauth/revoke` does not check the token's client (Plan decision 11).
- `AccessTokens::verify` takes the resource. The approve form posts the
  summary nonce. The end-to-end helpers register with
  `grant_types: ["authorization_code", "refresh_token"]`
  (`tests/support/oauth.rs`, `tests/server_oauth.rs`).

**Task 7, console (issue #251, PR #256):**

- The keys page's helpers take the page's `main` and check
  `main.isConnected` after each await before touching the drawer or
  reloading; the one-time code dialog still shows after navigating away,
  since it is the code's only appearance (`src/http/ui/js/pages/keys.js`).
- The dialog drops the code (its text and the copy handler's reference) when
  "I have stored it" closes it, Escape does not close it, and the journey
  checks the code is in neither `localStorage` nor `sessionStorage`
  (`tests/ui_e2e/keys.rs`).
- The codes table's empty text is "No sign-in codes to show.".

**Task 8, documentation (issue #252, PR #263):**

- The CLI reference moved from the "Web console" subsection "OAuth sign-in
  codes" into "OAuth connectors" in `docs/operations.md`, which also covers
  routing, egress for metadata documents, revocation and the console; the
  web console subsection keeps the JSON API and links to it.
- The release (v0.13.0) is prepared by `just release-prepare minor` on
  `main` after this PR merges, as for 0.12.0: the script refuses any branch
  but `main` and creates the release commit and a local tag itself.
