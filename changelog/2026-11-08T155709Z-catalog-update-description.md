# Change a catalog's description after creation

Issue #175. A catalog's description could only be set at `catalog create`,
so when a catalog's role changed, `list_catalogs` kept telling every agent
the old text, and the only fix was to delete and re-create the catalog
(losing its history, pending changesets and grants).

## Added

- `okf-mcp catalog update <name> --description <text>` (`src/cli.rs`). It
  refuses an unknown catalog, allows an empty description (as `create`'s
  default does), and prints the old and new text, quoted.
- `store::catalogs::update_description` (`src/store/catalogs.rs`): one
  `UPDATE ... FROM (SELECT ... FOR UPDATE)` statement returns the previous
  text, then bumps `server_state.admin_generation`, as every other `catalog`
  and `key` change does, in one transaction. The catalog's `generation` is
  left alone: it still means "content changed".

## Changed

- `CatalogState`'s description (`src/catalog.rs`) is no longer fixed at
  load. It is a `RwLock<Arc<str>>` outside the snapshot, and `description()`
  returns an `Arc<str>`. `App::poll` (`src/app.rs`) already lists every
  catalog row on each pass; it now copies each row's description in with
  `set_description`, which takes a write lock only when the text differs.
  So a running server's `list_catalogs` shows the new text within one
  `poll_interval`, with no restart and no snapshot rebuild.
- README "Command line" and `docs/operations.md` document the subcommand.

## Not done

- The optional `PATCH /catalogs/{name}` HTTP route from the issue is left
  for a follow-up; the CLI covers the acceptance criteria.
- Renaming a catalog stays out of scope (see the issue).

## Tests

- Store: replaces the text, returns the old one, leaves `generation`,
  bumps `admin_generation`; refuses an unknown catalog without a bump.
- Store, concurrency (from review): two updates queued behind a held row
  lock each return the text they replaced, so one reports the other's.
  Without `FOR UPDATE` in the subquery both report the original text, and
  the test fails (checked by removing it).
- CLI: `update` parses, with an empty `--description`; `--description` is
  required.
- Poll: a new description reaches the loaded `CatalogState` at the next
  pass, and the snapshot is the same `Arc` (not rebuilt).
