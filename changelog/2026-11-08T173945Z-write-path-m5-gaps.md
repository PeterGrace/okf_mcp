# Write path: the M5 gaps accepted at close-out

Issue #189. M5's close-out (`2026-11-03T114102Z-m5-writes.md`, "Follow-ups")
left four gaps in the write path open: pool limits, an untested defensive
branch in `write::apply`, an untested `parse_checked` guard in
`create_concept`, and an overlay cache with no size cap and duplicate builds.

## Pool limits

- **Configurable pool.** New `database_max_connections` setting (default 16,
  was a fixed 8). `db::connect` takes the size; `cli::connect` passes it for
  `serve` and every subcommand. A zero is refused at load.
- **Queued writers hold no connection.** `App::write_queue` is a
  `KeyedLocks` (new `src/keyed_lock.rs`: one fair tokio mutex per changeset
  id, removed from its map once nobody holds or waits for it).
  `write::apply` takes the changeset's lock there *before* it begins its
  transaction, so writers waiting their turn on one changeset wait in
  process instead of each holding a pooled connection while blocked on the
  changeset's row lock. The row lock is still taken. It still orders writers
  in other replicas, and orders writes against publish and discard.
- **Publish never takes a second connection.** `current_base` used to
  refresh a stale snapshot through the pool while the publish transaction
  held its own connection. `CatalogState::refresh_locked` now reads the
  generation and documents on the publish transaction's connection. That
  is consistent without a repeatable-read transaction, because the publish
  holds the catalog's advisory lock, so no import or publish can commit to
  the catalog meanwhile. `refresh` and `refresh_locked` share the
  check / rebuild-mutex / swap logic (`refresh_to`). `catalog::build` is
  split into its read and its `assemble` step.

## The `Busy` branch

Already resolved by M8: a write that waited on a changeset's row lock while
a publish or discard closed it is now `Refused` ("... was published or
discarded while this write waited"), not `Busy`. It is reachable and
tested: `a_write_waiting_on_a_changeset_that_closes_is_refused`
(`src/write.rs`), and `a_write_queued_behind_a_publish_is_refused` and
`a_write_queued_behind_a_discard_is_refused` (`src/mcp/changesets.rs`).
No change here.

## `parse_checked` in `create_concept`: a real bug

A probe over awkward strings (indicators, line breaks, control and
invisible characters, document markers, surrounding whitespace) in each
frontmatter shape found that the guard *is* reachable: a list of maps
(`x: [{"*a": "b"}]`) renders as a block-list item, `- "*a": b`, and the
parser refused it ("unquoted ': ' inside a plain value"). Two causes:

1. `parse_seq` only read `- key: value` as a mapping for an unquoted first
   key. It now reads a quoted first key too (`- "a": b`), as YAML does.
   `MAP_KEY` matches a quoted item only when a `:` follows the closing
   quote, so `- "a: b"` is still a string.
2. A plain key that a block key cannot start with (`...`, any key starting
   with `.`) was written bare. `block_item` now double-quotes it
   (`block_key`).

okf.py renders the same lines (the parity goldens still match) but cannot
parse them back. Both differences are listed under "Deliberate divergences"
in `src/okf/yaml.rs`.

## Overlay cache

- **Cap with LRU eviction.** New `overlay_cache_size` setting (default 64).
  `Overlays::store` evicts the least recently used *other* entry once the
  cache is past capacity. It uses a linear scan: the cache is small, and an
  ordered index would cost every lookup an update.
- **Single-flight builds.** `Overlays::get` takes a per-changeset
  `KeyedLocks` lock after a miss, then looks the cache up again. Concurrent
  first reads of one changeset now build its overlay once and share it.
  This lock is separate from the write queue, because a write holds the
  queue while it calls `get`.

## Tests

- `keyed_lock`: one holder per key, other keys unaffected, entries removed
  once unused and kept while a waiter needs them.
- `write::writers_queued_on_one_changeset_hold_no_connection`: with the row
  held, three writers produce one blocked lock request, not three (fails
  without the queue), and all three land.
- `mcp::changesets::a_publish_on_a_stale_snapshot_needs_one_connection`:
  with every pooled connection but one held, a publish on a stale snapshot
  completes (within 10 s, well inside sqlx's 30 s acquire timeout) and
  refreshes.
- `okf::ops::content::create_concept_writes_frontmatter_that_parses_back_for_awkward_text`:
  36 awkward strings across 7 shapes create successfully and parse back to
  the input (failed before the fix).
- `okf::yaml`: `a_block_item_map_with_an_awkward_key_parses_back`,
  `a_quoted_block_item_is_a_string_unless_a_colon_follows_it`.
- `overlay`: `past_its_capacity_the_cache_drops_the_least_recently_used`,
  `concurrent_first_reads_build_one_overlay` (fails without the build lock).
- `config`: both new keys read, default, and refuse zero.

## Review fixes (PR #197)

- **A cancelled waiter no longer leaves a `KeyedLocks` entry.** Cleanup
  used `Arc::strong_count`, but a waiter's clone lives inside its pending
  `lock_owned` future, before any guard exists. If the holder left while a
  waiter was queued and the waiter was then cancelled (a timeout, a client
  disconnect), the entry stayed forever. Each entry now keeps an explicit
  `users` count; a `Registration` taken under the map lock *before* the
  wait gives the count back on drop, so cancellation cleans up. Test:
  `a_waiter_cancelled_after_the_holder_leaves_removes_the_entry` (failed
  before).
- **A write no longer waits on a read's overlay build.** A read took the
  build lock and then waited for a pooled connection; a write, already
  holding its transaction's connection, then waited for that build lock. On
  a full pool the two waited on each other until the read's acquire timed
  out. `Overlays::get` now takes a `&PgPool` and is the only path that takes
  the build lock; the new `Overlays::get_in` (a write's transaction) reads
  on its own connection and never waits for it. A read and a write may then
  build the same overlay twice; `store` keeps the newer. Test:
  `a_write_does_not_wait_for_a_read_building_on_a_full_pool` (timed out
  before).
