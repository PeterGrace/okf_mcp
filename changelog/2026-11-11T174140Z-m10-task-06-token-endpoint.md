# M10 Task 6: token, refresh and revoke

## Added
- `POST /oauth/token`:
  - `authorization_code` checks the client, the exact redirect URI, `resource` and PKCE S256 (constant time) against the stored code, checks that the grant is live, and mints a 1-hour `okfat_` access token and a 30-day `okfrt_` refresh token;
  - `refresh_token` rotates the refresh token within its family; a refresh by another client, or for another `resource`, is refused and rolled back, so the token is not spent;
  - a replayed code or a reused refresh token revokes the whole token family and drops its access tokens from the cache;
  - success answers carry `Cache-Control: no-store` and `Pragma: no-cache`; errors are RFC 6749 `{error, error_description}`.
- `POST /oauth/revoke` (RFC 7009): always 200, an unknown token included. Revoking a refresh token revokes its family.
- `store::oauth::tokens::{RefreshUse, use_refresh, revoke_family, lock_family}`. Every transaction that adds tokens to a family or revokes it takes the family's advisory lock first, so a revocation racing a rotation either revokes the rotation's committed successors or makes the rotation find its parent revoked (PR #262 review; regression tests hold a rotation open across a reuse and a revoke).
- End-to-end OAuth tests (`tests/server_oauth.rs`, helpers in `tests/support/oauth.rs`):
  - CIMD (a local metadata host) and DCR flows from the 401 through discovery, consent and the token endpoint to an MCP tool call with a real rmcp client;
  - the audience check, refresh-token reuse, grant revocation at the next poll, and a static key beside OAuth.

## Changed
- `oauth::state::AuthorizationCode` gains `replayed`: a replay racing the first exchange sets it, and the exchange revokes what it minted.

## Notes
- The revocation endpoint does not check that the token belongs to the requesting client: clients are public, and holding a token already allows everything revoking it takes away (plan Decision 11).
- A failed exchange spends the code; presenting it again is a replay.
