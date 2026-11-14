# M14 task 05: provisioner keys

## Added

- Migration `0015_provisioner.sql`: immutable provisioner flag, restrictive key shape and a trigger refusing catalog grants.
- Provisioner creation through CLI `key create --provisioner`, REST and the console; persisted flag in key lists.
- Explicit 16-pair method/template allow-list, API manager/reader hooks, MCP/archive refusal and live admin-target guards.
- Schema, API, audit rollback, CLI and browser regression coverage.

## Changed

- Provisioner user lookup requires email and omits admins; user detail hides credentials, including keys whose grant lists are empty.
- Provisioner creation takes the shared ordered people locks before inserting and recording `key.created` in one transaction. Scoped admins cannot manage these server-wide keys.
- Provisioner user detail reuses its transaction connection for every read under the target lock, without borrowing a second pool slot. Existing store pool callers remain valid.
- Setup invitations refuse admin targets before checking whether a public URL is configured.
- The console creation form guards pending writes with a stable key.
- Task 5 plan corrected against shipped locking APIs and migration order; operations documents the CLI/API/console parity and the allow-list.

## Notes

- `0014_audit_row_locks.sql` was already shipped and is unchanged. Provisioner uses `0015`; downstream unlock and maintenance migrations remain `0016` and `0017`.
- The allow-list reserves `GET /api/v1/audit`; Task 8a supplies the reader and limits provisioners to their own entries.
- No claims ledger or claims MCP server is available; use a fresh read-only, different-model claims audit and record its results in the PR.
