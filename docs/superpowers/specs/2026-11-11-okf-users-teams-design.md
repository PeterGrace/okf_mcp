# okf-mcp users and teams: design

Status: delivered in milestone 11 (epic #278; sub-issues #279-#287, PRs #288-#295 and the PR for #287), 2026-11-12 (plan `docs/superpowers/plans/2026-11-11-okf-mcp-m11-users-teams.md`). What differs from this design is under "What shipped differently" at the end; the operator's view is `docs/operations.md`, "Users and teams".

## Goal

Tie every credential to a person and every catalog to an owner, so that
catalogs and (later) billing data hang off one object each, and so that a
later login system has a user to sign in as.

Today an API key, an OAuth grant (an `api_keys` row of kind `oauth`) and a
sign-in code each carry only `actor`, a free-text provenance label. One
person routinely holds several credentials with different actors
(`claude-code-tui`, `claude-ai-web`), so actor cannot serve as identity.
Catalogs have no owner at all.

The immediate use case: several people test the service. Some reach one
catalog, some reach several, and some share a catalog. A shared catalog has
to belong to a group rather than to any one of them.

## Non-goals (this round)

- **Login.** Users do not authenticate as themselves. No OIDC, passwords or
  sessions. The schema leaves room for a `user_identities` table later
  (see "Future work").
- **Access through ownership or membership.** Owning a catalog, or belonging
  to the team that owns it, grants no access. Access remains exactly the
  credential's explicit grants (`all_catalogs` / `key_catalogs` /
  `sign_in_code_catalogs`). Owner and access are independent.
- **Billing itself.** This work provides the owner that billing will
  reference; it records no usage.
- **Disabling or hard-deleting users and teams that have history** (see
  "Deletion").

## Decisions

| Question | Decision |
|---|---|
| Who creates users | Admins, through the CLI, REST API and console. |
| Login | Not now; the schema is shaped so it can be added. |
| What owns a catalog | Exactly one team. |
| Individuals | "Everyone is a team": creating a user also creates that user's personal team, with the user as its owner. |
| Groups (a shared catalog, a family) | A team with several members. A personal team may take more members. |
| What owns a credential | A user (never a team). Keys, sign-in codes and OAuth grants. |
| `actor` | Kept unchanged as per-credential provenance. It is no longer required on create; it defaults to the credential's name. |
| Does membership grant access | No (ownership only). Adding access through membership later needs no schema change. |
| Team roles | `owner` and `member`. A role only governs the team itself (its membership), never catalog access. |
| Existing rows | Left unassigned (NULL user or owner), never auto-attributed. Admins assign them. New rows must name a user or owner. |
| Deleting referenced users and teams | Refused (`RESTRICT`). Disabling is future work. |
| Web console | At parity with the CLI in this same work. |

## Data model

Migration `migrations/0009_users_teams.sql`, applied in one transaction.
Every timestamp comes from the server clock, as elsewhere; no column defaults
to `now()`.

```sql
CREATE TABLE users (
    id           text PRIMARY KEY DEFAULT gen_random_uuid()::text,
    name         text NOT NULL UNIQUE CHECK (name ~ '^[a-z0-9-]+$'),
    display_name text NOT NULL DEFAULT '',
    created_at   timestamptz NOT NULL
);

CREATE TABLE teams (
    id           text PRIMARY KEY DEFAULT gen_random_uuid()::text,
    name         text NOT NULL UNIQUE CHECK (name ~ '^[a-z0-9-]+$'),
    display_name text NOT NULL DEFAULT '',
    -- Set on exactly one team per user: the personal team made with it.
    personal_of  text UNIQUE REFERENCES users (id) ON DELETE RESTRICT,
    created_at   timestamptz NOT NULL
);

CREATE TABLE team_members (
    team_id    text NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    user_id    text NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    role       text NOT NULL CHECK (role IN ('owner', 'member')),
    created_at timestamptz NOT NULL,
    PRIMARY KEY (team_id, user_id)
);
CREATE INDEX team_members_user_id ON team_members (user_id);

-- Nullable only for rows that predate this migration; the server refuses
-- to create a key, code or catalog without one.
ALTER TABLE api_keys
    ADD COLUMN user_id text REFERENCES users (id) ON DELETE RESTRICT;
ALTER TABLE sign_in_codes
    ADD COLUMN user_id text REFERENCES users (id) ON DELETE RESTRICT;
ALTER TABLE catalogs
    ADD COLUMN owner_team_id text REFERENCES teams (id) ON DELETE RESTRICT;

CREATE INDEX api_keys_user_id ON api_keys (user_id) WHERE user_id IS NOT NULL;
CREATE INDEX sign_in_codes_user_id ON sign_in_codes (user_id) WHERE user_id IS NOT NULL;
CREATE INDEX catalogs_owner_team_id ON catalogs (owner_team_id)
    WHERE owner_team_id IS NOT NULL;
```

