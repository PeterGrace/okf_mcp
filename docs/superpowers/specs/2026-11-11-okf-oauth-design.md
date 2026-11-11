# okf-mcp OAuth 2.1 and directory readiness: design

Status: approved in brainstorming, 2026-11-11. Not yet planned.

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
| When OAuth is on | Only when `public_url` (HTTPS) is configured: issuer and resource URLs must be the externally visible origin, not the listen address. |
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
           ◄── 302 redirect_uri?code&state&iss
MCP client ── POST /oauth/token ───────────────► okfat_ + okfrt_
           ── POST /mcp (Bearer okfat_...) ─► BearerVerifier ─► Principal ─► tools
```

New module `src/oauth/`:

- `metadata.rs`: the two well-known documents.
- `clients.rs`: DCR registration, the CIMD fetcher and cache, client lookup.
- `authorize.rs`: the authorize endpoint, pending requests, the consent page.
- `token.rs`: the token and revoke endpoints, minting and rotation.
- `codes.rs`: sign-in codes (store access lives in `src/store/oauth.rs`).
- `error.rs`: `OAuthError` (`thiserror`) with its own `IntoResponse`.

`src/auth.rs` gains the `BearerVerifier` trait and its dispatch;
`src/store/oauth.rs` holds the queries; the CLI gains `okf-mcp oauth ...`;
the console's Keys page gains a sign-in codes panel.

## Endpoints

All are served by the existing binary on its existing port, and are mounted
only when OAuth is on. `<public_url>` below is the configured origin; the
resource identifier is `<public_url>/mcp`.

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

- Accepts JSON client metadata. Requires `redirect_uris` (each `https`, or
  `http` on a loopback host for native clients). `token_endpoint_auth_method`
  must be absent or `none`. `grant_types` must be a subset of
  `authorization_code` and `refresh_token`; `response_types` of `code`.
- Stores an `oauth_clients` row (`kind = 'dcr'`, `client_id` a fresh UUID) and
  answers 201 with the registered metadata and `client_id`. No
  `client_secret`.
- Refused with `slow_down` (429) beyond the global rate limit, or when 500 DCR
  clients without a grant already exist.

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
5. Store a pending authorization in memory and render consent step 1.

### `POST /oauth/authorize`

The consent form's target. Fields: `pending` (the pending authorization id),
`csrf`, `action` (`code`, `approve` or `deny`), `sign_in_code` (step 1 only).
See "Consent flow".

### `POST /oauth/token`

Form-encoded, per RFC 6749. Responses are `Cache-Control: no-store`.

- `grant_type=authorization_code`: requires `code`, `client_id`,
  `redirect_uri`, `code_verifier` and `resource`. The code must be live,
  unused and issued to that client with that redirect URI and resource;
  `BASE64URL(SHA256(code_verifier))` must equal its `code_challenge`. A code
  presented twice revokes every token minted from it (RFC 6749 section
  4.1.2). Answers `access_token` (`okfat_`), `token_type: Bearer`,
  `expires_in`, `refresh_token` (`okfrt_`).
- `grant_type=refresh_token`: requires `refresh_token` and `client_id`;
  `resource`, if given, must equal the token's. The refresh token must be live
  and belong to that client's grant. It is marked used and a new access and
  refresh token are minted in the same token family. **A refresh token that
  has already been used revokes the whole family** (reuse detection).
- A revoked grant, or one whose client no longer exists, is `invalid_grant`.

### `POST /oauth/revoke` (RFC 7009)

Form-encoded `token` (access or refresh) and `client_id`. Revokes that token;
revoking a refresh token revokes its family. Always 200, including for an
unknown token.

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
    rebinding cannot swap it;
  - one fetch per URL at a time.
- **Validation:** the document's `client_id` must exactly equal the URL it was
  fetched from. It needs `redirect_uris`. `token_endpoint_auth_method` must be
  absent or `none`. `client_name` is optional.
- **Cache:** the result is an `oauth_clients` row (`kind = 'cimd'`), refetched
  when its `Cache-Control` max-age lapses, with a floor of 5 minutes and a
  ceiling of 24 hours. A failed refetch keeps a cached document for up to its
  ceiling, then fails the authorization.

## Sign-in codes

Admin only. A code is a one-time credential bound to a grant template.

- CLI:
  - `okf-mcp oauth code create --name <name> --actor <actor> --access read|write
    (--catalog <c>... | --all) [--default <c>] [--expires 24h]` prints
    `okfsc_` plus 43 base64url characters once. Only its SHA-256 is stored.
  - `--access admin` is refused. `--expires` defaults to 24 hours and is
    refused above `code_max_expiry`.
  - `okf-mcp oauth code list` shows codes with their state (live, expired,
    redeemed, revoked).
  - `okf-mcp oauth code revoke <id>`.
  - `okf-mcp oauth grant list` and `okf-mcp oauth grant revoke <id>`: the key
    list and revoke, limited to `kind = 'oauth'`.
- JSON API (admin access, behind the existing `require_key`):
  `GET /api/v1/oauth/codes`, `POST /api/v1/oauth/codes` (answers the code once,
  `no-store`), `DELETE /api/v1/oauth/codes/{id}`. Grants are listed and revoked
  through the existing keys endpoints, which show `kind` and `client_name`.
- The console's Keys page gets a "Sign-in codes" panel: create (showing the
  code once, with a copy button), list and revoke. The key list marks OAuth
  grants and shows their client.

Codes are generated like keys (`keys::generate` with another prefix), so the
generator and hashing are shared.

## Consent flow

The page is server-rendered HTML (Pico CSS, the console's colour tokens,
adaptive light and dark), a plain form, no JavaScript.

1. **Step 1 (from `GET /oauth/authorize`).** It shows the client's name (from
   its metadata) and the host of its redirect URI. When the client came from
   DCR rather than a CIMD URL, the name is marked "unverified". It has one
   field for the sign-in code, plus Continue and Deny.
2. **Step 2 (`action=code`).** The code must be live, unexpired, unrevoked and
   unredeemed. The page then states what will be granted, without consuming
   the code: "**ChatGPT** (chatgpt.com) will get **write** access to
   **notes, work** as actor **example-actor**, default catalog **notes**."
   It has Approve and Deny.
3. **Approve (`action=approve`).** One transaction:
   1. lock the code row and re-check that it is live;
   2. mark it redeemed;
   3. insert the grant: an `api_keys` row with `kind = 'oauth'`, the code's
      access, actor and default catalog, `client_id`, and the name
      `<code name>@<client host>`, with its `key_catalogs` rows copied from
      the code;
   4. set the code's `grant_id`.

   Then mint the in-memory authorization code, drop the pending authorization
   and redirect to `redirect_uri` with `code`, `state` and `iss`.
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
- CSP: `default-src 'none'; style-src 'self'; img-src 'self' data:;
  frame-ancestors 'none'; base-uri 'none'; form-action 'self' <origin of this
  request's redirect URI>`. Browsers apply `form-action` to the redirect that
  follows a form POST, so `'self'` alone would block the hand-off to the
  client.
