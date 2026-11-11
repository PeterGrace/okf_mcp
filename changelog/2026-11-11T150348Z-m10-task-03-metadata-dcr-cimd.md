# M10 Task 3: discovery documents, dynamic client registration, CIMD

## Added

- `GET /.well-known/oauth-protected-resource` (also with the `/mcp` suffix):
  RFC 9728 Protected Resource Metadata naming `<public_url>/mcp` and this
  server as its authorization server.
- `GET /.well-known/oauth-authorization-server`: RFC 8414 metadata (S256
  PKCE, public clients, `client_id_metadata_document_supported`, RFC 9207
  `iss`). `registration_endpoint` is left out when `dcr_enabled = false`.
- `POST /oauth/register`: RFC 7591 registration for public clients (the
  metadata must be a JSON object, as must a CIMD document)
  (`token_endpoint_auth_method` `none`; `https` redirect URIs, or `http` on
  loopback). Refused with `slow_down` past the rate limit or while 500
  registered clients have no grant; 404 when `dcr_enabled = false`. The
  cap is checked in the same statement as the insert, under a
  transaction-scoped advisory lock, so concurrent registrations (from any
  process) cannot pass it.
- Client ID Metadata Documents: an `https://` `client_id` is fetched over
  HTTPS without redirects or a proxy, within 5 seconds (body included) and
  16 KiB (enforced while reading, with or without `Content-Length`), from a
  public address only. Literal IP hosts are checked before sending; names
  are checked in the DNS resolver, and reqwest connects only to the
  addresses it checked, so rebinding cannot slip past it. A name with any
  non-public address is refused outright. The document must name its own
  URL. It is cached for its `Cache-Control` max-age, clamped to 5 minutes
  and 24 hours; a failed refetch serves the cached copy for up to a day.
  Concurrent requests for one URL fetch it once.
- `oauth::clients::resolve`: an authorize request's `client_id` as a CIMD
  client (by URL) or a registered DCR client.
- Store: `insert_dcr` (capped, atomic), `upsert_cimd`, `count_unbound_dcr`.
- `tests/cimd_no_proxy.rs`: the fetcher ignores `HTTP_PROXY`/`ALL_PROXY`.

## Changed

- `url` 2.5.8 is now a direct dependency (it was already in the tree
  through reqwest).

## Notes

- The discovery documents carry `Access-Control-Allow-Origin: *`. Like
  every OAuth route they are `Cache-Control: no-store` (Task 2's layer); the
  plan's `public, max-age=300` would have been overwritten by it.
- With `cimd_allow_private` and a loopback `http` `public_url` (test rigs
  only), CIMD documents may also be fetched over `http`.
- The address classifier also refuses NAT64, 6to4 and IPv4-compatible
  forms of non-public IPv4 addresses.
- Each SSRF defence (https only, no redirects, deadline, streaming cap,
  resolver guard, refuse-on-any, pinning, literal-IP check, no proxy) was
  removed in turn during development and its test failed.