User and team names share one rule but separate namespaces. Creating user
`alice` creates personal team `alice`, so `user create` is refused (409) when
a team of that name exists. A personal team is not renamed.

In Rust, newtypes `UserId` and `TeamId` wrap the ids. A credential's user and
a catalog's owner are `Option<UserId>` / `Option<TeamId>` in the store layer;
`None` occurs only on legacy rows, and the create paths take the non-optional
type, so the compiler enforces "new rows always have one".

### OAuth grants

Redeeming a sign-in code already creates its grant row in one statement;
that statement now also copies the code's `user_id` to the grant. A grant
redeemed from a legacy (unassigned) code stays unassigned until assigned. The
consent page names the user as well as the actor: "... will get write access
to notes as alice (actor claude-ai-web)".

### Cache reload

Every new administrative write bumps `server_state.admin_generation`: user
and team create and delete, membership changes, `key assign` and
`catalog set-owner`. Running servers then reload their key cache, so a
credential's `Principal` (which gains its `user`) stays current.

## Deletion

Revoked keys are kept forever, and their rows reference their user, so a
user who has ever held a credential cannot be deleted under `RESTRICT`. This
is accepted for now:

- `user delete` succeeds only for a user with no credentials (live or
  revoked), no sign-in codes, and no team memberships other than their own
  personal team. It deletes the personal team with the user, and is refused
  if that team owns a catalog.
- `team delete` succeeds only for a non-personal team that owns no catalog.
  Its memberships cascade.
- `team remove-member` refuses to remove a team's last owner, and refuses to
  remove a personal team's user.

That keeps the owner of past billing data from ever disappearing. A
`disabled_at` state for users and teams (no new credentials; live ones
revoked) is future work.

## Admin surface

Every command below is admin-only and has a REST route and a console control.

### CLI

```
okf-mcp user create NAME [--display-name TEXT]   # also creates personal team NAME
okf-mcp user list                                 # name, display name, credentials, teams
okf-mcp user delete NAME
okf-mcp team create NAME [--display-name TEXT] --owner USER
okf-mcp team list                                 # name, personal, members, catalogs owned
okf-mcp team add-member TEAM USER [--role owner|member]   # role defaults to member
okf-mcp team set-role TEAM USER --role owner|member
okf-mcp team remove-member TEAM USER
okf-mcp team delete NAME
okf-mcp catalog create NAME --owner TEAM [...]    # --owner now required
okf-mcp catalog set-owner NAME --team TEAM
okf-mcp key create NAME --user USER [...]         # --user now required; --actor optional
okf-mcp key assign KEY --user USER                # keys and OAuth grants alike
okf-mcp oauth code create NAME --user USER [...]  # --user now required; --actor optional
okf-mcp oauth code assign CODE --user USER        # legacy unredeemed codes
okf-mcp unassigned                                # every key, grant, code and catalog with no user or owner
```

- `key assign` is the one `key` subcommand that also acts on OAuth grants
  (today `okf-mcp key ...` never acts on a grant); it changes only the user,
  never access.
- `key list` gains a USER column and `catalog list` an OWNER column, each
  showing `-` when unassigned.
- `catalog import` needs no change: it only ever imports into an existing
  catalog, which keeps its owner. Ownership is server state, not part of an
  archive, so export is unchanged too.

### REST (`/api/v1`, admin)

