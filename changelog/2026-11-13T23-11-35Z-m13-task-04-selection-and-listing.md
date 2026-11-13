# M13 Task 4: selection, REST search, `list_changesets` and listing (issue #366)

## Added
- `mcp::catalogs::SuspensionInfo` (`{team, notice}`) and `From<&SuspendedCatalog>` for it.
- A `suspended` field on `list_catalogs`' `CatalogInfo` and on `GET /api/v1/catalogs`' `CatalogSummary` (and `POST /api/v1/catalogs`' answer): null for an active catalog, else the owner team and its notice. Shown only to a credential that reaches the catalog, admins included.
- Tests:
  - `selection::tests`: `a_named_suspended_catalog_is_refused`, `a_suspended_default_is_refused`, `the_connection_list_drops_suspended_catalogs`, `a_connection_list_of_only_suspended_catalogs_is_refused`, `an_unreached_suspended_catalog_is_not_selected_rather_than_suspended`, `an_admin_selects_suspended_catalogs`.
  - `mcp::context::tests::every_selection_path_refuses_or_drops_a_suspended_catalog`: `select`, `states`, `read_set`, `one`, `report_catalog`, `check_connection` and the `search` tool.
  - `mcp::catalogs::tests::a_suspended_catalog_is_listed_marked_and_the_others_still_select`.
  - `mcp::changesets::suspension_tests::list_changesets_leaves_a_suspended_catalog_out_but_to_an_admin`.
  - `http::api::browse::tests::every_selection_backed_read_route_refuses_a_suspended_catalog_to_members`: tree, concept, links, history, document, health and every report, for keys and a member session.
  - `http::api::search::tests::search_leaves_out_or_refuses_a_suspended_catalog`.
  - `http::api::catalogs::tests::the_list_marks_a_suspended_catalog_and_never_hides_it`.
  - `http::api::changesets::tests::the_changeset_list_leaves_out_a_suspended_catalog_but_to_an_admin`.

## Changed
- `selection::resolve` refuses a named suspended catalog (the argument, the key's default or the server's default) with `Suspended`, and drops suspended catalogs from a connection's list, refusing only when none is left. Reach is checked first, and admins are exempt. Every MCP read tool and every REST route that runs a tool through a `ToolContext` (browse, health, reports, search) goes through it.
- `ToolContext::check_connection` refuses a connection list of only suspended catalogs, as `resolve` does.
- `GET /api/v1/search` without `catalogs` leaves suspended catalogs out of "every catalog the key reaches".
- `list_changesets` (and `GET /api/v1/changesets`) without `catalog` leaves suspended catalogs out; naming one is refused through `resolve`.
- `list_catalogs`' description mentions the `suspended` field.
- The M13 plan, Task 4: a dated correction note on what shipped.

## Notes
- A credential that does not reach a catalog never learns it is suspended: it still gets `CatalogNotSelected` (404), and a listing never shows it the catalog.
- With these rules and Task 3's, every path that chooses a catalog refuses, drops or marks a suspended one; export alone stays open.
- A REST search over every reached catalog, when each one is suspended, falls back to the key's default and is refused with `Suspended`.
- Tools that select their catalog before checking their own level (`open_changeset`, `maintenance_dispose`, the maintenance work tools) refuse a suspended catalog at the selection: a key that reaches it below the needed level gets 403 `Suspended` rather than 403 `Forbidden`. Reach still comes first. Task 3's maintenance route test expects `Suspended` for the read key's disposition now.
