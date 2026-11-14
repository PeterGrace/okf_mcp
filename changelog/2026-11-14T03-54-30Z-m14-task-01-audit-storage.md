# M14 Task 1: audit storage and the write helper

Issue #391, epic #390. Plan: `docs/superpowers/plans/2026-11-13-okf-mcp-m14-operator-api.md`, Task 1.

## Added
- Migration `0012_audit.sql`: `audit_people`, `audit_teams`,
  `audit_credentials`, `audit_entries` and `audit_access_counts`; the
  functions `audit_person`, `audit_team_ref` and `audit_credential`, which
  create or refresh an audit row and create nothing for a user deleted
  meanwhile (`FOR KEY SHARE` on the user); and the append-only trigger,
  which refuses every `UPDATE`, a `TRUNCATE`, and a `DELETE` of an entry
  younger than the retention period (`audit_retention_days`, default 400,
  at least 30).
- `src/store/audit.rs`: `Source`, `Via`, `AuditActor` (`cli`, `system`,
  `of`, `stored_label`), the `Action` list (one macro writes the enum,
  `as_str`, `parse` and `ALL`), `Target`, `Entry`, `record` (one entry in the
  caller's transaction), `team_ref`, `catalog_teams`, `key_teams`,
  `forget_person` and `refuse_admin_target`; and the `#[cfg(test)]` reader
  `audit::testing::entries` that later tasks assert with.
- `src/http/requester.rs`: `ClientIp`, `Requester` and the axum extractors
  for `Requester` and `AuditActor` (403 without a principal).
- `console_session::request_ip` (the body `client_ip` now shares) and
  `insert_client_ip`, called by `require_principal` (bearer and cookie
  paths) and `auth::require_key`, so every authenticated request carries
  its `ClientIp`.
- `ConsoleSession::for_test()` (tests only).
- Test `both_middlewares_record_the_client_ip_and_the_credentials_user`:
  the real middlewares in front of the extractors (beyond the plan's list).
- Tests `forgetting_a_user_clears_its_credentials_and_keeps_every_entry` and
  `forgetting_a_user_clears_a_key_assigned_to_it_after_it_was_recorded`
  (review of PR #412): a user's key as actor and a sign-in code as target
  (no `Entry.person`) resolve to the user's person, and `forget_person` sets
  their ids and labels to NULL while the entry and its references survive the
  user's deletion; a key assigned after it was recorded (an audit row with no
  person) is found through `api_keys` and cleared too.
- Tests `a_key_that_acted_before_it_was_assigned_leaves_no_name_once_its_user_is_forgotten`,
  `catalog_teams_is_the_owner_teams_audit_id_or_nothing` and
  `key_teams_is_the_distinct_owners_of_its_grants_and_nothing_else` (review
  round 2 of PR #412).

## Changed
- `Principal` has `user_id: Option<String>`: the key's `user_id` from
  `From<KeyRecord>`, the user's id from `for_session`, `None` in test
  literals.

## Notes
- `AuditActor::stored_label` names every key and grant by its kind
  (`api key`, `oauth grant`), even one with no user: such a key can be
  assigned a user later, and its label must stay in the mutable
  `audit_credentials` where `forget_person` can clear it (review round 2 of
  PR #412; the plan's Decision 17 and Task 1 are corrected).
- Nothing calls `record` yet: Tasks 4, 6a-6d and 7a instrument the store.
  No behaviour a user can see changes, so `docs/operations.md` is left to
  the tasks that add recorded actions (and Task 10's upgrade note).
- Plan corrections (dated `Corrected (issue #391)` notes in Task 1): the
  requester module is `pub mod requester;` (store code reaches it); the
  first store test stores the key `k1` it names, since `audit_credential`
  creates no row for a credential that does not exist; the TRUNCATE test
  uses `CASCADE`, so the trigger, not the foreign key from
  `audit_access_counts`, is what refuses it.