| Route | Does |
|---|---|
| `GET /users`, `POST /users`, `GET /users/{name}`, `DELETE /users/{name}` | List (with credential counts and teams), create (`{name, display_name}`), detail (its keys, grants, codes and teams), delete |
| `GET /teams`, `POST /teams`, `GET /teams/{name}`, `DELETE /teams/{name}` | List, create (`{name, display_name, owner}`), detail (members and catalogs owned), delete |
| `PUT /teams/{name}/members/{user}`, `DELETE /teams/{name}/members/{user}` | Add a member or set their role (`{role}`), remove a member |
| `PUT /keys/{id}/user`, `PUT /oauth/codes/{id}/user`, `PUT /catalogs/{name}/owner` | Assign a credential's user, a catalog's owner |
| `GET /unassigned` | Every unassigned key, grant, code and catalog |

The existing key, code and catalog create bodies gain a required `user` or
`owner`, and their list and detail responses gain `user` or `owner_team`
(`null` when unassigned). `GET /whoami` (any key) gains `user` and `teams`
for the calling credential.

### Errors

| Case | REST | CLI |
|---|---|---|
| Missing `user` / `owner` on create | 400 | usage error |
| Unknown user or team | 404 | non-zero, names it |
| Duplicate user or team name; user and team name clash; already a member | 409 | non-zero |
| Delete of a referenced user or team; removing a last owner or a personal team's user; deleting a personal team | 409 | non-zero, says what still references it |
| Any of these from a non-admin key | 403 | n/a |

## Web console

The console stays at parity with the CLI in this work. Following the
existing pattern (one module per page under `src/http/ui/js/pages/`, all
calls through `/api/v1`, rendering on the server):

- **Users** (`pages/users.js`): list, create drawer, delete (disabled with
  the reason while referenced), detail drawer with the user's keys, grants,
  codes and teams.
- **Teams** (`pages/teams.js`): list with a "personal" badge, create drawer
  (first owner required), detail drawer to add and remove members and change
  roles, delete (disabled for personal teams and teams owning catalogs).
- **Keys:** a required User picker on create, an optional actor, a USER
  column, and an Assign action on unassigned keys and grants.
- **Sign-in codes:** a required User picker on create, and Assign on legacy
  codes.
- **Catalogs:** a required Owner (team) picker on create, an OWNER column and
  a Change owner action.
- **Unassigned:** a view listing every unassigned row with an inline Assign,
  and a count in the shell while anything is unassigned.
- **Whoami panel:** shows the credential's user and teams.

`docs/operations.md` gains a parity table (CLI command, REST route, console
control) for this area, so drift shows up in review.

## Testing

- Store unit tests for every new function, against the test Postgres.
- A migration test: seed keys, grants, codes and catalogs at schema 0008,
  migrate, and check that every row survives with NULL ownership, that
  authentication still works, and that `unassigned` lists them all.
- CLI and REST tests for every command and route above, including each 4xx
  in the error table.
- Redemption: a grant inherits its code's user; a legacy code's grant stays
  unassigned.
- Cache reload: an assign or membership change is visible through `whoami`
  after the generation bump.
- Console journeys (rustwright, `tests/ui_e2e/`): new `users.rs` and
  `teams.rs` covering create user, create team, add member, create catalog
  with owner, create key for user and assign a legacy key; the existing
  keys and catalogs journeys updated for the required pickers.
- A parity test that fails when an admin subcommand in this area has no
  documented REST route.

## Future work (filed as issues)

