# M8 Task 3: writes, the changeset tools and the reads take `changeset`

Issue #162. Writes and draft reads switch to server-issued changeset handles
together: once writes land in handle-opened changesets, the label lookup no
longer finds them.

## Added

- `ToolContext::pin(handle)`: the call's optional changeset, resolved with
  `read` access.
- `ToolContext::report_catalog(catalog, pinned)`: a single-catalog report
  runs on `catalog`, else the pinned changeset's catalog, else the first
  selected catalog.
- `context::require_selected(pinned, states)`: a handle whose catalog the
  call does not read is refused (`Refused`, naming the catalog).
- `PendingParams { changeset }`: `pending_changes`' arguments.
- Test helpers `testing::open`, `open_at` and `changeset_row`; the
  integration harness's `open_changeset(client, catalog)`.
- Tests: `a_write_lands_in_the_named_changeset_only`,
  `a_write_to_a_closed_changeset_is_refused`,
  `another_key_cannot_write_to_a_handle`,
  `a_write_waiting_on_a_changeset_that_closes_is_refused` (the lock race,
  detected through `pg_locks`), `two_handles_of_one_key_stay_apart`,
  `concurrent_writes_to_one_handle_from_two_connections_both_land`,
  `a_write_without_a_changeset_is_a_parameter_error`,
  `a_read_without_a_changeset_is_published_even_for_a_writer_with_drafts`,
  `a_pinned_changeset_applies_only_to_its_own_catalog`,
  `get_concept_with_a_changeset_sees_its_draft`,
  `a_changeset_outside_the_selected_catalogs_is_refused`,
  `get_concept_refuses_a_changeset_outside_its_read_set`,
  `lint_counts_only_the_pinned_changeset_s_paths_as_uncommitted`,
  `a_report_defaults_to_the_pinned_changeset_s_catalog`.

## Changed

Tool parameters (breaking):

- Write tools: `create_concept`, `edit_document`, `replace_body`,
  `set_frontmatter`, `stamp`, `review`, `question`, `unquestion`,
  `add_source`, `log`, `index`, `move_concept`, `split_concept`,
  `delete_concept` take a required `changeset` and no longer take `catalog`.
- Changeset tools: `publish`, `discard`, `revert` take a required
  `changeset` instead of `catalog`; `pending_changes` takes only a required
  `changeset`.
- Read tools: `search`, `get_concept`, `read_document`, `open_questions`,
  `oldest`, `sprawl`, `links_report` and `lint` take an optional `changeset`.
  `lint`'s `pending` flag is replaced by `changeset`.

Results: `WriteReport`, `Published`, `Discarded` and `PendingChanges` name
the handle as `changeset: String` and drop `label`.

Behaviour:

- `write::apply(ctx, changeset, by, op)` resolves the handle, then takes the
  catalog row and the changeset row in the old order. A changeset closed
  while the write waited for its row is now refused ("was published or
  discarded while this write waited; open a new one"), not a defensive
  `Busy`; a row gone with its catalog is refused with `CATALOG_GONE`.
- A full `discard` closes the changeset for good: its handle is refused from
  then on. A write queued behind a publish or discard is refused instead of
  landing in a new changeset.
- Without `changeset` a read is always published, whatever the key has open.
- Overlay errors name the changeset's handle instead of its database id.
- The server instructions say to call `open_changeset` first.
- README: the "Changesets" section and the affected tool rows describe
  handles.

Removed: `own_changeset`, `OwnChangeset`, `changesets::get_or_create`,
`find_open`, `ChangesetKey` and `GET_OR_CREATE_TRIES`. `prune_empty`'s doc
comment now explains the race through `lock`'s re-read.

## Notes

- The plan expected a missing `changeset` to be a JSON-RPC error. rmcp 3.8.0's
  tool router turns that parameter error into an `isError` result whose text
  is ``failed to deserialize parameters: missing field `changeset` ``; the test
  and the plan say so.
- Labels stay (the header, the context's label, the column) until Task 5;
  nothing looks a changeset up by label any more. `adopt_changeset` still
  relabels, until Task 4 removes it.
- In a development database, changesets opened by label before this change
  have handles nothing hands out until Task 4's `list_changesets`.
