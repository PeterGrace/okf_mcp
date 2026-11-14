# M14 Task 6d: recording settings changes; audit retention (issue #399)

## Added

- The `audit_retention_days` setting (default 400, a whole number from 30
  to 3650), stored as a JSON number in the row the audit log's append-only
  trigger already reads. `settings set`, `PUT /api/v1/settings/{key}` and the
  Settings page take it like any other setting.
- `store::audit::prune(pool, before)` and the constants
  `DEFAULT_RETENTION_DAYS`, `MIN_RETENTION_DAYS` and `MAX_RETENTION_DAYS`.
- The daily prune: `App::prune_audit`, run from `poll` after the settings
  reload, at most once per 24 hours (and at the first poll after start). It
  deletes entries older than the setting plus a one-hour margin
  (`AUDIT_PRUNE_MARGIN_HOURS`), so a small skew between the server's and the
  database's clocks never makes the trigger refuse it. Nothing else deletes
  entries.
- `setting.changed` (`from`, `to`) and `setting.cleared` (`from`) entries,
  written in the change's own transaction. For `support_contact`, which can
  be an email address, a change records only `{"set": bool}` and a reset
  `{}`. Settings entries belong to no team.
- `docs/operations.md`: the `audit_retention_days` row, and in the
  "### Audit log" section (added by Task 7a) the two settings rows and the
  retention rules.
- Tests: retention range and default, an unreadable stored retention,
  entries for set and reset (and none for an unchanged value or a reset with
  no override), the support contact kept out of entries, a failed audit
  write rolling the change back, the CLI's actor, the REST and console
  actors, the prune, a prune reaching into retention deleting nothing, the
  daily poll, and (follow-up #414) the trigger with a configured retention
  and with a stored value below the 30-day floor.

## Changed

- `settings::set(pool, key, value, actor, now)` and
  `settings::reset(pool, key, actor, now)` take the `AuditActor` instead of a
  `by` string. Every caller changed, tests included.
- Setting the value already stored no longer rewrites the row or bumps
  `admin_generation`: it changes and records nothing.
- `settings list` shows `cli:<os user>` (the audit entry's actor label)
  instead of `cli` for an override written from the command line.
- `app::log_pruned` delegates to the new `log_pruned_retrying`, which names
  when the next pass runs, so the daily prune's failure says "in a day".

## Notes

- `set` bumps the administrative generation before it reads the previous
  value: `FOR UPDATE` cannot lock a row that does not exist yet, and the
  generation row serializes every settings write, so `from` is exact even
  for two concurrent first overrides.
- The plan (Task 6d, and Task 9a's settings) carries dated
  `Corrected (issue #399)` notes for these differences.