1. **Tighten to `NOT NULL`** (#296). Once `okf-mcp unassigned` reports nothing on a
   deployment, a migration makes `api_keys.user_id`, `sign_in_codes.user_id`
   and `catalogs.owner_team_id` `NOT NULL`, and the `Option`s go away.
2. **Disable users and teams** (`disabled_at`, #297): refuse new credentials and
   revoke live ones, keeping history.
3. **Login** (#298). `user_identities (user_id, provider, subject)`; users sign in
   to the console and approve OAuth consent as themselves, minting their own
   keys within limits an admin sets.
4. **Access through membership** (#299; option B in brainstorming): a credential
   scoped to "my catalogs" reaches its user's teams' catalogs.

## What shipped differently

Read the plan's "Decisions taken while planning" and its `Corrected (issue
#N)` notes for the detail; the per-task changelogs (`changelog/*-m11-task-*`)
give the reasons.

- **No `UserId`/`TeamId` newtypes** (plan Decision 1). The store is
  addressed by name, as catalogs are, so ids never leave SQL. The create
  inputs take a required `user: &str` / `owner_team: &str`, which gives the
  same compile-time guarantee; records carry `user: Option<String>` /
  `owner_team: Option<String>`, the name or `None` when unassigned.
- **A missing `user` or `owner` in a REST body is a 422, not a 400** (plan
  Decision 2), as for every other body (`deny_unknown_fields` with axum's
  `Json` extractor). A missing `--user` / `--owner` is clap's usage error
  (exit 2).
- **A sign-in code's actor is optional** (plan Decision 6), as this design
  planned; it is also trimmed, and a blank value counts as absent, so it
  defaults to the code's name on the CLI, over REST and in the console.
- **An unknown user or team is a new error, `OkfError::UnknownName`**
  (404, kind `UnknownName`, with `noun` and `name` in the JSON body), since
  `NotFound` is worded for documents.
- **"Admin-only" has two levels** (plan Decision 3). Creating, deleting and
  changing users, teams and memberships, `catalog set-owner` and
  `GET /unassigned` need an admin key that reaches every catalog, the rule
  `POST /catalogs` uses. Listing and reading users and teams needs any admin
  key, so a scoped admin can pick the user of a key it creates. Assigning a
  credential follows the existing rule for managing that credential. The
  console shows the controls a key may not use disabled, with the reason.
- **A user's detail shows only the credentials the caller manages.**
  `GET /users/{name}` answers `{user, keys, codes}`, filtered by the keys
  area's reach rule, so it reveals nothing outside a scoped admin's reach.
  Teams are in `user.teams`.
- **Adding an existing member is not a 409.** `team add-member` and
  `team set-role` (and `PUT /teams/{name}/members/{user}`) both upsert: they
  add the user, change its role, or report it unchanged
  (`{change: "added"|"role_changed"|"unchanged"}`). `team remove-member` also
  refuses (409) a user who is not a member.
- **`oauth code assign` takes a code in any state**, not only legacy
  unredeemed ones, and can change a user as well as set one. The grant a
  redeemed code became keeps its own user, assigned with `key assign`.
  `key assign` takes an id or an unambiguous live name, with `--actor` to
  narrow it, as the other `key` subcommands do.
- **Assigning a sign-in code bumps no administrative generation**: codes are
  not in the key cache and are read from the database on every use. Every
  other write in "Cache reload" bumps it.
- **List layouts.** No listing has a header row, so `cut -f` scripts need
  not skip one. `key list`, `oauth grant list` (before `client X`) and
  `oauth code list` end with a labelled `user U` cell, and `catalog list` has
  `owner T` before the description, each `-` when unassigned. `user list` is
  name, `credentials N`, `teams a,b`, then the display name; `team list` is
  name, `personal U`, `members u(role),...`, `catalogs c,...`, then the
  display name. Free text always comes last.
- **Races answer errors, not database failures.** `users::id_of` and
  `teams::id_of` read the row `FOR KEY SHARE`, so a create or assign racing
  a delete is `UnknownName` (404) rather than a foreign-key 500, and every
  path locks a user before a team, so a membership change cannot deadlock
  with a user's deletion.
- **The parity test is `tests/admin_parity.rs`**, since `tests/parity.rs`
  already holds the differential tests against `okf.py`. Besides the table
  rows, it checks that each row's REST route is in the API reference.
- **Unit-test databases start with a fixture user**, `test-user` (personal
  team `test-user`), as `crate::db::TEST_USER` / `TEST_TEAM`; integration
  tests create theirs through the public API (`tests/support/server.rs`).
- **The console's Unassigned view** is `#/users/unassigned`, linked from the
  Users page ("What is unassigned"), with its count as a badge on the Users
  link. The key, grant and code drawers have Assign (unassigned) or Change
  user, and the catalog drawer Change owner (or Assign).