- The page's stylesheet is served from the existing `/ui/` assets.

## Data model

### Migration `0008_oauth.sql`

```sql
CREATE TABLE oauth_clients (
    client_id     text PRIMARY KEY,           -- a UUID (dcr) or the document URL (cimd)
    kind          text NOT NULL CHECK (kind IN ('dcr', 'cimd')),
    client_name   text,
    redirect_uris text[] NOT NULL,
    metadata      jsonb NOT NULL,
    fetched_at    timestamptz,                -- cimd only: when the document was last fetched
    created_at    timestamptz NOT NULL
);

ALTER TABLE api_keys
    ADD COLUMN kind text NOT NULL DEFAULT 'key' CHECK (kind IN ('key', 'oauth')),
    ADD COLUMN client_id text REFERENCES oauth_clients (client_id) ON DELETE SET NULL,
    ALTER COLUMN sha256 DROP NOT NULL,
    ADD CONSTRAINT api_keys_kind_hash CHECK ((kind = 'key') = (sha256 IS NOT NULL)),
    ADD CONSTRAINT api_keys_oauth_caps
        CHECK (kind = 'key' OR (access <> 'admin' AND NOT maintenance_report));

CREATE TABLE sign_in_codes (
    id                 text PRIMARY KEY DEFAULT gen_random_uuid()::text,
    name               text NOT NULL CHECK (name ~ '^[A-Za-z0-9._-]+$'),
    sha256             bytea NOT NULL UNIQUE CHECK (length(sha256) = 32),
    access             text NOT NULL CHECK (access IN ('read', 'write')),
    all_catalogs       boolean NOT NULL,
    default_catalog_id bigint REFERENCES catalogs (id) ON DELETE SET NULL,
    actor              text NOT NULL,
    created_at         timestamptz NOT NULL,
    expires_at         timestamptz NOT NULL,
    redeemed_at        timestamptz,
    grant_id           text REFERENCES api_keys (id) ON DELETE SET NULL,
    revoked_at         timestamptz
);

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
    family_id  uuid NOT NULL,                -- one per authorization code; rotation keeps it
    resource   text NOT NULL,
    created_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL,
    used_at    timestamptz,                  -- refresh tokens: when rotated
    revoked_at timestamptz
);
CREATE INDEX oauth_tokens_family_id ON oauth_tokens (family_id);
CREATE INDEX oauth_tokens_grant_id ON oauth_tokens (grant_id);
CREATE INDEX oauth_tokens_expires_at ON oauth_tokens (expires_at);
```

The `api_keys_oauth_caps` check is the database's guarantee that no code path
can make an admin or maintenance-reporting grant.

### In memory

Both are a `std` `Mutex<HashMap>` pruned of expired entries on insert, held
only for a lookup or insert and never across an `.await` (the `SessionKeys`
pattern). A restart drops them, which costs at most a consent page in
progress.

