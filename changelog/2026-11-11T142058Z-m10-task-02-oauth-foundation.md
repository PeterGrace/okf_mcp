# M10 Task 2: OAuth foundation

## Added
- Migration `0008_oauth.sql`: `oauth_clients`, `sign_in_codes`,
  `sign_in_code_catalogs` and `oauth_tokens`. `api_keys` gains `kind`
  (`key` or `oauth`) and `client_id`. A grant row has no hash, and the
  database refuses an admin or maintenance-reporting grant
  (`api_keys_oauth_caps`).
- Configuration: `public_url` (turns OAuth on; must be an `https` origin,
  or `http` on loopback, with no user info, path or query, whose host is in
  `allowed_hosts`) and an `[oauth]` section (`access_token_ttl`,
  `refresh_token_ttl`, `code_max_expiry`, `dcr_enabled`,
  `cimd_allow_private`).
- `keys::generate_with(prefix)` and the secret prefixes `okfat_`, `okfrt_`,
  `okfsc_` and `okfac_` beside `okf_`, with property tests that no token
  matches two prefixes.
- `BearerVerifier`: the bearer middleware accepts `okfat_` OAuth access
  tokens beside `okf_` keys, checking each token's audience (RFC 8707) and
  its grant. Access tokens are cached in memory; a revocation generation
  keeps a lookup that straddles a revocation from caching the revoked token.
- With OAuth on, a 401 carries `WWW-Authenticate: Bearer resource_metadata="..."`
  (RFC 9728).
- `src/oauth/`: shared state for the authorization server (bounded expiring
  maps for pending authorizations and authorization codes, token-bucket
  rate limits, `OAuthError`), and stubs for the endpoints later tasks add.
  `src/store/oauth/`: token insert, lookup, revoke and prune; client lookup
  and pruning; sign-in code pruning; test helpers.
- The poll loop prunes expired OAuth tokens, unredeemed codes and unused
  registered clients once an hour.

## Changed
- `GET /api/v1/whoami` and `GET /api/v1/keys` include `kind` and
  `client_name`.
- The live name/actor uniqueness rule applies to API keys only.
- `require_key` takes the whole `App` as its state.

## Notes
- Nothing changes without `public_url`: keys authenticate as before and
  401s still say `Bearer`.
- Plan corrections: the client select list is a `client_columns!()` macro
  spliced with `concat!` (sqlx 0.9 refuses a `format!`ed `String` as SQL);
  `oauth::state::code_key` is `pub` (no caller before Task 5) and already
  carries Task 5's single-allocation body; an expired cached token is
  dropped without bumping the revocation generation.
