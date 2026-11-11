# Issue #241: the connection's catalog list is the session's scope

## Changed
- A connection's catalog list (`X-OKF-Catalogs`, else `?catalogs=`) is now a boundary, not just a default. When it is set:
  - `list_catalogs` offers only the listed catalogs the key reaches, so the key's default catalog and its other grants no longer show;
  - a tool's `catalog` / `catalogs` argument may only narrow the list; naming a catalog it leaves out fails with `CatalogNotSelected` naming it;
  - a changeset handle in a catalog the list leaves out does not resolve (`UnknownChangeset`), so a handle cannot reach an omitted catalog either; `list_changesets` without `catalog` lists only the offered catalogs;
  - the list is checked whole on every call: a name that does not exist or that the key does not reach fails the call (and shows in `list_catalogs.selection_error`) even when the argument names a good catalog. Nothing falls back to a default. `ToolContext::check_connection` applies the same check to the paths that do not select a catalog, using a changeset handle and `list_changesets` without `catalog` (PR #271 review).
- `selection::resolve` implements this; `selection::in_scope` and `ToolContext::offers` give the membership test the listing tools filter by.
- Without a connection list nothing changes: the argument, else the key's default, else the server's `default_catalog`; `list_catalogs` offers every catalog the key reaches.

## Added
- `X-OKF-Catalog` (singular, the spelling in the report) is accepted as an alias of `X-OKF-Catalogs`. A request with both reads them as one list, every `X-OKF-Catalogs` name first (duplicates collapse, so the first plural name stays the write target). Headers still take precedence over `?catalogs=`.
- `TestServer::client_at` in the integration test support, to connect on an MCP URL with a query.
- Tests: scope and narrowing in `selection`, `offers` and the handle refusal in `mcp::context`, the hidden default in `mcp::catalogs`, and an end-to-end test in `tests/server.rs` covering the plural header, the singular alias, the query, both headers together, an ungranted name and no list.

## Notes
- The README's "Connecting a client" section and `docs/operations.md` describe the boundary.
- Grants remain the upper bound: a list never makes a catalog reachable.