- **Pending authorizations**, keyed by a random id: client, redirect URI,
  state, PKCE challenge, resource, CSRF token, failed-attempt count, expiry
  (10 minutes).
- **Authorization codes**, keyed by SHA-256: grant id, client, redirect URI,
  PKCE challenge, resource, family id, expiry (60 seconds), used flag.

## Token validation

- `src/auth.rs` gains a trait:

  ```rust
  /// Turns a presented bearer token into the principal it acts as.
  trait BearerVerifier {
      /// `None` for an unknown, expired, revoked or wrong-audience token.
      async fn verify(&self, token: &str) -> Result<Option<Arc<Principal>>, OkfError>;
  }
  ```

  The middleware dispatches on prefix, with no fallback between kinds:
  `okf_` goes to `ApiKeys` (today's `KeyCache`, unchanged behaviour) and
  `okfat_` goes to `OAuthTokens`; anything else is 401. The external-issuer
  round adds a third implementation and its own prefix rule. Dispatch is a
  `match`, not a `dyn` list, because the set is fixed at compile time.
- `OAuthTokens::verify`:
  1. Hash the token and look it up in an in-memory map from hash to
     `{grant_id, resource, expires_at}`. The map is filled when a token is
     minted and, on a miss, from one indexed query (live, unrevoked, access
     kind).
  2. Check that `resource` equals `<public_url>/mcp`. This is the audience
     check.
  3. Check that the token has not expired.
  4. Look up the grant's `Principal` by id in the key cache.

  Step 4 makes revoking a grant take effect within `poll_interval`, exactly as
  revoking a key does today. RFC 7009 revocation and family revocation also
  evict the affected entries from the map at once. The map is pruned of
  expired entries on insert.
- `KeyCache` gains a `by_id` map holding the grants, which have no hash.
- `Principal` gains `kind: CredentialKind` (`ApiKey` or `OAuth { client_name }`)
  for `/whoami` and the console. `key_id` is the grant's id, so session
  binding (`src/http/sessions.rs`) and the `changesets.key_id` foreign key are
  unchanged.
- Every 401 from the bearer middleware carries
  `WWW-Authenticate: Bearer resource_metadata="<public_url>/.well-known/oauth-protected-resource"`
  when OAuth is on, and plain `Bearer` when it is off.

## Cleanup and abuse limits

The existing poll loop also:

- deletes tokens 7 days after they expire;
- deletes sign-in codes 7 days after they expire unredeemed;
- deletes DCR clients more than 24 hours old with no grant.

`/oauth/register` and `/oauth/token` share a global token-bucket rate limit
(an in-memory token bucket answering 429), and `/oauth/authorize` POSTs another. All tokens
and codes carry prefixes (`okfat_`, `okfrt_`, `okfsc_`) that secret scanners
can match. Only their SHA-256 is stored, and none are ever logged.

## Tool annotations

rmcp 3.8's `#[tool(annotations(...))]` sets them. Every tool gets:

- a `title` in Title Case;
- `readOnlyHint`, `destructiveHint` and `idempotentHint`;
- `openWorldHint: false`, because every tool acts only on this server's
  catalogs.

| Class | Tools | readOnly | destructive | idempotent |
|---|---|---|---|---|
| Reads | `search`, `get_concept`, `read_document`, `list_catalogs`, `list_changesets`, `pending_changes`, `history`, `lint`, `sprawl`, `links_report`, `oldest`, `open_questions`, `maintenance_list_runs`, `maintenance_get_run`, `maintenance_list_findings`, `maintenance_get_finding`, `maintenance_read_revision` | true | false | true |
| Staged edits (unpublished, undone by `discard`) | `open_changeset`, `create_concept`, `edit_document`, `replace_body`, `set_frontmatter`, `stamp`, `review`, `question`, `unquestion`, `add_source`, `remove_source`, `log`, `index`, `move_concept`, `split_concept`, `maintenance_claim`, `maintenance_renew`, `maintenance_submit`, `maintenance_dispose` | false | false | false |
| Irreversible or deleting | `publish`, `revert`, `discard`, `delete_concept`, `maintenance_close` | false | true | false |

A unit test lists every registered tool and fails if any tool lacks a title
or any of the four hints. The test also checks that no tool appears in more
than one class, so a new tool must be classified.

## Errors

- OAuth endpoints answer RFC 6749 JSON errors (`error`,
  `error_description`) from `OAuthError`: `invalid_request`,
  `invalid_client`, `invalid_grant`, `unauthorized_client`,
  `unsupported_grant_type`, `unsupported_response_type`, `invalid_target`,
  `access_denied`, and `slow_down` with 429. `invalid_client` is 401, the rest
  are 400.
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
loopback host (local testing).

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
  - a how-to, "Connecting ChatGPT or Claude.ai over OAuth": set
    `public_url`, mint a code, paste `<public_url>/mcp` into the connector,
    enter the code;
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
