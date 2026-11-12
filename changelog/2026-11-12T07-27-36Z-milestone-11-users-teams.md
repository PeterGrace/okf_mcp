# Milestone 11: users and teams (epic #278)

Every API key, OAuth grant and sign-in code belongs to a user, and every
catalog to a team, so catalogs and later billing hang off one object each,
and a later login system has a user to sign in as. Creating a user also
creates its personal team. Ownership grants no access: a credential reaches
exactly the catalogs it is granted, as before. Rows from before migration
`0009` stay unassigned and keep working until an admin assigns them. The
CLI, the REST API and the web console manage users, teams, membership and
assignment at parity. Design:
`docs/superpowers/specs/2026-11-11-okf-users-teams-design.md`; plan:
`docs/superpowers/plans/2026-11-11-okf-mcp-m11-users-teams.md` (PR #277).

## Added

- **Task 1, schema (#279, PR #288):** migration `0009_users_teams.sql`
  (`users`, `teams`, `team_members`; nullable `api_keys.user_id`,
  `sign_in_codes.user_id` and `catalogs.owner_team_id`, each `ON DELETE
  RESTRICT`), its upgrade test, and `OkfError::UnknownName` (404).
- **Task 2, store (#280, PR #289):** `store::users` and `store::teams`
  (personal teams, `owner`/`member` roles, the deletion and last-owner
  rules, every write bumping the administrative generation), and the
  unit-test fixture user `test-user`.
- **Task 3, catalog ownership (#281, PR #290):** `catalogs::create` takes an
  owner team, `catalogs::set_owner`, `Catalog.owner_team`, and the owner on
  the loaded catalog state without a rebuild.
- **Task 4, credential users (#282, PR #291):** a required user on new keys
  and sign-in codes, redemption copying it to the grant,
  `keys::assign_user_by_id` / `assign_user_target` (keys and grants),
  `codes::assign_user`, and `store::unassigned`.
- **Task 5, identity (#283, PR #292):** `Principal.user`; `whoami` gains
  `user` and `teams`; the consent page names the user.
- **Task 6, CLI (#284, PR #294):** `okf-mcp user ...`, `team ...`,
  `catalog set-owner`, `key assign`, `oauth code assign` and `unassigned`;
  required `--user` / `--owner`; `user U` and `owner T` list cells.
- **Task 7, REST (#285, PR #293):** `/api/v1/users`, `/teams`, membership,
  `PUT /keys/{id}/user`, `PUT /oauth/codes/{id}/user`,
  `PUT /catalogs/{catalog}/owner` and `/unassigned`; required `user` /
  `owner` in create bodies.
- **Task 8, console (#286, PR #295):** the Users and Teams pages, user and
  team pickers, Assign / Change user on key, grant and code drawers, Change
  owner on catalogs, the Unassigned view (`#/users/unassigned`) and its
  badge on the Users link.
- **Task 9, documentation (#287):** "Users and teams" in
  `docs/operations.md` with the migration walkthrough, deletion rules, the
  API reference and the CLI/REST/console parity table, checked by
  `tests/admin_parity.rs`; the README; the spec's "What shipped
  differently".

## Changed

- `okf-mcp catalog create` needs `--owner`, and `key create` and
  `oauth code create` need `--user`; the matching REST bodies need `owner` /
  `user` (422 without).
- A sign-in code's actor is optional (CLI, REST and console) and defaults
  to the code's name, as a key's does.
- `key list`, `oauth grant list` and `oauth code list` end with `user U`,
  and `catalog list` has `owner T` before the description (`-` when
  unassigned). Key, code and catalog views carry `user` / `owner_team`.
- `okf-mcp key assign` is the one `key` subcommand that also acts on OAuth
  grants.
- `users::id_of` and `teams::id_of` lock `FOR KEY SHARE`, and every path
  locks a user before a team, so races with a delete answer 404, never a
  database error or a deadlock.

## Notes

- Upgrading assigns nothing. Run `okf-mcp unassigned` (or open Users →
  "What is unassigned") and assign each row until nothing is left;
  unassigned credentials keep working meanwhile.
- Users, teams, membership, `set-owner` and the unassigned listing need an
  admin key that reaches every catalog; reading users and teams needs any
  admin key.
- What differs from the design is in the spec's "What shipped differently":
  no `UserId`/`TeamId` newtypes, 422 rather than 400 for a missing field,
  and the others.
- Follow-up issues (the spec's "Future work"):
  - #296 make the credential user and catalog owner `NOT NULL` once nothing
    is unassigned;
  - #297 disable users and teams instead of deleting them;
  - #298 user login (`user_identities`);
  - #299 access through team membership.
- Billing and usage records stay out of scope; teams are the owner they
  will reference.
