# OKF MCP Server, Milestone 11 (users and teams): Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. In this project each task is one GitHub sub-issue and one PR (`github-issue-to-pr`).

**Goal:** Every API key, OAuth grant and sign-in code belongs to a user and every catalog to a team, so catalogs and later billing hang off one object each; the CLI, REST API and web console manage users, teams, membership and assignment at parity.

**Architecture:**
- **Three new tables** (`users`, `teams`, `team_members`) and three nullable foreign keys (`api_keys.user_id`, `sign_in_codes.user_id`, `catalogs.owner_team_id`) in migration `0009`. Rows that predate it stay NULL ("unassigned"). Every create path requires a user or owner, so only legacy rows can be NULL.
- **"Everyone is a team."** Creating a user creates that user's personal team in the same transaction, with the user as its owner. Catalogs are owned by exactly one team.
- **Ownership grants no access.** `Principal::reaches` and `require` are untouched. The principal only gains `user: Option<String>` for display (`whoami`, consent, logs).
- **Name-addressed store API**, like catalogs. `store::users` and `store::teams` take and return names; ids stay inside SQL.

**Tech Stack:** Rust 2021, axum 0.8, sqlx 0.9 (Postgres), clap, the existing vanilla-JS console, rustwright browser tests. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-11-okf-users-teams-design.md`.

---

## Scope

| Task | Delivers |
|---|---|
| 1 | Migration 0009, its upgrade test, `OkfError::UnknownName` (404) |
| 2 | `store::users`, `store::teams`, the unit-test fixture user |
| 3 | Catalog ownership: `catalogs::create` takes an owner, `set_owner`, `Catalog.owner_team` |
| 4 | Credential users: `NewKey.user`, `NewSignInCode.user`, redemption copies it, `assign_user` for keys/grants/codes, `store::unassigned` |
| 5 | `Principal.user`, `whoami` gains `user` and `teams`, consent names the user |
| 6 | CLI: `okf-mcp user ...`, `team ...`, `catalog set-owner`, `key assign`, `oauth code assign`, `unassigned`, required `--owner`/`--user` |
| 7 | REST: `/users`, `/teams`, members, assign routes, `/unassigned`, required `user`/`owner` in bodies |
| 8 | Console: Users and Teams pages, pickers and Assign on Keys / Codes / Catalogs, Unassigned view and badge |
| 9 | Docs: `operations.md` sections and parity table, parity test, milestone changelog, follow-up issues |

Deliberately deferred (spec "Non-goals" and "Future work"); Task 9 files an issue for each:

| Item | Where it goes |
|---|---|
| `NOT NULL` on `api_keys.user_id`, `sign_in_codes.user_id`, `catalogs.owner_team_id` | Follow-up issue, once `okf-mcp unassigned` is empty on a deployment |
| Disabling users and teams (`disabled_at`) | Follow-up issue |
| User login (`user_identities`, console sign-in as a user) | Follow-up issue |
| Access through team membership | Follow-up issue |
| Billing and usage records | Out of scope; teams are the owner it will reference |

## Decisions taken while planning

1. **No `UserId`/`TeamId` newtypes.** The spec planned newtypes so the compiler would enforce "new rows always have one". The store API is name-addressed (as for catalogs), so ids never leave SQL. The same guarantee comes from the required, non-`Option` `user: &str` and `owner_team: &str` fields on the create inputs. Records expose `user: Option<String>` / `owner_team: Option<String>` (the name, `None` when unassigned).
2. **Missing `user`/`owner` in a REST body is a 422**, not the spec's 400. Every existing body uses `#[serde(deny_unknown_fields)]` with axum's `Json` extractor, which answers a missing required field with 422. Changing that for three routes would be inconsistent. The CLI's missing flag is clap's usage error (exit 2).
3. **Who may administer users and teams.** Creating, deleting and changing users, teams and memberships, `catalog set-owner` and `GET /unassigned` need an admin key that reaches every catalog. That is the rule `POST /catalogs` already uses (`require_every_catalog`), because users and teams are not catalog-scoped. `GET /users` and `GET /teams` need any admin key, so a scoped admin can still pick a user for the keys it creates. `PUT /keys/{id}/user` and `PUT /oauth/codes/{id}/user` follow the existing `managed` rule for that credential.
4. **`key assign` reaches OAuth grants too**, as the spec says. It is the only `key` subcommand that does. It changes `user_id` only.
5. **The unit-test fixture.** `db::test_db()` creates user `test-user` (personal team `test-user`) after migrating, exposed as `crate::db::TEST_USER` / `TEST_TEAM`. The roughly 100 existing test call sites then gain one argument or field, mechanically. Tests that count users or teams account for the fixture. Integration tests (`tests/support/server.rs`) create the same user explicitly through the public API.
6. **`actor` on sign-in codes becomes optional** (defaulting to the code's name, as keys already do). It is still trimmed, and a blank value counts as absent.
7. **Unknown user or team is `OkfError::UnknownName { noun, name }`** (404). `NotFound` is worded for documents.

## Verified facts the code relies on

- `store::catalogs::bump_admin_generation(&mut PgConnection)` exists and every admin write already calls it inside its transaction (`src/store/catalogs.rs:286`).
- `store::catalogs::is_unique_violation(&sqlx::Error)` (`src/store/catalogs.rs:318`).
- `OkfError::Refused` maps to 409, `Forbidden` to 403, `NotFound`/`CatalogNotSelected` to 404 in `AppError::status` (`src/http/mod.rs:84`). That match has no `_` arm, so a new variant must be added there.
- Redemption inserts the grant in one statement in `store::oauth::codes::redeem` (`src/store/oauth/codes.rs:426`).
- `KeyView::from` and `SignInCodeView::new` destructure every field, so adding a field to the records fails to compile until the views decide on it (intended).
- The console finds pages through `ASSETS` under `js/pages/` (`src/http/ui.rs:40`). Adding `users.js` and `teams.js` registers them, and the nav link goes in `src/http/ui/index.html`'s "Administer" group.
- Existing migration-upgrade tests replay migration files one by one through `sqlx::raw_sql` (`src/db.rs:107`). Task 1 follows that pattern.

## Conventions for every task

- Branch per task from `main`, one PR per task, linked to its sub-issue.
- TDD: write the failing test, run it, implement, run it, commit.
- Before each commit: `just fmt`, `just lint`, `just test` (and `just test-ui` for Task 8).
- Comments for a Python-expert, Rust-novice reader on Rust-specific constructs, as in the existing code.
- Every public item gets a doc comment with `# Arguments`, `# Returns` and `# Errors`.
- One changelog entry per task, `changelog/<UTC ts>-m11-task-<NN>-<slug>.md` (`date -u +%Y-%m-%dT%H-%M-%SZ`), with `## Added`, `## Changed` and `## Notes` like the existing entries. Task 9 also writes the milestone changelog.
- The code in this plan has not been compiled. If an implementation has to differ, fix this plan in the same PR (inline, or with a dated `> **Corrected (issue #N).**` note), then grep later tasks for the changed symbol.
- Placeholders only in tests and docs (`alice`, `family`, `example.com`). No real hostnames or names.
- Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Interfaces shared across tasks

```rust
// src/store/users.rs (Task 2)
pub struct NewUser<'a> { pub name: &'a str, pub display_name: &'a str }
pub struct UserRecord {
    pub name: String,
    pub display_name: String,
    pub created_at: DateTime<Utc>,
    /// Keys and grants (live and revoked) plus sign-in codes naming this user.
    pub credentials: i64,
    /// Team names, sorted; the personal team included.
    pub teams: Vec<String>,
}
pub async fn create(pool: &PgPool, new: &NewUser<'_>, now: DateTime<Utc>) -> Result<UserRecord, OkfError>;
pub async fn list(pool: &PgPool) -> Result<Vec<UserRecord>, OkfError>;
pub async fn find(pool: &PgPool, name: &str) -> Result<UserRecord, OkfError>;
pub async fn delete(pool: &PgPool, name: &str) -> Result<(), OkfError>;
pub(crate) async fn id_of(conn: &mut PgConnection, name: &str) -> Result<String, OkfError>;

// src/store/teams.rs (Task 2)
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Role { Owner, Member }           // as_str / parse like Access
pub struct NewTeam<'a> { pub name: &'a str, pub display_name: &'a str, pub owner: &'a str }
pub struct Member { pub user: String, pub role: Role }
pub struct TeamRecord {
    pub name: String,
    pub display_name: String,
    /// The user whose personal team this is.
    pub personal_of: Option<String>,
    pub created_at: DateTime<Utc>,
    /// Sorted by user name.
    pub members: Vec<Member>,
    /// Names of the catalogs it owns, sorted.
    pub catalogs: Vec<String>,
}
pub async fn create(pool: &PgPool, new: &NewTeam<'_>, now: DateTime<Utc>) -> Result<TeamRecord, OkfError>;
pub async fn list(pool: &PgPool) -> Result<Vec<TeamRecord>, OkfError>;
pub async fn find(pool: &PgPool, name: &str) -> Result<TeamRecord, OkfError>;
pub async fn set_member(pool: &PgPool, team: &str, user: &str, role: Role, now: DateTime<Utc>) -> Result<MemberChange, OkfError>;
pub async fn remove_member(pool: &PgPool, team: &str, user: &str) -> Result<(), OkfError>;
pub async fn delete(pool: &PgPool, name: &str) -> Result<(), OkfError>;
pub(crate) async fn id_of(conn: &mut PgConnection, name: &str) -> Result<String, OkfError>; // added by Task 3 (its first caller)
pub enum MemberChange { Added, RoleChanged, Unchanged }

// src/store/catalogs.rs (Task 3)
pub struct Catalog { /* existing fields */ pub owner_team: Option<String> }
pub async fn create(pool: &PgPool, name: &str, description: &str, owner_team: &str, now: DateTime<Utc>) -> Result<Catalog, OkfError>;
pub async fn set_owner(pool: &PgPool, name: &str, team: &str) -> Result<Option<String>, OkfError>; // previous owner

// src/store/keys.rs (Task 4)
pub struct NewKey<'a> { /* existing */ pub user: &'a str }
pub struct KeyRecord { /* existing */ pub user: Option<String> }
pub async fn assign_user_by_id(pool: &PgPool, id: &str, user: &str) -> Result<String, OkfError>;   // key or grant; returns its name
pub async fn assign_user_target(pool: &PgPool, target: &str, actor: Option<&str>, user: &str) -> Result<String, OkfError>;

// src/store/oauth/codes.rs (Task 4)
pub struct NewSignInCode<'a> { /* existing; actor stays &str but may be "" */ pub user: &'a str }
pub struct SignInCodeRecord { /* existing */ pub user: Option<String> }
pub async fn assign_user(pool: &PgPool, id: &str, user: &str) -> Result<(), OkfError>;

// src/store/unassigned.rs (Task 4)
pub struct Unassigned {
    pub keys: Vec<KeyRecord>,            // kind Key or OAuth, live and revoked
    pub codes: Vec<SignInCodeRecord>,
    pub catalogs: Vec<String>,
}
pub async fn list(pool: &PgPool) -> Result<Unassigned, OkfError>;

// src/auth.rs (Task 5)
pub struct Principal { /* existing */ pub user: Option<String> }

// src/db.rs (Task 2), #[cfg(test)]
pub(crate) const TEST_USER: &str = "test-user";
pub(crate) const TEST_TEAM: &str = "test-user";
```

## File map

| File | Task | Change |
|---|---|---|
| `migrations/0009_users_teams.sql` | 1 | create |
| `src/db.rs` | 1, 2 | upgrade test; table list; fixture user in `test_db()` |
| `src/error.rs`, `src/http/mod.rs` | 1 | `UnknownName` variant, its JSON and 404 |
| `src/store/users.rs`, `src/store/teams.rs` | 2 | create |
| `src/store/mod.rs` | 2, 4 | `pub mod users; pub mod teams; pub mod unassigned;` |
| `src/store/catalogs.rs` + every `catalogs::create(` caller | 3 | owner argument |
| `src/app.rs:608` | 3 | `owner_team` in the literal |
| `src/store/keys.rs`, `src/store/oauth/codes.rs` + every `NewKey {` / `NewSignInCode {` | 4 | user field, assign |
| `src/store/unassigned.rs` | 4 | create |
| `src/auth.rs`, `src/http/api/identity.rs`, `src/oauth/consent.rs`, `src/oauth/authorize.rs` | 5 | user on principal, whoami, consent |
| `src/cli.rs`, `src/cli/people.rs` | 6 | new subcommands (people.rs holds user/team/unassigned handlers) |
| `src/http/api/people.rs` | 7 | create: users, teams, members, unassigned |
| `src/http/api/{mod,keys,oauth,catalogs}.rs` | 7 | merge routes; `user`/`owner` fields; assign routes |
| `src/http/ui/js/pages/{users,teams}.js` | 8 | create |
| `src/http/ui/js/pages/{keys,catalogs}.js`, `core/shell.js`, `index.html` | 8 | pickers, columns, Assign, badge, nav |
| `tests/ui_e2e/{users,teams}.rs`, `tests/ui_e2e/main.rs` | 8 | create, register |
| `tests/support/server.rs` | 3, 4 | fixture user, owner, key user |
| `docs/operations.md`, `README.md` | 9 | user/team sections, parity table |
| `tests/parity.rs` | 9 | create |

## Task dependencies

1 → 2 → 3 → 4 → 5, then 6 and 7 in parallel (both need 5), 8 needs 7, 9 needs 6 and 8.

---

### Task 1: Migration 0009 and the `UnknownName` error

**Files:**
- Create: `migrations/0009_users_teams.sql`
- Modify: `src/db.rs` (table-list test; new upgrade test)
- Modify: `src/error.rs` (variant), `src/http/mod.rs` (status), wherever `OkfError` is matched exhaustively (the compiler lists them)

- [ ] **Step 1: Write the failing upgrade test** in `src/db.rs`'s `tests` module, after the existing 0008 test:

```rust
#[tokio::test(flavor = "multi_thread")]
async fn migration_0009_keeps_every_credential_and_catalog_unassigned() {
    // Arrange: the schema as of 0008, with one catalog, one key, one grant
    // and one sign-in code, written as the pre-0009 server wrote them.
    let db = pg_support::TestDb::start().await;
    for migration in [
        include_str!("../migrations/0001_schema.sql"),
        include_str!("../migrations/0002_changeset_identity.sql"),
        include_str!("../migrations/0003_changeset_handles.sql"),
        include_str!("../migrations/0004_drop_changeset_labels.sql"),
        include_str!("../migrations/0005_key_names.sql"),
        include_str!("../migrations/0006_maintenance.sql"),
        include_str!("../migrations/0007_maintenance_assessment_evidence.sql"),
        include_str!("../migrations/0008_oauth.sql"),
    ] {
        sqlx::raw_sql(migration).execute(&db.pool).await.unwrap();
    }
    sqlx::raw_sql(
        "INSERT INTO catalogs (name, created_at) VALUES ('legacy', now());
         INSERT INTO api_keys (name, sha256, access, all_catalogs, actor, created_at)
             VALUES ('old', decode(repeat('ab', 32), 'hex'), 'write', true, 'old', now());
         INSERT INTO oauth_clients (client_id, kind, redirect_uris, metadata, created_at)
             VALUES ('c1', 'dcr', '{https://client.example.com/cb}', '{}', now());
         INSERT INTO api_keys (name, kind, client_id, access, all_catalogs, actor, created_at)
             VALUES ('old.grant', 'oauth', 'c1', 'read', true, 'old', now());
         INSERT INTO sign_in_codes (name, sha256, access, all_catalogs, actor, created_at, expires_at)
             VALUES ('old-code', decode(repeat('cd', 32), 'hex'), 'read', true, 'old',
                     now(), now() + interval '1 day');",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    // Act
    sqlx::raw_sql(include_str!("../migrations/0009_users_teams.sql"))
        .execute(&db.pool)
        .await
        .unwrap();

    // Assert: every row survived, none was attributed to anyone.
    let unassigned: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM api_keys WHERE user_id IS NULL), \
                (SELECT count(*) FROM sign_in_codes WHERE user_id IS NULL), \
                (SELECT count(*) FROM catalogs WHERE owner_team_id IS NULL)",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(unassigned, (2, 1, 1));
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(users, 0, "the migration creates no user");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_user_with_a_credential_cannot_be_deleted() {
    let db = test_db().await;
    sqlx::raw_sql(
        "INSERT INTO users (name, created_at) VALUES ('alice', now());
         INSERT INTO api_keys (name, sha256, access, all_catalogs, actor, created_at, user_id)
             SELECT 'k', decode(repeat('ef', 32), 'hex'), 'read', true, 'k', now(), id
             FROM users WHERE name = 'alice';",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    let deleted = sqlx::query("DELETE FROM users WHERE name = 'alice'")
        .execute(&db.pool)
        .await;

    let error = deleted.expect_err("RESTRICT refuses");
    assert_eq!(
        error.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23503"),
        "a foreign-key violation"
    );
}
```

Add `"team_members"`, `"teams"` and `"users"` to the sorted expected list in `migrations_create_the_schema_and_rerun_as_a_no_op`.

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test --lib db::tests -- --nocapture`
Expected: FAIL. The new tests can't find `migrations/0009_users_teams.sql` (compile error from `include_str!`).

- [ ] **Step 3: Write the migration** `migrations/0009_users_teams.sql`:

```sql
-- Users and teams (spec 2026-11-11-okf-users-teams-design.md). A credential
-- belongs to a user; a catalog belongs to a team; every user has a personal
-- team. Ownership grants no access. Rows from before this migration stay
-- NULL ("unassigned") until an admin assigns them; the server refuses to
-- create new ones without a user or owner. Every timestamp is supplied by
-- the server clock.

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
-- The primary key leads with team_id; a user's teams need their own index.
CREATE INDEX team_members_user_id ON team_members (user_id);

ALTER TABLE api_keys
    ADD COLUMN user_id text REFERENCES users (id) ON DELETE RESTRICT;
ALTER TABLE sign_in_codes
    ADD COLUMN user_id text REFERENCES users (id) ON DELETE RESTRICT;
ALTER TABLE catalogs
    ADD COLUMN owner_team_id text REFERENCES teams (id) ON DELETE RESTRICT;

-- Serve the RESTRICT checks, the per-user and per-team listings, and
-- `unassigned` (which scans the NULL side; tables are small).
CREATE INDEX api_keys_user_id ON api_keys (user_id) WHERE user_id IS NOT NULL;
CREATE INDEX sign_in_codes_user_id ON sign_in_codes (user_id) WHERE user_id IS NOT NULL;
CREATE INDEX catalogs_owner_team_id ON catalogs (owner_team_id)
    WHERE owner_team_id IS NOT NULL;
```

- [ ] **Step 4: Add `OkfError::UnknownName`** in `src/error.rs`, after `CatalogNotSelected`:

```rust
    /// No user or team by that name.
    #[error("{noun} {name:?} does not exist")]
    UnknownName {
        /// `"user"` or `"team"`.
        noun: &'static str,
        /// The name as given.
        name: String,
    },
```

In `src/http/mod.rs` `status()`, add `| OkfError::UnknownName { .. }` to the `NOT_FOUND` arm. Then run `cargo build`. The other exhaustive matches on `OkfError` are `OkfError::kind` and `OkfError::to_json` in `src/error.rs`: the variant's kind is `"UnknownName"`, and its body carries `noun` and `name`. Add a unit test next to the existing status tests in `src/http/mod.rs`:

> **Corrected (issue #279).** The plan named a JSON code `"unknown_name"` and an MCP "invalid params" class. Neither exists: error kinds are the variant names (`OkfError::kind`, so `"UnknownName"`), and MCP tool errors use the same `to_json` body as REST. The shipped body is `{"error": "UnknownName", "message": "user \"alice\" does not exist", "noun": "user", "name": "alice"}`.

```rust
#[test]
fn an_unknown_user_or_team_is_a_404() {
    let error = AppError(OkfError::UnknownName { noun: "user", name: "alice".into() });
    assert_eq!(error.status(), StatusCode::NOT_FOUND);
    assert_eq!(error.0.to_string(), "user \"alice\" does not exist");
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test --lib -- db::tests http::tests` (libtest takes several filters after `--`)
Expected: PASS.

- [ ] **Step 6: Changelog and commit**

Write `changelog/<ts>-m11-task-01-schema.md`. Run `just fmt && just lint && just test`.

```bash
git add migrations/0009_users_teams.sql src/db.rs src/error.rs src/http/mod.rs src changelog
git commit -m "feat(store): add users, teams and ownership columns (migration 0009)"
```

---

### Task 2: `store::users`, `store::teams` and the test fixture

**Files:**
- Create: `src/store/users.rs`, `src/store/teams.rs`
- Modify: `src/store/mod.rs`, `src/db.rs` (`test_db()` fixture and constants)

Rules (spec "Deletion" and "Data model"), all enforced in one transaction that ends in `bump_admin_generation`:

| Function | Refuses (`OkfError::Refused`, 409) | `UnknownName` (404) |
|---|---|---|
| `users::create` | invalid name; a user **or team** of that name exists | none |
| `users::delete` | user still referenced by any key, grant or code; member of any team but its personal one; personal team owns a catalog | user |
| `teams::create` | invalid name; team or user of that name exists | owner |
| `teams::set_member` | none (`Owner`→`Member` on the last owner is refused) | team, user |
| `teams::remove_member` | last owner; the personal team's own user; not a member | team, user |
| `teams::delete` | personal team; owns a catalog | team |

Names use `catalogs::is_valid_name` (same `^[a-z0-9-]+$` rule).

- [ ] **Step 1: Write the failing tests** at the bottom of `src/store/users.rs` (create the file with just the module doc and `#[cfg(test)] mod tests` first):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_db;
    use crate::store::teams;

    fn now() -> DateTime<Utc> {
        // A fixed instant: tests never read the wall clock.
        DateTime::parse_from_rfc3339("2026-11-11T12:00:00Z").unwrap().with_timezone(&Utc)
    }

    fn alice() -> NewUser<'static> {
        NewUser { name: "alice", display_name: "Alice Example" }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn creating_a_user_creates_its_personal_team_with_it_as_owner() {
        let db = test_db().await;

        let user = create(&db.pool, &alice(), now()).await.unwrap();

        assert_eq!(user.name, "alice");
        assert_eq!(user.teams, ["alice"]);
        let team = teams::find(&db.pool, "alice").await.unwrap();
        assert_eq!(team.personal_of.as_deref(), Some("alice"));
        assert_eq!(team.members.len(), 1);
        assert_eq!(team.members[0].user, "alice");
        assert_eq!(team.members[0].role, teams::Role::Owner);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_user_name_taken_by_a_team_is_refused() {
        let db = test_db().await;
        create(&db.pool, &alice(), now()).await.unwrap();
        teams::create(&db.pool, &teams::NewTeam { name: "family", display_name: "", owner: "alice" }, now())
            .await
            .unwrap();

        let clash = create(&db.pool, &NewUser { name: "family", display_name: "" }, now()).await;

        assert!(matches!(clash, Err(OkfError::Refused { .. })), "{clash:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_invalid_or_duplicate_name_is_refused() {
        let db = test_db().await;
        create(&db.pool, &alice(), now()).await.unwrap();

        let duplicate = create(&db.pool, &alice(), now()).await;
        let invalid = create(&db.pool, &NewUser { name: "Alice", display_name: "" }, now()).await;

        assert!(matches!(duplicate, Err(OkfError::Refused { .. })));
        assert!(matches!(invalid, Err(OkfError::Refused { .. })));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_name_that_loses_the_insert_race_is_refused_not_a_database_error() {
        // `check_free` passes for both of two concurrent creates; the INSERT
        // then hits the UNIQUE constraint. Reproduce that deterministically:
        // a transaction that passed `check_free` before another committed the
        // same team name.
        let db = test_db().await;
        create(&db.pool, &alice(), now()).await.unwrap();
        let mut tx = db.pool.begin().await.unwrap();
        check_free(&mut tx, "family").await.unwrap();
        teams::create(&db.pool, &teams::NewTeam { name: "family", display_name: "", owner: "alice" }, now())
            .await
            .unwrap();

        let inserted = sqlx::query_scalar::<_, String>(
            "INSERT INTO teams (name, created_at) VALUES ('family', now()) RETURNING id",
        )
        .fetch_one(&mut *tx)
        .await;

        let mapped = or_name_taken(inserted, "family");
        assert!(matches!(mapped, Err(OkfError::Refused { .. })), "{mapped:?}");
    }

    #[tokio::test]
    async fn concurrent_user_and_team_creates_of_one_name_give_one_winner_and_a_409() {
        let db = test_db().await;
        create(&db.pool, &alice(), now()).await.unwrap();
        let user = NewUser { name: "family", display_name: "" };
        let team = teams::NewTeam { name: "family", display_name: "", owner: "alice" };

        // `tokio::join!` runs both futures concurrently, like `asyncio.gather`.
        let (made_user, made_team) = tokio::join!(
            create(&db.pool, &user, now()),
            teams::create(&db.pool, &team, now()),
        );

        let refused = [made_user.is_err(), made_team.is_err()];
        assert_eq!(refused.iter().filter(|r| **r).count(), 1, "{made_user:?} {made_team:?}");
        for result in [made_user.err(), made_team.err()].into_iter().flatten() {
            assert!(matches!(result, OkfError::Refused { .. }), "{result:?}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_includes_the_fixture_and_counts_credentials() {
        let db = test_db().await;
        create(&db.pool, &alice(), now()).await.unwrap();

        let users = list(&db.pool).await.unwrap();

        let names: Vec<&str> = users.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(names, ["alice", crate::db::TEST_USER]);
        assert_eq!(users[0].credentials, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unreferenced_user_is_deleted_with_its_personal_team() {
        let db = test_db().await;
        create(&db.pool, &alice(), now()).await.unwrap();
        let before = crate::store::catalogs::admin_generation(&db.pool).await.unwrap();

        delete(&db.pool, "alice").await.unwrap();

        assert!(matches!(find(&db.pool, "alice").await, Err(OkfError::UnknownName { .. })));
        assert!(matches!(teams::find(&db.pool, "alice").await, Err(OkfError::UnknownName { .. })));
        let after = crate::store::catalogs::admin_generation(&db.pool).await.unwrap();
        assert!(after > before, "the delete bumps the admin generation");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_user_in_another_team_is_not_deleted() {
        let db = test_db().await;
        create(&db.pool, &alice(), now()).await.unwrap();
        teams::set_member(&db.pool, crate::db::TEST_TEAM, "alice", teams::Role::Member, now())
            .await
            .unwrap();

        let refused = delete(&db.pool, "alice").await;

        assert!(matches!(refused, Err(OkfError::Refused { .. })), "{refused:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deleting_an_unknown_user_is_unknown_name() {
        let db = test_db().await;
        let missing = delete(&db.pool, "nobody").await;
        assert!(matches!(missing, Err(OkfError::UnknownName { noun: "user", .. })));
    }
}
```

> **Corrected (issue #280).** The deletion guards are tested here, not in Tasks 3 and 4: review asked for them, and migration 0009's columns let the tests seed the references with SQL. `store::users::tests` adds `a_user_named_by_a_key_is_not_deleted`, `a_user_named_by_a_sign_in_code_is_not_deleted`, `a_user_whose_personal_team_owns_a_catalog_is_not_deleted` and the two-transaction `a_catalog_assigned_during_the_delete_is_refused_not_a_database_error`. `store::teams::tests` adds `a_team_that_owns_a_catalog_is_not_deleted`. Each asserts `Refused` and that the user or team, the reference and the admin generation are unchanged. Catalog ownership is seeded by `users::tests::seed_catalog_owned_by` (`catalogs::create`, then `UPDATE catalogs SET owner_team_id`).

Tests for `src/store/teams.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{test_db, TEST_USER};
    use crate::store::users::{self, NewUser};

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-11T12:00:00Z").unwrap().with_timezone(&Utc)
    }

    async fn family(pool: &PgPool) {
        users::create(pool, &NewUser { name: "alice", display_name: "" }, now()).await.unwrap();
        create(pool, &NewTeam { name: "family", display_name: "The family", owner: "alice" }, now())
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_team_starts_with_its_owner() {
        let db = test_db().await;
        family(&db.pool).await;

        let team = find(&db.pool, "family").await.unwrap();

        assert_eq!(team.personal_of, None);
        assert_eq!(team.display_name, "The family");
        assert_eq!(
            team.members.iter().map(|m| (m.user.as_str(), m.role)).collect::<Vec<_>>(),
            [("alice", Role::Owner)]
        );
        assert!(team.catalogs.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unknown_owner_is_unknown_name() {
        let db = test_db().await;
        let made = create(&db.pool, &NewTeam { name: "x", display_name: "", owner: "nobody" }, now()).await;
        assert!(matches!(made, Err(OkfError::UnknownName { noun: "user", .. })));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_member_adds_then_changes_the_role_then_is_unchanged() {
        let db = test_db().await;
        family(&db.pool).await;

        let added = set_member(&db.pool, "family", TEST_USER, Role::Member, now()).await.unwrap();
        let changed = set_member(&db.pool, "family", TEST_USER, Role::Owner, now()).await.unwrap();
        let same = set_member(&db.pool, "family", TEST_USER, Role::Owner, now()).await.unwrap();

        assert_eq!((added, changed, same), (MemberChange::Added, MemberChange::RoleChanged, MemberChange::Unchanged));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_last_owner_cannot_be_removed_or_demoted() {
        let db = test_db().await;
        family(&db.pool).await;

        let removed = remove_member(&db.pool, "family", "alice").await;
        let demoted = set_member(&db.pool, "family", "alice", Role::Member, now()).await;

        assert!(matches!(removed, Err(OkfError::Refused { .. })), "{removed:?}");
        assert!(matches!(demoted, Err(OkfError::Refused { .. })), "{demoted:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_personal_teams_own_user_cannot_be_removed() {
        let db = test_db().await;
        family(&db.pool).await;
        set_member(&db.pool, "alice", TEST_USER, Role::Owner, now()).await.unwrap();

        let removed = remove_member(&db.pool, "alice", "alice").await;

        assert!(matches!(removed, Err(OkfError::Refused { .. })), "{removed:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_member_is_removed() {
        let db = test_db().await;
        family(&db.pool).await;
        set_member(&db.pool, "family", TEST_USER, Role::Member, now()).await.unwrap();

        remove_member(&db.pool, "family", TEST_USER).await.unwrap();

        assert_eq!(find(&db.pool, "family").await.unwrap().members.len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_personal_team_is_never_deleted_but_a_shared_one_is() {
        let db = test_db().await;
        family(&db.pool).await;

        let personal = delete(&db.pool, "alice").await;
        delete(&db.pool, "family").await.unwrap();

        assert!(matches!(personal, Err(OkfError::Refused { .. })));
        assert!(matches!(find(&db.pool, "family").await, Err(OkfError::UnknownName { .. })));
    }
}
```

The "team owning a catalog cannot be deleted" test belongs to Task 3.

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test --lib -- store::users store::teams`
Expected: compile errors (functions not defined).

- [ ] **Step 3: Add the fixture** to `src/db.rs`:

```rust
/// The user (and its personal team) every unit-test database starts with,
/// so tests that create keys, codes and catalogs have an owner to name.
#[cfg(test)]
pub(crate) const TEST_USER: &str = "test-user";
/// [`TEST_USER`]'s personal team.
#[cfg(test)]
pub(crate) const TEST_TEAM: &str = TEST_USER;

/// A migrated throwaway database for one test, with [`TEST_USER`].
#[cfg(test)]
pub(crate) async fn test_db() -> pg_support::TestDb {
    let db = pg_support::TestDb::start().await;
    migrate(&db.pool).await.expect("migrations apply");
    let fixture = crate::store::users::NewUser { name: TEST_USER, display_name: "" };
    crate::store::users::create(&db.pool, &fixture, chrono::Utc::now())
        .await
        .expect("create the fixture user");
    db
}
```

- [ ] **Step 4: Implement `src/store/users.rs`**

```rust
//! Users: the people credentials belong to (spec "Data model"). Creating a
//! user also creates its personal team, so every user can own catalogs.
//! Users are addressed by name; ids stay inside SQL.

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};

use crate::error::OkfError;
use crate::store::catalogs::{bump_admin_generation, is_unique_violation, is_valid_name};

/// A user to create.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewUser<'a> {
    /// `[a-z0-9-]+`; also its personal team's name.
    pub name: &'a str,
    /// Free text shown beside the name.
    pub display_name: &'a str,
}

/// A stored user.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct UserRecord {
    /// Its name.
    pub name: String,
    /// Its display name (may be empty).
    pub display_name: String,
    /// When it was created (server clock).
    pub created_at: DateTime<Utc>,
    /// How many keys, grants (live or revoked) and sign-in codes name it.
    pub credentials: i64,
    /// Its teams' names, sorted, its personal team included.
    pub teams: Vec<String>,
}

// One SELECT shared by `list` and `find` (a macro so `concat!` can append
// each query's tail at compile time; see `select_codes!` in codes.rs).
macro_rules! select_users {
    () => {
        "SELECT u.name, u.display_name, u.created_at, \
                (SELECT count(*) FROM api_keys k WHERE k.user_id = u.id) \
              + (SELECT count(*) FROM sign_in_codes s WHERE s.user_id = u.id) AS credentials, \
                COALESCE((SELECT array_agg(t.name ORDER BY t.name) FROM team_members m \
                          JOIN teams t ON t.id = m.team_id WHERE m.user_id = u.id), \
                         '{}'::text[]) AS teams \
         FROM users u"
    };
}

/// Refuses a name that is not `[a-z0-9-]+`.
pub(crate) fn check_name(noun: &str, name: &str) -> Result<(), OkfError> {
    if is_valid_name(name) {
        Ok(())
    } else {
        Err(OkfError::Refused {
            reason: format!("{noun} name {name:?} must be lowercase letters, digits and '-'"),
        })
    }
}

/// Refuses `name` if a user or a team already has it: a user's personal
/// team takes the user's name, so the two must not clash.
///
/// # Errors
/// [`OkfError::Refused`] when taken; [`OkfError::Database`].
pub(crate) async fn check_free(conn: &mut PgConnection, name: &str) -> Result<(), OkfError> {
    let taken: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM users WHERE name = $1) \
             OR EXISTS (SELECT 1 FROM teams WHERE name = $1)",
    )
    .bind(name)
    .fetch_one(&mut *conn)
    .await?;
    if taken {
        return Err(name_taken(name));
    }
    Ok(())
}

/// The refusal for a name a user or team already has.
fn name_taken(name: &str) -> OkfError {
    OkfError::Refused {
        reason: format!("the name {name:?} is already taken by a user or team"),
    }
}

/// Maps a UNIQUE violation from inserting a user or team row to
/// [`name_taken`]. `check_free` only narrows the race: two creates of one
/// name (a user and a team, or two of either) can both pass it, and then the
/// `users.name` or `teams.name` constraint decides inside the INSERT. Every
/// user and team insert goes through this, so the loser always gets the 409.
///
/// # Arguments
/// * `inserted` - the INSERT's result.
/// * `name` - the name being created.
///
/// # Errors
/// [`name_taken`] for a unique violation; [`OkfError::Database`] otherwise.
// Generic over `T`: the inserts return an id (`String`) or nothing.
pub(crate) fn or_name_taken<T>(inserted: Result<T, sqlx::Error>, name: &str) -> Result<T, OkfError> {
    inserted.map_err(|error| {
        if is_unique_violation(&error) {
            name_taken(name)
        } else {
            error.into()
        }
    })
}

/// Creates a user and its personal team (the user as owner), and bumps the
/// administrative generation, in one transaction.
///
/// # Arguments
/// * `pool` - the database.
/// * `new` - its name and display name.
/// * `now` - the creation time (server clock).
///
/// # Returns
/// The new user.
///
/// # Errors
/// [`OkfError::Refused`] for an invalid name or one a user or team already
/// has; [`OkfError::Database`] otherwise.
pub async fn create(
    pool: &PgPool,
    new: &NewUser<'_>,
    now: DateTime<Utc>,
) -> Result<UserRecord, OkfError> {
    check_name("user", new.name)?;
    let mut tx = pool.begin().await?;
    check_free(&mut tx, new.name).await?;
    let inserted = sqlx::query_scalar::<_, String>(
        "INSERT INTO users (name, display_name, created_at) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(new.name)
    .bind(new.display_name)
    .bind(now)
    .fetch_one(&mut *tx)
    .await;
    let user_id = or_name_taken(inserted, new.name)?;
    // The personal team's insert can lose the race too: to a concurrent
    // `teams::create` of the same name, which no `users` constraint sees.
    let inserted = sqlx::query_scalar::<_, String>(
        "INSERT INTO teams (name, display_name, personal_of, created_at) \
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(new.name)
    .bind(new.display_name)
    .bind(&user_id)
    .bind(now)
    .fetch_one(&mut *tx)
    .await;
    let team_id = or_name_taken(inserted, new.name)?;
    sqlx::query(
        "INSERT INTO team_members (team_id, user_id, role, created_at) VALUES ($1, $2, 'owner', $3)",
    )
    .bind(&team_id)
    .bind(&user_id)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(UserRecord {
        name: new.name.to_string(),
        display_name: new.display_name.to_string(),
        created_at: now,
        credentials: 0,
        teams: vec![new.name.to_string()],
    })
}

/// Every user, by name.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn list(pool: &PgPool) -> Result<Vec<UserRecord>, OkfError> {
    Ok(sqlx::query_as::<_, UserRecord>(concat!(select_users!(), " ORDER BY u.name"))
        .fetch_all(pool)
        .await?)
}

/// The user called `name`.
///
/// # Errors
/// [`OkfError::UnknownName`] if there is none; [`OkfError::Database`].
pub async fn find(pool: &PgPool, name: &str) -> Result<UserRecord, OkfError> {
    sqlx::query_as::<_, UserRecord>(concat!(select_users!(), " WHERE u.name = $1"))
        .bind(name)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| unknown(name))
}

/// The id of the user called `name`, inside a caller's transaction.
///
/// # Errors
/// [`OkfError::UnknownName`]; [`OkfError::Database`].
pub(crate) async fn id_of(conn: &mut PgConnection, name: &str) -> Result<String, OkfError> {
    sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(name)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| unknown(name))
}

fn unknown(name: &str) -> OkfError {
    OkfError::UnknownName { noun: "user", name: name.to_string() }
}

/// Deletes a user that nothing references, with its personal team, and
/// bumps the administrative generation (spec "Deletion").
///
/// # Errors
/// [`OkfError::UnknownName`]; [`OkfError::Refused`] while a key, grant or
/// code names the user, it belongs to another team, or its personal team
/// owns a catalog; [`OkfError::Database`].
pub async fn delete(pool: &PgPool, name: &str) -> Result<(), OkfError> {
    let mut tx = pool.begin().await?;
    // `FOR UPDATE` holds the row so a credential created concurrently waits
    // (its foreign-key check takes `FOR KEY SHARE`) and then fails cleanly.
    let user_id: String = sqlx::query_scalar("SELECT id FROM users WHERE name = $1 FOR UPDATE")
        .bind(name)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| unknown(name))?;
    // > **Corrected (issue #280).** Shipped code also locks the personal team
    // > (`SELECT id FROM teams WHERE personal_of = $1 FOR UPDATE`) before the
    // > counts, counts catalogs by that team id, and deletes the team by id:
    // > otherwise a catalog assigned to the team concurrently is missed and
    // > the team's DELETE fails as a database error instead of a 409. Read
    // > `src/store/users.rs`.
    let (credentials, other_teams, owned): (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM api_keys WHERE user_id = $1) \
              + (SELECT count(*) FROM sign_in_codes WHERE user_id = $1), \
                (SELECT count(*) FROM team_members m JOIN teams t ON t.id = m.team_id \
                 WHERE m.user_id = $1 AND t.personal_of IS DISTINCT FROM $1), \
                (SELECT count(*) FROM catalogs c JOIN teams t ON t.id = c.owner_team_id \
                 WHERE t.personal_of = $1)",
    )
    .bind(&user_id)
    .fetch_one(&mut *tx)
    .await?;
    let refuse = |what: String| Err(OkfError::Refused {
        reason: format!("user {name:?} cannot be deleted: {what}"),
    });
    if credentials > 0 {
        return refuse(format!("{credentials} key(s), grant(s) or sign-in code(s) name it"));
    }
    if other_teams > 0 {
        return refuse(format!("it belongs to {other_teams} other team(s)"));
    }
    if owned > 0 {
        return refuse(format!("its personal team owns {owned} catalog(s)"));
    }
    // Memberships cascade with the team; the team row goes before the user
    // row because `teams.personal_of` restricts the user's delete.
    sqlx::query("DELETE FROM teams WHERE personal_of = $1")
        .bind(&user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(&user_id)
        .execute(&mut *tx)
        .await?;
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(())
}
```

- [ ] **Step 5: Implement `src/store/teams.rs`.** Write it in the same style as `users.rs`:
  - `Role` with `as_str`/`parse`. Derive `clap::ValueEnum` and, for the REST bodies, `serde::Deserialize` with `#[serde(rename_all = "lowercase")]`.
  - `MemberChange` derives `Debug, Clone, Copy, PartialEq, Eq`.
  - `TeamRecord` is assembled from two queries: one row query, plus one members query (`SELECT u.name, m.role FROM team_members m JOIN users u ... ORDER BY u.name`).
  - `catalogs` comes from `array_agg(c.name ORDER BY c.name)` over `catalogs c WHERE c.owner_team_id = t.id`.
  - `list` runs the members query once for all teams (`WHERE m.team_id = ANY($1)`) and groups the rows in Rust, so there is no query per team.
  - The core of `set_member` and `remove_member`:

```rust
/// Adds `user` to `team` with `role`, or changes its role, and bumps the
/// administrative generation when anything changed.
///
/// # Errors
/// [`OkfError::UnknownName`] for the team or user; [`OkfError::Refused`]
/// for demoting the team's last owner; [`OkfError::Database`].
pub async fn set_member(
    pool: &PgPool,
    team: &str,
    user: &str,
    role: Role,
    now: DateTime<Utc>,
) -> Result<MemberChange, OkfError> {
    let mut tx = pool.begin().await?;
    let team_id = lock_team(&mut tx, team).await?;
    let user_id = users::id_of(&mut tx, user).await?;
    let current: Option<String> =
        sqlx::query_scalar("SELECT role FROM team_members WHERE team_id = $1 AND user_id = $2")
            .bind(&team_id)
            .bind(&user_id)
            .fetch_optional(&mut *tx)
            .await?;
    let change = match current.as_deref().map(Role::parse) {
        None => {
            sqlx::query(
                "INSERT INTO team_members (team_id, user_id, role, created_at) VALUES ($1, $2, $3, $4)",
            )
            .bind(&team_id)
            .bind(&user_id)
            .bind(role.as_str())
            .bind(now)
            .execute(&mut *tx)
            .await?;
            MemberChange::Added
        }
        Some(Some(existing)) if existing == role => return Ok(MemberChange::Unchanged),
        Some(Some(Role::Owner)) => {
            refuse_last_owner(&mut tx, &team_id, team, user).await?;
            update_role(&mut tx, &team_id, &user_id, role).await?;
            MemberChange::RoleChanged
        }
        Some(Some(Role::Member)) => {
            update_role(&mut tx, &team_id, &user_id, role).await?;
            MemberChange::RoleChanged
        }
        Some(None) => unreachable!("the schema checks team_members.role"),
    };
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(change)
}

/// The team's id, its row locked `FOR UPDATE` so concurrent membership
/// changes to one team run one at a time (the last-owner check cannot race).
async fn lock_team(conn: &mut PgConnection, name: &str) -> Result<String, OkfError> {
    sqlx::query_scalar("SELECT id FROM teams WHERE name = $1 FOR UPDATE")
        .bind(name)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| unknown(name))
}

/// Refuses when `user` is the only owner of the team.
async fn refuse_last_owner(
    conn: &mut PgConnection,
    team_id: &str,
    team: &str,
    user: &str,
) -> Result<(), OkfError> {
    let owners: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM team_members WHERE team_id = $1 AND role = 'owner'",
    )
    .bind(team_id)
    .fetch_one(&mut *conn)
    .await?;
    if owners <= 1 {
        return Err(OkfError::Refused {
            reason: format!("{user:?} is the last owner of team {team:?}; add another owner first"),
        });
    }
    Ok(())
}
```

  - `remove_member` locks the team and resolves the user. It then refuses when the team's `personal_of` is this user, when the user isn't a member (409, "not a member"), and when it's the last owner. Otherwise it deletes the row and bumps.
  - `delete` locks the team and refuses when `personal_of IS NOT NULL` or when any catalog has `owner_team_id = id`. Otherwise it deletes the team (memberships cascade) and bumps.
  - `create` checks the name, calls `users::check_free`, resolves the owner with `users::id_of`, inserts the team (its result through `users::or_name_taken`, so a team that loses the race to a concurrent user or team of that name is a 409, not a database error), inserts the owner membership, and bumps.

> **Corrected (issue #280).** What shipped differs from the sketch above in four internal ways; read `src/store/teams.rs`. (1) `lock_team` returns a private `LockedTeam { id, personal_of }` (the personal owner's user id), which `remove_member` and `delete` need, so `set_member` uses `locked.id`. (2) `set_member` matches on a private `current_role` helper returning `Option<Role>` (the `Role::parse` failure is an `expect`, as for `Access`), so there is no `unreachable!` arm and no `update_role` helper. (3) The members query selects the team's name and is grouped in Rust by name (`list` runs it unfiltered, `find` with `WHERE t.name = $1`), instead of `WHERE m.team_id = ANY($1)`. (4) `teams::id_of` is not in this task: nothing called it, and `-D warnings` refuses dead code. Task 3, its first caller, adds it beside `lock_team` (`SELECT id FROM teams WHERE name = $1`, `UnknownName { noun: "team" }` when absent). `users::check_name` is `pub(crate)` so `teams::create` reuses it, and `name_taken` is private. Extra tests beyond the plan: `role_spellings_round_trip`, `list_groups_members_by_team`, `set_member_on_an_unknown_team_or_user_is_unknown_name`, `concurrent_removal_of_two_owners_leaves_one` and `a_member_is_removed_but_a_non_member_is_refused` (replacing `a_member_is_removed`).

Register both modules in `src/store/mod.rs` (`pub mod teams;`, `pub mod users;`, keeping the list sorted).

- [ ] **Step 6: Run the tests**

Run: `cargo test --lib -- store::users store::teams db::tests`
Expected: PASS.

- [ ] **Step 7: Run the whole suite.** The fixture adds a user to every unit-test database. Fix any existing test that counts `admin_generation` from zero, by reading it before acting instead.

> **Corrected (issue #280).** Nine tests counted it from zero: six in `store::catalogs::tests` and three in `store::keys::tests`. Each now reads `let start = admin_generation(..)` right after `test_db()` and asserts `start + N`. `db::tests::migrations_create_the_schema_and_rerun_as_a_no_op` asserts that migrations leave it at 0, so it migrates a bare `pg_support::TestDb` (twice) instead of using `test_db()`. Later tasks' tests that assert an absolute generation must do the same.

Run: `cargo test --lib`
Expected: PASS.

- [ ] **Step 8: Changelog and commit**

```bash
git add src/store/users.rs src/store/teams.rs src/store/mod.rs src/db.rs changelog
git commit -m "feat(store): users with personal teams, teams and membership"
```

---

### Task 3: Catalog ownership

**Files:**
- Modify: `src/store/catalogs.rs` (`Catalog.owner_team`, `create`, `set_owner`, `list`, `find`)
- Modify: every `catalogs::create(` caller (grep: 72 sites; 3 non-test: `src/cli.rs:536`, `src/http/api/catalogs.rs:165`, `tests/support/server.rs:123`)
- Modify: `src/app.rs:608` (struct literal)
- Modify: `src/store/teams.rs` tests (catalog-owning team delete)

- [ ] **Step 1: Write the failing tests** in `src/store/catalogs.rs` tests:

```rust
#[tokio::test(flavor = "multi_thread")]
async fn a_new_catalog_is_owned_by_the_named_team() {
    let db = test_db().await;

    let catalog = create(&db.pool, "pg", "", TEST_TEAM, now()).await.unwrap();

    assert_eq!(catalog.owner_team.as_deref(), Some(TEST_TEAM));
    assert_eq!(find(&db.pool, "pg").await.unwrap().owner_team.as_deref(), Some(TEST_TEAM));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_owner_is_unknown_name_and_creates_nothing() {
    let db = test_db().await;

    let made = create(&db.pool, "pg", "", "nobody", now()).await;

    assert!(matches!(made, Err(OkfError::UnknownName { noun: "team", .. })), "{made:?}");
    assert!(list(&db.pool).await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn set_owner_moves_a_catalog_and_returns_the_previous_owner() {
    let db = test_db().await;
    crate::store::users::create(&db.pool, &crate::store::users::NewUser { name: "alice", display_name: "" }, now())
        .await
        .unwrap();
    create(&db.pool, "pg", "", TEST_TEAM, now()).await.unwrap();
    let before = admin_generation(&db.pool).await.unwrap();

    let previous = set_owner(&db.pool, "pg", "alice").await.unwrap();

    assert_eq!(previous.as_deref(), Some(TEST_TEAM));
    assert_eq!(find(&db.pool, "pg").await.unwrap().owner_team.as_deref(), Some("alice"));
    assert!(admin_generation(&db.pool).await.unwrap() > before);
}

#[tokio::test(flavor = "multi_thread")]
async fn set_owner_assigns_a_legacy_catalog() {
    let db = test_db().await;
    sqlx::query("INSERT INTO catalogs (name, created_at) VALUES ('legacy', now())")
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(find(&db.pool, "legacy").await.unwrap().owner_team, None);

    let previous = set_owner(&db.pool, "legacy", TEST_TEAM).await.unwrap();

    assert_eq!(previous, None);
}
```

> **Corrected (issue #280).** Do not add the test below: Task 2 already ships `teams::tests::a_team_that_owns_a_catalog_is_not_deleted` (and the user-side catalog guards in `users::tests`). When `catalogs::create` gains its owner argument, update `users::tests::seed_catalog_owned_by` to pass the team to `catalogs::create` and drop its `UPDATE`, and give the race test's `catalogs::create` call an owner (`TEST_TEAM`), keeping its explicit `UPDATE` inside the held transaction.

In `src/store/teams.rs` tests (superseded, see above):

```rust
#[tokio::test(flavor = "multi_thread")]
async fn a_team_that_owns_a_catalog_is_not_deleted() {
    let db = test_db().await;
    family(&db.pool).await;
    crate::store::catalogs::create(&db.pool, "shared", "", "family", now()).await.unwrap();

    let refused = delete(&db.pool, "family").await;

    assert!(matches!(refused, Err(OkfError::Refused { .. })), "{refused:?}");
    assert_eq!(find(&db.pool, "family").await.unwrap().catalogs, ["shared"]);
}
```

- [ ] **Step 2: Run them to see them fail** (`cargo test --lib store::catalogs`). Expected: compile error, wrong arity.

- [ ] **Step 3: Implement**
  - Add `pub owner_team: Option<String>` (doc: "The team that owns it; `None` only for a catalog from before migration 0009 not yet assigned") to `Catalog`.
  - Every `query_as::<_, Catalog>` now selects `(SELECT t.name FROM teams t WHERE t.id = c.owner_team_id) AS owner_team`, with the table aliased `c`.
  - Add `pub(crate) async fn id_of(conn: &mut PgConnection, name: &str) -> Result<String, OkfError>` to `src/store/teams.rs` (Task 2 left it out because nothing called it yet): `SELECT id FROM teams WHERE name = $1`, mapping no row to the module's `unknown(name)` (`UnknownName { noun: "team" }`).
  - `create` gains `owner_team: &str` before `now`. It resolves `teams::id_of(&mut tx, owner_team)?` inside the transaction before the insert, and inserts `owner_team_id`. `RETURNING` cannot use the subquery, so the returned row is built from `RETURNING id, name, description, generation, created_at` plus `owner_team: Some(owner_team.to_string())`. Use a small private `#[derive(sqlx::FromRow)] struct InsertedCatalog` for the insert.
  - `set_owner(pool, name, team)`:

```rust
/// Makes `team` the owner of catalog `name`, and bumps the administrative
/// generation, in one transaction.
///
/// # Returns
/// The previous owner's name (`None` for a legacy catalog).
///
/// # Errors
/// [`OkfError::CatalogNotSelected`] for an unknown catalog;
/// [`OkfError::UnknownName`] for an unknown team; [`OkfError::Database`].
pub async fn set_owner(pool: &PgPool, name: &str, team: &str) -> Result<Option<String>, OkfError> {
    let mut tx = pool.begin().await?;
    let team_id = crate::store::teams::id_of(&mut tx, team).await?;
    // Lock the row and read the old owner, then change it: two statements
    // in one transaction, so nothing changes the row in between.
    // `Option<Option<String>>`: the outer `None` is "no such catalog", the
    // inner one "a legacy catalog with no owner".
    let previous: Option<Option<String>> = sqlx::query_scalar(
        "SELECT (SELECT t.name FROM teams t WHERE t.id = c.owner_team_id) \
         FROM catalogs c WHERE c.name = $1 FOR UPDATE OF c",
    )
    .bind(name)
    .fetch_optional(&mut *tx)
    .await?;
    let previous = previous.ok_or_else(|| OkfError::CatalogNotSelected { catalog: name.to_string() })?;
    sqlx::query("UPDATE catalogs SET owner_team_id = $2 WHERE name = $1")
        .bind(name)
        .bind(&team_id)
        .execute(&mut *tx)
        .await?;
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(previous)
}
```

- [ ] **Step 4: Update every caller.** Test code under `src/`: insert `crate::db::TEST_TEAM` as the fourth argument. Most sites match one regular shape, so:

```bash
grep -rl 'catalogs::create(' src | xargs perl -0pi -e \
  's/catalogs::create\(\s*([^,]+?),\s*([^,]+?),\s*("[^"]*"|[a-z_]+),\s*/catalogs::create($1, $2, $3, crate::db::TEST_TEAM, /g'
```

Then fix by hand what the compiler still reports:
  - `src/cli.rs` `CatalogCommand::Create` gains `--owner` in Task 6. Until then, pass `&owner` from a new required field there. To keep this task compiling, add the clap field now: `#[arg(long)] owner: String`, documented as "The team that owns it".
  - `src/http/api/catalogs.rs` `NewCatalog` gains `owner: String`. It is required, so a missing one is a 422 (Decision 2). `CatalogSummary` gains `owner_team: Option<String>`, taken from the loaded state (see below).
  - `src/app.rs:608` adds `owner_team: None` to its literal, or reads it from the row it builds from.
  - `tests/support/server.rs` `start_with`: after `migrate`, create the user `tester` through `okf_mcp::store::users::create` and pass `"tester"` as the owner. Also update `tests/roundtrip.rs` and `tests/smoke.rs` the same way.
  - Revert the substitution in `src/store/catalogs.rs`'s own non-test `create` definition if the regex touched it. It shouldn't: the definition is `pub async fn create(`, not `catalogs::create(`.

The loaded catalog state (`CatalogState`, `src/catalog.rs`) carries the description from the row. Carry `owner_team` the same way (`fn owner_team(&self) -> Option<&str>`), so `GET /catalogs` and `catalog list` show it without a query. An owner change bumps `admin_generation`, so the poll reloads it, just as it does for a description change. Follow `description()` exactly.

- [ ] **Step 5: Run the whole suite** (`cargo test`). Expected: PASS.

- [ ] **Step 6: Changelog and commit**

```bash
git commit -am "feat(store): catalogs are owned by a team"
```

---

### Task 4: Credentials belong to users

**Files:**
- Modify: `src/store/keys.rs` (`NewKey.user`, `KeyRecord.user`, `list`, `create_with_id`, assign)
- Modify: `src/store/oauth/codes.rs` (`NewSignInCode.user`, optional actor, `SignInCodeRecord.user`, `redeem`, `assign_user`)
- Create: `src/store/unassigned.rs`
- Modify: every `NewKey {` (24) and `NewSignInCode {` (13) construction; `tests/support/server.rs`; `src/http/api/testing.rs`; the views (`KeyView`, `SignInCodeView`) gain `user`

- [ ] **Step 1: Write the failing tests.** In `src/store/keys.rs` tests:

```rust
#[tokio::test(flavor = "multi_thread")]
async fn a_key_belongs_to_its_user() {
    let db = test_db().await;
    let created = create_with_id(&db.pool, &new_key("k"), now()).await.unwrap();

    let record = list(&db.pool).await.unwrap().into_iter().find(|r| r.id == created.id).unwrap();

    assert_eq!(record.user.as_deref(), Some(TEST_USER));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_for_an_unknown_user_is_unknown_name() {
    let db = test_db().await;
    let new = NewKey { user: "nobody", ..new_key("k") };

    let made = create_with_id(&db.pool, &new, now()).await;

    assert!(matches!(made, Err(OkfError::UnknownName { noun: "user", .. })), "{made:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn assigning_a_legacy_key_sets_its_user_and_bumps_the_generation() {
    let db = test_db().await;
    insert_legacy_test_key(&db.pool, "legacy", Access::Read, "legacy", now()).await;
    let before = catalogs::admin_generation(&db.pool).await.unwrap();

    let name = assign_user_by_id(&db.pool, "legacy", TEST_USER).await.unwrap();

    assert_eq!(name, "legacy");
    let record = list(&db.pool).await.unwrap().into_iter().find(|r| r.id == "legacy").unwrap();
    assert_eq!(record.user.as_deref(), Some(TEST_USER));
    assert!(catalogs::admin_generation(&db.pool).await.unwrap() > before);
}

// > **Corrected (issue #280).** Superseded: Task 2 ships
// > `users::tests::a_user_named_by_a_key_is_not_deleted` and
// > `a_user_named_by_a_sign_in_code_is_not_deleted`, which seed `user_id`
// > with SQL. Do not add this test; the `new_key` helper below is still
// > needed by the other Task 4 tests.
#[tokio::test(flavor = "multi_thread")]
async fn a_user_with_a_key_is_not_deleted() {
    let db = test_db().await;
    users::create(&db.pool, &users::NewUser { name: "alice", display_name: "" }, now()).await.unwrap();
    create_with_id(&db.pool, &NewKey { user: "alice", ..new_key("k") }, now()).await.unwrap();

    let refused = users::delete(&db.pool, "alice").await;

    assert!(matches!(refused, Err(OkfError::Refused { .. })), "{refused:?}");
    assert_eq!(users::find(&db.pool, "alice").await.unwrap().credentials, 1);
}
```

Add a `new_key(name)` helper to that test module if none exists. It returns a `NewKey` with `Access::Read`, `Grants::All`, `actor: name` and `user: TEST_USER`.

In `src/store/oauth/codes.rs` tests:

```rust
#[tokio::test(flavor = "multi_thread")]
async fn a_redeemed_grant_inherits_its_codes_user() {
    let db = test_db().await;
    let client = insert_test_client(&db.pool).await;
    let code = create(&db.pool, &new_code("laptop"), now()).await.unwrap();

    let grant = redeem(&db.pool, &code.id, &client, now()).await.unwrap();

    let record = keys::list(&db.pool).await.unwrap().into_iter().find(|r| r.id == grant).unwrap();
    assert_eq!(record.user.as_deref(), Some(TEST_USER));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_legacy_codes_grant_stays_unassigned() {
    let db = test_db().await;
    let client = insert_test_client(&db.pool).await;
    let code = create(&db.pool, &new_code("laptop"), now()).await.unwrap();
    sqlx::query("UPDATE sign_in_codes SET user_id = NULL WHERE id = $1")
        .bind(&code.id)
        .execute(&db.pool)
        .await
        .unwrap();

    let grant = redeem(&db.pool, &code.id, &client, now()).await.unwrap();

    let record = keys::list(&db.pool).await.unwrap().into_iter().find(|r| r.id == grant).unwrap();
    assert_eq!(record.user, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_blank_actor_defaults_to_the_codes_name() {
    let db = test_db().await;
    let code = create(&db.pool, &NewSignInCode { actor: "  ", ..new_code("laptop") }, now()).await.unwrap();

    let record = list(&db.pool).await.unwrap().into_iter().find(|r| r.id == code.id).unwrap();

    assert_eq!(record.actor, "laptop");
}

#[tokio::test(flavor = "multi_thread")]
async fn assign_user_sets_a_codes_user() {
    let db = test_db().await;
    let code = create(&db.pool, &new_code("laptop"), now()).await.unwrap();
    sqlx::query("UPDATE sign_in_codes SET user_id = NULL WHERE id = $1")
        .bind(&code.id)
        .execute(&db.pool)
        .await
        .unwrap();

    assign_user(&db.pool, &code.id, TEST_USER).await.unwrap();

    let record = list(&db.pool).await.unwrap().into_iter().find(|r| r.id == code.id).unwrap();
    assert_eq!(record.user.as_deref(), Some(TEST_USER));
}
```

The existing test `an_empty_actor_is_refused` (or similar) for codes is replaced by `a_blank_actor_defaults_to_the_codes_name` (Decision 6). Delete the old one.

In `src/store/unassigned.rs` tests:

```rust
#[tokio::test(flavor = "multi_thread")]
async fn unassigned_lists_exactly_the_rows_with_no_user_or_owner() {
    let db = test_db().await;
    insert_legacy_test_key(&db.pool, "legacy", Access::Read, "legacy", now()).await;
    keys::create(&db.pool, &NewKey { user: TEST_USER, ..test_key("owned") }, now()).await.unwrap();
    sqlx::query("INSERT INTO catalogs (name, created_at) VALUES ('orphan', now())")
        .execute(&db.pool)
        .await
        .unwrap();
    catalogs::create(&db.pool, "owned", "", TEST_TEAM, now()).await.unwrap();

    let found = list(&db.pool).await.unwrap();

    assert_eq!(found.keys.iter().map(|k| k.id.as_str()).collect::<Vec<_>>(), ["legacy"]);
    assert!(found.codes.is_empty());
    assert_eq!(found.catalogs, ["orphan"]);
}
```

- [ ] **Step 2: Run them to see them fail.** Expected: compile errors.

- [ ] **Step 3: Implement keys**
  - `NewKey` gains `pub user: &'a str`, documented as "The user it belongs to (must exist)".
  - In `create_with_id`, resolve `let user_id = users::id_of(&mut tx, new.user).await?;` right after `resolve_grants`. Insert it as `user_id` (`$9`).
  - `KeyRecord` and `KeyRow` gain `user: Option<String>`. `list` adds `LEFT JOIN users u ON u.id = k.user_id`, selects `u.name AS user` and adds `u.name` to the `GROUP BY`.
  - `assign_user_by_id(pool, id, user)` resolves the user and runs `UPDATE api_keys SET user_id = $2 WHERE id = $1 RETURNING name` (any kind, revoked included: history is attributed too). A missing row is `OkfError::NotFound { path: format!("keys/{id}") }`. It then bumps and commits.
  - `assign_user_target(pool, target, actor, user)` resolves `target` the way `revoke_target` does (id first, then an unambiguous live name, with `actor` as the tie-break), *including* `kind = 'oauth'` rows, then delegates. Read `revoke_target`'s resolver. If it filters `kind = 'key'`, add a `include_grants: bool` parameter to the shared resolver rather than copying it.

- [ ] **Step 4: Implement codes**
  - `NewSignInCode` gains `pub user: &'a str`.
  - In `create`, a blank actor becomes `new.name`, replacing the refusal: `let actor = match new.actor.trim() { "" => new.name, trimmed => trimmed };`. Resolve the `user_id` after `resolve_grants` and insert it.
  - `CodeRow`/`SignInCodeRecord` gain `user: Option<String>`. In `select_codes!`, add `LEFT JOIN users u ON u.id = s.user_id`, select `u.name AS user`, and add `u.name` to each `GROUP BY` that uses the macro (`list` and `find_live`).
  - `redeem`: `LockedCode` gains `user_id: Option<String>`, and the grant insert adds `user_id` (`$8`, bound to `&code.user_id`).
  - `assign_user(pool, id, user)` runs `UPDATE sign_in_codes SET user_id = $2 WHERE id = $1`. Zero rows is `NotFound { path: format!("oauth/codes/{id}") }`. A code row isn't part of the key cache, so there is no generation bump.

- [ ] **Step 5: Implement `src/store/unassigned.rs`**

```rust
//! Rows from before migration 0009 that no admin has assigned yet: keys and
//! grants with no user, sign-in codes with no user, catalogs with no owner.

use sqlx::PgPool;

use crate::error::OkfError;
use crate::store::keys::{self, KeyRecord};
use crate::store::oauth::codes::{self, SignInCodeRecord};

/// Everything still unassigned.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Unassigned {
    /// Keys and grants with no user, live and revoked, in `keys::list` order.
    pub keys: Vec<KeyRecord>,
    /// Sign-in codes with no user, in `codes::list` order.
    pub codes: Vec<SignInCodeRecord>,
    /// Catalogs with no owner, by name.
    pub catalogs: Vec<String>,
}

impl Unassigned {
    /// Whether nothing is left to assign (the `NOT NULL` follow-up's gate).
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.codes.is_empty() && self.catalogs.is_empty()
    }
}

/// Every unassigned row.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn list(pool: &PgPool) -> Result<Unassigned, OkfError> {
    // The key and code listings already carry `user`; filtering them reuses
    // their joins instead of repeating them. The tables hold at most a few
    // thousand rows, so reading them whole is cheap.
    let keys = keys::list(pool).await?.into_iter().filter(|k| k.user.is_none()).collect();
    let codes = codes::list(pool).await?.into_iter().filter(|c| c.user.is_none()).collect();
    let catalogs = sqlx::query_scalar(
        "SELECT name FROM catalogs WHERE owner_team_id IS NULL ORDER BY name",
    )
    .fetch_all(pool)
    .await?;
    Ok(Unassigned { keys, codes, catalogs })
}
```

Register `pub mod unassigned;` in `src/store/mod.rs`.

- [ ] **Step 6: Update every construction site.** `cargo build --all-targets` lists each `NewKey`/`NewSignInCode` missing `user` (E0063):
  - Test code under `src/` adds `user: crate::db::TEST_USER`.
  - `tests/support/server.rs` `TestServer::key` adds `user: "tester"`. So do the other `tests/` sites.
  - The CLI and the REST handlers get their real field in Tasks 6 and 7. For now, add the clap field `#[arg(long)] user: String` to `KeyCreateArgs` and the code-create args, and `user: String` to `CreateKeyBody`/`CreateCodeBody`, and pass them through.
  - `KeyView` and `SignInCodeView` destructure every field, so they now fail to compile. Add `pub user: Option<String>` to both views and map it through.

- [ ] **Step 7: Run the whole suite** (`cargo test`). Expected: PASS. CLI tests in `src/cli.rs` that run `key create` need a user. Have them run `user create` first in Task 6. For now, insert one through `store::users::create` in their setup.

- [ ] **Step 8: Changelog and commit**

```bash
git commit -am "feat(store): keys, grants and sign-in codes belong to a user"
```

---

### Task 5: The principal's user, `whoami` and consent

**Files:**
- Modify: `src/auth.rs` (`Principal.user`, `From<KeyRecord>`)
- Modify: `src/http/api/identity.rs` (`user`, `teams`)
- Modify: `src/oauth/consent.rs` (`GrantSummary.user`, summary text), `src/oauth/authorize.rs:223`

- [ ] **Step 1: Write the failing tests**

`src/http/api/identity.rs`:

```rust
#[tokio::test]
async fn whoami_names_the_keys_user_and_its_teams() {
    let test = app().await;
    let keys = keys(&test).await;

    let (status, body) = send(&test, "GET", "/api/v1/whoami", Some(&keys.read), None).await;

    assert_eq!(status.as_u16(), 200);
    assert_eq!(body["user"], crate::db::TEST_USER);
    assert_eq!(body["teams"], serde_json::json!([crate::db::TEST_TEAM]));
}

#[tokio::test]
async fn whoami_of_an_unassigned_key_has_no_user() {
    let test = app().await;
    let key = crate::store::keys::insert_legacy_test_key(
        &test.pool, "legacy", crate::store::keys::Access::Read, "legacy", chrono::Utc::now(),
    )
    .await;
    test.app.poll().await.unwrap();

    let (_, body) = send(&test, "GET", "/api/v1/whoami", Some(&key), None).await;

    assert_eq!(body["user"], serde_json::Value::Null);
    assert_eq!(body["teams"], serde_json::json!([]));
}
```

Adapt `test.pool` / `test.app` to the actual `TestApp` field names in `src/http/api/testing.rs`.

`src/oauth/consent.rs`, replacing the expectation in `the_summary_names_access_catalogs_actor_and_default`:

```rust
let grant = GrantSummary {
    access: "write",
    catalogs: "notes".to_string(),
    actor: "example-actor".to_string(),
    user: Some("alice".to_string()),
    default_catalog: None,
};
let html = summary_page_html(&view(), &grant, "nonce");
assert!(html.contains("as <strong>alice</strong> (actor <strong>example-actor</strong>)"));
```

Add a second case with `user: None`, which keeps today's wording ("as actor <strong>example-actor</strong>").

`src/auth.rs`: in the existing `KeyCache` test that checks `found.actor`, also assert `found.user.as_deref() == Some(TEST_USER)`.

- [ ] **Step 2: Run them to see them fail.**

- [ ] **Step 3: Implement**
  - `Principal` gains `pub user: Option<String>` (doc: "The user this credential belongs to; `None` for one not yet assigned. Display only: it grants nothing."). `From<KeyRecord>` moves `record.user` into it. Fix every `Principal { .. }` literal in tests (the compiler lists them) with `user: None`.
  - `WhoAmI` gains `user: Option<String>` and `teams: Vec<String>`. In `whoami`, the handler already scans `key_rows::list` for the name, so take `user` from the same record (not the principal, in case the row changed since the last poll). Then `teams` is `users::find(pool, user).await?.teams` when the user is `Some` and still exists. Map `UnknownName` to an empty list, since a user deleted mid-flight is not an error here.
  - `GrantSummary` gains `user: Option<String>`. `authorize.rs` `summary()` fills it from `record.user.clone()`. In `summary_page_html`, the "as actor …" fragment becomes:

```rust
let who = match &grant.user {
    Some(user) => format!(
        "as <strong>{}</strong> (actor <strong>{}</strong>)",
        escape(user),
        escape(&grant.actor)
    ),
    None => format!("as actor <strong>{}</strong>", escape(&grant.actor)),
};
```

    Splice `{who}` into the existing paragraph in place of `as actor <strong>{actor}</strong>`, and drop the now-unused `actor = …` format argument.

- [ ] **Step 4: Run the tests** (`cargo test --lib -- auth http::api::identity oauth::consent`), then `cargo test`. Expected: PASS.

- [ ] **Step 5: Changelog and commit**

```bash
git commit -am "feat(auth): show a credential's user in whoami and on the consent page"
```

---

### Task 6: CLI

**Files:**
- Modify: `src/cli.rs` (`Command::User`, `Command::Team`, `Command::Unassigned`; `CatalogCommand::SetOwner`; `KeyCommand::Assign`; `CodeCommand::Assign`; `key list` / `catalog list` / `oauth code list` columns; `mod people;`)
- Create: `src/cli/people.rs` (user, team and unassigned handlers, so `cli.rs` doesn't grow by another few hundred lines)

Output follows the existing `println!` style of `catalog create` ("created catalog X") and the column layout of `key list`. Read `KeyCommand::List`'s printer and reuse its column helper (or factor one out of it, in this task, if it's inline).

- [ ] **Step 1: Write the failing tests** in `src/cli.rs`'s test module, following the existing pattern at `src/cli.rs:794` (start a `TestDb`, run the binary's `run` with parsed args against `db.url`, capture stdout). Name and assert:

```rust
#[tokio::test(flavor = "multi_thread")]
async fn user_create_list_and_delete() {
    let db = crate::db::pg_support::TestDb::start().await;
    run_cli(&db, &["migrate"]).await.unwrap();

    let created = run_cli(&db, &["user", "create", "alice", "--display-name", "Alice Example"]).await.unwrap();
    let listed = run_cli(&db, &["user", "list"]).await.unwrap();
    let deleted = run_cli(&db, &["user", "delete", "alice"]).await.unwrap();

    assert_eq!(created, "created user alice (personal team alice)\n");
    assert!(listed.contains("alice") && listed.contains("Alice Example"), "{listed}");
    assert_eq!(deleted, "deleted user alice\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn team_membership_round_trip() {
    let db = crate::db::pg_support::TestDb::start().await;
    run_cli(&db, &["migrate"]).await.unwrap();
    run_cli(&db, &["user", "create", "alice"]).await.unwrap();
    run_cli(&db, &["user", "create", "bob"]).await.unwrap();

    run_cli(&db, &["team", "create", "family", "--owner", "alice"]).await.unwrap();
    let added = run_cli(&db, &["team", "add-member", "family", "bob"]).await.unwrap();
    let promoted = run_cli(&db, &["team", "set-role", "family", "bob", "--role", "owner"]).await.unwrap();
    let removed = run_cli(&db, &["team", "remove-member", "family", "alice"]).await.unwrap();
    let listed = run_cli(&db, &["team", "list"]).await.unwrap();

    assert_eq!(added, "added bob to team family as member\n");
    assert_eq!(promoted, "bob is now owner of team family\n");
    assert_eq!(removed, "removed alice from team family\n");
    assert!(listed.contains("family") && listed.contains("bob(owner)"), "{listed}");
}

#[tokio::test(flavor = "multi_thread")]
async fn catalog_create_needs_an_owner_and_set_owner_moves_it() {
    let db = crate::db::pg_support::TestDb::start().await;
    run_cli(&db, &["migrate"]).await.unwrap();
    run_cli(&db, &["user", "create", "alice"]).await.unwrap();
    run_cli(&db, &["team", "create", "family", "--owner", "alice"]).await.unwrap();

    let missing = parse_cli(&["catalog", "create", "notes"]);
    run_cli(&db, &["catalog", "create", "notes", "--owner", "alice"]).await.unwrap();
    let moved = run_cli(&db, &["catalog", "set-owner", "notes", "--team", "family"]).await.unwrap();
    let listed = run_cli(&db, &["catalog", "list"]).await.unwrap();

    assert!(missing.is_err(), "clap requires --owner");
    assert_eq!(moved, "catalog notes: owner alice -> family\n");
    assert!(listed.contains("family"), "{listed}");
}

#[tokio::test(flavor = "multi_thread")]
async fn key_assign_and_unassigned() {
    let db = crate::db::pg_support::TestDb::start().await;
    run_cli(&db, &["migrate"]).await.unwrap();
    run_cli(&db, &["user", "create", "alice"]).await.unwrap();
    let pool = crate::db::connect(&db.url, 1).await.unwrap();
    crate::store::keys::insert_legacy_test_key(
        &pool, "legacy", crate::store::keys::Access::Read, "legacy", chrono::Utc::now(),
    )
    .await;

    let before = run_cli(&db, &["unassigned"]).await.unwrap();
    let assigned = run_cli(&db, &["key", "assign", "legacy", "--user", "alice"]).await.unwrap();
    let after = run_cli(&db, &["unassigned"]).await.unwrap();

    assert!(before.contains("key legacy"), "{before}");
    assert_eq!(assigned, "key legacy now belongs to alice\n");
    assert_eq!(after, "nothing is unassigned\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_a_user_with_a_key_fails_and_says_why() {
    let db = crate::db::pg_support::TestDb::start().await;
    run_cli(&db, &["migrate"]).await.unwrap();
    run_cli(&db, &["user", "create", "alice"]).await.unwrap();
    run_cli(&db, &["key", "create", "k", "--access", "read", "--all", "--user", "alice"]).await.unwrap();

    let refused = run_cli(&db, &["user", "delete", "alice"]).await;

    let message = refused.unwrap_err().to_string();
    assert!(message.contains("name it"), "{message}");
}
```

`run_cli` and `parse_cli` are whatever the existing CLI tests at `src/cli.rs:794` use (reuse them; if each test inlines the call, factor a `run_cli(db, args) -> anyhow::Result<String>` helper into the test module first). Also update every existing CLI test that runs `key create`, `oauth code create` or `catalog create` to pass `--user` / `--owner`, after a `user create`.

- [ ] **Step 2: Run them to see them fail.**

- [ ] **Step 3: Implement the clap definitions** in `src/cli.rs`:

```rust
    /// Create, list or delete users (each with a personal team).
    #[command(subcommand)]
    User(UserCommand),
    /// Create, list or delete teams, and manage their members.
    #[command(subcommand)]
    Team(TeamCommand),
    /// List keys, grants, sign-in codes and catalogs with no user or owner.
    Unassigned,
```

```rust
/// `okf-mcp user ...`.
#[derive(Debug, Subcommand)]
pub enum UserCommand {
    /// Create a user and its personal team of the same name.
    Create {
        /// Its name (`[a-z0-9-]+`); also its personal team's.
        name: String,
        /// Free text shown beside the name.
        #[arg(long, default_value = "")]
        display_name: String,
    },
    /// List users with their credential counts and teams.
    List,
    /// Delete a user nothing references, with its personal team.
    Delete {
        /// Its name.
        name: String,
    },
}

/// `okf-mcp team ...`.
#[derive(Debug, Subcommand)]
pub enum TeamCommand {
    /// Create a team with its first owner.
    Create {
        /// Its name (`[a-z0-9-]+`).
        name: String,
        /// Free text shown beside the name.
        #[arg(long, default_value = "")]
        display_name: String,
        /// The user who owns it first.
        #[arg(long)]
        owner: String,
    },
    /// List teams with their members and the catalogs they own.
    List,
    /// Add a user to a team.
    AddMember {
        /// The team.
        team: String,
        /// The user.
        user: String,
        /// Its role.
        #[arg(long, value_enum, default_value = "member")]
        role: Role,
    },
    /// Change a member's role.
    SetRole {
        /// The team.
        team: String,
        /// The member.
        user: String,
        /// The new role.
        #[arg(long, value_enum)]
        role: Role,
    },
    /// Remove a member (never a team's last owner).
    RemoveMember {
        /// The team.
        team: String,
        /// The member.
        user: String,
    },
    /// Delete a team that owns no catalog (never a personal team).
    Delete {
        /// Its name.
        name: String,
    },
}
```

`CatalogCommand::Create` gets a documented `owner: String` (from Task 3). Add:

```rust
    /// Make a team the owner of a catalog.
    SetOwner {
        /// The catalog.
        name: String,
        /// The team that owns it from now on.
        #[arg(long)]
        team: String,
    },
```

`KeyCommand` adds the following. `CodeCommand` adds an `Assign { id, #[arg(long)] user }` variant documented as "Give an unassigned sign-in code a user".

```rust
    /// Give a key or OAuth grant a user (its access is unchanged).
    Assign {
        /// The key's or grant's id, or an unambiguous active name.
        id: String,
        /// Select this actor when `id` is a name.
        #[arg(long)]
        actor: Option<String>,
        /// The user it belongs to.
        #[arg(long)]
        user: String,
    },
```

Make the code-create `--actor` optional (`Option<String>`, passed as `""` when absent).

- [ ] **Step 4: Implement the handlers** in `src/cli/people.rs`: `pub(super) async fn user(pool, clock, command: UserCommand) -> anyhow::Result<()>`, `team(...)`, `unassigned(pool)`. They print exactly the strings the tests assert:
  - `user list`: `NAME  DISPLAY NAME  CREDENTIALS  TEAMS`, teams comma-joined.
  - `team list`: `NAME  PERSONAL  MEMBERS  CATALOGS`, members as `user(role)` comma-joined.
  - `unassigned`: one line per row, `key NAME (id ID, actor ACTOR)`, `grant NAME (id ID)`, `code NAME (id ID)` and `catalog NAME`, or `nothing is unassigned`.

  Each mutating handler calls nothing else: a store function already bumps `admin_generation`, which is how a running server notices. In `cli.rs`'s dispatcher, add the arms and `mod people;`.

`key list` and `oauth code list` gain a `USER` column, and `catalog list` an `OWNER` column, each showing `-` for `None`.

- [ ] **Step 5: Run the tests** (`cargo test --lib cli`), then `cargo test`. Expected: PASS.

- [ ] **Step 6: Changelog and commit**

```bash
git commit -am "feat(cli): user, team, assign, set-owner and unassigned commands"
```

---

### Task 7: REST API

**Files:**
- Create: `src/http/api/people.rs` (`/users`, `/teams`, `/teams/{name}/members/{user}`, `/unassigned`)
- Modify: `src/http/api/mod.rs` (`mod people;`, `.merge(people::routes())`)
- Modify: `src/http/api/keys.rs` (`PUT /keys/{id}/user`; `user` in body and view)
- Modify: `src/http/api/oauth.rs` (`PUT /oauth/codes/{id}/user`; `user` in body and view; optional actor)
- Modify: `src/http/api/catalogs.rs` (`PUT /catalogs/{name}/owner`; `owner` in body; `owner_team` in summary)
- Modify: `src/http/api/testing.rs` (keys fixture users)

Use `require_every_catalog` from `catalogs.rs`. If it is private, move it beside `require_admin` in `mod.rs` as `pub(crate)` (Decision 3). After every mutation, call `app.poll()` (log a failure, don't return it), as `changed()` in `keys.rs` does, so the answer and the console see the change at once.

- [ ] **Step 1: Write the failing tests** in `src/http/api/people.rs`, using the existing `testing::{app, keys, send, assert_matrix, Row}` helpers (read `assert_matrix` in `testing.rs`; it checks one route against the read/write/admin/scoped-admin keys):

```rust
#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::http::api::testing::{app, keys, send};

    #[tokio::test]
    async fn an_admin_creates_lists_and_deletes_a_user() {
        let test = app().await;
        let keys = keys(&test).await;

        let (created, body) = send(&test, "POST", "/api/v1/users", Some(&keys.admin),
            Some(json!({"name": "alice", "display_name": "Alice Example"}))).await;
        let (_, listed) = send(&test, "GET", "/api/v1/users", Some(&keys.admin), None).await;
        let (deleted, _) = send(&test, "DELETE", "/api/v1/users/alice", Some(&keys.admin), None).await;

        assert_eq!(created.as_u16(), 201);
        assert_eq!(body["teams"], json!(["alice"]));
        assert!(listed["users"].as_array().unwrap().iter().any(|u| u["name"] == "alice"));
        assert_eq!(deleted.as_u16(), 204);
    }

    #[tokio::test]
    async fn user_and_team_writes_need_an_admin_reaching_every_catalog() {
        let test = app().await;
        let keys = keys(&test).await;
        let body = Some(json!({"name": "alice", "display_name": ""}));

        let (read, _) = send(&test, "POST", "/api/v1/users", Some(&keys.read), body.clone()).await;
        let (scoped, _) = send(&test, "POST", "/api/v1/users", Some(&keys.scoped_admin), body.clone()).await;
        let (listing, _) = send(&test, "GET", "/api/v1/users", Some(&keys.scoped_admin), None).await;

        assert_eq!(read.as_u16(), 403);
        assert_eq!(scoped.as_u16(), 403);
        assert_eq!(listing.as_u16(), 200, "any admin may list users to pick one");
    }

    #[tokio::test]
    async fn membership_routes_and_their_refusals() {
        let test = app().await;
        let keys = keys(&test).await;
        let admin = Some(keys.admin.as_str());
        send(&test, "POST", "/api/v1/users", admin, Some(json!({"name": "alice"}))).await;
        send(&test, "POST", "/api/v1/teams", admin,
             Some(json!({"name": "family", "owner": "alice"}))).await;

        let (added, _) = send(&test, "PUT", "/api/v1/teams/family/members/test-user", admin,
                              Some(json!({"role": "member"}))).await;
        let (last_owner, _) = send(&test, "DELETE", "/api/v1/teams/family/members/alice", admin, None).await;
        let (unknown, _) = send(&test, "PUT", "/api/v1/teams/family/members/nobody", admin,
                                Some(json!({"role": "member"}))).await;
        let (_, detail) = send(&test, "GET", "/api/v1/teams/family", admin, None).await;

        assert_eq!(added.as_u16(), 200);
        assert_eq!(last_owner.as_u16(), 409);
        assert_eq!(unknown.as_u16(), 404);
        assert_eq!(detail["members"], json!([
            {"user": "alice", "role": "owner"},
            {"user": "test-user", "role": "member"},
        ]));
    }

    #[tokio::test]
    async fn a_missing_user_on_key_create_is_a_422() {
        let test = app().await;
        let keys = keys(&test).await;

        let (status, _) = send(&test, "POST", "/api/v1/keys", Some(&keys.admin),
            Some(json!({"name": "k", "access": "read", "all": true}))).await;

        assert_eq!(status.as_u16(), 422);
    }

    #[tokio::test]
    async fn assigning_a_legacy_key_and_listing_unassigned() {
        let test = app().await;
        let keys = keys(&test).await;
        crate::store::keys::insert_legacy_test_key(
            &test.pool, "legacy", crate::store::keys::Access::Read, "legacy", chrono::Utc::now(),
        ).await;

        let (_, before) = send(&test, "GET", "/api/v1/unassigned", Some(&keys.admin), None).await;
        let (assigned, view) = send(&test, "PUT", "/api/v1/keys/legacy/user", Some(&keys.admin),
                                    Some(json!({"user": "test-user"}))).await;
        let (_, after) = send(&test, "GET", "/api/v1/unassigned", Some(&keys.admin), None).await;

        assert_eq!(before["keys"][0]["id"], "legacy");
        assert_eq!(assigned.as_u16(), 200);
        assert_eq!(view["user"], "test-user");
        assert_eq!(after, json!({"keys": [], "codes": [], "catalogs": []}));
    }

    #[tokio::test]
    async fn an_assign_reaches_the_key_cache_after_the_generation_bump() {
        // Spec "Testing: cache reload": the principal a request runs as
        // carries the new user once the route's poll has reloaded the cache.
        let test = app().await;
        let keys = keys(&test).await;
        let legacy = crate::store::keys::insert_legacy_test_key(
            &test.pool, "legacy", crate::store::keys::Access::Read, "legacy", chrono::Utc::now(),
        ).await;
        test.app.poll().await.unwrap();
        assert_eq!(test.app.keys().find(&legacy).unwrap().user, None);

        send(&test, "PUT", "/api/v1/keys/legacy/user", Some(&keys.admin),
             Some(json!({"user": "test-user"}))).await;

        assert_eq!(test.app.keys().find(&legacy).unwrap().user.as_deref(), Some("test-user"));
    }

    #[tokio::test]
    async fn set_owner_on_a_catalog() {
        let test = app().await;
        let keys = keys(&test).await;
        send(&test, "POST", "/api/v1/users", Some(&keys.admin), Some(json!({"name": "alice"}))).await;

        let (status, body) = send(&test, "PUT", "/api/v1/catalogs/main/owner", Some(&keys.admin),
                                  Some(json!({"team": "alice"}))).await;
        let (_, listing) = send(&test, "GET", "/api/v1/catalogs", Some(&keys.admin), None).await;

        assert_eq!(status.as_u16(), 200);
        assert_eq!(body, json!({"name": "main", "previous": "test-user", "owner_team": "alice"}));
        assert_eq!(listing["catalogs"][0]["owner_team"], "alice");
    }
}
```

Adapt `keys.scoped_admin` and `test.pool` to the fixture's real field names. If the `Keys` fixture has no scoped admin key, add one there (an admin granted only `main`). The `display_name` field is `#[serde(default)]`.

- [ ] **Step 2: Run them to see them fail.**

- [ ] **Step 3: Implement `src/http/api/people.rs`**

| Route | Handler | Guard | Body / answer |
|---|---|---|---|
| `GET /users` | `list_users` | `require_admin` | `{users: [UserView]}` |
| `POST /users` | `create_user` | `require_every_catalog` | `{name, display_name?}` → 201 `UserView` |
| `GET /users/{name}` | `user_detail` | `require_admin` | `UserDetail { user: UserView, keys: [KeyView], codes: [SignInCodeView] }` (keys filtered by `manages`) |
| `DELETE /users/{name}` | `delete_user` | `require_every_catalog` | 204 |
| `GET /teams` | `list_teams` | `require_admin` | `{teams: [TeamView]}` |
| `POST /teams` | `create_team` | `require_every_catalog` | `{name, display_name?, owner}` → 201 `TeamView` |
| `GET /teams/{name}` | `team_detail` | `require_admin` | `TeamView` |
| `DELETE /teams/{name}` | `delete_team` | `require_every_catalog` | 204 |
| `PUT /teams/{name}/members/{user}` | `set_member` | `require_every_catalog` | `{role}` → 200 `{change: "added"\|"role_changed"\|"unchanged", team: TeamView}` |
| `DELETE /teams/{name}/members/{user}` | `remove_member` | `require_every_catalog` | 200 `TeamView` |
| `GET /unassigned` | `unassigned` | `require_every_catalog` | `{keys: [KeyView], codes: [SignInCodeView], catalogs: [String]}` |

  - `UserView` mirrors `UserRecord` with `created_at` as `format_timestamp`.
  - `TeamView` mirrors `TeamRecord`: `personal_of`, `members: [{user, role}]` and `catalogs`.
  - Bodies are `#[serde(deny_unknown_fields)]` structs.
  - `KeyView` and `SignInCodeView` are reused from `keys.rs` / `oauth.rs`. Make them, and `SignInCodeView::new`, `pub(crate)` if they aren't.

- [ ] **Step 4: Extend keys, codes and catalogs**
  - `CreateKeyBody.user: String` and `CreateCodeBody.user: String` are required. `CreateCodeBody.actor` becomes `Option<String>`.
  - `PUT /keys/{id}/user`, with body `{user}`: `managed(app, &principal, &id)?` (any kind, including grants), then `key_rows::assign_user_by_id`, then `changed(...)`.
  - `PUT /oauth/codes/{id}/user`: the same scoping as the existing `DELETE /oauth/codes/{id}`. Answers `SignInCodeView`.
  - `NewCatalog.owner: String`. `PUT /catalogs/{name}/owner`, with body `{team}`, uses `require_every_catalog` and `catalog_rows::set_owner`, then `settle(&app)`. It answers `{name, previous, owner_team}`.
  - `CatalogSummary.owner_team` comes from `state.owner_team()` (Task 3).

- [ ] **Step 5: Run the tests** (`cargo test --lib http::api`), then `cargo test`. Expected: PASS.

- [ ] **Step 6: Changelog and commit**

```bash
git commit -am "feat(api): users, teams, membership, assign and unassigned routes"
```

---

### Task 8: Web console

**Files:**
- Create: `src/http/ui/js/pages/users.js`, `src/http/ui/js/pages/teams.js`
- Modify: `src/http/ui/index.html` (nav: Users, Teams under "Administer"), `src/http/ui/js/pages/keys.js`, `src/http/ui/js/pages/catalogs.js`, `src/http/ui/js/core/shell.js` (unassigned badge), `src/http/ui/js/core/router.js` only if a default list needs it (it doesn't)
- Create: `tests/ui_e2e/users.rs`, `tests/ui_e2e/teams.rs`; modify `tests/ui_e2e/main.rs` (`mod users; mod teams;`), `tests/ui_e2e/keys.rs`, `tests/ui_e2e/catalogs.rs`

All rendering uses `h()` from `core/dom.js`, `listTable`, `openDrawer`, `confirmDialog` and `toast`, as `catalogs.js` does. No inline script or style (the CSP forbids them, and `pages_have_no_inline_script_or_style` style tests check it). Every interactive element gets a stable `id` or `data-test`, which the journeys use.

- [ ] **Step 1: Write the failing journeys.** `tests/ui_e2e/users.rs`:

```rust
//! The Users page: create a user (and see its personal team), open its
//! detail, and the delete that is refused while it holds a key.

use okf_mcp::config::Config;
use okf_mcp::store::keys::Access;

use crate::harness::drive;
use crate::support::server::{files, TestServer};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admin_creates_a_user_and_deletes_it() {
    let server = TestServer::start(vec![("main", files(&[("index.md", "# Main\n")]))], Config::default()).await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.open("/users");
        ui.wait_visible("[data-test=user-tester]");
        ui.click("#user-new");
        ui.fill("#user-form input[name=name]", "alice");
        ui.fill("#user-form input[name=display_name]", "Alice Example");
        ui.click("#user-form button[type=submit]");
        ui.wait_visible("[data-test=user-alice]");
        ui.wait_text("[data-test=user-alice]", "alice");
        ui.click("[data-test=user-alice]");
        ui.wait_text("#user-detail", "Personal team: alice");
        ui.click("#user-delete");
        ui.click("#confirm-ok");
        ui.wait_hidden("[data-test=user-alice]");
        ui.assert_no_console_errors();
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_user_holding_a_key_shows_why_it_cannot_be_deleted() {
    let server = TestServer::start(vec![("main", files(&[("index.md", "# Main\n")]))], Config::default()).await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.open("/users");
        // `tester` owns console-admin itself.
        ui.click("[data-test=user-tester]");
        ui.wait_visible("#user-delete[disabled]");
        ui.wait_text("#user-delete-reason", "key");
    })
    .await;
}
```

`tests/ui_e2e/teams.rs`, the journey from the spec:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn create_team_add_member_create_owned_catalog_and_key_for_user() {
    let server = TestServer::start(vec![("main", files(&[("index.md", "# Main\n")]))], Config::default()).await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        // A user to add.
        ui.open("/users");
        ui.click("#user-new");
        ui.fill("#user-form input[name=name]", "alice");
        ui.click("#user-form button[type=submit]");
        ui.wait_visible("[data-test=user-alice]");
        // A team owned by tester, with alice as a member.
        ui.open("/teams");
        ui.click("#team-new");
        ui.fill("#team-form input[name=name]", "family");
        ui.fill("#team-form select[name=owner]", "tester");
        ui.click("#team-form button[type=submit]");
        ui.click("[data-test=team-family]");
        ui.fill("#member-form select[name=user]", "alice");
        ui.click("#member-form button[type=submit]");
        ui.wait_text("#team-members", "alice");
        // A catalog the team owns.
        ui.open("/catalogs");
        ui.click("#catalog-new");
        ui.fill("#catalog-create-form input[name=name]", "shared");
        ui.fill("#catalog-create-form select[name=owner]", "family");
        ui.click("#catalog-create-form button[type=submit]");
        ui.wait_text("[data-catalog=shared]", "shared");
        ui.wait_text("[data-test=catalog-owner-shared]", "family");
        // A key for alice.
        ui.open("/keys");
        ui.click("#key-new");
        ui.fill("#key-form input[name=name]", "alice-laptop");
        ui.fill("#key-form select[name=user]", "alice");
        ui.click("#key-form input[name=catalog][value=shared]");
        ui.click("#key-form button[type=submit]");
        ui.click("#key-secret-done");
        ui.wait_text("[data-test=key-alice-laptop]", "alice");
        ui.assert_no_console_errors();
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_legacy_key_is_assigned_from_the_unassigned_view() {
    let server = TestServer::start(vec![("main", files(&[("index.md", "# Main\n")]))], Config::default()).await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    server.legacy_key("legacy").await; // new TestServer helper: insert a key row with user_id NULL, poll
    drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.wait_text("#unassigned-badge", "1");
        ui.open("/users/unassigned");
        ui.fill("[data-test=assign-key-legacy] select", "tester");
        ui.click("[data-test=assign-key-legacy] button");
        ui.wait_text("#unassigned-empty", "Nothing is unassigned");
        ui.wait_hidden("#unassigned-badge");
    })
    .await;
}
```

Add `TestServer::legacy_key(name)` to `tests/support/server.rs`: it inserts an `api_keys` row through raw SQL with `user_id` NULL (as `insert_legacy_test_key` does) and polls. If `ui.fill` doesn't set `<select>` values, add a `ui.select(selector, value)` helper to `harness.rs` that runs `el.value = v; el.dispatchEvent(new Event('change', {bubbles: true}))` through `eval`.

Update `tests/ui_e2e/keys.rs` and `tests/ui_e2e/catalogs.rs`: their create journeys now choose a user or owner (`tester`) before submitting.

- [ ] **Step 2: Run them to see them fail** (`just test-ui`). Expected: FAIL (the pages don't exist yet).

- [ ] **Step 3: Implement `pages/users.js`**

```js
// Users: the people credentials belong to. Any admin lists them (to pick one
// for a key); an admin that reaches every catalog creates and deletes them.
// '#/users/unassigned' shows every key, grant, code and catalog with no user
// or owner, each with an inline Assign.
import { h } from '../core/dom.js';
import { api, enc } from '../core/api.js';
import { store } from '../core/store.js';
import { toast } from '../core/toast.js';
import { openDrawer, closeDrawer } from '../core/drawer.js';
import { ago, count } from '../core/format.js';
import { listTable } from '../components/list-table.js';
import { confirmDialog } from '../components/confirm.js';
import { refreshUnassigned } from '../core/shell.js';

export async function render(main, rest) {
    if (rest[0] === 'unassigned') return renderUnassigned(main);
    const manager = store.can('admin') && store.whoami.all_catalogs;
    const columns = [
        { key: 'name', label: 'Name', sort: u => u.name,
          render: u => h('code', { dataset: { test: `user-${u.name}` } }, u.name) },
        { key: 'display_name', label: 'Display name',
          render: u => u.display_name || h('span', { class: 'muted' }, '-') },
        { key: 'credentials', label: 'Credentials', sort: u => u.credentials, class: 'num' },
        { key: 'teams', label: 'Teams', render: u => u.teams.join(', ') },
        { key: 'created_at', label: 'Created', sort: u => u.created_at, render: u => ago(u.created_at) },
    ];
    const table = listTable(columns, [], { onRow: openDetail, empty: 'No users yet.' });
    const actions = h('div', { class: 'page-actions' },
        manager ? h('button', { id: 'user-new', onclick: openCreate }, 'New user') : null,
        h('a', { href: '#/users/unassigned' }, 'Unassigned'));
    main.append(h('h1', {}, 'Users'), actions, table);
    await load();

    async function load() {
        try { table.redraw((await api.get('/users')).users); } catch { /* toasted */ }
    }

    function openCreate() {
        const form = h('form', { id: 'user-form' },
            h('label', {}, 'Name', h('input', { name: 'name', required: true, pattern: '[a-z0-9-]+',
                                                autocomplete: 'off', placeholder: 'also the personal team' })),
            h('label', {}, 'Display name', h('input', { name: 'display_name', autocomplete: 'off' })),
            h('button', { type: 'submit' }, 'Create user'));
        form.addEventListener('submit', async event => {
            event.preventDefault();
            const data = new FormData(form);
            const name = String(data.get('name') ?? '').trim();
            try {
                await api.post('/users', { name, display_name: String(data.get('display_name') ?? '') });
            } catch { return; }
            toast(`Created user ${name} and team ${name}.`, 'ok');
            if (!main.isConnected) return;
            closeDrawer();
            await load();
        });
        openDrawer('New user', form);
    }

    async function openDetail(user) {
        let detail;
        try { detail = await api.get(`/users/${enc(user.name)}`); } catch { return; }
        if (!main.isConnected) return;
        const reason = detail.user.credentials > 0
            ? `${count(detail.user.credentials, 'key, grant or code', 'keys, grants or codes')} name this user.`
            : detail.user.teams.length > 1 ? 'This user belongs to other teams.' : '';
        const remove = h('button', { id: 'user-delete', class: 'danger', disabled: !manager || !!reason,
            onclick: async () => {
                if (!await confirmDialog(`Delete user ${user.name} and its personal team?`)) return;
                try { await api.del(`/users/${enc(user.name)}`); } catch { return; }
                toast(`Deleted user ${user.name}.`, 'ok');
                if (!main.isConnected) return;
                closeDrawer();
                await load();
            } }, 'Delete user');
        openDrawer(user.name, h('div', { id: 'user-detail' },
            h('p', {}, `Personal team: ${user.name}`),
            h('p', {}, `Teams: ${detail.user.teams.join(', ')}`),
            h('h3', {}, 'Keys and grants'),
            h('ul', {}, detail.keys.map(k => h('li', {}, h('code', {}, k.name), ` ${k.kind}, ${k.access}`,
                                                   k.revoked_at ? ' (revoked)' : ''))),
            h('h3', {}, 'Sign-in codes'),
            h('ul', {}, detail.codes.map(c => h('li', {}, h('code', {}, c.name), ` ${c.state}`))),
            remove,
            reason ? h('p', { id: 'user-delete-reason', class: 'muted small' }, reason) : null));
    }
}

// Every unassigned row with a user (or team) picker and an Assign button.
async function renderUnassigned(main) {
    main.append(h('h1', {}, 'Unassigned'));
    const body = h('div');
    main.append(body);
    await draw();

    async function draw() {
        let rows, users, teams;
        try {
            [rows, users, teams] = await Promise.all([
                api.get('/unassigned'), api.get('/users'), api.get('/teams')]);
        } catch { return; }
        if (!main.isConnected) return;
        const userNames = users.users.map(u => u.name);
        const teamNames = teams.teams.map(t => t.name);
        const items = [
            ...rows.keys.map(k => assignRow(`key-${k.name}`, `${k.kind === 'oauth' ? 'Grant' : 'Key'} ${k.name} (actor ${k.actor})`,
                                            userNames, user => api.put(`/keys/${enc(k.id)}/user`, { user }))),
            ...rows.codes.map(c => assignRow(`code-${c.name}`, `Sign-in code ${c.name}`,
                                             userNames, user => api.put(`/oauth/codes/${enc(c.id)}/user`, { user }))),
            ...rows.catalogs.map(name => assignRow(`catalog-${name}`, `Catalog ${name}`,
                                                   teamNames, team => api.put(`/catalogs/${enc(name)}/owner`, { team }))),
        ];
        body.replaceChildren(items.length ? h('ul', { class: 'assign-list' }, items)
                                          : h('p', { id: 'unassigned-empty', class: 'muted' }, 'Nothing is unassigned.'));
    }

    function assignRow(test, label, choices, assign) {
        const select = h('select', {}, choices.map(name => h('option', { value: name }, name)));
        return h('li', { dataset: { test: `assign-${test}` } }, label, ' ', select,
            h('button', { onclick: async () => {
                try { await assign(select.value); } catch { return; }
                toast(`Assigned ${label}.`, 'ok');
                await refreshUnassigned();
                if (main.isConnected) await draw();
            } }, 'Assign'));
    }
}
```

- [ ] **Step 4: Implement `pages/teams.js`** with the same structure:
  - **List:** a table with a `data-test=team-<name>` cell and a `personal` badge when `personal_of` is set. Columns: members (`count`) and catalogs (joined).
  - **`#team-new`:** opens `#team-form`, with `name`, `display_name` and an `owner` `<select>` filled from `GET /users`, and POSTs `/teams`.
  - **Detail drawer:** `#team-members` is a list with a role `<select>` per member (PUT `/teams/{t}/members/{u}` `{role}` on change) and a remove button (DELETE).
  - **`#member-form`:** a user `<select>` of users not yet members, plus a role, and PUTs.
  - **`#team-delete`:** disabled with a reason for personal teams and teams that own catalogs.
  - Every write runs only when `store.whoami.all_catalogs`. Without it, the controls render disabled.

- [ ] **Step 5: Keys, codes, catalogs and the shell**
  - **`keys.js`** `newKey` and `newCode` forms: add `h('label', {}, 'User', h('select', { name: 'user', required: true }, ...))`, filled from `GET /users` when the drawer opens. Send `user` in the body. In `newCode`, the actor input loses `required` and gets the placeholder "defaults to the name". Add a `{ key: 'user', label: 'User', sort: k => k.user ?? '', render: k => k.user ?? h('span', { class: 'muted' }, 'unassigned') }` column to both tables. In `showKey`, every key and grant gets the same user picker, preselected with its current user (or empty when unassigned), and an `#key-assign` button labelled "Assign" when unassigned and "Change user" otherwise, that PUTs `/keys/{id}/user`. This matches the CLI and REST, which reassign an already-assigned credential too. `showCode` gets the same for codes. Extend `tests/ui_e2e/keys.rs` with a journey that changes an assigned key's user from `tester` to a second user and sees the USER column follow.
  - **`catalogs.js`:** the create form gets an `owner` `<select>` filled from `GET /teams`, sent as `owner`. Add an `Owner` column rendering `h('span', { dataset: { test: `catalog-owner-${row.name}` } }, row.owner_team ?? 'unassigned')`. `openEdit` gets a "Change owner" select plus button, PUT `/catalogs/{name}/owner`, shown when `creator`.
  - **`index.html`:** in the `data-need="admin"` nav group, add `<a class="nav-link" data-nav="users" href="#/users">Users <span id="unassigned-badge" class="nav-badge" hidden></span></a>` and `<a class="nav-link" data-nav="teams" href="#/teams">Teams</a>`.
  - **`shell.js`:**

```js
// The count of unassigned rows on the Users link, for admins that may
// assign them; hidden at zero and for everyone else.
export async function refreshUnassigned() {
    const badge = document.getElementById('unassigned-badge');
    if (!badge) return;
    if (!(store.can('admin') && store.whoami?.all_catalogs)) { badge.hidden = true; return; }
    let rows;
    try { rows = await api.get('/unassigned', { quiet: true }); } catch { badge.hidden = true; return; }
    const total = rows.keys.length + rows.codes.length + rows.catalogs.length;
    badge.textContent = String(total);
    badge.hidden = total === 0;
}
```

    Call it from `main.js` `enter()` after `refreshFindingsBadge()`, and from the keys and catalogs pages after an assign. Also show `me.user` in `#whoami` when set: `${me.user ? me.user + ' / ' : ''}${me.name ?? me.key_id} - ${me.access}`.

- [ ] **Step 6: Run the journeys** (`just test-ui`) and the unit tests that check the embedded assets (`cargo test --lib http::ui`). Expected: PASS.

- [ ] **Step 7: Changelog and commit**

```bash
git commit -am "feat(console): users and teams pages, user and owner pickers, unassigned view"
```

---

### Task 9: Documentation, parity test and follow-ups

**Files:**
- Modify: `docs/operations.md` (new "Users and teams" section, REST rows, parity table, updated `key create` / `oauth code create` / `catalog create` examples with `--user` / `--owner`; `--actor` no longer required for codes)
- Modify: `README.md` if it shows `key create` or `catalog create`
- Create: `tests/parity.rs`
- Modify: the spec's status line: "delivered in … (plan `docs/superpowers/plans/2026-11-11-okf-mcp-m11-users-teams.md`)", plus a "What shipped differently" section listing Decisions 1, 2 and 6

- [ ] **Step 1: Write the failing parity test** `tests/parity.rs`:

```rust
//! Every admin subcommand for users, teams and assignment has a documented
//! REST route and console control (spec "Web console"): the parity table in
//! docs/operations.md must list each one, so the CLI, API and console cannot
//! drift apart unnoticed.

use clap::CommandFactory;
use okf_mcp::cli::Cli;

/// The subcommands under parity, as `okf-mcp <path>`.
const COVERED: &[&str] = &[
    "user create", "user list", "user delete",
    "team create", "team list", "team add-member", "team set-role", "team remove-member", "team delete",
    "catalog set-owner", "key assign", "oauth code assign", "unassigned",
];

#[test]
fn every_covered_subcommand_exists_and_is_in_the_parity_table() {
    let cli = Cli::command();
    let docs = include_str!("../docs/operations.md");
    let table = docs
        .split("### CLI, API and console parity")
        .nth(1)
        .expect("docs/operations.md has the parity section");
    for path in COVERED {
        // Walk the clap tree to prove the subcommand exists.
        let mut command = &cli;
        for part in path.split(' ') {
            command = command
                .find_subcommand(part)
                .unwrap_or_else(|| panic!("`okf-mcp {path}` is not a subcommand"));
        }
        assert!(table.contains(&format!("`okf-mcp {path}`")), "parity table lacks `okf-mcp {path}`");
    }
}

#[test]
fn every_user_and_team_subcommand_is_covered() {
    let cli = Cli::command();
    for group in ["user", "team"] {
        for sub in cli.find_subcommand(group).expect(group).get_subcommands() {
            let path = format!("{group} {}", sub.get_name());
            assert!(COVERED.contains(&path.as_str()), "`okf-mcp {path}` is missing from COVERED");
        }
    }
}
```

Run it: `cargo test --test parity`. It fails: the section doesn't exist yet. If `okf_mcp::cli::Cli` isn't public, make the module `pub` in `src/lib.rs` (check `src/lib.rs` first; `tests/` already uses `okf_mcp::config`).

- [ ] **Step 2: Write the docs.** In `docs/operations.md`, after the API keys section, add `## Users and teams`, covering:
  - The model in five lines: users, personal teams, teams, catalog ownership, and that ownership grants no access.
  - The CLI commands, with examples using `alice` and `family`.
  - Migrating an existing deployment: run `okf-mcp unassigned`, then `user create`, `key assign`, `oauth code assign` and `catalog set-owner` until it prints "nothing is unassigned".
  - Deletion rules.

  Add the REST routes to the routes table. Then add `### CLI, API and console parity`:

| CLI | REST | Console |
|---|---|---|
| `okf-mcp user create` | `POST /api/v1/users` | Users → New user |
| `okf-mcp user list` | `GET /api/v1/users` | Users |
| `okf-mcp user delete` | `DELETE /api/v1/users/{name}` | Users → user → Delete user |
| `okf-mcp team create` | `POST /api/v1/teams` | Teams → New team |
| `okf-mcp team list` | `GET /api/v1/teams` | Teams |
| `okf-mcp team add-member` | `PUT /api/v1/teams/{name}/members/{user}` | Teams → team → Add member |
| `okf-mcp team set-role` | `PUT /api/v1/teams/{name}/members/{user}` | Teams → team → role |
| `okf-mcp team remove-member` | `DELETE /api/v1/teams/{name}/members/{user}` | Teams → team → Remove |
| `okf-mcp team delete` | `DELETE /api/v1/teams/{name}` | Teams → team → Delete team |
| `okf-mcp catalog set-owner` | `PUT /api/v1/catalogs/{name}/owner` | Catalogs → catalog → Change owner |
| `okf-mcp key assign` | `PUT /api/v1/keys/{id}/user` | API keys → key → Assign; Users → Unassigned |
| `okf-mcp oauth code assign` | `PUT /api/v1/oauth/codes/{id}/user` | API keys → code → Assign; Users → Unassigned |
| `okf-mcp unassigned` | `GET /api/v1/unassigned` | Users → Unassigned (badge on Users) |

- [ ] **Step 3: Run** `cargo test --test parity`, then `just fmt && just lint && just test`. Expected: PASS.

- [ ] **Step 4: File the follow-up issues** (spec "Future work") with `gh issue create`, one each:
  1. "Make credential user and catalog owner NOT NULL once nothing is unassigned"
  2. "Disable users and teams instead of deleting them"
  3. "User login (user_identities)"
  4. "Access through team membership"

  Each body cites the spec section, and each is labelled `enhancement`.

- [ ] **Step 5: Changelogs and commit.** Write this task's entry and the milestone changelog `changelog/<UTC ts>-milestone-11-users-teams.md` (Added / Changed / Notes across all nine tasks, the epic number, and the follow-up issue numbers from Step 4).

```bash
git commit -am "docs: users and teams operations, parity table and test"
```
