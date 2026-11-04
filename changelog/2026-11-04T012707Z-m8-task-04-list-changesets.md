# M8 Task 4: `list_changesets` replaces `stranded_sessions` and `adopt_changeset`

Issue #163. With handles, a session that lost its handle needs to find it
again, and an admin takes another key's work over by using its handle
directly (M8 Decisions 3 and 8). Moving a changeset to a connection
(`adopt_changeset`) and listing only idle ones (`stranded_sessions`) no
longer fit.

## Added

- The `list_changesets` tool (`read` access, optional `catalog`): the open
  changesets the key may use, empty ones included. A key sees its own; an
  `admin` key sees every key's in the catalogs it reaches. Each row has the
  handle (`changeset`), catalog, key, actor, `created`, `last_change`, `idle`
  (no change within `session_idle_timeout`) and its `/`-prefixed paths,
  sorted. Rows are by catalog, oldest change first.
- `store::changesets::list_open(pool, catalog_ids, key_id)` and `ListedRow`.
- Tests: `list_changesets_shows_a_key_its_own_and_an_admin_everyone_s`,
  `an_admin_may_publish_another_key_s_changeset` (the revisions carry the
  admin's actor), `a_published_changeset_is_no_longer_listed`,
  `list_open_lists_open_changesets_with_their_sorted_paths` and
  `list_changesets_is_listed_and_the_old_tools_are_gone`.

## Changed

- Removed the `stranded_sessions` and `adopt_changeset` tools, their params
  and results, and the store's `stranded`, `StrandedRow`, `RawStranded` and
  `adopt`, with their tests.
- The server instructions name `list_changesets` as the way to find a lost
  handle.
- README: the `stranded_sessions` and `adopt_changeset` rows became
  `list_changesets` rows. The README's and the operations guide's
  `session_idle_timeout` notes say an untouched changeset counts as `idle`.

## Notes

- The query keeps `stranded`'s shape with a `LEFT JOIN`. Its `array_agg`
  needs `FILTER (WHERE e.path IS NOT NULL)`, because an empty changeset's
  joined row has a `NULL` path and `array_agg` would otherwise return
  `{NULL}`. The plan's Task 4 has a "Corrected" note.
- The bigint changeset id no longer reaches clients through any tool.
- Review of PR #170: the tool description also says empty changesets are
  listed, and that one with no write within 24 hours is discarded and drops
  off the list.
