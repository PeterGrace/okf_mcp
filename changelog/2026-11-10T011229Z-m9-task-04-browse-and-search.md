# M9 Task 4: Browse and search

## Added

- `src/render.rs`: markdown to safe HTML with pulldown-cmark 0.13.6
  (tables, strikethrough, task lists).
  - Raw HTML is shown as text.
  - Links are limited to `http`, `https`, `mailto` and the same catalog's
    documents, which become console routes.
  - Images become links.
  - Heading ids are `section-<slug>`, with the outline's slug in `data-slug`.
- API:
  - `GET /api/v1/catalogs/{catalog}/tree`, `concepts/{path}`,
    `links/{path}`, `history/{path}`, `revisions/{n}/{path}` and
    `documents/{path}`;
  - `GET /api/v1/search`.
- Console pages:
  - Browse: a collapsible tree; a concept view with a provenance line and
    Content, Links, History and Raw tabs; a revision diff in the drawer; a
    folder shows its `index.md`.
  - Search.
- Browser journeys: the tree and links between concepts, raw HTML shown
  as text, backlinks, a history diff, search, and switching catalogs while
  browsing (which also proves the page adds its switcher listener once,
  not once per render).

## Changed

- `unified_diff` in `src/mcp/changesets.rs` is `pub(crate)`, so revision
  diffs reuse it.
- `api::context` and the journey harness's `Ui::wait_hidden` lose their
  `#[allow(dead_code)]`: this task is their first caller.

## Notes

- An axum wildcard must come last, so the per-document routes are
  `/links/{path}`, `/history/{path}` and `/revisions/{n}/{path}` rather
  than the design's `/concepts/{path}/links` (and so on).
- Without `catalogs`, `/api/v1/search` searches every catalog the key
  reaches.
- Table alignment is dropped and outside images are shown as links,
  because the console's CSP blocks inline styles and outside images.
- The revision drawer is outside the page's container, so Browse opens it
  only if the page is still shown after the revision's request
  (`view.isConnected`), per the plan's front-end conventions.
