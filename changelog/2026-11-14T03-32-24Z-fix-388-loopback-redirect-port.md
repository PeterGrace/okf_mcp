# Fix: loopback redirect URIs match on any port (RFC 8252 section 7.3)

Issue #388.

## Fixed
- Claude Code could not connect over OAuth: its Client ID Metadata Document
  registers portless loopback redirect URIs (`http://localhost/callback`,
  `http://127.0.0.1/callback`) and it sends the ephemeral port it listens
  on, which `/oauth/authorize` refused by exact string equality ("The
  request's redirect URI is not one the client registered"). The authorize
  step now accepts any port when both the registered and the requested URI
  are `http` on a loopback host, as RFC 8252 section 7.3 requires. It
  applies to every client source, since all of them go through the same
  check.

## Changed
- `src/oauth/clients.rs`: `redirect_uri_matches(registered, requested)`,
  used by `/oauth/authorize`. Both URIs must parse (a URI that fails to
  parse never matches). Outside loopback `http` it is exact string
  equality. On loopback `http` (`is_loopback_host`, reused) only the port
  may differ: the text before the port (scheme, host as written) and after
  it (path and query) must be identical, there is no fragment, and the URI
  must be written as the `url` parser writes it apart from its port, so an
  upper-case host, a `..` segment, user information or a bare `:` get no
  leniency.
- `/oauth/authorize` keeps the request's URI, port included, with the
  pending authorization: the final redirect goes there, the consent page's
  CSP `form-action` names its origin with the port, and the code stores it.
- `docs/operations.md`, "OAuth connectors": a paragraph on loopback redirect
  URIs.
- Test helpers: `authorize_query_for`, `insert_loopback_client` and the
  `LOOPBACK_*` constants in `src/oauth/testing.rs`;
  `CimdHost::start_registering` and `OAuthClient::exchange_at` in
  `tests/support/oauth.rs`.

## Notes
- The token endpoint is unchanged: it still requires exact equality with
  the `redirect_uri` the authorize request used (RFC 6749 section 4.1.3).
  `a_loopback_code_exchanges_only_with_the_port_it_was_issued_for` and the
  end-to-end CIMD test prove another port, or the portless registered URI,
  is refused there.
- `https` URIs, and `http` on a non-loopback host, keep the exact match,
  port included.
