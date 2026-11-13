# OKF MCP Server, Milestone 14 (operator API): Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. In this project each task is one GitHub sub-issue and one PR (`github-issue-to-pr`); a lettered task (6a, 6b, ...) is its own sub-issue and PR.

> **Depends on milestone 13 (team suspension, epic #362, sub-issues #363-#370).** This plan assumes the M13 plan (`docs/superpowers/plans/2026-11-13-okf-mcp-m13-team-suspension.md`) is implemented as written before Task 1 starts: migration `0011`, `OkfError::Suspended`, `Principal.suspended`, `Principal::check_not_suspended`, `Principal::require_export`, `require = require_level + check_not_suspended`, the selection rules, `teams::suspend` / `unsuspend` and `PUT /api/v1/teams/{name}/suspension`. Every M13 symbol used below is listed in "Verified facts" as "from the M13 plan, Task N". They are not on `main` yet; the founder revisits this plan's accuracy once M13 delivers. If an M13 symbol shipped under another name, correct this plan first (see Conventions).

**Goal:** An operator's own tooling can drive okf-mcp safely and leave a trail: the product name people see is the operator's; a user can be found by email and deleted in one call; a narrow "provisioner" key manages accounts, teams and catalogs but can read nothing; every administrative change, catalog write, export and admin read of other people's data is in a stored, append-only audit log that admins read in full and team owners read for their team; and, on a server that turns it on, operator staff must unlock a customer's catalog (with a reason and a time limit) before opening it.

**Architecture:**
- **One audit table, written in the change's own transaction.** `store::audit::record(&mut tx, actor, entry, now)` is called inside every instrumented store function, so a change and its entry commit or roll back together. People are referred to through `audit_people` (deletion renames the row to `deleted user #N`); personal teams, whose names are user names, are written as `person:<id>` team references for the same reason. A trigger refuses `UPDATE` and any `DELETE` of an entry younger than the retention period.
- **The actor travels as an argument.** Every instrumented store function takes `actor: &AuditActor`: the credential id, the acting user's id (for `audit_people`), its actor label, whether it is an operator credential (admin or provisioner), whether it is a provisioner, the client IP and the source (`cli`, `api`, `console`, `system`). REST handlers get it from an axum extractor; the CLI builds `AuditActor::cli()` (the operating-system user name).
- **Provisioner: one allow-list.** A `provisioner` key reaches no catalog (no grants, refused by a trigger) and is refused by MCP and the archive routes. A middleware on `/api/v1` refuses it on every route not in `PROVISIONER_ROUTES` (matched route template and method); the store functions behind the allowed routes refuse it on admin users, under the row locks they already take.
- **Access gates in the key cache.** The key cache loads an `AccessGates` value (team memberships per user, admin users, entitlements, `require_unlock`, active unlocks, catalogs with maintenance off) shared by every principal, as M13 shares its suspension map. `Principal::admission(catalog, access, now)` decides whether a content read or write is a customer's, the maintenance runner's, an unlocked operator's, an admin read to record, or refused with `UnlockRequired` / `MaintenanceOff`. It runs at the content choke points only: `selection::resolve`, `ToolContext::changeset`, the REST routes that read the store directly, export and import. Administrative routes keep `require`, so deleting a catalog or suspending a team never needs an unlock.
- **Access accounting off the request path.** `ToolContext` notes each recorded admission once per call; `App::note_access` writes it on a tracked background task (an hourly `catalog.admin_read` bucket with an exact request count, or an unlock's tally).

**Tech Stack:** Rust 2021, axum 0.8, sqlx 0.9 (Postgres), clap, apalis (the existing mail queue), the vanilla-JS console, rustwright browser tests. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-13-okf-operator-api-design.md`.

---

## Conventions for every task

- Branch per task from `main`, one PR per task, linked to its sub-issue. Follow `AGENTS.md` (review rounds, keeping branches current, `tools/agentic/`).
- TDD: write the failing test, run it, implement, run it, commit.
- Gates only through `tools/agentic/gate.sh`: `fmt`, `lint` and `test` before every commit, plus `test-ui` for every task that changes the console (2, 3, 5, 8c, 9e). Use `gate.sh narrow <cargo test args>` while iterating; it passes its arguments straight to `cargo test`, which takes **one** test-name filter per run, so run several filters as several commands.
- Comments for a Python-expert, Rust-novice reader on Rust-specific constructs, as in the existing code. Every public item gets a doc comment with `# Arguments`, `# Returns` and `# Errors`. No `.unwrap()` outside tests.
- One changelog entry per task, `changelog/<UTC ts>-m14-task-<NN>-<slug>.md` (timestamp from `date -u +%Y-%m-%dT%H-%M-%SZ`; `<NN>` is `01`, `06a`, ...), with `## Added`, `## Changed` and `## Notes`. Task 10 also writes the milestone changelog.
- The code in this plan has not been compiled. If an implementation has to differ, fix this plan in the same PR (inline, or with a dated `> **Corrected (issue #N).**` note), then grep later tasks for the changed symbol.
- Changing a store function's signature means changing every caller in the same PR, tests included: find them with `grep -rn "<module>::<function>(" src tests`.
- Console DOM rule: every conditional child goes through `h()` or `fill()` from `core/dom.js`, never the DOM's native `append` / `replaceChildren` with a possibly-null child (issue #302).
- Placeholders only in tests and docs: `alice`, `bob`, `staff`, `family`, `example.com`, `Acme Notes`. No real hostnames, domains or names, and nothing specific to one operator: the features are generic.
- Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Verified facts the code relies on

Checked against `origin/main` at `9a84af6` (v0.15.0) on 2026-11-13, plus the merged M13 plan. Line numbers are on that commit; M13 moves some of them.

**From the M13 plan (not yet on `main`):**
- Migration `0011_team_suspension.sql`, so this milestone's migrations are `0012` to `0016` (M13 plan, Task 1).
- `teams::suspend(pool, name, notice, now)` and `teams::unsuspend(pool, name)`, each locking the team with `lock_team` and bumping `admin_generation` (M13 plan, Task 1).
- `OkfError::Suspended { catalog, team, notice }`, kind `"Suspended"`, 403 (M13 plan, Task 2).
- `Principal.suspended: Arc<Suspensions>`, `Principal::suspension`, `check_not_suspended`, `require_export`, and `require` = private `require_level` + `check_not_suspended`; `KeyCache::from_records(records, ceilings, suspended, now)`; `Principal::for_session(user, ceiling, suspended)`; `Keys::suspensions()` (M13 plan, Task 3). Export calls `require_export` (`src/http/archives.rs`, M13 plan Task 3 Step 4).
- `selection::resolve` routes named catalogs through `refuse_suspended` and a connection list through `without_suspended`, both calling `principal.check_not_suspended` (M13 plan, Task 4 Step 3); the REST search and `list_changesets` filter with `check_not_suspended` (M13 plan, Task 4 Steps 4 and 5b).
- `PUT /api/v1/teams/{name}/suspension` (`set_suspension` in `src/http/api/people.rs`, `require_every_catalog`, then `settle`) and `TeamView.suspended_at` / `suspension_notice` (M13 plan, Task 5); `okf-mcp team suspend NAME [--notice]` and `team unsuspend NAME` in `src/cli/people.rs` (M13 plan, Task 6).

**On `main`:**
- `Principal` has `key_id, access, maintenance_report, reach, default_catalog, actor, kind, user` (user *name*) and no user id (`src/auth.rs:76-99`). `KeyRecord.user_id` exists (`src/store/keys.rs:190-191`); `From<KeyRecord>` drops it (`src/auth.rs:216-243`). A session's user id is in `CredentialKind::Session { user_id }` (`src/auth.rs:67-70`). `grep -rn "Principal {" src tests` finds 12 literals.
- `Principal::require` is used by administrative routes that read no content: catalog description, member access and delete (`src/http/api/catalogs.rs:244`, `:322`, `:379`), key grants (`src/http/api/keys.rs:281`, `:286`, `:464`, `:481`). Content routes that check access themselves: `src/http/api/browse.rs:328` (a revision read), export and import (`src/http/archives.rs:110`, `:182`), `open_changeset` (`src/mcp/changesets.rs:235`), `ToolContext::changeset` (`src/mcp/context.rs:384`), and the maintenance tools after `ctx.one` (`src/mcp/maintenance/work.rs:90-91`, `reports.rs:76`).
- `selection::resolve(principal, sources, exists)` and `Sources { argument, connection, server_default }` (`src/selection.rs:54-61`, `:182-188`); `ToolContext::select` builds `Sources` (`src/mcp/context.rs:160-175`).
- `ToolContext { app, principal, connection, catalogs }`, built by `ToolContext::new` and `from_parts` (`src/mcp/context.rs:51-109`); REST builds one with `api::context(app, principal)` (`src/http/api/mod.rs:109`), 18 callers in `health.rs`, `changesets.rs`, `browse.rs`, `maintenance.rs` and `search.rs`.
- `require_admin` (`access >= Admin`), `require_every_catalog` (admin and `Reach::All`) and `settle` (`src/http/api/mod.rs:121`, `:140`, `:159`). The API router puts `require_principal` in a `route_layer` (`src/http/api/mod.rs:66-72`) and is nested at `/api/v1` (`src/http/mod.rs:268`).
- axum 0.8.9: inside a nested router's layer, `MatchedPath` holds the full template, outer prefix included (`axum-0.8.9/src/extract/matched_path.rs:278-317`); this server already logs `/api/v1/users/{name}/sessions/{id_prefix}` from it (`src/http/mod.rs:291-302`). `FromRequestParts<S>::from_request_parts(parts, state) -> impl Future + Send` (`axum-core-0.5.6/src/extract/mod.rs:53-63`).
- `require_principal` inserts `Arc<Principal>` for a bearer credential, and also a `ConsoleSession` for a cookie (`src/http/console_session.rs:306-359`); `require_key` does the same for MCP and the archive routes (`src/auth.rs:710-731`). `client_ip(parts, &config.auth)` honours `trusted_proxy_header` (`src/http/console_session.rs:100-110`). IPs are stored by binding text and casting `$n::inet` (`src/store/sign_in_events.rs:182-191`).
- `KeyCache::read` loads keys, ceilings and users with `tokio::try_join!` (`src/auth.rs:418-429`); `App::reload_keys_if_changed` reloads keys then settings when `admin_generation` moves (`src/app.rs:428-458`).
- `users::delete` refuses while any credential names the user, it is in another team, or its personal team owns a catalog; it locks the user, then the personal team (`src/store/users.rs:666-730`). The schema makes `api_keys.user_id`, `sign_in_codes.user_id` and `team_members.user_id` `ON DELETE RESTRICT`, `teams.personal_of` `RESTRICT` (`migrations/0009_users_teams.sql:20`, `:25`, `:35-38`). Postgres names an unnamed column foreign key `<table>_<column>_fkey`. Sessions, passkeys, identities, email tokens, sign-in events and entitlements already cascade (`migrations/0010_user_login.sql`). `changesets.key_id` is `ON DELETE SET NULL` (`migrations/0001_schema.sql:70`).
- `users::set_admin` locks every admin row in id order, then the user (`src/store/users.rs:546-559`); `teams::set_member` resolves the user (`FOR KEY SHARE`) before locking the team (`src/store/teams.rs:286-290`).
- `keys::insert_key` refuses an empty grant list through `resolve_grants` (`src/store/keys.rs:693-698`), and is shared by `create_with_id` and `create_self_issued` (`src/store/keys.rs:377-387`, `:511-556`). There are 46 `NewKey { ... }` literals, so the provisioner gets its own store function instead of a `NewKey` field.
- `keys::list` selects one row per key (`src/store/keys.rs:735-792`). `manages_grants` is vacuously true for a key with no catalogs and a caller with `Reach::Catalogs` (`src/http/api/keys.rs:219-230`).
- `CreateKeyBody { name, access, all, catalogs, default_catalog, actor, maintenance_report, user }` (`src/http/api/keys.rs:156-177`); `KeyCreateArgs` requires `--access` and one of `--catalogs` / `--all` (`src/cli.rs:439-469`).
- `settings::Settings`, `SettingKey` (7 keys in `ALL`), `parse_value`, `apply`, `display`, `set(pool, key, value, by, now)` and `reset(pool, key)` (`src/store/settings.rs:35`, `:76-110`, `:174`, `:203`, `:246`, `:331`, `:376`). The CLI passes `by = "cli"` (`src/cli/admin.rs:22`, `:50`), the API `principal.actor` (`src/http/api/settings.rs:175-181`). The Settings page renders every row generically and special-cases `password_sign_in` as a boolean (`src/http/ui/js/pages/settings.js:83-99`).
- Mail is rendered at enqueue time: `queue::enqueue_in` calls `render(template)` (`src/mail/queue.rs:170-177`); `render` hard-codes "okf" in four templates (`src/mail/templates.rs:119-180`). `pages::document(title, body, scripts)` hard-codes `- okf` and the brand line (`src/http/pages.rs:47-80`); `index.html` hard-codes `okf console` and two brand spans (`src/http/ui/index.html:6`, `:16`, `:31`). `ui::routes<S>()` is generic over the state, so it cannot read settings (`src/http/ui.rs:24-31`).
- `catalogs::create`, `set_owner`, `update_description`, `delete(pool, name, force)` (`src/store/catalogs.rs:76`, `:138`, `:178`, `:267`); `ceiling::set_member_access`, `entitle`, `unentitle` (`src/store/ceiling.rs:247`, `:312`, `:363`); `ceiling::may_set_member_access` is "the one permission hook" (`src/store/ceiling.rs:410-423`); `import::import(pool, catalog, files, &ImportOptions { replace, scope, expect_generation, actor, now })` (`src/store/import.rs:36-48`, `:126`).
- `store::publish::publish(app, state, changeset, actor, message)` writes revisions, bumps the catalog generation and sets `Published` in one transaction (`src/store/publish.rs:116-186`). MCP `discard` closes a changeset under publish's locks in its own transaction (`src/mcp/changesets.rs:494-553`); `revert` writes through `write::apply` (`src/mcp/changesets.rs:577-640`), whose transaction commits at `src/write.rs:188`.
- `store::export::export(pool, catalog, out)` reads in a read-only `REPEATABLE READ` transaction and compresses after it (`src/store/export.rs:36-71`).
- Maintenance runs leave `running` in several places: `recover` (`src/store/maintenance/mod.rs:127-139`) and `runs.rs:283`, `:299`, `:529`, `:654`; every transition writes `ended_at` from the server clock. Findings carry `run_id` (`migrations/0006_maintenance.sql:62-80`).
- The runner turns any `isError` tool result into `RunnerError::Permission` (`src/investigator/contract.rs:205-213`); `RunSummary::exit_code` is 0 only for reasons `completed` and `empty` (`src/investigator/types.rs:108-119`); `kind()` maps errors to reasons (`src/investigator/supervisor.rs:1052-1069`); the supervisor tests fail a tool with `c.fail = Some((tool, error))` (`src/investigator/supervisor_tests.rs:651-675`).
- `tests/admin_parity.rs`: `COVERED`, `SCANNED_GROUPS = ["user", "team", "settings", "mail"]`, `parity_row` matching the first cell `` `okf-mcp <path>` ``, and the clap walk that splits `path` on spaces (`tests/admin_parity.rs:28-63`, `:84-100`, `:115-130`).
- Test helpers: `crate::db::test_db()`, `TEST_USER` / `TEST_TEAM` (`src/db.rs:100-110`); `api::testing::{app, keys, every_catalog_admin, send, Browser, fail_on, allow}` (`src/http/api/testing.rs:39`, `:61`, `:101`, `:127`, `:340`, `:506`, `:524`); `Browser::session_for(test, name)` (`:459`). Test catalogs are owned by `TEST_TEAM`, and every `keys()` key belongs to `TEST_USER`.
- tokio 1.54 `JoinSet::spawn`, `try_join_next`, `join_next` (`tokio-1.54.1/src/task/join_set.rs:142`, `:296`, `:323`). `tokio-util` is built without its `rt` feature (`Cargo.toml:112`), so there is no `TaskTracker`; `JoinSet` is used instead. chrono 0.4.45 `DurationRound::duration_trunc` (`chrono-0.4.45/src/round.rs:106-159`, re-exported at `lib.rs:574`).
- `ui_e2e` harness: `drive`, `login`, `sign_in_password`, `open`, `click`, `fill`, `select`, `wait_text`, `wait_visible`, `wait_hidden`, `wait_text_gone` (`tests/ui_e2e/harness.rs:90-236`); `TestServer::key`, `user_with_password`, `reporter_key`, `app` (`tests/support/server.rs:223`, `:302`, `:329`, `:90`); fixture owner `OWNER = "tester"` (`:44`).

## Interfaces shared across tasks

Each item is defined in the task on its comment line. Later tasks use these exact names.

```rust
// ---- src/store/audit.rs (Task 1) ----
pub enum Source { Cli, Api, Console, System }               // as_str: "cli" | "api" | "console" | "system"
pub struct AuditActor {
    pub source: Source,
    pub credential: Option<String>,  // key or grant id; None for sessions, the CLI and the system
    pub user_id: Option<String>,     // users.id of the acting person, for audit_people
    pub label: String,               // the credential's actor, the OS user, or a system label
    pub operator: bool,              // admin credential or provisioner: owners see "an operator"
    pub provisioner: bool,           // refuses admin targets in the store (Task 5 sets it)
    pub ip: Option<std::net::IpAddr>,
}
impl AuditActor {
    pub fn cli() -> Self;
    pub fn system(label: &str) -> Self;
    pub fn of(principal: &Principal, requester: &Requester) -> Self;
}
pub enum Action { /* one variant per action name; as_str() gives "user.created" etc. */ }
pub enum Target<'a> { User(&'a str /* users.id */), Team(&'a str), Catalog(&'a str),
                      Key(&'a str), Grant(&'a str), Code(&'a str), Setting(&'a str), Unlock(i64) }
pub struct Entry<'a> { pub action: Action, pub target: Target<'a>, pub person: Option<&'a str>,
                       pub teams: &'a [String], pub details: serde_json::Value }
pub async fn record(conn: &mut PgConnection, actor: &AuditActor, entry: &Entry<'_>,
                    now: DateTime<Utc>) -> Result<i64, OkfError>;
pub async fn team_ref(conn: &mut PgConnection, team: &str) -> Result<String, OkfError>;
pub async fn catalog_teams(conn: &mut PgConnection, catalog_id: i64) -> Result<Vec<String>, OkfError>;
pub async fn key_teams(conn: &mut PgConnection, key_id: &str) -> Result<Vec<String>, OkfError>;
pub async fn forget_person(conn: &mut PgConnection, user_id: &str) -> Result<(), OkfError>;
pub fn refuse_admin_target(actor: &AuditActor, is_admin: bool, user: &str) -> Result<(), OkfError>;
pub async fn record_admin_read(pool: &PgPool, actor: &AuditActor, catalog: &str,
                               now: DateTime<Utc>) -> Result<(), OkfError>;          // Task 7b
pub async fn prune(pool: &PgPool, before: DateTime<Utc>) -> Result<u64, OkfError>;   // Task 6d
pub enum AccessKind { Read, Write, Export }                                          // Task 7b
pub async fn list(pool: &PgPool, viewer: &Viewer, query: &AuditQuery<'_>)
                  -> Result<Vec<AuditRecord>, OkfError>;                              // Task 8a

// ---- src/http/requester.rs (Task 1) ----
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientIp(pub Option<std::net::IpAddr>);          // inserted by require_principal / require_key
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Requester { pub ip: Option<std::net::IpAddr>, pub source: Source }
// FromRequestParts<Arc<App>> for Requester and for AuditActor

// ---- src/auth.rs ----
pub struct Principal { /* existing */ pub user_id: Option<String>,   // Task 1
                       pub provisioner: bool,                       // Task 5
                       pub gates: Arc<AccessGates> }                // Task 7b
pub struct AccessGates { /* Task 7b: members, admins; Task 9b: entitled, require_unlock,
                            unlocks, maintenance_off */ }
pub enum Admission { Customer, Untracked, AdminRead,               // Task 7b
                     Maintenance, Unlocked { unlock_id: i64 } }    // Task 9b
impl Principal {
    pub fn admission(&self, catalog: &str, access: Access, now: DateTime<Utc>)
        -> Result<Admission, OkfError>;                             // Task 7b, refusals Task 9b
    pub fn check_open(&self, catalog: &str, access: Access, now: DateTime<Utc>)
        -> Result<Admission, OkfError>;                             // Task 9b: suspension, then admission
    pub fn require_contents(&self, access: Access, catalog: &str, now: DateTime<Utc>)
        -> Result<Admission, OkfError>;                             // Task 9b: require + check_open
}

// ---- src/http/api/mod.rs (Task 5) ----
pub(crate) const PROVISIONER_ROUTES: &[(&str, &str)];
pub(crate) fn require_manager(principal: &Principal) -> Result<(), OkfError>;  // provisioner or every-catalog admin
pub(crate) fn require_reader(principal: &Principal) -> Result<(), OkfError>;   // provisioner or any admin

// ---- src/error.rs (Task 9a) ----
OkfError::UnlockRequired { catalog: String, has_user: bool }   // kind "UnlockRequired", 403
OkfError::MaintenanceOff { catalog: String }                   // kind "Forbidden", 403, body adds "maintenance": "off"

// ---- src/store/unlocks.rs (Task 9a) ----
pub enum Category { Support, DeletionRequest, ExportRequest, SecurityInvestigation, Legal, Other }
pub struct NewUnlock<'a> { pub catalog: &'a str, pub user: &'a str, pub category: Category,
                           pub reason: &'a str, pub hours: u32 }
pub struct UnlockRecord { /* id, catalog, owner_team, user, person, category, reason,
                             created_at, expires_at, ended_at, reads, writes, exports */ }
pub async fn create(pool: &PgPool, new: &NewUnlock<'_>, policy: &UnlockPolicy,
                    actor: &AuditActor, now: DateTime<Utc>) -> Result<UnlockRecord, OkfError>;
pub async fn end(pool: &PgPool, id: i64, actor: &AuditActor, now: DateTime<Utc>)
                 -> Result<UnlockRecord, OkfError>;
pub async fn list(pool: &PgPool, filter: &UnlockFilter<'_>) -> Result<Vec<UnlockRecord>, OkfError>;
pub async fn live(pool: &PgPool, now: DateTime<Utc>) -> Result<Vec<LiveUnlock>, OkfError>;
pub async fn tally(pool: &PgPool, id: i64, kind: AccessKind) -> Result<(), OkfError>;   // Task 9b
```

## Task dependencies

Each line is `task (the tasks it needs)`; a task starts when every task in its parentheses has merged. Tasks on one line can run in parallel.

```
wave 1:  1 ()          2 ()          3 ()
wave 2:  4 (1)         7a (1)        6d (1, 2)
wave 3:  6a (4)        6b (4)
wave 4:  5 (3, 6a, 6b)
wave 5:  6c (5)        8a (5)        9a (5, 6d)
wave 6:  7b (6c, 7a)   8b (8a)       8c (8a)       9d (9a)
wave 7:  9b (7b, 9a)
wave 8:  9c (9b)
wave 9:  9e (8c, 9c, 9d)
wave 10: 10 (all)
```

Why each edge, and the shared files to merge with care:
- **1, 2 and 3 start together.** 2 touches `settings.rs`, `mail/`, `pages.rs` and the console shell; 3 touches `users.rs` (one new reader), `people.rs` (`list_users`), `cli/people.rs`, `users.js` and `tests/admin_parity.rs`; 1 touches `auth.rs` (one field) and the two middlewares. They share only `docs/operations.md` (different sections) and `src/store/mod.rs` (one line each: keep both).
- **4 and 7a need 1** (`store::audit`, `AuditActor`, `Requester`). **6d needs 1 and 2** (2 adds the branding keys to `settings.rs`, 6d the actor and retention). 4, 7a and 6d can run in parallel: 4 owns `users::delete`, 7a `publish`, `discard`, `revert` and `ToolContext`, 6d `settings.rs`.
- **6a and 6b need 4**: 6a rewrites the rest of `users.rs`, and 6b's guards call `users::id_and_admin_of`, which 4 adds. 6a and 6b run in parallel; both change `src/cli/people.rs`, `src/http/api/people.rs` and `src/store/ceiling.rs`, in different functions (6a the user and entitlement ones, 6b the team and member-access ones). Merge `main` before review and keep both sides.
- **5 needs 3, 6a and 6b**: its allow-list includes the lookup (3), and its admin-target refusals are the guards 4, 6a and 6b put in the store, which only refuse once 5 sets `AuditActor.provisioner`.
- **6c, 8a and 9a need 5.** 6c changes `keys.rs` after 5's `create_provisioner`; 8a's provisioner viewer needs `Principal.provisioner`; 9a's migration `0015` must follow 5's `0014` (migrations merge in number order: `0012` Task 1, `0013` Task 4, `0014` Task 5, `0015` Task 9a, `0016` Task 9c). 9a also needs 6d (`settings.rs`). 6c, 8a and 9a share no file but `docs/operations.md`.
- **7b needs 6c and 7a**: its tests create keys with 6c's signatures, and its `ToolContext::admit` uses 7a's requester. **8b and 8c need 8a** (the reader and the routes); 8b and 9d both add a command group to `src/cli.rs` and `tests/admin_parity.rs`: keep both. **9d needs 9a** (`store::unlocks`) and can run beside 9b.
- **9b needs 7b and 9a** (it extends `AccessGates` and `admission`, and reads `unlocks::live`). **9c needs 9b** (its runner test needs the `MaintenanceOff` refusal at the choke points). **9e needs 8c, 9c and 9d** (the console for all three; it and 8c both add nav links to `index.html` and the manifest).
- **10** last: it edits the sections of `docs/operations.md` the tasks leave alone.

---

## Scope

| Task | Delivers |
|---|---|
| 1 | Migration `0012`: `audit_people`, `audit_entries`, `audit_access_counts`, the person and team-reference functions, the append-only trigger; `store::audit` write helper; `Requester`, `ClientIp` and the `AuditActor` extractor; `Principal.user_id` |
| 2 | Branding: `product_name` and `support_contact` settings; mail, server pages and console shell use them |
| 3 | `GET /api/v1/users?email=`, `user list --email`, the Users filter box; the parity test learns `--flag` entries |
| 4 | Migration `0013`; user deletion with credentials, refusals, discarded changesets and anonymized people |
| 5 | Migration `0014`; the provisioner key, its allow-list, admin-target refusals, CLI, REST and console |
| 6a | Audit of user changes: create, email, admin flag, invite, entitlements, sessions |
| 6b | Audit of team and catalog changes: teams, members, suspension, catalogs, owner, member access, description, import |
| 6c | Audit of credential changes: keys, grants, reporting flag, assignment, sign-in codes, OAuth grants |
| 6d | Audit of settings; `audit_retention_days` and the daily prune |
| 7a | Audit of catalog writes: publish, revert, an admin's discard; `ToolContext` carries the requester |
| 7b | `AccessGates`, admissions, exports (`catalog.exported`) and hourly `catalog.admin_read` buckets |
| 8a | Reading the log: `store::audit::list`, owner redaction, `GET /api/v1/audit`, `GET /api/v1/audit/export`, `/me` owned teams |
| 8b | `okf-mcp audit list` and `audit export`; parity |
| 8c | Console Audit page |
| 9a | Migration `0015`; unlock settings, `store::unlocks`, the two error kinds, the owner email |
| 9b | The unlock check at every content choke point; tallies; the maintenance runner's exemption |
| 9c | Migration `0016`; the per-catalog `maintenance` flag (store, REST, CLI), `catalog.maintenance_run` entries, the runner's skip |
| 9d | `POST/GET/DELETE /api/v1/unlocks`, `okf-mcp unlock create/list/end`; parity |
| 9e | Console: Unlock form on refusal, Unlocks page, catalog maintenance switch |
| 10 | Documentation, README status, spec "What shipped differently", close-out |

### Deliberately deferred

| Item | Where it goes |
|---|---|
| Team owners managing members themselves (#352), seat counts, membership notifications | Milestone 15 |
| Self-service account deletion (#355) | Milestone 15 or later (a thin layer over Task 4) |
| Recording ordinary members' reads ("read accounting", `catalog.read`) | A later opt-in setting; nothing here assumes it won't exist |
| Streaming entries to an outside log service | Later; entries are queryable and exportable |
| Making credential user and catalog owner `NOT NULL` (#296) | Unchanged by this milestone |
| Disabling (not deleting) a user (#297) | Closed by Task 4: deletion meets its goal (spec Part 3) |
| Multiple replicas | Unchanged: single replica |

## File map

| File | Change | Tasks |
|---|---|---|
| `migrations/0012_audit.sql` | Create | 1 |
| `migrations/0013_user_deletion.sql` | Create | 4 |
| `migrations/0014_provisioner.sql` | Create | 5 |
| `migrations/0015_unlocks.sql` | Create | 9a |
| `migrations/0016_maintenance_audit.sql` | Create | 9c |
| `src/store/audit.rs` | Create (write helper); `prune` (6d); `record_admin_read`, `record_export` (7b); `list`, `Viewer`, `AuditQuery`, `AuditRecord` (8a) | 1, 6d, 7b, 8a |
| `src/store/unlocks.rs` | Create | 9a, 9b |
| `src/store/gates.rs` | Create (memberships, admins); entitlements, unlocks, maintenance flags, `require_unlock` (9b) | 7b, 9b |
| `src/store/maintenance/tests.rs` | The `maintenance_run` entry test | 9c |
| `tests/support/server.rs` | `owner_with_password` | 9e |
| `src/store/mod.rs` | `pub mod audit;` (1), `pub mod gates;` (7b), `pub mod unlocks;` (9a) | 1, 7b, 9a |
| `src/http/requester.rs` | Create: `ClientIp`, `Requester`, extractors | 1 |
| `src/http/mod.rs` | `mod requester`; `/ui/brand.json` (2) | 1, 2 |
| `src/auth.rs` | `Principal.user_id` (1); `provisioner`, MCP refusal (5); `AccessGates`, `Admission`, cache wiring (7b); unlock and maintenance checks (9b) | 1, 5, 7b, 9b |
| `src/http/console_session.rs` | `ClientIp` insertion (1); `for_session` user id (1); gates (7b) | 1, 7b |
| `src/store/settings.rs` | `product_name`, `support_contact`, `Brand`, `brand_in` (2); audit, `audit_retention_days` (6d); unlock settings (9a) | 2, 6d, 9a |
| `src/mail/templates.rs`, `src/mail/queue.rs` | Brand (2); `Template::Unlocked` (9a) | 2, 9a |
| `src/http/pages.rs`, `src/http/signin.rs`, `src/oauth/consent.rs`, `src/http/oidc.rs` | Brand on server pages | 2 |
| `src/http/ui/index.html`, `js/core/shell.js` | Brand (2); nav links (8c, 9e) | 2, 8c, 9e |
| `src/store/users.rs` | `find_record_by_email` (3); `delete` (4); actors and guards elsewhere (6a) | 3, 4, 6a |
| `src/store/teams.rs` | Actors, guards (6b); `owned_by` (8a) | 6b, 8a |
| `src/store/catalogs.rs` | Actors (6b); `maintenance` column and `set_maintenance` (9c) | 6b, 9c |
| `src/store/ceiling.rs` | Actors (6a entitlements, 6b member access); provisioner in `may_set_member_access` (5) | 5, 6a, 6b |
| `src/store/keys.rs` | `create_provisioner`, `KeyRecord.provisioner` (5); actors (6c) | 5, 6c |
| `src/store/oauth/codes.rs`, `src/store/oauth/grants.rs` | Actors | 6c |
| `src/store/user_sessions.rs`, `src/mail/invite.rs` | Actors | 6a |
| `src/store/import.rs` | `ImportOptions.audit` | 6b |
| `src/store/publish.rs`, `src/mcp/changesets.rs`, `src/write.rs`, `src/okf/ops` (`OpOutcome`) | Write audit | 7a |
| `src/mcp/context.rs` | Requester (7a); admissions (7b); `require_contents` (9b) | 7a, 7b, 9b |
| `src/selection.rs` | `Sources.now`; `check_open` | 9b |
| `src/store/export.rs`, `src/http/archives.rs` | `catalog.exported` (7b); unlock on export and import (9b) | 7b, 9b |
| `src/app.rs` | Access writes (7b); audit prune (6d) | 6d, 7b |
| `src/error.rs`, `src/http/mod.rs` (status) | Two error kinds | 9a |
| `src/http/api/mod.rs` | Provisioner gate, `require_manager`, `require_reader` (5); `context` signature (7a); `mod audit`, `mod unlocks` (8a, 9d) | 5, 7a, 8a, 9d |
| `src/http/api/people.rs` | Lookup (3); delete (4); provisioner (5); actors (6a, 6b) | 3, 4, 5, 6a, 6b |
| `src/http/api/catalogs.rs` | Provisioner (5); actors (6b); `maintenance` route and field (9c) | 5, 6b, 9c |
| `src/http/api/keys.rs`, `src/http/api/oauth.rs` | Provisioner body (5); actors (6c) | 5, 6c |
| `src/http/api/settings.rs` | Actors | 6d |
| `src/http/api/me.rs` | Actors on self-service keys and grants (6c); `owned_teams` (8a) | 6c, 8a |
| `src/http/api/{health,changesets,browse,maintenance,search}.rs` | `context(app, principal, requester)` | 7a |
| `src/http/api/audit.rs` | Create | 8a |
| `src/http/api/unlocks.rs` | Create | 9d |
| `src/mcp/maintenance/work.rs`, `src/mcp/maintenance/reports.rs` | `require_contents` | 9b |
| `src/investigator/{error,contract,supervisor,types}.rs` | `MaintenanceOff` skip | 9c |
| `src/cli.rs`, `src/cli/people.rs`, `src/cli/admin.rs` | `--email` (3); delete (4); `--provisioner` (5); actors (6a-6d, 7b); `audit` (8b); `set-maintenance` (9c); `unlock` (9d) | 3, 4, 5, 6a-6d, 7b, 8b, 9c, 9d |
| `src/cli/audit.rs`, `src/cli/unlock.rs` | Create | 8b, 9d |
| `tests/admin_parity.rs` | Flag entries (3); covered commands (5, 8b, 9c, 9d) | 3, 5, 8b, 9c, 9d |
| `src/http/ui/js/pages/users.js` | Filter box | 3 |
| `src/http/ui/js/pages/keys.js` | Provisioner checkbox and pill | 5 |
| `src/http/ui/js/pages/audit.js` | Create | 8c |
| `src/http/ui/js/pages/unlocks.js`, `js/components/unlock.js` | Create | 9e |
| `src/http/ui/js/core/api.js`, `pages/browse.js`, `pages/catalogs.js`, `pages/settings.js` | Unlock flow, maintenance switch, boolean settings | 2, 9e |
| `src/http/ui.rs` | Manifest pages (`audit`, `unlocks`) | 8c, 9e |
| `tests/ui_e2e/{settings,users,keys,audit,unlocks}.rs`, `tests/ui_e2e/main.rs` | Journeys | 2, 3, 5, 8c, 9e |
| `docs/operations.md` | Rows and sections per task; the rest in 10 | all |
| `README.md`, the spec, the milestone changelog | Close-out | 10 |
| `changelog/` | One entry per task | all |

---

### Task 1: Audit storage and the write helper

**Files:**
- Create: `migrations/0012_audit.sql`, `src/store/audit.rs`, `src/http/requester.rs`
- Modify: `src/store/mod.rs`, `src/http/mod.rs` (`mod requester;`), `src/auth.rs` (`Principal.user_id`, `require_key` inserts `ClientIp`), `src/http/console_session.rs` (`require_principal` inserts `ClientIp`; `client_ip` shares its body), every `Principal { ... }` literal (`user_id: None,`)

- [ ] **Step 1: Write the migration**

```sql
-- The audit log (spec 2026-11-13-okf-operator-api-design.md, Part 5).
-- Entries are written in the same transaction as the change they record,
-- never updated, and deleted only past the retention period. People are
-- referred to through audit_people, so deleting a user renames one row
-- ("deleted user #N") and leaves every entry intact. Every timestamp is
-- written by the server from its clock.

CREATE TABLE audit_people (
    id           bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- The user while it exists; NULL once deleted. No foreign key: deleting
    -- a user must never touch the log.
    user_id      text UNIQUE,
    name         text NOT NULL,
    display_name text NOT NULL DEFAULT ''
);

CREATE TABLE audit_entries (
    id               bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    at               timestamptz NOT NULL,
    action           text NOT NULL CHECK (action ~ '^[a-z]+\.[a-z_]+$'),
    source           text NOT NULL CHECK (source IN ('cli', 'api', 'console', 'system')),
    -- A key or grant id; NULL for a console session, the CLI and the system.
    actor_credential text,
    actor_person_id  bigint REFERENCES audit_people (id),
    actor_label      text NOT NULL,
    -- An admin credential or a provisioner: owners see "an operator".
    actor_operator   boolean NOT NULL,
    client_ip        inet,
    target_kind      text NOT NULL CHECK (target_kind IN
                         ('user', 'team', 'catalog', 'key', 'grant', 'code', 'setting', 'unlock')),
    -- NULL for a user target, which is named only through target_person_id.
    target_name      text,
    target_person_id bigint REFERENCES audit_people (id),
    -- Team references the entry belongs to: a shared team's name, or
    -- 'person:<audit_people.id>' for a personal team (whose name is a user's).
    teams            text[] NOT NULL DEFAULT '{}',
    details          jsonb NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(details) = 'object'),
    CONSTRAINT audit_entries_user_target CHECK (
        (target_kind = 'user') = (target_name IS NULL)
        AND (target_kind <> 'user' OR target_person_id IS NOT NULL)
    )
);
-- Newest first is `ORDER BY id DESC` (the primary key); these serve the
-- time window, the retention prune, a team's view and a credential's view.
CREATE INDEX audit_entries_at ON audit_entries (at);
CREATE INDEX audit_entries_teams ON audit_entries USING gin (teams);
CREATE INDEX audit_entries_credential ON audit_entries (actor_credential)
    WHERE actor_credential IS NOT NULL;

-- Request counts of the hourly catalog.admin_read entries (Task 7b). The
-- entry itself never changes; its count lives here.
CREATE TABLE audit_access_counts (
    bucket   text PRIMARY KEY,
    entry_id bigint REFERENCES audit_entries (id) ON DELETE CASCADE,
    requests bigint NOT NULL CHECK (requests > 0)
);
CREATE INDEX audit_access_counts_entry_id ON audit_access_counts (entry_id);

-- The audit_people row of a user, created or refreshed from users. NULL
-- for an unknown or NULL id (the INSERT then inserts nothing).
CREATE FUNCTION audit_person(p_user_id text) RETURNS bigint LANGUAGE sql AS $$
    INSERT INTO audit_people (user_id, name, display_name)
    SELECT id, name, display_name FROM users WHERE id = p_user_id
    ON CONFLICT (user_id) DO UPDATE
        SET name = EXCLUDED.name, display_name = EXCLUDED.display_name
    RETURNING id
$$;

-- How an entry names a team: its name, or 'person:<id>' for a personal team.
CREATE FUNCTION audit_team_ref(p_team_id text) RETURNS text LANGUAGE sql AS $$
    SELECT CASE WHEN t.personal_of IS NULL THEN t.name
                ELSE 'person:' || audit_person(t.personal_of) END
    FROM teams t WHERE t.id = p_team_id
$$;

-- Append-only: no UPDATE ever; a DELETE only of an entry older than the
-- retention period (the audit_retention_days setting, default 400, at
-- least 30). The CLI and the API never delete; the daily prune does.
CREATE FUNCTION audit_entries_append_only() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    retention integer;
BEGIN
    IF TG_OP <> 'DELETE' THEN
        RAISE EXCEPTION 'audit entries are append-only' USING ERRCODE = 'insufficient_privilege';
    END IF;
    SELECT GREATEST(30, COALESCE(
               (SELECT (value #>> '{}')::integer FROM server_settings
                 WHERE key = 'audit_retention_days'), 400))
      INTO retention;
    IF OLD.at >= now() - make_interval(days => retention) THEN
        RAISE EXCEPTION 'audit entry % is inside the retention period', OLD.id
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    RETURN OLD;
END
$$;
CREATE TRIGGER audit_entries_append_only BEFORE UPDATE OR DELETE ON audit_entries
    FOR EACH ROW EXECUTE FUNCTION audit_entries_append_only();
CREATE TRIGGER audit_entries_no_truncate BEFORE TRUNCATE ON audit_entries
    FOR EACH STATEMENT EXECUTE FUNCTION audit_entries_append_only();
```

(The trigger compares against the database clock: it only decides whether a delete is allowed, it writes no timestamp. The prune in Task 6d leaves a one-hour margin so a small skew never trips it.)

- [ ] **Step 2: Write the failing store tests** (in `src/store/audit.rs`'s `mod tests`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{test_db, TEST_TEAM, TEST_USER};
    use crate::store::users::{self, NewUser};
    use chrono::TimeDelta;

    fn now() -> DateTime<Utc> {
        "2026-11-13T12:00:00Z".parse().unwrap()
    }

    async fn user_id(pool: &PgPool, name: &str) -> String {
        sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    fn admin_actor(user_id: &str) -> AuditActor {
        AuditActor {
            source: Source::Api,
            credential: Some("k1".to_string()),
            user_id: Some(user_id.to_string()),
            label: "root".to_string(),
            operator: true,
            provisioner: false,
            ip: Some("192.0.2.7".parse().unwrap()),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn record_writes_one_entry_with_its_actor_target_and_teams() {
        let db = test_db().await;
        let me = user_id(&db.pool, TEST_USER).await;
        let mut tx = db.pool.begin().await.unwrap();
        let team = team_ref(&mut tx, TEST_TEAM).await.unwrap();

        let id = record(
            &mut tx,
            &admin_actor(&me),
            &Entry {
                action: Action::CatalogCreated,
                target: Target::Catalog("main"),
                person: None,
                teams: std::slice::from_ref(&team),
                details: serde_json::json!({}),
            },
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let row: (String, String, Option<String>, String, bool, Option<String>, String, Vec<String>) =
            sqlx::query_as(
                "SELECT e.action, e.source, e.actor_credential, p.name, e.actor_operator, \
                        e.client_ip::text, e.target_name, e.teams \
                 FROM audit_entries e JOIN audit_people p ON p.id = e.actor_person_id \
                 WHERE e.id = $1",
            )
            .bind(id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(row.0, "catalog.created");
        assert_eq!(row.1, "api");
        assert_eq!(row.2.as_deref(), Some("k1"));
        assert_eq!(row.3, TEST_USER);
        assert!(row.4);
        assert_eq!(row.5.as_deref(), Some("192.0.2.7/32"));
        assert_eq!(row.6, "main");
        // TEST_TEAM is TEST_USER's personal team: a person reference, never its name.
        assert!(row.7[0].starts_with("person:"), "{:?}", row.7);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_user_target_is_a_person_and_forgetting_renames_it() {
        let db = test_db().await;
        users::create(&db.pool, &NewUser { name: "alice", display_name: "Alice" }, now())
            .await
            .unwrap();
        let alice = user_id(&db.pool, "alice").await;
        let mut tx = db.pool.begin().await.unwrap();
        record(
            &mut tx,
            &AuditActor::cli(),
            &Entry {
                action: Action::UserCreated,
                target: Target::User(&alice),
                person: None,
                teams: &[],
                details: serde_json::json!({}),
            },
            now(),
        )
        .await
        .unwrap();

        forget_person(&mut tx, &alice).await.unwrap();
        tx.commit().await.unwrap();

        let (name, display, linked): (String, String, Option<String>) = sqlx::query_as(
            "SELECT p.name, p.display_name, p.user_id FROM audit_entries e \
             JOIN audit_people p ON p.id = e.target_person_id",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert!(name.starts_with("deleted user #"), "{name}");
        assert_eq!(display, "");
        assert_eq!(linked, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_log_refuses_update_and_early_delete_and_allows_an_old_delete() {
        let db = test_db().await;
        let mut tx = db.pool.begin().await.unwrap();
        let entry = |action| Entry {
            action,
            target: Target::Setting("product_name"),
            person: None,
            teams: &[],
            details: serde_json::json!({}),
        };
        let recent = record(&mut tx, &AuditActor::cli(), &entry(Action::SettingChanged), Utc::now())
            .await
            .unwrap();
        let old_at = Utc::now() - TimeDelta::days(401);
        let old = record(&mut tx, &AuditActor::cli(), &entry(Action::SettingCleared), old_at)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let updated = sqlx::query("UPDATE audit_entries SET action = 'x.y' WHERE id = $1")
            .bind(recent)
            .execute(&db.pool)
            .await;
        let early = sqlx::query("DELETE FROM audit_entries WHERE id = $1")
            .bind(recent)
            .execute(&db.pool)
            .await;
        let late = sqlx::query("DELETE FROM audit_entries WHERE id = $1")
            .bind(old)
            .execute(&db.pool)
            .await
            .unwrap();
        let truncated = sqlx::query("TRUNCATE audit_entries").execute(&db.pool).await;

        assert!(updated.is_err(), "UPDATE is refused");
        assert!(early.is_err(), "a recent entry cannot be deleted");
        assert_eq!(late.rows_affected(), 1, "an entry past 400 days can");
        assert!(truncated.is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_entry_rolls_back_with_the_change_it_records() {
        let db = test_db().await;
        let mut tx = db.pool.begin().await.unwrap();
        record(
            &mut tx,
            &AuditActor::cli(),
            &Entry {
                action: Action::SettingChanged,
                target: Target::Setting("product_name"),
                person: None,
                teams: &[],
                details: serde_json::json!({"to": "x"}),
            },
            now(),
        )
        .await
        .unwrap();
        // The change fails: dropping the transaction rolls both back.
        drop(tx);

        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_entries")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn only_a_provisioner_is_refused_an_admin_target() {
        let mut actor = AuditActor::cli();
        assert!(refuse_admin_target(&actor, true, "root").is_ok());
        actor.provisioner = true;
        assert!(refuse_admin_target(&actor, false, "alice").is_ok());
        assert!(matches!(
            refuse_admin_target(&actor, true, "root"),
            Err(OkfError::Forbidden { .. })
        ));
    }

    #[test]
    fn every_action_name_is_unique_and_well_formed() {
        let names: std::collections::HashSet<&str> = Action::ALL.iter().map(|a| a.as_str()).collect();
        assert_eq!(names.len(), Action::ALL.len());
        let pattern = regex::Regex::new(r"^[a-z]+\.[a-z_]+$").unwrap();
        assert!(names.iter().all(|name| pattern.is_match(name)), "{names:?}");
    }
}
```

And in `src/http/requester.rs`'s tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;

    #[tokio::test(flavor = "multi_thread")]
    async fn the_requester_reads_the_ip_and_tells_a_console_session_from_a_key() {
        let test = crate::http::api::testing::app().await;
        let ip: std::net::IpAddr = "192.0.2.9".parse().unwrap();
        let (mut keyed, ()) = Request::builder()
            .extension(ClientIp(Some(ip)))
            .body(())
            .unwrap()
            .into_parts();
        let (mut console, ()) = Request::builder()
            .extension(ClientIp(None))
            .extension(crate::http::console_session::ConsoleSession::for_test())
            .body(())
            .unwrap()
            .into_parts();

        let keyed = Requester::from_request_parts(&mut keyed, &test.app).await.unwrap();
        let console = Requester::from_request_parts(&mut console, &test.app).await.unwrap();

        assert_eq!(keyed, Requester { ip: Some(ip), source: Source::Api });
        assert_eq!(console.source, Source::Console);
    }
}
```

(`ConsoleSession::for_test()` is a `#[cfg(test)]` constructor to add beside `ConsoleSession`; its fields are an id, a CSRF token and an expiry.)

- [ ] **Step 3: Run them and see them fail**

Run: `tools/agentic/gate.sh narrow --lib store::audit::tests`, then `tools/agentic/gate.sh narrow --lib http::requester::tests`
Expected: compile errors (`store::audit`, `http::requester` do not exist).

- [ ] **Step 4: Implement `src/store/audit.rs`**

```rust
//! The audit log (spec "Part 5"): one row per administrative change,
//! catalog write, export and admin read, written by [`record`] inside the
//! transaction of the change it records. People are referred to through
//! `audit_people` ([`forget_person`] anonymizes a deleted user), and a
//! personal team is written as `person:<id>` ([`team_ref`]) because its
//! name is its user's. Never a secret: no key, token, password, link,
//! session id or email address goes into an entry.

use std::net::IpAddr;

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};

use crate::auth::{CredentialKind, Principal};
use crate::error::OkfError;
use crate::http::requester::Requester;
use crate::store::keys::Access;

/// Where a change came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `okf-mcp` on the server's command line.
    Cli,
    /// A bearer key or OAuth grant (REST or MCP).
    Api,
    /// A signed-in console session.
    Console,
    /// The server itself (an OAuth code redeemed, a maintenance run ending).
    System,
}

impl Source {
    /// The stored spelling.
    ///
    /// # Returns
    /// `"cli"`, `"api"`, `"console"` or `"system"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Cli => "cli",
            Source::Api => "api",
            Source::Console => "console",
            Source::System => "system",
        }
    }
}

/// Who made a change, as an entry records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditActor {
    /// Where it came from.
    pub source: Source,
    /// The key or grant id; `None` for a session, the CLI and the system.
    pub credential: Option<String>,
    /// The acting user's `users.id`, if any (a legacy key has none).
    pub user_id: Option<String>,
    /// The credential's actor, the OS user, or a system label.
    pub label: String,
    /// An admin credential or a provisioner: owners see "an operator".
    pub operator: bool,
    /// A provisioner key: store functions refuse it an admin target.
    pub provisioner: bool,
    /// The client address of a web request.
    pub ip: Option<IpAddr>,
}

impl AuditActor {
    /// The command line: the operating-system user, as the operator.
    ///
    /// # Returns
    /// The actor; its label is `$USER` (else `$LOGNAME`, else `cli`).
    pub fn cli() -> Self {
        // `or_else` tries the next variable only if the first is missing.
        let label = std::env::var("USER")
            .or_else(|_| std::env::var("LOGNAME"))
            .ok()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "cli".to_string());
        Self {
            source: Source::Cli,
            credential: None,
            user_id: None,
            label,
            operator: true,
            provisioner: false,
            ip: None,
        }
    }

    /// The server itself.
    ///
    /// # Arguments
    /// * `label` - what acted (`"oauth"`, `"maintenance"`).
    ///
    /// # Returns
    /// The actor.
    pub fn system(label: &str) -> Self {
        Self {
            source: Source::System,
            credential: None,
            user_id: None,
            label: label.to_string(),
            operator: false,
            provisioner: false,
            ip: None,
        }
    }

    /// The caller of a web request.
    ///
    /// # Arguments
    /// * `principal` - the credential.
    /// * `requester` - its IP and source.
    ///
    /// # Returns
    /// The actor. A console session has no credential id: `session:<name>`
    /// would name the person, which entries do only through `audit_people`.
    pub fn of(principal: &Principal, requester: &Requester) -> Self {
        let credential = match principal.kind {
            CredentialKind::Session { .. } => None,
            CredentialKind::ApiKey | CredentialKind::OAuth { .. } => Some(principal.key_id.clone()),
        };
        Self {
            source: requester.source,
            credential,
            user_id: principal.user_id.clone(),
            label: principal.actor.clone(),
            // Task 5 adds `|| principal.provisioner`.
            operator: principal.access == Access::Admin,
            // Task 5: `principal.provisioner`.
            provisioner: false,
            ip: requester.ip,
        }
    }
}

// One list of actions, so the enum, `as_str` and `ALL` cannot drift apart.
// `macro_rules!` writes the three from it at compile time.
macro_rules! actions {
    ($($variant:ident => $name:literal),+ $(,)?) => {
        /// What an entry records (spec "What is recorded").
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Action { $($variant),+ }

        impl Action {
            /// Every action.
            pub const ALL: &'static [Action] = &[$(Action::$variant),+];

            /// The stored name.
            ///
            /// # Returns
            /// For example `"user.created"`.
            pub fn as_str(self) -> &'static str {
                match self { $(Action::$variant => $name),+ }
            }

            /// Reads a stored name (for the `action` filter).
            ///
            /// # Returns
            /// The action, or `None` for an unknown name.
            pub fn parse(name: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|action| action.as_str() == name)
            }
        }
    };
}

actions! {
    UserCreated => "user.created",
    UserDeleted => "user.deleted",
    UserEmailChanged => "user.email_changed",
    UserAdminChanged => "user.admin_changed",
    UserInvited => "user.invited",
    UserEntitlementSet => "user.entitlement_set",
    UserEntitlementRemoved => "user.entitlement_removed",
    UserSessionsRevoked => "user.sessions_revoked",
    TeamCreated => "team.created",
    TeamDeleted => "team.deleted",
    TeamMemberAdded => "team.member_added",
    TeamRoleChanged => "team.role_changed",
    TeamMemberRemoved => "team.member_removed",
    TeamSuspended => "team.suspended",
    TeamUnsuspended => "team.unsuspended",
    CatalogCreated => "catalog.created",
    CatalogDeleted => "catalog.deleted",
    CatalogOwnerChanged => "catalog.owner_changed",
    CatalogMemberAccessChanged => "catalog.member_access_changed",
    CatalogDescriptionChanged => "catalog.description_changed",
    CatalogImported => "catalog.imported",
    CatalogMaintenanceChanged => "catalog.maintenance_changed",
    KeyCreated => "key.created",
    KeyRevoked => "key.revoked",
    KeyCatalogGranted => "key.catalog_granted",
    KeyCatalogUngranted => "key.catalog_ungranted",
    KeyUserAssigned => "key.user_assigned",
    KeyReportFlagChanged => "key.report_flag_changed",
    OauthCodeCreated => "oauth.code_created",
    OauthCodeRevoked => "oauth.code_revoked",
    OauthGrantCreated => "oauth.grant_created",
    OauthGrantRevoked => "oauth.grant_revoked",
    SettingChanged => "setting.changed",
    SettingCleared => "setting.cleared",
    CatalogPublished => "catalog.published",
    CatalogReverted => "catalog.reverted",
    CatalogChangesetDiscarded => "catalog.changeset_discarded",
    CatalogExported => "catalog.exported",
    CatalogAdminRead => "catalog.admin_read",
    CatalogMaintenanceRun => "catalog.maintenance_run",
    CatalogUnlocked => "catalog.unlocked",
    CatalogUnlockEnded => "catalog.unlock_ended",
}

/// What an entry is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target<'a> {
    /// A user, by `users.id` (stored only as a person).
    User(&'a str),
    /// A team, by name.
    Team(&'a str),
    /// A catalog, by name.
    Catalog(&'a str),
    /// An API key, by id.
    Key(&'a str),
    /// An OAuth grant, by id.
    Grant(&'a str),
    /// A sign-in code, by id.
    Code(&'a str),
    /// A setting, by key.
    Setting(&'a str),
    /// An unlock, by id.
    Unlock(i64),
}

impl Target<'_> {
    /// The stored kind and name (`None` for a user).
    fn parts(&self) -> (&'static str, Option<String>) {
        match *self {
            Target::User(_) => ("user", None),
            Target::Team(name) => ("team", Some(name.to_string())),
            Target::Catalog(name) => ("catalog", Some(name.to_string())),
            Target::Key(id) => ("key", Some(id.to_string())),
            Target::Grant(id) => ("grant", Some(id.to_string())),
            Target::Code(id) => ("code", Some(id.to_string())),
            Target::Setting(key) => ("setting", Some(key.to_string())),
            Target::Unlock(id) => ("unlock", Some(id.to_string())),
        }
    }
}

/// One entry to record.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry<'a> {
    /// What happened.
    pub action: Action,
    /// What it happened to.
    pub target: Target<'a>,
    /// A user the entry is also about (`users.id`): a member added to a
    /// team, the new user of a key. Ignored for a [`Target::User`].
    pub person: Option<&'a str>,
    /// Team references ([`team_ref`], [`catalog_teams`], [`key_teams`]).
    pub teams: &'a [String],
    /// A small JSON object; never a secret or an email address.
    pub details: serde_json::Value,
}

/// Writes one entry inside the caller's transaction.
///
/// # Arguments
/// * `conn` - the change's transaction (`&mut tx`).
/// * `actor` - who.
/// * `entry` - what.
/// * `now` - when (server clock).
///
/// # Returns
/// The entry's id.
///
/// # Errors
/// [`OkfError::Database`], which fails the caller's change with it.
pub async fn record(
    conn: &mut PgConnection,
    actor: &AuditActor,
    entry: &Entry<'_>,
    now: DateTime<Utc>,
) -> Result<i64, OkfError> {
    let (kind, name) = entry.target.parts();
    let person = match entry.target {
        Target::User(user_id) => Some(user_id),
        _ => entry.person,
    };
    // `audit_person` creates or refreshes the person; NULL in, NULL out.
    Ok(sqlx::query_scalar(
        "INSERT INTO audit_entries (at, action, source, actor_credential, actor_person_id, \
             actor_label, actor_operator, client_ip, target_kind, target_name, \
             target_person_id, teams, details) \
         VALUES ($1, $2, $3, $4, audit_person($5), $6, $7, $8::inet, $9, $10, \
                 audit_person($11), $12, $13) \
         RETURNING id",
    )
    .bind(now)
    .bind(entry.action.as_str())
    .bind(actor.source.as_str())
    .bind(&actor.credential)
    .bind(&actor.user_id)
    .bind(&actor.label)
    .bind(actor.operator)
    .bind(actor.ip.map(|ip| ip.to_string()))
    .bind(kind)
    .bind(name)
    .bind(person)
    .bind(entry.teams)
    .bind(&entry.details)
    .fetch_one(&mut *conn)
    .await?)
}

/// How entries name a team.
///
/// # Arguments
/// * `conn` - a connection or transaction.
/// * `team` - the team's name.
///
/// # Returns
/// Its name, or `person:<id>` for a personal team.
///
/// # Errors
/// [`OkfError::UnknownName`]; [`OkfError::Database`].
pub async fn team_ref(conn: &mut PgConnection, team: &str) -> Result<String, OkfError> {
    sqlx::query_scalar("SELECT audit_team_ref(id) FROM teams WHERE name = $1")
        .bind(team)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| OkfError::UnknownName { noun: "team", name: team.to_string() })
}

/// The team reference of a catalog's owner team: what a catalog event
/// belongs to.
///
/// # Arguments
/// * `conn` - a connection or transaction.
/// * `catalog_id` - the catalog.
///
/// # Returns
/// One reference, or none for a catalog with no owner yet.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn catalog_teams(conn: &mut PgConnection, catalog_id: i64) -> Result<Vec<String>, OkfError> {
    let team: Option<String> = sqlx::query_scalar(
        "SELECT audit_team_ref(owner_team_id) FROM catalogs \
         WHERE id = $1 AND owner_team_id IS NOT NULL",
    )
    .bind(catalog_id)
    .fetch_optional(&mut *conn)
    .await?;
    // `into_iter().collect()` turns `Option<T>` into a zero- or one-item Vec.
    Ok(team.into_iter().collect())
}

/// The team references of the catalogs a key or grant is granted: what a
/// credential event belongs to. An all-catalogs credential belongs to no
/// team (operator only; plan Decision 6).
///
/// # Errors
/// [`OkfError::Database`].
pub async fn key_teams(conn: &mut PgConnection, key_id: &str) -> Result<Vec<String>, OkfError> {
    Ok(sqlx::query_scalar(
        "SELECT audit_team_ref(t.owner_team_id) FROM \
           (SELECT DISTINCT c.owner_team_id FROM key_catalogs g \
              JOIN catalogs c ON c.id = g.catalog_id \
             WHERE g.key_id = $1 AND c.owner_team_id IS NOT NULL) t \
         ORDER BY 1",
    )
    .bind(key_id)
    .fetch_all(&mut *conn)
    .await?)
}

/// Stops the log identifying a user that is being deleted: its person row
/// becomes `deleted user #<id>`. Called in the deletion's transaction.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn forget_person(conn: &mut PgConnection, user_id: &str) -> Result<(), OkfError> {
    sqlx::query(
        "UPDATE audit_people SET user_id = NULL, name = 'deleted user #' || id, \
                display_name = '' WHERE user_id = $1",
    )
    .bind(user_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Refuses a provisioner acting on an admin user (spec Part 4). Store
/// functions call it after reading the target's admin flag under a lock
/// that `users::set_admin` conflicts with, so the check cannot race.
///
/// # Arguments
/// * `actor` - the caller.
/// * `is_admin` - the target's flag, read under that lock.
/// * `user` - its name, for the message.
///
/// # Errors
/// [`OkfError::Forbidden`] for a provisioner and an admin target.
pub fn refuse_admin_target(actor: &AuditActor, is_admin: bool, user: &str) -> Result<(), OkfError> {
    if actor.provisioner && is_admin {
        return Err(OkfError::Forbidden {
            reason: format!("a provisioner key cannot act on admin user {user:?}"),
        });
    }
    Ok(())
}
```

And the reader every later task's tests use (`#[cfg(test)]`, so it ships in no build):

```rust
/// Test helpers shared by every module that records entries.
#[cfg(test)]
pub(crate) mod testing {
    use sqlx::PgPool;

    /// One stored entry, flattened for assertions.
    #[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
    pub(crate) struct RecordedEntry {
        pub source: String,
        pub actor_label: String,
        pub actor_operator: bool,
        /// The actor's person name, if any.
        pub actor_person: Option<String>,
        pub target_kind: String,
        pub target: Option<String>,
        /// The target person's name (a user target, or `Entry.person`).
        pub person: Option<String>,
        pub teams: Vec<String>,
        pub details: serde_json::Value,
        /// A `catalog.admin_read` entry's request count.
        pub requests: Option<i64>,
    }

    /// Every entry with this action, oldest first.
    pub(crate) async fn entries(pool: &PgPool, action: &str) -> Vec<RecordedEntry> {
        sqlx::query_as::<_, RecordedEntry>(
            "SELECT e.source, e.actor_label, e.actor_operator, ap.name AS actor_person, \
                    e.target_kind, e.target_name AS target, tp.name AS person, e.teams, \
                    e.details, c.requests \
             FROM audit_entries e \
             LEFT JOIN audit_people ap ON ap.id = e.actor_person_id \
             LEFT JOIN audit_people tp ON tp.id = e.target_person_id \
             LEFT JOIN audit_access_counts c ON c.entry_id = e.id \
             WHERE e.action = $1 ORDER BY e.id",
        )
        .bind(action)
        .fetch_all(pool)
        .await
        .expect("read the audit entries")
    }
}
```

`src/store/mod.rs`: `pub mod audit;`.

- [ ] **Step 5: Implement `src/http/requester.rs` and the middleware changes**

```rust
//! Who is asking, beyond the principal: the client address and whether
//! the request came from a console session. The audit log records both.

use std::net::IpAddr;
use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;

use crate::app::App;
use crate::auth::Principal;
use crate::error::OkfError;
use crate::http::console_session::ConsoleSession;
use crate::http::AppError;
use crate::store::audit::{AuditActor, Source};

/// The client address of a request, as `client_ip` read it; inserted by
/// `require_principal` and `require_key`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientIp(pub Option<IpAddr>);

/// A request's address and source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Requester {
    /// The client address, when known.
    pub ip: Option<IpAddr>,
    /// `Console` for a session cookie, else `Api`.
    pub source: Source,
}

impl Requester {
    /// The requester of a request's parts.
    ///
    /// # Returns
    /// Its address (from [`ClientIp`]) and source.
    pub fn of(parts: &Parts) -> Self {
        Self {
            ip: parts.extensions.get::<ClientIp>().and_then(|ip| ip.0),
            source: if parts.extensions.get::<ConsoleSession>().is_some() {
                Source::Console
            } else {
                Source::Api
            },
        }
    }
}

// An extractor: a handler argument `requester: Requester` is filled from
// the request (Python frameworks inject arguments the same way).
impl FromRequestParts<Arc<App>> for Requester {
    // `Infallible`: this extractor never refuses a request.
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &Arc<App>) -> Result<Self, Self::Rejection> {
        Ok(Self::of(parts))
    }
}

impl FromRequestParts<Arc<App>> for AuditActor {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, _: &Arc<App>) -> Result<Self, Self::Rejection> {
        let principal = parts
            .extensions
            .get::<Arc<Principal>>()
            .ok_or_else(|| AppError(OkfError::Forbidden { reason: "no credential".to_string() }))?;
        Ok(AuditActor::of(principal, &Requester::of(parts)))
    }
}
```

In `src/http/console_session.rs`, split `client_ip`'s body into

```rust
/// [`client_ip`] from a request's extensions and headers, for middleware
/// that holds the whole `Request` rather than its `Parts`.
pub fn request_ip(extensions: &Extensions, headers: &HeaderMap, config: &AuthConfig) -> Option<IpAddr>
```

and have `client_ip` call it. In `require_principal`, before each `next.run(request)`, insert `ClientIp(request_ip(request.extensions(), request.headers(), &app.config().auth))`; do the same in `crate::auth::require_key`.

In `src/auth.rs`, add to `Principal` (after `user`):

```rust
    /// The `users.id` of the credential's user (`None` for a legacy key):
    /// what the audit log and the access gates key people by. It grants
    /// nothing by itself.
    pub user_id: Option<String>,
```

`From<KeyRecord>`: `user_id: record.user_id,`. `for_session`: `user_id: Some(user.id.clone()),`. Every other literal (`grep -rn "Principal {" src tests`): `user_id: None,`.

- [ ] **Step 6: Run the tests**

Run: `tools/agentic/gate.sh narrow --lib store::audit::tests`, then `tools/agentic/gate.sh narrow --lib http::requester::tests`, then `tools/agentic/gate.sh test`.
Expected: PASS, and every existing test still passes.

- [ ] **Step 7: Gates and commit**

Run `tools/agentic/gate.sh fmt` and `tools/agentic/gate.sh lint`; write the changelog entry.

```bash
git add migrations/0012_audit.sql src/store/audit.rs src/store/mod.rs src/http/requester.rs \
        src/http/mod.rs src/http/console_session.rs src/auth.rs src tests changelog/
git commit -m "feat(audit): append-only audit storage and the in-transaction write helper (M14 task 1)" \
           -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Branding settings

**Files:**
- Modify: `src/store/settings.rs`, `src/mail/templates.rs`, `src/mail/queue.rs`, `src/http/pages.rs` and its callers (`src/http/signin.rs`, `src/oauth/consent.rs`, `src/http/oidc.rs`, `src/http/passkeys.rs` if it renders pages: `grep -rn "pages::page\|pages::scripted_page\|pages::document" src`), `src/http/mod.rs` (`/ui/brand.json`), `src/http/ui/js/core/shell.js`, `src/http/ui/js/pages/settings.js`, `docs/operations.md` (Settings table)
- Test: `tests/ui_e2e/settings.rs`

- [ ] **Step 1: Write the failing tests**

In `src/store/settings.rs`'s tests:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn product_name_and_support_contact_are_validated_and_listed() {
        let db = test_db().await;
        let defaults = AuthConfig::default();

        let empty = set(&db.pool, SettingKey::ProductName, "  ", "cli", now()).await;
        let long = set(&db.pool, SettingKey::ProductName, &"x".repeat(61), "cli", now()).await;
        let control = set(&db.pool, SettingKey::ProductName, "Acme\u{7}", "cli", now()).await;
        let http = set(&db.pool, SettingKey::SupportContact, "http://example.com", "cli", now()).await;
        let bad_mail = set(&db.pool, SettingKey::SupportContact, "help@", "cli", now()).await;
        set(&db.pool, SettingKey::ProductName, "Acme Notes", "cli", now()).await.unwrap();
        set(&db.pool, SettingKey::SupportContact, "https://example.com/help", "cli", now())
            .await
            .unwrap();

        let loaded = load(&db.pool, &defaults).await.unwrap();
        for refused in [empty, long, control, http, bad_mail] {
            assert!(matches!(refused, Err(OkfError::LimitExceeded { limit: "setting", .. })));
        }
        assert_eq!(loaded.brand(), Brand {
            product_name: "Acme Notes".to_string(),
            support_contact: Some("https://example.com/help".to_string()),
        });
        set(&db.pool, SettingKey::SupportContact, "none", "cli", now()).await.unwrap();
        assert_eq!(load(&db.pool, &defaults).await.unwrap().support_contact, None);
    }

    #[test]
    fn the_defaults_are_okf_and_no_contact() {
        let settings = Settings::from_config(&AuthConfig::default());
        assert_eq!(settings.product_name, "okf");
        assert_eq!(settings.support_contact, None);
    }
```

(Task 6d changes `set`'s `by` argument to an `AuditActor`; this task keeps `by`.)

In `src/mail/templates.rs`'s tests:

```rust
    fn acme() -> Brand {
        Brand {
            product_name: "Acme Notes".to_string(),
            support_contact: Some("help@example.com".to_string()),
        }
    }

    #[test]
    fn every_template_uses_the_product_name_and_ends_with_the_contact() {
        let templates = [
            Template::Setup { link: "https://example.com/l".into(), user: "alice".into() },
            Template::Reset { link: "https://example.com/l".into() },
            Template::MethodAdded { method: "a passkey".into() },
            Template::Test,
        ];
        for template in &templates {
            let (subject, text, html) = render(template, &acme());
            assert!(!format!("{subject}{text}{html}").contains("okf"), "{template:?}: {text}");
            assert!(text.contains("Acme Notes") || subject.contains("Acme Notes"), "{template:?}");
            assert!(text.trim_end().ends_with("Questions? Contact help@example.com"), "{text}");
            assert!(html.contains("Questions? Contact help@example.com"));
        }
    }

    #[test]
    fn the_default_brand_renders_as_before() {
        let (subject, text, _) = render(&Template::Test, &Brand::default());
        assert_eq!(subject, "okf test email");
        assert!(!text.contains("Questions?"));
    }
```

Update the existing template tests to pass `&Brand::default()`; their expected text is unchanged.

In `src/http/pages.rs`'s tests (create the module if there is none):

```rust
    #[test]
    fn a_page_names_the_product_and_links_the_contact() {
        let brand = Brand {
            product_name: "Acme <Notes>".to_string(),
            support_contact: Some("https://example.com/help".to_string()),
        };
        let html = document("Sign in", "<p>x</p>", &[], &brand);
        assert!(html.contains("<title>Sign in - Acme &lt;Notes&gt;</title>"));
        assert!(html.contains("<p class=\"brand\">Acme &lt;Notes&gt;</p>"));
        assert!(html.contains("<a href=\"https://example.com/help\">https://example.com/help</a>"));
        assert!(!document("Sign in", "", &[], &Brand::default()).contains("Questions?"));
    }
```

And a route test in `src/http/mod.rs`'s tests:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn brand_json_is_public_and_follows_the_settings() {
        let test = crate::http::api::testing::app().await;
        crate::store::settings::set(test.app.pool(), SettingKey::ProductName, "Acme Notes", "cli",
                                    chrono::Utc::now())
            .await
            .unwrap();
        test.app.poll().await.unwrap();

        let (status, body) = crate::http::api::testing::send(&test, "GET", "/ui/brand.json", None, None).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({"product_name": "Acme Notes", "support_contact": null}));
    }
```

The journey (append to `tests/ui_e2e/settings.rs`):

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_product_name_set_on_the_settings_page_brands_the_console() {
    let server = server().await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.open("/settings");
        ui.fill("[data-test=setting-input-product_name]", "Acme Notes");
        ui.click("[data-test=setting-save-product_name]");
        ui.wait_text("#toasts", "product_name");
        ui.open("/health");
        ui.wait_text(".brand-block .brand", "Acme Notes");
        let title = ui.eval("() => document.title", serde_json::Value::Null);
        assert_eq!(title, "Acme Notes console");
    })
    .await;
}
```

(Use the module's own `server()` helper; if the toast text after saving differs, wait for the one the page shows today.)

- [ ] **Step 2: Run them and see them fail**

Run: `tools/agentic/gate.sh narrow --lib store::settings::tests`, `tools/agentic/gate.sh narrow --lib mail::templates::tests`, `tools/agentic/gate.sh narrow --lib http::pages::tests`, `tools/agentic/gate.sh narrow --lib http::tests::brand_json`
Expected: compile errors (`SettingKey::ProductName`, `Brand`, the extra `render` and `document` arguments).

- [ ] **Step 3: Implement the settings** (`src/store/settings.rs`)

`Settings` gains:

```rust
    /// The product name in mail, server pages and the console (`okf`).
    pub product_name: String,
    /// An email address or `https://` URL people are pointed to; `None`
    /// shows no contact line.
    pub support_contact: Option<String>,
```

with `from_config` setting `product_name: DEFAULT_PRODUCT_NAME.to_string(), support_contact: None` (they are runtime settings, not `[auth]` configuration: spec "My calls"). Add:

```rust
/// The product name with no override.
pub const DEFAULT_PRODUCT_NAME: &str = "okf";

/// The longest product name, in characters.
pub const MAX_PRODUCT_NAME_CHARS: usize = 60;

/// What people see the server as: the two branding settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Brand {
    /// The product name.
    pub product_name: String,
    /// The support contact, if set.
    pub support_contact: Option<String>,
}

impl Default for Brand {
    fn default() -> Self {
        Self { product_name: DEFAULT_PRODUCT_NAME.to_string(), support_contact: None }
    }
}

impl Settings {
    /// The branding settings.
    ///
    /// # Returns
    /// A copy of the product name and support contact.
    pub fn brand(&self) -> Brand {
        Brand {
            product_name: self.product_name.clone(),
            support_contact: self.support_contact.clone(),
        }
    }
}
```

`SettingKey` gains `ProductName` (`product_name`) and `SupportContact` (`support_contact`), at the end of `ALL` (now 9). `parse_value`:

```rust
        SettingKey::ProductName => check_product_name(text).map(|name| Value::String(name.to_string())),
        SettingKey::SupportContact if text == "none" => Ok(Value::Null),
        SettingKey::SupportContact => check_support_contact(text).map(|_| Value::String(text.to_string())),
```

`apply`:

```rust
        SettingKey::ProductName => {
            let text = value.as_str().ok_or_else(|| "product_name must be a string".to_string())?;
            settings.product_name = check_product_name(text)?.to_string();
        }
        SettingKey::SupportContact => {
            settings.support_contact = match value {
                Value::Null => None,
                other => {
                    let text = other.as_str().ok_or_else(|| "support_contact must be a string".to_string())?;
                    check_support_contact(text)?;
                    Some(text.to_string())
                }
            };
        }
```

`display`: the name, and the contact or `none`. The checks:

```rust
/// A product name as stored: trimmed, 1 to 60 characters, no control
/// characters.
fn check_product_name(text: &str) -> Result<&str, String> {
    let name = text.trim();
    // `chars().count()`: characters, not bytes.
    let length = name.chars().count();
    if length == 0 || length > MAX_PRODUCT_NAME_CHARS || name.chars().any(char::is_control) {
        return Err(format!(
            "product_name takes 1 to {MAX_PRODUCT_NAME_CHARS} characters with no control characters"
        ));
    }
    Ok(name)
}

/// A support contact: one plain email address, or an `https://` URL with
/// a host.
fn check_support_contact(text: &str) -> Result<(), String> {
    let refuse = || format!("support_contact takes an email address, an https:// URL or none, not {text:?}");
    if text.starts_with("https://") {
        let url = url::Url::parse(text).map_err(|_| refuse())?;
        return url.host_str().filter(|host| !host.is_empty()).map(drop).ok_or_else(refuse);
    }
    crate::store::users::check_email(text).map_err(|_| refuse())
}
```

And the reader the mail queue uses, so a mail always has the current brand whoever enqueues it (CLI or server):

```rust
/// The branding settings, read inside the caller's connection or
/// transaction. A stored value this server cannot read falls back to the
/// default, as in [`load`].
///
/// # Errors
/// [`OkfError::Database`].
pub async fn brand_in(conn: &mut PgConnection) -> Result<Brand, OkfError> {
    let rows = sqlx::query_as::<_, StoredSetting>(
        "SELECT key, value, updated_at, updated_by FROM server_settings \
         WHERE key IN ('product_name', 'support_contact')",
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(overlay(&AuthConfig::default(), &rows).brand())
}
```

- [ ] **Step 4: Mail** (`src/mail/templates.rs`, `src/mail/queue.rs`)

`render(template: &Template, brand: &Brand) -> (String, String, String)`. Every literal "okf" becomes `brand.product_name` through `format!` (subject and body: "Set up your {name} account", "An administrator invited you to {name}. ...", "... your {name} account ...", "{name} test email", "This is a test email from {name}. Mail delivery works."), escaped in the HTML twin. After the last paragraph, when `brand.support_contact` is set:

```rust
/// The closing contact line, when the operator set one.
fn contact_line(brand: &Brand) -> Option<String> {
    brand.support_contact.as_deref().map(|contact| format!("Questions? Contact {contact}"))
}
```

appended to the text (`\n\n{line}\n`) and as one more escaped `<p>` to the HTML. In `enqueue_in`, read the brand on the same connection first: `let brand = settings::brand_in(&mut *conn).await?; let (subject, text, html) = render(template, &brand);`.

- [ ] **Step 5: Server pages and the console**

`pages::document(title, body, scripts, brand: &Brand)`: `<title>{title} - {product}</title>`, `<p class="brand">{product}</p>`, and after `{body}`, when a contact is set:

```rust
    let contact = brand.support_contact.as_deref().map_or_else(String::new, |contact| {
        // An https URL is a link; an address is plain text.
        let shown = if contact.starts_with("https://") {
            format!("<a href=\"{0}\">{0}</a>", escape(contact))
        } else {
            escape(contact)
        };
        format!("        <p class=\"muted small support\">Questions? Contact {shown}</p>\n")
    });
```

`page` and `scripted_page` take `brand: &Brand` and pass it on; every caller passes `&app.settings().brand()` (each already holds `app`, or receives it: thread it where it does not).

`src/http/mod.rs`: a public route beside `ui::routes()`, which cannot read state:

```rust
/// `GET /ui/brand.json`: the product name and support contact, for the
/// console shell before anyone signs in. Public: both are shown on the
/// sign-in page anyway.
async fn brand(State(app): State<Arc<App>>) -> Json<serde_json::Value> {
    let brand = app.settings().brand();
    Json(serde_json::json!({
        "product_name": brand.product_name,
        "support_contact": brand.support_contact,
    }))
}
```

routed as `.route("/ui/brand.json", get(brand))` in the open routes (with the UI's security headers layer if `ui::routes` applies one: reuse `ui::security_headers`).

`src/http/ui/js/core/shell.js`, at boot, before the login check:

```js
// The operator's product name (Settings → product_name), on the login
// card, in the sidebar and in the tab title. textContent only.
async function applyBrand() {
    let brand;
    try { brand = await (await fetch('/ui/brand.json', { cache: 'no-store' })).json(); } catch { return; }
    if (!brand?.product_name) return;
    document.title = `${brand.product_name} console`;
    for (const el of document.querySelectorAll('.brand')) el.textContent = brand.product_name;
}
```

`index.html` keeps `okf` as the text shown until the fetch answers.

- [ ] **Step 6: Docs**

`docs/operations.md` "### Settings": two rows: `product_name` (default `okf`; 1 to 60 characters, no control characters; shown in every email, the console and the sign-in, setup, reset and consent pages) and `support_contact` (default `none`; an email address or `https://` URL; adds "Questions? Contact ..." to every email and the server pages). `mail.from` stays configuration.

- [ ] **Step 7: Run, gate, commit**

Run the four narrow tests, `tools/agentic/gate.sh test`, `tools/agentic/gate.sh test-ui`, then `fmt` and `lint`.

```bash
git add src/store/settings.rs src/mail src/http src/oauth docs/operations.md tests/ui_e2e/settings.rs changelog/
git commit -m "feat(settings): product_name and support_contact brand mail, pages and the console (M14 task 2)" \
           -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Find a user by email

**Files:**
- Modify: `src/store/users.rs`, `src/http/api/people.rs`, `src/cli.rs` (`UserCommand::List`), `src/cli/people.rs`, `src/http/ui/js/pages/users.js`, `tests/admin_parity.rs`, `docs/operations.md` (user table, parity row, API row)
- Test: `tests/ui_e2e/users.rs`

- [ ] **Step 1: Write the failing tests**

`src/store/users.rs`:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn find_record_by_email_ignores_letter_case_and_finds_none_for_others() {
        let db = test_db().await;
        seed_user(&db.pool, "alice", Some("Alice@Example.com")).await;

        let found = find_record_by_email(&db.pool, "alice@EXAMPLE.com").await.unwrap();
        let none = find_record_by_email(&db.pool, "bob@example.com").await.unwrap();
        let malformed = find_record_by_email(&db.pool, "not an address\0").await.unwrap();

        assert_eq!(found.map(|user| user.name), Some("alice".to_string()));
        assert_eq!(none, None);
        assert_eq!(malformed, None);
    }
```

`src/http/api/people.rs`:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn users_by_email_answers_zero_or_one_and_needs_an_admin() {
        let test = app().await;
        let root = every_catalog_admin(&test).await;
        let scoped = keys(&test).await;
        user(&test, &root, "alice").await;
        send(&test, "PUT", "/api/v1/users/alice/email", Some(&root),
             Some(json!({"email": "alice@example.com"}))).await;

        let (found, one) = send(&test, "GET", "/api/v1/users?email=ALICE@example.com", Some(&root), None).await;
        let (_, none) = send(&test, "GET", "/api/v1/users?email=nobody@example.com", Some(&root), None).await;
        let (refused, _) = send(&test, "GET", "/api/v1/users?email=alice@example.com", Some(&scoped.write), None).await;

        assert_eq!(found, StatusCode::OK);
        assert_eq!(one["users"].as_array().unwrap().len(), 1);
        assert_eq!(one["users"][0]["name"], "alice");
        assert_eq!(none["users"], json!([]));
        assert_eq!(refused, StatusCode::FORBIDDEN);
    }
```

`src/cli/people.rs`:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn user_list_with_an_email_prints_only_that_user() {
        let db = crate::db::test_db().await;
        crate::store::users::tests::seed_user(&db.pool, "alice", Some("alice@example.com")).await;
        let config = crate::config::Config::default();

        let listed = user(&db.pool, &config, UserCommand::List { email: Some("ALICE@example.com".into()) }, now())
            .await
            .unwrap();
        let none = user(&db.pool, &config, UserCommand::List { email: Some("x@example.com".into()) }, now())
            .await
            .unwrap();

        assert!(listed.starts_with("alice\t"), "{listed}");
        assert_eq!(listed.lines().count(), 1);
        assert_eq!(none, "");
    }
```

`tests/admin_parity.rs`: add `"user list --email",` to `COVERED`, and a test that the walk understands flags:

```rust
#[test]
fn a_covered_flag_is_an_option_of_its_subcommand() {
    let cli = Cli::command();
    for path in COVERED.iter().filter(|path| path.contains(" --")) {
        let (command_path, flag) = path.split_once(" --").expect("a flag entry");
        let mut command = &cli;
        for part in command_path.split(' ') {
            command = command.find_subcommand(part).unwrap_or_else(|| panic!("`okf-mcp {path}`"));
        }
        assert!(
            command.get_arguments().any(|arg| arg.get_long() == Some(flag)),
            "`okf-mcp {command_path}` has no --{flag}"
        );
    }
}
```

and make `every_covered_subcommand_exists_and_is_in_the_parity_table` walk only the part before ` --` (`path.split(" --").next()`), still requiring the row for the whole `path`.

The journey (append to `tests/ui_e2e/users.rs`):

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_users_filter_matches_names_and_emails() {
    let server = server().await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    users::create(server.app.pool(), &NewUser { name: "alice", display_name: "" }, Utc::now()).await.unwrap();
    users::create(server.app.pool(), &NewUser { name: "bob", display_name: "" }, Utc::now()).await.unwrap();
    users::set_email(server.app.pool(), "bob", Some("robert@example.com")).await.unwrap();
    drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.open("/users");
        ui.wait_visible("[data-test=user-alice]");
        ui.fill("#users-filter", "ROBERT@");
        ui.wait_hidden("[data-test=user-alice]");
        ui.wait_visible("[data-test=user-bob]");
        ui.fill("#users-filter", "");
        ui.wait_visible("[data-test=user-alice]");
    })
    .await;
}
```

(If 6a has merged first, `set_email` takes an `&AuditActor`: pass `&AuditActor::cli()`.)

- [ ] **Step 2: Run them and see them fail**

Run: `tools/agentic/gate.sh narrow --lib store::users::tests::find_record_by_email`, `tools/agentic/gate.sh narrow --lib http::api::people::tests::users_by_email`, `tools/agentic/gate.sh narrow --lib cli::people::tests::user_list_with_an_email`, `tools/agentic/gate.sh narrow --test admin_parity`
Expected: compile errors, then the parity failure "`okf-mcp user list --email` is not a subcommand".

- [ ] **Step 3: Implement**

`src/store/users.rs`:

```rust
/// The user whose email is `email`, compared case-insensitively, as the
/// user list shows it.
///
/// # Arguments
/// * `pool` - the database.
/// * `email` - the address as typed.
///
/// # Returns
/// The user, or `None` (emails are unique in any letter case, so at most one).
///
/// # Errors
/// [`OkfError::Database`].
pub async fn find_record_by_email(pool: &PgPool, email: &str) -> Result<Option<UserRecord>, OkfError> {
    // As in `find_by_email`: text no address can be never reaches SQL.
    if !is_lookup_email(email.trim()) {
        return Ok(None);
    }
    Ok(sqlx::query_as::<_, UserRecord>(concat!(select_users!(), " WHERE u.email = $1::citext"))
        .bind(email.trim())
        .fetch_optional(pool)
        .await?)
}
```

`src/http/api/people.rs`:

```rust
/// `GET /users`' query.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct UserQuery {
    /// Only the user with this email (any letter case).
    email: Option<String>,
}
```

`list_users` takes `Query(query): Query<UserQuery>` and answers, with an email, `users::find_record_by_email(..).await?.into_iter().map(UserView::from).collect()`; without one, the list as today. Update its doc comment and the module doc.

`src/cli.rs`: `UserCommand::List { #[arg(long)] email: Option<String> }` (doc: "Only the user with this email address (any letter case)."). `src/cli/people.rs`: the same branch, printing with `format_user`.

`users.js`: a search box in the page head, filtering the table client-side on name, display name and email:

```js
    const filter = h('input', { type: 'search', id: 'users-filter', placeholder: 'Filter by name or email',
                                'aria-label': 'Filter users', autocomplete: 'off' });
    filter.addEventListener('input', () => {
        const text = filter.value.trim().toLowerCase();
        table.setFilter(text ? user => [user.name, user.display_name, user.email ?? '']
            .some(value => value.toLowerCase().includes(text)) : null);
    });
```

placed in the `actions` div before New user.

`docs/operations.md`: the `user list` row gains `[--email ADDRESS]` ("only the user with that email, any letter case; prints nothing for none"); the parity table gains

```markdown
| `okf-mcp user list --email` | `GET /api/v1/users` | Users → the filter box |
```

and the API reference's `GET /api/v1/users` row documents `?email=` (0 or 1 users, the same view). Add to the parity section's lead: "A row may name a subcommand's flag (`user list --email`); the test checks the flag exists."

- [ ] **Step 4: Run, gate, commit**

Run the four tests, `tools/agentic/gate.sh test`, `tools/agentic/gate.sh test-ui`, `fmt`, `lint`.

```bash
git add src/store/users.rs src/http/api/people.rs src/cli.rs src/cli/people.rs src/http/ui/js/pages/users.js \
        tests/admin_parity.rs tests/ui_e2e/users.rs docs/operations.md changelog/
git commit -m "feat(users): find a user by email over REST, the CLI and the console (M14 task 3)" \
           -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Deleting a user

**Files:**
- Create: `migrations/0013_user_deletion.sql`
- Modify: `src/store/users.rs` (`delete`, `DeletedUser`), `src/http/api/people.rs` (`delete_user`), `src/cli/people.rs` (`UserCommand::Delete`), `docs/operations.md` ("Deletion rules", the `user delete` row, the API row)

- [ ] **Step 1: Write the migration**

```sql
-- Deleting a user (spec Part 3): the account and every credential it ever
-- had go in one transaction; catalogs and their history stay. The code's
-- three refusals (a personal team that owns catalogs, the last owner of a
-- shared team, the last admin user) replace the database's RESTRICT.
-- teams.personal_of stays RESTRICT: deletion removes the personal team
-- itself, after checking it owns nothing.
ALTER TABLE api_keys
    DROP CONSTRAINT api_keys_user_id_fkey,
    ADD CONSTRAINT api_keys_user_id_fkey
        FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE;
ALTER TABLE sign_in_codes
    DROP CONSTRAINT sign_in_codes_user_id_fkey,
    ADD CONSTRAINT sign_in_codes_user_id_fkey
        FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE;
ALTER TABLE team_members
    DROP CONSTRAINT team_members_user_id_fkey,
    ADD CONSTRAINT team_members_user_id_fkey
        FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE;
```

(No `IF EXISTS`: a database whose constraints are named otherwise should stop the migration rather than keep `RESTRICT` silently. Deleting a key cascades its `key_catalogs` and `oauth_tokens`; `changesets.key_id`, `maintenance_runs.key_id` and `sign_in_codes.grant_id` are set to NULL, and the open changesets are discarded first, below.)

- [ ] **Step 2: Write the failing store tests** (in `src/store/users.rs`'s tests)

```rust
    use crate::store::audit::AuditActor;
    use crate::store::keys::{self as key_rows, Access, Grants, NewKey};

    /// A key for `user` on `shared`; revoked when `revoked`.
    async fn key_for(pool: &PgPool, user: &str, name: &str, revoked: bool) -> String {
        let catalogs = ["shared".to_string()];
        let created = key_rows::create_with_id(
            pool,
            &NewKey {
                name,
                access: Access::Write,
                maintenance_report: false,
                grants: Grants::Catalogs(&catalogs),
                default_catalog: None,
                actor: name,
                user,
                self_issued: false,
                expires_at: None,
                levels: &[],
            },
            now(),
        )
        .await
        .unwrap();
        if revoked {
            key_rows::revoke_by_id(pool, &created.id, now()).await.unwrap();
        }
        created.id
    }

    async fn count(pool: &PgPool, sql: &str) -> i64 {
        sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string())).fetch_one(pool).await.unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deleting_a_user_removes_its_credentials_and_keeps_the_history() {
        let db = test_db().await;
        seed_user(&db.pool, "alice", Some("alice@example.com")).await;
        teams::create(&db.pool, &teams::NewTeam { name: "family", display_name: "", owner: crate::db::TEST_USER }, now())
            .await
            .unwrap();
        teams::set_member(&db.pool, "family", "alice", teams::Role::Member, now()).await.unwrap();
        seed_catalog_owned_by(&db.pool, "family").await;
        let live = key_for(&db.pool, "alice", "alice-laptop", false).await;
        key_for(&db.pool, "alice", "alice-old", true).await;
        crate::store::ceiling::entitle(&db.pool, "alice", "shared", Access::Read, now()).await.unwrap();
        // A revision alice's key wrote: history keeps its actor text.
        sqlx::query(
            "INSERT INTO revisions (catalog_id, path, revision, op, content, executable, actor, \
                                    message, created_at) \
             SELECT id, 'a.md', 1, 'create', 'x', false, 'alice-laptop', 'm', $1 \
             FROM catalogs WHERE name = 'shared'",
        )
        .bind(now())
        .execute(&db.pool)
        .await
        .unwrap();

        let deleted = delete(&db.pool, "alice", false, &AuditActor::cli(), now()).await.unwrap();

        assert_eq!(deleted, DeletedUser { keys: 2, codes: 0, discarded: 0 });
        assert!(matches!(find(&db.pool, "alice").await, Err(OkfError::UnknownName { .. })));
        assert_eq!(count(&db.pool, "SELECT count(*) FROM api_keys").await, 0);
        assert_eq!(count(&db.pool, "SELECT count(*) FROM user_catalogs").await, 0);
        assert_eq!(count(&db.pool, "SELECT count(*) FROM teams WHERE name = 'alice'").await, 0);
        assert_eq!(teams::find(&db.pool, "family").await.unwrap().members.len(), 1);
        assert_eq!(
            count(&db.pool, "SELECT count(*) FROM revisions WHERE actor = 'alice-laptop'").await,
            1
        );
        let (person,): (String,) = sqlx::query_as(
            "SELECT p.name FROM audit_entries e JOIN audit_people p ON p.id = e.target_person_id \
             WHERE e.action = 'user.deleted'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert!(person.starts_with("deleted user #"), "{person}");
        let _ = live;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_deleted_users_open_changesets_are_discarded_and_audited() {
        let db = test_db().await;
        seed_user(&db.pool, "alice", None).await;
        seed_catalog_owned_by(&db.pool, crate::db::TEST_TEAM).await;
        let key = key_for(&db.pool, "alice", "alice-laptop", false).await;
        let catalog_id: i64 = sqlx::query_scalar("SELECT id FROM catalogs WHERE name = 'shared'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        let mut tx = db.pool.begin().await.unwrap();
        let changeset = crate::store::changesets::open(&mut tx, catalog_id, &key, "alice-laptop", now())
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let deleted = delete(&db.pool, "alice", false, &AuditActor::cli(), now()).await.unwrap();

        assert_eq!(deleted.discarded, 1);
        let state: String = sqlx::query_scalar("SELECT state FROM changesets WHERE id = $1")
            .bind(changeset.id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(state, "discarded");
        let details: serde_json::Value = sqlx::query_scalar(
            "SELECT details FROM audit_entries WHERE action = 'catalog.changeset_discarded'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(details["changeset"], changeset.handle);
        assert_eq!(details["reason"], "user_deleted");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deletion_is_refused_for_owned_catalogs_a_sole_owner_and_the_last_admin() {
        let db = test_db().await;
        seed_user(&db.pool, "alice", None).await;
        seed_catalog_owned_by(&db.pool, "alice").await;
        let owns = delete(&db.pool, "alice", false, &AuditActor::cli(), now()).await;
        catalogs::delete(&db.pool, "shared", true).await.unwrap();
        teams::create(&db.pool, &teams::NewTeam { name: "family", display_name: "", owner: "alice" }, now())
            .await
            .unwrap();
        let sole = delete(&db.pool, "alice", false, &AuditActor::cli(), now()).await;
        teams::set_member(&db.pool, "family", crate::db::TEST_USER, teams::Role::Owner, now()).await.unwrap();
        set_admin(&db.pool, "alice", true, true).await.unwrap();
        let last_admin = delete(&db.pool, "alice", false, &AuditActor::cli(), now()).await;
        let by_the_cli = delete(&db.pool, "alice", true, &AuditActor::cli(), now()).await;

        let reason = |result: Result<DeletedUser, OkfError>| match result {
            Err(OkfError::Refused { reason }) => reason,
            other => panic!("expected a refusal, got {other:?}"),
        };
        assert!(reason(owns).contains("\"shared\""));
        assert!(reason(sole).contains("last owner of team(s) \"family\""));
        assert!(reason(last_admin).contains("last admin user"));
        assert!(by_the_cli.is_ok(), "the CLI may delete the last admin");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_key_created_while_deletion_waits_is_deleted_with_the_user() {
        let db = test_db().await;
        seed_user(&db.pool, "alice", None).await;
        seed_catalog_owned_by(&db.pool, crate::db::TEST_TEAM).await;
        // A key creation in flight holds alice `FOR KEY SHARE` (`id_of`).
        let mut creating = db.pool.begin().await.unwrap();
        id_of(&mut creating, "alice").await.unwrap();
        let pool = db.pool.clone();
        let deletion = tokio::spawn(async move {
            delete(&pool, "alice", false, &AuditActor::cli(), now()).await
        });
        until_a_lock_is_awaited(&db.pool).await;
        sqlx::query(
            "INSERT INTO api_keys (id, name, sha256, access, all_catalogs, actor, created_at, user_id) \
             SELECT 'late', 'late', sha256('late'::bytea), 'read', false, 'late', $1, id \
             FROM users WHERE name = 'alice'",
        )
        .bind(now())
        .execute(&mut *creating)
        .await
        .unwrap();
        creating.commit().await.unwrap();

        let deleted = deletion.await.unwrap().unwrap();

        assert_eq!(deleted.keys, 1, "the waiting deletion saw the committed key");
        assert_eq!(count(&db.pool, "SELECT count(*) FROM api_keys").await, 0);
    }
```

(`delete` takes the admin rows' lock first only when there are admins; the waiting point is the user row. Task 6a later passes an `AuditActor` to `entitle`, `set_admin` and `set_member` too; whichever task merges second updates these test calls.)

`src/http/api/people.rs` (replace the old "refused while a credential names it" test expectations):

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn deleting_a_user_over_rest_takes_its_keys_and_refuses_the_last_admin() {
        let test = app().await;
        let root = every_catalog_admin(&test).await;
        user(&test, &root, "alice").await;
        send(&test, "POST", "/api/v1/keys", Some(&root),
             Some(json!({"name": "alice-k", "access": "read", "catalogs": ["main"], "user": "alice"}))).await;
        sqlx::query("UPDATE users SET is_admin = (name = 'alice')").execute(test.app.pool()).await.unwrap();

        let (last, body) = send(&test, "DELETE", "/api/v1/users/alice", Some(&root), None).await;
        sqlx::query("UPDATE users SET is_admin = true WHERE name = $1")
            .bind(TEST_USER)
            .execute(test.app.pool())
            .await
            .unwrap();
        let (deleted, _) = send(&test, "DELETE", "/api/v1/users/alice", Some(&root), None).await;

        assert_eq!(last, StatusCode::CONFLICT, "{body}");
        assert_eq!(deleted, StatusCode::NO_CONTENT);
        let (_, keys) = send(&test, "GET", "/api/v1/keys", Some(&root), None).await;
        assert!(!keys.to_string().contains("alice-k"));
    }
```

- [ ] **Step 3: Run them and see them fail**

Run: `tools/agentic/gate.sh narrow --lib store::users::tests::delet`, then `tools/agentic/gate.sh narrow --lib store::users::tests::a_key_created_while`, then `tools/agentic/gate.sh narrow --lib http::api::people::tests::deleting_a_user`
Expected: compile errors (`DeletedUser`, the new arguments), then the old refusals.

- [ ] **Step 4: Implement** (`src/store/users.rs`; replace `delete`)

```rust
/// What a user deletion removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeletedUser {
    /// Keys and OAuth grants, revoked ones included.
    pub keys: i64,
    /// Sign-in codes, in every state.
    pub codes: i64,
    /// Open changesets of those keys, discarded.
    pub discarded: i64,
}

/// The shared teams whose only owner is `$1`.
const SOLE_OWNED_TEAMS: &str = "\
    SELECT t.name FROM team_members m JOIN teams t ON t.id = m.team_id \
    WHERE m.user_id = $1 AND m.role = 'owner' AND t.personal_of IS NULL \
      AND NOT EXISTS (SELECT 1 FROM team_members o \
                      WHERE o.team_id = t.id AND o.role = 'owner' AND o.user_id <> $1) \
    ORDER BY t.name";

/// Deletes a user and everything that is only theirs, in one transaction
/// (spec Part 3): its keys, grants and sign-in codes (revoked ones too),
/// sessions, sign-in methods, emailed links, entitlements, memberships and
/// personal team. Its keys' open changesets are discarded and audited.
/// Catalogs, their history (whose `actor` text stays) and every audit
/// entry are kept; the user's `audit_people` row becomes `deleted user #N`.
///
/// Lock order: every admin row in id order, then the user (as
/// [`set_admin`]), then its personal team, then its shared teams in id
/// order, then its open changesets in id order. A key or code being
/// created for the user holds it `FOR KEY SHARE` ([`id_of`]); the deletion
/// waits for it and then deletes it too.
///
/// # Arguments
/// * `pool` - the database.
/// * `name` - the user's name.
/// * `allow_last_admin` - lets the last admin user go (the CLI, with
///   operator access; never the API).
/// * `actor` - who, for the audit log; a provisioner is refused an admin.
/// * `now` - the server clock.
///
/// # Returns
/// What went.
///
/// # Errors
/// [`OkfError::UnknownName`]; [`OkfError::Forbidden`] for a provisioner and
/// an admin user; [`OkfError::Refused`] naming the catalogs its personal
/// team owns, the shared teams it is the only owner of, or that it is the
/// last admin user; [`OkfError::Database`].
pub async fn delete(
    pool: &PgPool,
    name: &str,
    allow_last_admin: bool,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<DeletedUser, OkfError> {
    if !is_valid_name(name) {
        return Err(unknown(name));
    }
    let mut tx = pool.begin().await?;
    let admins: Vec<String> =
        sqlx::query_scalar("SELECT id FROM users WHERE is_admin ORDER BY id FOR UPDATE")
            .fetch_all(&mut *tx)
            .await?;
    let (user_id, is_admin): (String, bool) =
        sqlx::query_as("SELECT id, is_admin FROM users WHERE name = $1 FOR UPDATE")
            .bind(name)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| unknown(name))?;
    audit::refuse_admin_target(actor, is_admin, name)?;
    let team_id: Option<String> =
        sqlx::query_scalar("SELECT id FROM teams WHERE personal_of = $1 FOR UPDATE")
            .bind(&user_id)
            .fetch_optional(&mut *tx)
            .await?;
    // Its shared teams, so no owner is added or removed while we count.
    sqlx::query(
        "SELECT t.id FROM teams t JOIN team_members m ON m.team_id = t.id \
         WHERE m.user_id = $1 AND t.personal_of IS NULL ORDER BY t.id FOR UPDATE OF t",
    )
    .bind(&user_id)
    .execute(&mut *tx)
    .await?;
    let owned: Vec<String> =
        sqlx::query_scalar("SELECT name FROM catalogs WHERE owner_team_id = $1 ORDER BY name")
            .bind(&team_id)
            .fetch_all(&mut *tx)
            .await?;
    let sole: Vec<String> = sqlx::query_scalar(SOLE_OWNED_TEAMS)
        .bind(&user_id)
        .fetch_all(&mut *tx)
        .await?;
    // `{:?}` of each name quotes it, so the list reads `"a", "b"`.
    let quoted = |names: &[String]| names.iter().map(|n| format!("{n:?}")).collect::<Vec<_>>().join(", ");
    let blocker = if !owned.is_empty() {
        Some(format!(
            "its personal team owns catalog(s) {}; delete them or move them to another team \
             first (catalog set-owner)",
            quoted(&owned)
        ))
    } else if !sole.is_empty() {
        Some(format!("it is the last owner of team(s) {}; add another owner first", quoted(&sole)))
    } else if is_admin && !allow_last_admin && admins.len() <= 1 {
        Some("it is the last admin user; make another user an admin first".to_string())
    } else {
        None
    };
    if let Some(what) = blocker {
        return Err(OkfError::Refused { reason: format!("user {name:?} cannot be deleted: {what}") });
    }
    let discarded = discard_open_changesets(&mut tx, &user_id, actor, now).await?;
    let (keys, codes): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM api_keys WHERE user_id = $1), \
                (SELECT count(*) FROM sign_in_codes WHERE user_id = $1)",
    )
    .bind(&user_id)
    .fetch_one(&mut *tx)
    .await?;
    // Recorded while the user row exists, so the entry's person resolves.
    audit::record(
        &mut tx,
        actor,
        &Entry {
            action: Action::UserDeleted,
            target: Target::User(&user_id),
            person: None,
            teams: &[],
            details: serde_json::json!({"keys": keys, "codes": codes, "changesets_discarded": discarded}),
        },
        now,
    )
    .await?;
    if let Some(team_id) = &team_id {
        sqlx::query("DELETE FROM teams WHERE id = $1").bind(team_id).execute(&mut *tx).await?;
    }
    // Cascades: keys (and their grants and tokens), codes, memberships,
    // sessions, passkeys, identities, emailed links, entitlements, events.
    sqlx::query("DELETE FROM users WHERE id = $1").bind(&user_id).execute(&mut *tx).await?;
    audit::forget_person(&mut tx, &user_id).await?;
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(DeletedUser { keys, codes, discarded })
}

/// Discards the open changesets of `user_id`'s keys (else their `key_id`
/// would become NULL and leave them open with no owner), one audit entry
/// each.
///
/// # Returns
/// How many.
///
/// # Errors
/// [`OkfError::Database`].
async fn discard_open_changesets(
    conn: &mut PgConnection,
    user_id: &str,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<i64, OkfError> {
    let open: Vec<(i64, String, i64, String, i64)> = sqlx::query_as(
        "SELECT c.id, c.handle, c.catalog_id, cat.name, \
                (SELECT count(*) FROM changeset_entries e WHERE e.changeset_id = c.id) \
         FROM changesets c JOIN api_keys k ON k.id = c.key_id \
                           JOIN catalogs cat ON cat.id = c.catalog_id \
         WHERE k.user_id = $1 AND c.state = 'open' ORDER BY c.id FOR UPDATE OF c",
    )
    .bind(user_id)
    .fetch_all(&mut *conn)
    .await?;
    for (id, handle, catalog_id, catalog, files) in &open {
        changesets::set_state(conn, *id, ChangesetState::Discarded, now).await?;
        let teams = audit::catalog_teams(conn, *catalog_id).await?;
        audit::record(
            conn,
            actor,
            &Entry {
                action: Action::CatalogChangesetDiscarded,
                target: Target::Catalog(catalog),
                person: Some(user_id),
                teams: &teams,
                details: serde_json::json!({"changeset": handle, "files": files, "reason": "user_deleted"}),
            },
            now,
        )
        .await?;
    }
    // `try_from`: a `usize` count into the `i64` the summary reports.
    Ok(i64::try_from(open.len()).unwrap_or(i64::MAX))
}
```

Imports: `use crate::store::audit::{self, Action, AuditActor, Entry, Target};`, `use crate::store::changesets::{self, ChangesetState};`.

Add the reader Task 6b's guards use:

```rust
/// The id and admin flag of the user called `name`, inside a caller's
/// transaction, held `FOR KEY SHARE` as [`id_of`] holds it. `set_admin`
/// takes `FOR UPDATE` on the same row, so the flag cannot change before the
/// caller commits: a provisioner guard on it cannot race a promotion.
///
/// # Errors
/// [`OkfError::UnknownName`]; [`OkfError::Database`].
pub(crate) async fn id_and_admin_of(conn: &mut PgConnection, name: &str) -> Result<(String, bool), OkfError> {
    sqlx::query_as("SELECT id, is_admin FROM users WHERE name = $1 FOR KEY SHARE")
        .bind(name)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| unknown(name))
}
```


`delete_user` (REST): `actor: AuditActor` extractor argument; `users::delete(app.pool(), &name, false, &actor, app.clock().now()).await?;` (still 204). Update its doc comment ("409 for a personal team that owns catalogs, the last owner of a shared team, or the last admin user").

`src/cli/people.rs` `UserCommand::Delete`: `let deleted = users::delete(pool, &name, true, &AuditActor::cli(), now).await?;` printing `deleted user {name} ({keys} key(s) and grant(s), {codes} sign-in code(s), {discarded} open changeset(s) discarded)`. Update `UserCommand::Delete`'s doc in `src/cli.rs` ("Delete a user, with its credentials, sessions and personal team; catalogs and history stay").

`docs/operations.md`: rewrite "### Deletion rules" for users (what goes, what stays, the three refusals, that the CLI may delete the last admin), drop "Disabling ... is future work (#297)", and update the `user delete` row and `DELETE /api/v1/users/{name}`'s row.

- [ ] **Step 5: Run, gate, commit**

Run the narrow tests, then `tools/agentic/gate.sh test` (fix every other `users::delete(` caller), `fmt`, `lint`.

```bash
git add migrations/0013_user_deletion.sql src/store/users.rs src/http/api/people.rs src/cli.rs \
        src/cli/people.rs docs/operations.md changelog/
git commit -m "feat(users): deleting a user takes its credentials; catalogs and history stay (M14 task 4)" \
           -m "Closes #297's goal by deletion." \
           -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: The provisioner key

**Files:**
- Create: `migrations/0014_provisioner.sql`
- Modify: `src/store/keys.rs` (`create_provisioner`, `KeyRecord.provisioner`, `list`), `src/auth.rs` (`Principal.provisioner`, `require_key`), `src/store/audit.rs` (`AuditActor::of`), `src/http/api/mod.rs` (allow-list, gate, `require_manager`, `require_reader`), `src/http/api/people.rs`, `src/http/api/catalogs.rs`, `src/store/ceiling.rs` (`may_set_member_access`), `src/http/api/keys.rs` (body, `KeyView.provisioner`), `src/cli.rs` (`KeyCreateArgs`, `run_key`, `format_key_record`), `src/http/ui/js/pages/keys.js`, `tests/admin_parity.rs`, `docs/operations.md`
- Test: `tests/ui_e2e/keys.rs`

- [ ] **Step 1: Write the migration**

```sql
-- The provisioner key (spec Part 4): a server-wide flag for automation that
-- creates and removes accounts, teams and catalogs but reaches no catalog.
-- Set only at creation; it holds no grants, which a trigger enforces.
ALTER TABLE api_keys
    ADD COLUMN provisioner boolean NOT NULL DEFAULT false,
    ADD CONSTRAINT api_keys_provisioner_shape CHECK (
        NOT provisioner
        OR (kind = 'key' AND access = 'read' AND NOT all_catalogs AND NOT self_issued
            AND NOT maintenance_report AND default_catalog_id IS NULL AND user_id IS NOT NULL)
    );

CREATE FUNCTION key_catalogs_refuse_provisioner() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM api_keys WHERE id = NEW.key_id AND provisioner) THEN
        RAISE EXCEPTION 'a provisioner key reaches no catalog' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER key_catalogs_refuse_provisioner BEFORE INSERT OR UPDATE ON key_catalogs
    FOR EACH ROW EXECUTE FUNCTION key_catalogs_refuse_provisioner();

CREATE FUNCTION api_keys_provisioner_fixed() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.provisioner IS DISTINCT FROM OLD.provisioner THEN
        RAISE EXCEPTION 'the provisioner flag is set only at creation'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER api_keys_provisioner_fixed BEFORE UPDATE OF provisioner ON api_keys
    FOR EACH ROW EXECUTE FUNCTION api_keys_provisioner_fixed();
```

- [ ] **Step 2: Write the failing tests**

`src/store/keys.rs`:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn a_provisioner_key_has_no_grants_and_the_schema_keeps_it_so() {
        let db = test_db().await;
        crate::store::catalogs::create(
            &db.pool,
            &crate::store::catalogs::NewCatalog { name: "main", description: "", owner_team: crate::db::TEST_TEAM },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();

        let created = create_provisioner(
            &db.pool,
            &NewProvisioner { name: "signup", actor: "signup", user: crate::db::TEST_USER },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let grant = sqlx::query(
            "INSERT INTO key_catalogs (key_id, catalog_id) SELECT $1, id FROM catalogs WHERE name = 'main'",
        )
        .bind(&created.id)
        .execute(&db.pool)
        .await;
        let flip = sqlx::query("UPDATE api_keys SET provisioner = false WHERE id = $1")
            .bind(&created.id)
            .execute(&db.pool)
            .await;
        let record = list(&db.pool).await.unwrap().into_iter().find(|k| k.id == created.id).unwrap();

        assert!(record.provisioner);
        assert!(record.catalogs.is_empty() && !record.all_catalogs);
        assert!(grant.is_err(), "the trigger refuses a grant");
        assert!(flip.is_err(), "the flag is fixed at creation");
        let principal = crate::auth::Principal::from(record);
        assert!(principal.provisioner && !principal.reaches("main"));
    }
```

`src/http/api/mod.rs`'s tests (or a new `src/http/api/provisioner_tests.rs` included with `#[cfg(test)] mod provisioner_tests;`):

```rust
    use serde_json::json;
    use crate::http::api::testing::{app, every_catalog_admin, send};

    /// A provisioner key for TEST_USER, polled in.
    async fn provisioner(test: &crate::mcp::testing::TestApp) -> String {
        let created = crate::store::keys::create_provisioner(
            test.app.pool(),
            &crate::store::keys::NewProvisioner { name: "signup", actor: "signup", user: crate::db::TEST_USER },
            &crate::store::audit::AuditActor::cli(),
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        test.app.poll().await.unwrap();
        created.key
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_provisioner_runs_the_signup_and_offboarding_routes() {
        let test = app().await;
        let key = provisioner(&test).await;
        let p = Some(key.as_str());

        let steps: Vec<(&str, &str, Option<serde_json::Value>, u16)> = vec![
            ("POST", "/api/v1/users", Some(json!({"name": "alice"})), 201),
            ("PUT", "/api/v1/users/alice/email", Some(json!({"email": "alice@example.com"})), 200),
            ("GET", "/api/v1/users?email=alice@example.com", None, 200),
            ("GET", "/api/v1/users/alice", None, 200),
            ("POST", "/api/v1/teams", Some(json!({"name": "acme", "owner": "alice"})), 201),
            ("GET", "/api/v1/teams/acme", None, 200),
            ("POST", "/api/v1/users", Some(json!({"name": "bob"})), 201),
            ("PUT", "/api/v1/teams/acme/members/bob", Some(json!({"role": "member"})), 200),
            ("POST", "/api/v1/catalogs", Some(json!({"name": "acme-notes", "owner": "acme"})), 201),
            ("PUT", "/api/v1/catalogs/acme-notes/member-access", Some(json!({"access": "read"})), 200),
            ("PUT", "/api/v1/teams/acme/suspension", Some(json!({"suspended": true})), 200),
            ("PUT", "/api/v1/teams/acme/suspension", Some(json!({"suspended": false})), 200),
            ("DELETE", "/api/v1/teams/acme/members/bob", None, 200),
            ("DELETE", "/api/v1/catalogs/acme-notes", None, 200),
            ("DELETE", "/api/v1/teams/acme", None, 204),
            ("DELETE", "/api/v1/users/bob", None, 204),
        ];
        for (method, uri, body, want) in steps {
            let (status, answer) = send(&test, method, uri, p, body).await;
            assert_eq!(status.as_u16(), want, "{method} {uri}: {answer}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_provisioner_never_reaches_an_admin_user() {
        let test = app().await;
        let root = every_catalog_admin(&test).await;
        let key = provisioner(&test).await;
        let p = Some(key.as_str());
        send(&test, "POST", "/api/v1/users", Some(&root), Some(json!({"name": "staff"}))).await;
        send(&test, "PUT", "/api/v1/users/staff/email", Some(&root), Some(json!({"email": "staff@example.com"}))).await;
        send(&test, "PUT", "/api/v1/users/staff/admin", Some(&root), Some(json!({"admin": true}))).await;
        send(&test, "POST", "/api/v1/teams", Some(&root), Some(json!({"name": "ops", "owner": crate::db::TEST_USER}))).await;

        let (lookup, found) = send(&test, "GET", "/api/v1/users?email=staff@example.com", p, None).await;
        let refused = [
            ("GET", "/api/v1/users/staff", None),
            ("PUT", "/api/v1/users/staff/email", Some(json!({"email": "x@example.com"}))),
            ("POST", "/api/v1/users/staff/invite", None),
            ("DELETE", "/api/v1/users/staff", None),
            ("PUT", "/api/v1/teams/ops/members/staff", Some(json!({"role": "member"}))),
            ("POST", "/api/v1/teams", Some(json!({"name": "mine", "owner": "staff"}))),
        ];

        assert_eq!(lookup, axum::http::StatusCode::OK);
        assert_eq!(found["users"], json!([]), "an admin is not found by a provisioner");
        for (method, uri, body) in refused {
            let (status, answer) = send(&test, method, uri, p, body).await;
            assert_eq!(status.as_u16(), 403, "{method} {uri}: {answer}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_provisioner_is_refused_every_other_route_and_every_catalog() {
        let test = app().await;
        let key = provisioner(&test).await;
        let p = Some(key.as_str());
        let refused = [
            ("GET", "/api/v1/users", None),
            ("GET", "/api/v1/teams", None),
            ("GET", "/api/v1/keys", None),
            ("POST", "/api/v1/keys", Some(json!({"name": "x", "access": "admin", "all": true, "user": crate::db::TEST_USER}))),
            ("PUT", "/api/v1/settings/product_name", Some(json!({"value": "x"}))),
            ("PUT", "/api/v1/users/x/admin", Some(json!({"admin": true}))),
            ("PUT", "/api/v1/users/x/entitlements/main", Some(json!({"access": "read"}))),
            ("DELETE", "/api/v1/users/x/sessions", None),
            ("GET", "/api/v1/unassigned", None),
            ("GET", "/api/v1/catalogs/main/tree", None),
            ("GET", "/api/v1/catalogs/main/export", None),
            ("GET", "/api/v1/search?q=alpha", None),
            ("GET", "/api/v1/whoami", None),
        ];
        for (method, uri, body) in refused {
            let (status, answer) = send(&test, method, uri, p, body).await;
            assert_eq!(status.as_u16(), 403, "{method} {uri}: {answer}");
        }
        // Not an MCP credential either.
        let (mcp, _) = send(&test, "POST", "/mcp", p, Some(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}))).await;
        assert_eq!(mcp.as_u16(), 403);
    }

    #[test]
    fn the_allow_list_is_the_specs_and_names_routes_by_template() {
        assert_eq!(PROVISIONER_ROUTES.len(), 16);
        assert!(provisioner_may(&axum::http::Method::DELETE, "/api/v1/users/{name}"));
        assert!(!provisioner_may(&axum::http::Method::PUT, "/api/v1/users/{name}/admin"));
        assert!(!provisioner_may(&axum::http::Method::GET, "/api/v1/teams"));
    }
```

(If `GET /api/v1/whoami` is spelled differently, use the identity route `identity::routes()` defines.)

`src/http/api/keys.rs`:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn post_keys_makes_a_provisioner_only_for_an_every_catalog_admin_and_with_no_grants() {
        let test = app().await;
        let root = every_catalog_admin(&test).await;
        let scoped = keys(&test).await;
        let body = json!({"name": "signup", "provisioner": true, "user": TEST_USER});

        let (denied, _) = send(&test, "POST", "/api/v1/keys", Some(&scoped.admin), Some(body.clone())).await;
        let (mixed, _) = send(&test, "POST", "/api/v1/keys", Some(&root),
            Some(json!({"name": "s2", "provisioner": true, "catalogs": ["main"], "user": TEST_USER}))).await;
        let (made, created) = send(&test, "POST", "/api/v1/keys", Some(&root), Some(body)).await;

        assert_eq!(denied, StatusCode::FORBIDDEN);
        assert_eq!(mixed, StatusCode::CONFLICT);
        assert_eq!(made, StatusCode::OK);
        assert_eq!(created["key"]["provisioner"], true);
        assert!(created["secret"].as_str().unwrap().starts_with("okf_"));
    }
```

`src/cli.rs`'s tests:

```rust
    #[test]
    fn key_create_provisioner_takes_no_grants_or_access() {
        let ok = Cli::try_parse_from(["okf-mcp", "key", "create", "signup", "--provisioner", "--user", "alice"]);
        let with_catalogs = Cli::try_parse_from([
            "okf-mcp", "key", "create", "signup", "--provisioner", "--catalogs", "main", "--user", "alice",
        ]);
        let with_access = Cli::try_parse_from([
            "okf-mcp", "key", "create", "signup", "--provisioner", "--access", "admin", "--user", "alice",
        ]);
        let plain_without_access = Cli::try_parse_from(["okf-mcp", "key", "create", "k", "--all", "--user", "alice"]);

        assert!(ok.is_ok());
        assert!(with_catalogs.is_err() && with_access.is_err());
        assert!(plain_without_access.is_err(), "--access is still required without --provisioner");
    }
```

`tests/admin_parity.rs`: `"key create --provisioner",` in `COVERED`.

The journey (append to `tests/ui_e2e/keys.rs`):

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admin_creates_a_provisioner_key_that_reaches_no_catalog() {
    let server = server().await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.open("/keys");
        ui.click("#key-new");
        ui.fill("#key-form input[name=name]", "signup");
        ui.click("#key-form input[name=provisioner]");
        ui.wait_visible("#key-form fieldset.key-reach[disabled]");
        ui.click("#key-form button[type=submit]");
        ui.click("#key-secret-done");
        ui.wait_visible("[data-test=key-provisioner-signup]");
    })
    .await;
}
```

(`fieldset.key-reach` is the access, catalogs, default and reporting controls, wrapped in one fieldset so the checkbox can disable them together.)

- [ ] **Step 3: Run them and see them fail**

Run: `tools/agentic/gate.sh narrow --lib store::keys::tests::a_provisioner`, `tools/agentic/gate.sh narrow --lib provisioner`, `tools/agentic/gate.sh narrow --lib http::api::keys::tests::post_keys_makes`, `tools/agentic/gate.sh narrow --lib cli::tests::key_create_provisioner`, `tools/agentic/gate.sh narrow --test admin_parity`
Expected: compile errors, then failing statuses.

- [ ] **Step 4: Implement the store and the principal**

`src/store/keys.rs` (and the 20 `KeyRecord { ... }` literals, `provisioner: false`): `KeyRecord.provisioner: bool` (doc: "A provisioner: reaches no catalog; may call the routes in `api::PROVISIONER_ROUTES`"), `KeyRow.provisioner`, `k.provisioner` in `list`'s SELECT and `GROUP BY`. Then:

```rust
/// A provisioner key to create (spec Part 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewProvisioner<'a> {
    /// Grouping name, `[A-Za-z0-9._-]+`.
    pub name: &'a str,
    /// The actor its changes are recorded as.
    pub actor: &'a str,
    /// The user it belongs to (must exist).
    pub user: &'a str,
}

/// Creates a provisioner key: no catalogs, no default, `read` (never used:
/// it reaches nothing), not a reporter, not self-issued. Bumps the
/// administrative generation and records `key.created`, in one transaction.
///
/// # Arguments
/// * `pool` - the database.
/// * `new` - its name, actor and user.
/// * `audit` - who creates it.
/// * `now` - the server clock.
///
/// # Returns
/// Its id and the key itself (shown once).
///
/// # Errors
/// [`OkfError::Refused`] for an invalid name or a live name and actor
/// already taken; [`OkfError::UnknownName`] for an unknown user;
/// [`OkfError::Io`] if the OS random source fails; [`OkfError::Database`].
pub async fn create_provisioner(
    pool: &PgPool,
    new: &NewProvisioner<'_>,
    audit: &AuditActor,
    now: DateTime<Utc>,
) -> Result<CreatedKey, OkfError> {
    if !is_valid_name(new.name) {
        return Err(OkfError::Refused {
            reason: format!("key name {:?} must be letters, digits, '.', '_' and '-'", new.name),
        });
    }
    let mut tx = pool.begin().await?;
    let user_id = users::id_of(&mut tx, new.user).await?;
    let key = crate::keys::generate()?;
    let inserted = sqlx::query_scalar::<_, String>(
        "INSERT INTO api_keys (name, sha256, access, maintenance_report, all_catalogs, actor, \
                               created_at, user_id, provisioner) \
         VALUES ($1, $2, 'read', false, false, $3, $4, $5, true) RETURNING id",
    )
    .bind(new.name)
    .bind(crate::keys::hash(&key).as_slice())
    .bind(new.actor)
    .bind(now)
    .bind(&user_id)
    .fetch_one(&mut *tx)
    .await;
    let id = live_name_or(inserted, new.name, new.actor)?;
    audit::record(
        &mut tx,
        audit,
        &Entry {
            action: Action::KeyCreated,
            target: Target::Key(&id),
            person: Some(&user_id),
            teams: &[],
            details: serde_json::json!({"provisioner": true}),
        },
        now,
    )
    .await?;
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(CreatedKey { id, key })
}
```

`live_name_or` is the `api_keys_live_name_actor` mapping `insert_key` does inline today (`src/store/keys.rs:452-469`); move it into this helper and call it from both:

```rust
/// An insert's id, or the "name and actor taken" refusal for the live
/// name constraint; any other error unchanged.
fn live_name_or(inserted: Result<String, sqlx::Error>, name: &str, actor: &str) -> Result<String, OkfError>
```

`src/auth.rs`: `Principal.provisioner: bool` (doc: "A provisioner key (Task 5): it reaches no catalog, is not an MCP credential, and may call only the allow-listed routes"); `From<KeyRecord>` sets it; `for_session` and every literal `false`. In `require_key`, refuse it before inserting:

```rust
        Ok(Some(principal)) if principal.provisioner => AppError(OkfError::Forbidden {
            reason: "a provisioner key manages accounts, teams and catalogs over /api/v1; it is \
                     not an MCP or archive credential"
                .to_string(),
        })
        .into_response(),
```

`src/store/audit.rs` `AuditActor::of`: `operator: principal.access == Access::Admin || principal.provisioner, provisioner: principal.provisioner,`.

- [ ] **Step 5: The allow-list and the gate** (`src/http/api/mod.rs`)

```rust
/// Every route a provisioner key may call, as `(method, matched template)`
/// (spec Part 4). The one list: [`provisioner_gate`] refuses every other
/// route before its handler runs, so adding a route never widens a
/// provisioner by accident.
pub(crate) const PROVISIONER_ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/v1/users"),
    ("GET", "/api/v1/users/{name}"),
    ("POST", "/api/v1/users"),
    ("PUT", "/api/v1/users/{name}/email"),
    ("POST", "/api/v1/users/{name}/invite"),
    ("DELETE", "/api/v1/users/{name}"),
    ("GET", "/api/v1/teams/{name}"),
    ("POST", "/api/v1/teams"),
    ("PUT", "/api/v1/teams/{name}/members/{user}"),
    ("DELETE", "/api/v1/teams/{name}/members/{user}"),
    ("DELETE", "/api/v1/teams/{name}"),
    ("PUT", "/api/v1/teams/{name}/suspension"),
    ("POST", "/api/v1/catalogs"),
    ("DELETE", "/api/v1/catalogs/{catalog}"),
    ("PUT", "/api/v1/catalogs/{catalog}/member-access"),
    ("GET", "/api/v1/audit"),
];

/// Whether a provisioner may call this route.
///
/// # Arguments
/// * `method` - the request's method.
/// * `template` - its matched route (`MatchedPath`), never the literal path.
///
/// # Returns
/// `true` for an allow-listed pair.
pub(crate) fn provisioner_may(method: &Method, template: &str) -> bool {
    PROVISIONER_ROUTES
        .iter()
        .any(|(allowed, route)| method.as_str() == *allowed && template == *route)
}

/// axum middleware, inside `require_principal`: refuses a provisioner on
/// every route that is not in [`PROVISIONER_ROUTES`].
///
/// # Returns
/// The route's response, or 403.
async fn provisioner_gate(request: Request, next: Next) -> Response {
    let provisioner = request
        .extensions()
        .get::<Arc<Principal>>()
        .is_some_and(|principal| principal.provisioner);
    // `MatchedPath` holds the template with the `/api/v1` prefix (verified facts).
    let allowed = || {
        request
            .extensions()
            .get::<MatchedPath>()
            .is_some_and(|path| provisioner_may(request.method(), path.as_str()))
    };
    if provisioner && !allowed() {
        return AppError(OkfError::Forbidden {
            reason: "a provisioner key may not call this route".to_string(),
        })
        .into_response();
    }
    next.run(request).await
}

/// Refuses all but a provisioner or an admin that reaches every catalog:
/// the writes in [`PROVISIONER_ROUTES`].
///
/// # Errors
/// As [`require_every_catalog`].
pub(crate) fn require_manager(principal: &Principal) -> Result<(), OkfError> {
    if principal.provisioner {
        return Ok(());
    }
    require_every_catalog(principal)
}

/// Refuses all but a provisioner or an admin: the reads in
/// [`PROVISIONER_ROUTES`].
///
/// # Errors
/// As [`require_admin`].
pub(crate) fn require_reader(principal: &Principal) -> Result<(), OkfError> {
    if principal.provisioner {
        return Ok(());
    }
    require_admin(principal)
}
```

In `routes`, add the gate inside the principal layer (a later `route_layer` wraps the earlier ones and runs first):

```rust
        .route_layer(middleware::from_fn(provisioner_gate))
        .route_layer(middleware::from_fn_with_state(Arc::clone(app), require_principal))
```

- [ ] **Step 6: The allowed handlers**

`src/http/api/people.rs`:
- `list_users`: `require_reader`; a provisioner must give `email` (else 403 "a provisioner looks users up by email"); its result leaves out admin users (`filter(|user| !(principal.provisioner && user.is_admin))`).
- `user_detail`, `team_detail`: `require_reader`; a provisioner gets 403 for an admin user's detail, and no keys or codes in it (`manages` is vacuously true for a credential with no catalogs).
- `create_user`, `set_email`, `invite_user`, `delete_user`, `create_team`, `set_member`, `remove_member`, `delete_team`, `set_suspension` (M13): `require_every_catalog` becomes `require_manager`. Their store functions already refuse a provisioner on an admin user through `audit::refuse_admin_target` (Tasks 4, 6a, 6b); the `AuditActor` extractor now carries `provisioner`.
- Update the module doc's access list.

`src/http/api/catalogs.rs`: `create_catalog` uses `require_manager`; `delete_catalog` skips `principal.require(Access::Admin, &catalog)?` for a provisioner (`if !principal.provisioner { ... }`); `set_member_access` keeps `ceiling::may_set_member_access`, which becomes:

```rust
pub fn may_set_member_access(principal: &Principal, catalog: &str) -> bool {
    principal.provisioner
        || (matches!(principal.reach, Reach::All) && principal.level(catalog) == Some(Access::Admin))
}
```

The entitlement routes use the same hook: refuse a provisioner there explicitly (`if principal.provisioner { 403 }`), since they are not on the allow-list anyway and the gate answers first.

`src/http/api/keys.rs`: `CreateKeyBody` gains `#[serde(default)] provisioner: bool` and `access` becomes `Option<String>` (required unless `provisioner`: 422 naming `access` otherwise). With `provisioner`: `require_every_catalog` (403 below), and any of `access`, `all`, `catalogs`, `default_catalog` or `maintenance_report` is 409 `Refused` ("a provisioner key takes no access, catalogs, default or reporting flag"); then `create_provisioner(app.pool(), &NewProvisioner { name, actor, user }, &actor_of_request, now)`. `KeyView.provisioner: bool`.

`src/cli.rs` `KeyCreateArgs`:

```rust
    /// Access level (not with `--provisioner`).
    #[arg(long, value_enum, required_unless_present = "provisioner")]
    pub access: Option<Access>,
    /// Granted catalogs, comma-separated.
    #[arg(long, value_delimiter = ',', required_unless_present_any = ["all", "provisioner"],
          conflicts_with = "all")]
    pub catalogs: Vec<String>,
    /// A provisioner key: manages accounts, teams and catalogs over the API
    /// and reaches no catalog (spec Part 4).
    #[arg(long, conflicts_with_all = ["access", "catalogs", "all", "default", "maintenance_report"])]
    pub provisioner: bool,
```

`run_key`: `if args.provisioner { create_provisioner(..., &AuditActor::cli(), now) }` else as today with `args.access.expect("clap requires --access without --provisioner")`. `format_key_record` adds `provisioner yes|no`.

`keys.js`: in the New key form, a `provisioner` switch before the other controls, which wraps them in `h('fieldset', { class: 'key-reach' }, ...)` and sets `fieldset.disabled = checkbox.checked`; the body sends `{ name, provisioner: true, user }` when checked. In the list's Name column, `key.provisioner ? [' ', h('span', { dataset: { test: `key-provisioner-${key.name}` } }, pill('provisioner', 'warn'))] : null` (through `h()`).

`docs/operations.md`: the `key create` row gains `--provisioner`; the parity row

```markdown
| `okf-mcp key create --provisioner` | `POST /api/v1/keys` | API keys → New key → Provisioner |
```

the `POST /api/v1/keys` row documents `provisioner`, and a new subsection under "Users and teams", **"Provisioner keys"**: what it may call (the 16 routes), what it may not, that it is refused on admin users, that it is not an MCP credential, that every use is audited, and that it is protected like an admin key (it can take over any non-admin account).

- [ ] **Step 7: Run, gate, commit**

Run the narrow tests, `tools/agentic/gate.sh test`, `tools/agentic/gate.sh test-ui`, `fmt`, `lint`.

```bash
git add migrations/0014_provisioner.sql src tests docs/operations.md changelog/
git commit -m "feat(auth): provisioner keys manage accounts, teams and catalogs and read nothing (M14 task 5)" \
           -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Recording administrative changes (Tasks 6a to 6d): the common pattern

Each of these four tasks gives a group of store functions an `actor: &AuditActor` argument (named `audit` in `src/store/keys.rs`, where `actor` already means a key's actor label) and calls `audit::record` inside the function's own transaction, after the change and before `commit`. A function that wrote nothing (an unchanged value, a no-op) records nothing. A function that would pass five arguments takes a small input struct instead (`AdminChange`, `NewEntitlement`, `Membership`, `NewCatalog`), as the code rules require. Every caller changes in the same PR: the CLI passes `&AuditActor::cli()`; REST handlers take an `actor: AuditActor` extractor argument (Task 1) and pass `&actor`; tests pass `&AuditActor::cli()`.

The shape, for `users::set_email`:

```rust
pub async fn set_email(
    pool: &PgPool,
    name: &str,
    email: Option<&str>,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    // ... checks and UPDATE as today, also returning `u.is_admin` ...
    audit::refuse_admin_target(actor, is_admin, name)?;
    if changed {
        // ... DELETE FROM email_tokens, bump_admin_generation, as today ...
        audit::record(
            &mut tx,
            actor,
            &Entry {
                action: Action::UserEmailChanged,
                target: Target::User(&user_id),
                person: None,
                teams: &[],
                // Never the address: only that it changed (spec "People in entries").
                details: serde_json::json!({"cleared": email.is_none()}),
            },
            now,
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}
```

Each task's tests follow one template, using the helper Task 1 adds (`audit::testing::entries`):

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn set_email_records_one_entry_and_none_for_no_change() {
        let db = test_db().await;
        seed_user(&db.pool, "alice", None).await;

        set_email(&db.pool, "alice", Some("alice@example.com"), &AuditActor::cli(), now()).await.unwrap();
        set_email(&db.pool, "alice", Some("ALICE@example.com"), &AuditActor::cli(), now()).await.unwrap();

        let entries = audit::testing::entries(&db.pool, "user.email_changed").await;
        assert_eq!(entries.len(), 1, "a change of letter case only is not a change");
        assert_eq!(entries[0].person.as_deref(), Some("alice"));
        assert_eq!(entries[0].details, serde_json::json!({"cleared": false}));
        assert!(!entries[0].details.to_string().contains('@'), "no email in details");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_audit_write_fails_the_change() {
        let db = test_db().await;
        seed_user(&db.pool, "alice", None).await;
        crate::http::api::testing::fail_on(&db.pool, "BEFORE INSERT ON audit_entries FOR EACH ROW").await;

        let refused = set_email(&db.pool, "alice", Some("alice@example.com"), &AuditActor::cli(), now()).await;

        assert!(refused.is_err());
        assert_eq!(find(&db.pool, "alice").await.unwrap().email, None, "the change rolled back");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_provisioner_cannot_set_an_admins_email() {
        let db = test_db().await;
        seed_user(&db.pool, "staff", None).await;
        set_admin(&db.pool, "staff", AdminChange { on: true, allow_last: true }, &AuditActor::cli(), now())
            .await
            .unwrap();
        let mut provisioner = AuditActor::cli();
        provisioner.provisioner = true;

        let refused = set_email(&db.pool, "staff", Some("x@example.com"), &provisioner, now()).await;

        assert!(matches!(refused, Err(OkfError::Forbidden { .. })));
        assert_eq!(find(&db.pool, "staff").await.unwrap().email, None);
    }
```

Write one "records one entry" test per row of the task's table (checking `action`, the person or target, `teams` and the `details` keys), one forced-failure test per store module, and one provisioner test per guarded function. The guard reads the target's `is_admin` under a lock `users::set_admin` conflicts with (`FOR UPDATE`, `FOR NO KEY UPDATE`, `FOR SHARE` or `FOR KEY SHARE` on the user row), so it cannot race a promotion.

Run per task: `tools/agentic/gate.sh narrow --lib store::<module>::tests` for each module touched, then `tools/agentic/gate.sh test` (it finds every caller the signature change missed), `fmt`, `lint`. Each task adds its actions to `docs/operations.md`'s new "### Audit log" action table (Task 1 adds no docs; the first of 6a to 6d to merge creates the section with a one-line lead, "Every change below writes one entry in the change's own transaction", and the others add rows).

---

### Task 6a: Recording user changes

**Files:** `src/store/users.rs`, `src/store/ceiling.rs` (entitlements), `src/store/user_sessions.rs`, `src/mail/invite.rs`, `src/cli/people.rs`, `src/cli.rs` (if `run` calls these), `src/http/api/people.rs`, every test caller, `docs/operations.md` (audit table rows)

| Function (new signature) | Action | Target, person | Teams | Details | Guard |
|---|---|---|---|---|---|
| `users::create(pool, new, actor, now)` | `user.created` | `User(id)` | none | `{}` | |
| `users::set_email(pool, name, email, actor, now)` | `user.email_changed` (when changed) | `User` | none | `{"cleared": bool}` | yes |
| `users::set_admin(pool, name, change: AdminChange { on, allow_last }, actor, now) -> bool` | `user.admin_changed` (when changed) | `User` | none | `{"from": bool, "to": bool}` | (a provisioner never reaches the route) |
| `invite::send_setup(pool, config, user, actor, now)` | `user.invited` | `User` | none | `{}` | yes: `SELECT id, is_admin FROM users WHERE name = $1 FOR SHARE` first in its transaction |
| `ceiling::entitle(pool, &NewEntitlement { user, catalog, access }, actor, now)` | `user.entitlement_set` | `User` | `catalog_teams` of the catalog | `{"catalog", "access", "previous": "read"\|"write"\|null}` | |
| `ceiling::unentitle(pool, user, catalog, actor, now)` | `user.entitlement_removed` (when one was removed) | `User` | `catalog_teams` | `{"catalog"}` | |
| `users::end_sessions(pool, name, actor, now) -> u64` | `user.sessions_revoked` | `User` | none | `{"sessions": n}` | |
| `users::end_session_by_prefix(pool, name, id_prefix, actor, now)` (new: the admin route's one-session revoke, in one transaction with the user's row lock; `user_sessions::revoke_by_prefix` gets a `revoke_by_prefix_in(conn, ..)` body it shares) | `user.sessions_revoked` | `User` | none | `{"sessions": 1}` | |

The self-service `DELETE /me/sessions/...` keeps calling `user_sessions::revoke_by_prefix` and records nothing: it is the user's own session, not an administrative change. Callers of `users::create` outside the CLI and the API (an OIDC first sign-in, if it creates users: `grep -rn "users::create(" src`) pass `&AuditActor::system("oidc")`.

Commit:

```bash
git add src tests docs/operations.md changelog/
git commit -m "feat(audit): record user, entitlement, invitation and session changes (M14 task 6a)" \
           -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6b: Recording team and catalog changes

**Files:** `src/store/teams.rs`, `src/store/catalogs.rs`, `src/store/ceiling.rs` (`set_member_access`), `src/store/import.rs`, `src/cli.rs` (`run_catalog`, `Command::Import`), `src/cli/people.rs` (`team`), `src/http/api/people.rs` (team routes and M13's `set_suspension`), `src/http/api/catalogs.rs`, `src/http/archives.rs` (import), `src/mcp/testing.rs` and `src/db.rs` test setup, every test caller, `docs/operations.md`

| Function (new signature) | Action | Target, person | Teams | Details | Guard |
|---|---|---|---|---|---|
| `teams::create(pool, new, actor, now)` | `team.created` | `Team(name)`, person = owner | `[team_ref]` | `{}` | owner, via `users::id_and_admin_of` (Task 4) |
| `teams::set_member(pool, &Membership { team, user, role }, actor, now)` | `team.member_added` or `team.role_changed` | `Team`, person = user | `[team_ref]` | `{"role", "from": role\|null}` | user |
| `teams::remove_member(pool, team, user, actor, now)` | `team.member_removed` | `Team`, person = user | `[team_ref]` | `{}` | user |
| `teams::delete(pool, name, actor, now)` | `team.deleted` | `Team` | `[team_ref]` (read before the delete) | `{}` | |
| `teams::suspend(pool, name, notice, actor, now)` (M13 plan, Task 1) | `team.suspended` | `Team` | `[team_ref]` | `{"notice": bool, "already": bool}` | |
| `teams::unsuspend(pool, name, actor, now)` (M13) | `team.unsuspended` (when lifted) | `Team` | `[team_ref]` | `{}` | |
| `catalogs::create(pool, &NewCatalog { name, description, owner_team }, actor, now)` | `catalog.created` | `Catalog` | `catalog_teams` | `{}` | |
| `catalogs::set_owner(pool, name, team, actor, now)` | `catalog.owner_changed` | `Catalog` | old and new team refs | `{"from": ref\|null, "to": ref}` | |
| `catalogs::update_description(pool, name, description, actor, now)` | `catalog.description_changed` (when changed) | `Catalog` | `catalog_teams` | `{"length": n}` (not the text) | |
| `catalogs::delete(pool, name, force, actor, now)` | `catalog.deleted` | `Catalog` | `catalog_teams` (read before the delete) | `{"forced": bool, "generation": n}` | |
| `ceiling::set_member_access(pool, catalog, access, actor, now)` | `catalog.member_access_changed` (when changed) | `Catalog` | `catalog_teams` | `{"from", "to"}` | |
| `import::import(pool, catalog, files, &ImportOptions { ..., audit: &AuditActor })` | `catalog.imported` | `Catalog` | `catalog_teams` | `{"replace", "scope", "written": n, "deleted": n, "generation": n}` | |

`team_ref` and `catalog_teams` are read in the change's transaction, so a personal team is written as `person:<id>` (Task 1). M13's `set_suspension` handler and `team suspend` / `unsuspend` commands pass the actor. `catalogs::create` is called from many tests (`grep -rn "catalogs::create(" src tests` finds about 90): the change is mechanical (`&NewCatalog { name, description, owner_team }, &AuditActor::cli(),`).

Commit:

```bash
git add src tests docs/operations.md changelog/
git commit -m "feat(audit): record team, membership, suspension, catalog and import changes (M14 task 6b)" \
           -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6c: Recording credential changes

**Files:** `src/store/keys.rs`, `src/store/oauth/codes.rs`, `src/store/oauth/grants.rs`, `src/cli.rs` (`run_key`, `run_oauth`), `src/http/api/keys.rs`, `src/http/api/oauth.rs`, `src/http/api/me.rs`, the consent and token handlers that call `grants::create_self_issued` and `codes::redeem` (`grep -rn "create_self_issued\|codes::redeem" src`), every test caller, `docs/operations.md`

| Function (new signature; `audit: &AuditActor` before `now`) | Action | Target, person | Teams | Details |
|---|---|---|---|---|
| `keys::create_with_id`, `keys::create` | `key.created` | `Key(id)`, person = user | `key_teams` (after the grants are inserted) | `{"access", "all_catalogs", "catalogs": n, "maintenance_report", "self_issued": false, "expires": bool}` |
| `keys::create_self_issued(pool, new, max_live, audit, now)` | `key.created` | `Key`, person = user | `key_teams` | as above with `"self_issued": true` |
| `keys::revoke_target`, `keys::revoke_by_id` | `key.revoked`, or `oauth.grant_revoked` for an OAuth grant | `Key` or `Grant` | `key_teams` | `{}` |
| `keys::grant_target`, `grant_by_id` | `key.catalog_granted` | `Key` | `catalog_teams` of the catalog | `{"catalog"}` |
| `keys::ungrant_target`, `ungrant_by_id` | `key.catalog_ungranted` | `Key` | `catalog_teams` | `{"catalog"}` |
| `keys::assign_user_by_id`, `assign_user_target` | `key.user_assigned` | `Key` or `Grant`, person = new user | `key_teams` | `{}` |
| `codes::assign_user(pool, id, user, audit, now)` | `key.user_assigned` | `Code`, person = new user | none | `{}` |
| `keys::set_maintenance_report_target`, `_by_id` (when changed) | `key.report_flag_changed` | `Key` | `key_teams` | `{"from", "to"}` |
| `codes::create` | `oauth.code_created` | `Code`, person = user | `audit::code_teams` (new here: `key_teams`' query over `sign_in_code_catalogs`) | `{"access", "all_catalogs", "catalogs": n}` |
| `codes::revoke` | `oauth.code_revoked` | `Code` | as above | `{}` |
| `codes::redeem(pool, code_id, client, now)` (signature unchanged) | `oauth.grant_created` | `Grant`, person = the code's user | `key_teams` of the new grant | `{"code": id, "client": client_host(client)}`, actor `AuditActor::system("oauth")` |
| `grants::create_self_issued(pool, new, audit, now)` (consent) | `oauth.grant_created` | `Grant`, person = user | `key_teams` | `{"client": client_host(client)}` |

Never the key, code, token or its hash in `details`. The provisioner's `key.created` is already recorded by `create_provisioner` (Task 5). Self-service routes in `me.rs` (creating and revoking one's own keys and grants) record with the session's actor: they are credential changes.

Commit:

```bash
git add src tests docs/operations.md changelog/
git commit -m "feat(audit): record key, grant, sign-in code and assignment changes (M14 task 6c)" \
           -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6d: Recording settings; retention

**Files:** `src/store/settings.rs`, `src/store/audit.rs` (`prune`), `src/app.rs` (the daily prune), `src/cli/admin.rs`, `src/http/api/settings.rs`, `docs/operations.md`

- [ ] **Step 1: Settings write entries**

| Function (new signature) | Action | Target | Details |
|---|---|---|---|
| `settings::set(pool, key, value, actor, now)` (`by` is gone; `updated_by` is `actor.label`, which is what both callers pass today) | `setting.changed` | `Setting(key)` | `{"from": stored value\|null, "to": stored value}`; for `support_contact` only `{"set": bool}` (it can be an email address) |
| `settings::reset(pool, key, actor, now)` (when there was an override) | `setting.cleared` | `Setting(key)` | `{"from": stored value}` (`{}` for `support_contact`) |

`set` reads the previous value with `SELECT value FROM server_settings WHERE key = $1 FOR UPDATE` in its transaction. Settings entries belong to no team.

- [ ] **Step 2: Write the failing retention tests**

`src/store/settings.rs`:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn audit_retention_is_at_least_30_days_and_defaults_to_400() {
        let db = test_db().await;
        let low = set(&db.pool, SettingKey::AuditRetentionDays, "29", &AuditActor::cli(), now()).await;
        set(&db.pool, SettingKey::AuditRetentionDays, "90", &AuditActor::cli(), now()).await.unwrap();

        assert!(matches!(low, Err(OkfError::LimitExceeded { .. })));
        assert_eq!(Settings::from_config(&AuthConfig::default()).audit_retention_days, 400);
        assert_eq!(load(&db.pool, &AuthConfig::default()).await.unwrap().audit_retention_days, 90);
    }
```

`src/store/audit.rs`:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn prune_deletes_entries_past_retention_and_keeps_the_rest() {
        let db = test_db().await;
        let mut tx = db.pool.begin().await.unwrap();
        let setting = |action| Entry {
            action,
            target: Target::Setting("product_name"),
            person: None,
            teams: &[],
            details: serde_json::json!({}),
        };
        record(&mut tx, &AuditActor::cli(), &setting(Action::SettingChanged), Utc::now() - TimeDelta::days(500))
            .await
            .unwrap();
        record(&mut tx, &AuditActor::cli(), &setting(Action::SettingCleared), Utc::now())
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let pruned = prune(&db.pool, Utc::now() - TimeDelta::days(400) - TimeDelta::hours(1)).await.unwrap();

        assert_eq!(pruned, 1);
        assert_eq!(testing::entries(&db.pool, "setting.cleared").await.len(), 1);
    }
```

- [ ] **Step 3: Implement retention**

`Settings.audit_retention_days: u32` (default `audit::DEFAULT_RETENTION_DAYS = 400`); `SettingKey::AuditRetentionDays` (`audit_retention_days`), a whole number from `audit::MIN_RETENTION_DAYS = 30` to `3650`. The trigger (Task 1) reads the same row as a JSON number, so store it as `Value::from(n)`.

```rust
/// Deletes entries older than `before`: the daily retention prune, the one
/// delete of the log (the trigger refuses younger entries).
///
/// # Arguments
/// * `pool` - the database.
/// * `before` - the cut-off: now, minus the retention, minus a margin.
///
/// # Returns
/// How many went.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn prune(pool: &PgPool, before: DateTime<Utc>) -> Result<u64, OkfError> {
    Ok(sqlx::query("DELETE FROM audit_entries WHERE at < $1")
        .bind(before)
        .execute(pool)
        .await?
        .rows_affected())
}
```

`src/app.rs`: a field `audit_pruned: std::sync::Mutex<Option<Instant>>`, `const AUDIT_PRUNE_EVERY: Duration = 24 hours`, and in `poll` after `prune_oauth`:

```rust
    /// At most once per [`AUDIT_PRUNE_EVERY`], deletes audit entries past the
    /// `audit_retention_days` setting, with an hour's margin so a small skew
    /// between this clock and the database's never trips the trigger.
    /// Called only under `polling`; a failure is logged and waits a day.
    async fn prune_audit(&self) {
        let now_instant = std::time::Instant::now();
        {
            let mut pruned = self.audit_pruned.lock().expect("audit prune lock poisoned");
            if pruned.is_some_and(|at| now_instant.duration_since(at) < AUDIT_PRUNE_EVERY) {
                return;
            }
            *pruned = Some(now_instant);
        }
        let days = i64::from(self.settings().audit_retention_days);
        let before = self.clock.now() - TimeDelta::days(days) - TimeDelta::hours(1);
        log_pruned("old audit entries", audit::prune(&self.pool, before).await);
    }
```

`docs/operations.md`: the Settings table gains `audit_retention_days`; the audit section says entries older than it are deleted daily, and nothing else deletes them.

- [ ] **Step 4: Run, gate, commit**

```bash
git add src docs/operations.md changelog/
git commit -m "feat(audit): record settings changes; audit_retention_days and the daily prune (M14 task 6d)" \
           -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7a: Recording catalog writes

**Files:**
- Modify: `src/mcp/context.rs` (`requester`, `audit_actor`), `src/http/api/mod.rs` (`context`), `src/http/api/{health,changesets,browse,maintenance,search}.rs` (the 18 callers), `src/store/publish.rs`, `src/mcp/changesets.rs` (`publish`, `discard`, `revert`), `src/write.rs` (`apply_audited`), `docs/operations.md`

- [ ] **Step 1: Write the failing tests** (in `src/mcp/changesets.rs`'s tests; reuse its `writer`, `open`, `pending_entry` helpers)

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn a_publish_records_one_entry_with_the_handle_files_and_generations() {
        let test = crate::mcp::testing::app_with(&[("main", &[("index.md", "# Main\n")])], Config::default()).await;
        let ctx = writer(&test, "w1", "main").await;
        let handle = open(&ctx).await;
        crate::write::apply(&ctx, &handle, None, |ws, _| {
            ws.write_atomic("a.md", "---\ntitle: A\n---\n# A\n")?;
            Ok(OpOutcome { touched: vec!["a.md".into()], replay: BTreeMap::new(), message: "w".into(), notes: vec![] })
        })
        .await
        .unwrap();

        publish(&ctx, PublishParams { changeset: handle.clone(), message: "add a".into() }).await.unwrap();

        let entries = crate::store::audit::testing::entries(test.app.pool(), "catalog.published").await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].target.as_deref(), Some("main"));
        assert_eq!(entries[0].details["changeset"], handle.as_str());
        assert_eq!(entries[0].details["files"], 1);
        assert_eq!(entries[0].details["generation_to"], entries[0].details["generation_from"].as_i64().unwrap() + 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_revert_is_recorded_when_it_is_written_and_an_admins_discard_too() {
        let test = crate::mcp::testing::app_with(&[("main", &[("index.md", "# Main\n")])], Config::default()).await;
        let owner = writer(&test, "w1", "main").await;
        let handle = open(&owner).await;
        revert(&owner, revert_to(&handle, "/index.md", 1)).await.unwrap();
        let admin = crate::mcp::testing::context(
            &test,
            crate::mcp::testing::principal_as("root", Access::Admin, &[], None),
            None,
        );
        crate::mcp::testing::store_key(&test, "root").await;

        discard(&owner, discard_paths(&handle, Some(&["/index.md"]))).await.unwrap();
        discard(&admin, discard_paths(&handle, None)).await.unwrap();

        let reverted = crate::store::audit::testing::entries(test.app.pool(), "catalog.reverted").await;
        let discarded = crate::store::audit::testing::entries(test.app.pool(), "catalog.changeset_discarded").await;
        assert_eq!(reverted.len(), 1);
        assert_eq!(reverted[0].details["path"], "/index.md");
        assert_eq!(reverted[0].details["revision"], 1);
        assert_eq!(discarded.len(), 1, "the owner's own discard is not an administrative change");
        assert_eq!(discarded[0].details["reason"], "admin");
    }
```

(Adapt the helper calls to the module's own: `writer`, `open`, `revert_to`, `discard_paths` exist in its tests; `store_key` makes `root` a stored key so the changeset ownership check sees it.)

- [ ] **Step 2: Run them and see them fail**

Run: `tools/agentic/gate.sh narrow --lib mcp::changesets::tests::a_publish_records`, then `tools/agentic/gate.sh narrow --lib mcp::changesets::tests::a_revert_is_recorded`
Expected: no entries.

- [ ] **Step 3: The requester in `ToolContext`**

`ToolContext` gains `requester: Requester`. `ToolContext::new` keeps its three arguments and sets `Requester { ip: None, source: Source::Api }`; add

```rust
    /// The same context, for a request with this address and source.
    ///
    /// # Returns
    /// The context.
    pub fn with_requester(mut self, requester: Requester) -> Self {
        self.requester = requester;
        self
    }

    /// Who this call is, for the audit log.
    ///
    /// # Returns
    /// The principal with the request's IP and source.
    pub fn audit_actor(&self) -> AuditActor {
        AuditActor::of(&self.principal, &self.requester)
    }
```

`from_parts` uses `.with_requester(Requester::of(parts))`. `api::context(app, principal, requester: Requester)` calls `ToolContext::new(..).with_requester(requester)`; each of the 18 REST callers takes a `requester: Requester` extractor argument and passes it.

- [ ] **Step 4: Publish, discard and revert**

`store::publish::publish(app, state, &PublishInput { changeset, actor, message }, audit: &AuditActor)` (a struct: the function would otherwise take six arguments). The generation bump becomes `UPDATE catalogs SET generation = generation + 1 WHERE id = $1 RETURNING generation`; when revisions were written, before `set_state`:

```rust
        let teams = audit::catalog_teams(&mut tx, state.id()).await?;
        audit::record(
            &mut tx,
            audit,
            &Entry {
                action: Action::CatalogPublished,
                target: Target::Catalog(state.name()),
                person: None,
                teams: &teams,
                // The diff stays in the catalog's history, found by the handle.
                details: serde_json::json!({
                    "changeset": locked.handle,
                    "files": revisions.len(),
                    "generation_from": generation - 1,
                    "generation_to": generation,
                }),
            },
            now,
        )
        .await?;
```

A publish that wrote no revision is not a catalog write and records nothing. `mcp::changesets::publish` passes `&ctx.audit_actor()`.

`discard`: when the changeset is closed (`paths` is `None`) by a principal that does not own it (`changeset.key_id.as_deref() != Some(&principal.key_id)`, an admin), record `catalog.changeset_discarded` in its transaction with `{"changeset", "files": removed, "reason": "admin"}`.

`src/write.rs`:

```rust
/// An audit entry an edit records in its own transaction (a revert).
#[derive(Debug, Clone, PartialEq)]
pub struct OpAudit {
    /// The action.
    pub action: Action,
    /// Its details; the changeset handle is added.
    pub details: serde_json::Value,
}

/// [`apply`], recording `audit` in the edit's transaction before it commits.
///
/// # Errors
/// As [`apply`]; [`OkfError::Database`] from the entry.
pub async fn apply_audited<F>(
    ctx: &ToolContext,
    changeset: &str,
    by: Option<&str>,
    audit: Option<OpAudit>,
    op: F,
) -> Result<WriteReport, OkfError>
```

`apply` becomes `apply_audited(ctx, changeset, by, None, op)`. Before `tx.commit()`, with `Some(entry)`: merge `"changeset": changeset_row.handle` into `entry.details`, read `audit::catalog_teams(&mut tx, state.id())` and record `Target::Catalog(state.name())` with `ctx.audit_actor()`. `revert` calls `apply_audited(.., Some(OpAudit { action: Action::CatalogReverted, details: json!({"path": format!("/{path}"), "revision": revision}) }), ..)`.

Moves, splits and concept deletes are publishes and are recorded as such (spec).

- [ ] **Step 5: Run, gate, commit**

Run both narrow tests, `tools/agentic/gate.sh test`, `fmt`, `lint`. `docs/operations.md`'s audit table gains the three write actions.

```bash
git add src docs/operations.md changelog/
git commit -m "feat(audit): record publishes, reverts and admins' discards (M14 task 7a)" \
           -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7b: Access gates, exports and admin reads

**Files:**
- Create: `src/store/gates.rs`
- Modify: `src/auth.rs` (`AccessGates`, `Admission`, `Principal.gates`, `admission`, cache wiring, `Keys::gates`), `src/http/console_session.rs` (`for_session` gets the gates), `src/store/audit.rs` (`AccessKind`, `record_admin_read`), `src/store/export.rs`, `src/http/archives.rs` (export), `src/cli.rs` (`Command::Export`), `src/mcp/context.rs` (`admit`), `src/http/api/browse.rs` (the revision route), `src/app.rs` (`note_access`, `settle_access`), `src/http/mod.rs` (`serve` settles at shutdown), every `Principal { ... }` literal (`gates: Default::default(),`), `docs/operations.md`

- [ ] **Step 1: Write the failing tests**

`src/auth.rs`:

```rust
    fn gates_with_member(user_id: &str, catalog: &str) -> Arc<AccessGates> {
        Arc::new(AccessGates {
            members: HashMap::from([(user_id.to_string(), HashSet::from([catalog.to_string()]))]),
            ..AccessGates::default()
        })
    }

    #[test]
    fn an_admin_reading_outside_its_teams_is_an_admin_read_and_a_member_is_a_customer() {
        let gates = gates_with_member("u-member", "main");
        let principal = |user: Option<&str>, access| Principal {
            user_id: user.map(str::to_string),
            gates: Arc::clone(&gates),
            ..writer_on(&["main"], access, Arc::default())
        };
        let now = Utc::now();

        let member_admin = principal(Some("u-member"), Access::Admin).admission("main", Access::Read, now);
        let staff = principal(Some("u-staff"), Access::Admin).admission("main", Access::Read, now);
        let legacy = principal(None, Access::Admin).admission("main", Access::Read, now);
        let staff_write = principal(Some("u-staff"), Access::Admin).admission("main", Access::Write, now);
        let reader = principal(Some("u-staff"), Access::Read).admission("main", Access::Read, now);

        assert_eq!(member_admin.unwrap(), Admission::Customer);
        assert_eq!(staff.unwrap(), Admission::AdminRead);
        assert_eq!(legacy.unwrap(), Admission::AdminRead, "a key with no user always counts");
        assert_eq!(staff_write.unwrap(), Admission::Untracked, "writes are recorded as publishes");
        assert_eq!(reader.unwrap(), Admission::Untracked);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_cache_loads_memberships_and_admins_for_every_principal() {
        let db = crate::db::test_db().await;
        crate::store::catalogs::create(
            &db.pool,
            &crate::store::catalogs::NewCatalog { name: "main", description: "", owner_team: crate::db::TEST_TEAM },
            &crate::store::audit::AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();

        let keys = Keys::load(&db.pool, Arc::new(crate::clock::SystemClock)).await.unwrap();
        let me: String = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(crate::db::TEST_USER)
            .fetch_one(&db.pool)
            .await
            .unwrap();

        assert!(keys.gates().members[&me].contains("main"));
    }
```

(`writer_on` is M13's test helper in this module, Task 3 of its plan.)

`src/store/audit.rs`:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn admin_reads_are_one_entry_per_credential_catalog_and_hour_with_an_exact_count() {
        let db = test_db().await;
        crate::store::catalogs::create(
            &db.pool,
            &crate::store::catalogs::NewCatalog { name: "main", description: "", owner_team: TEST_TEAM },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let actor = admin_actor(&user_id(&db.pool, TEST_USER).await);

        // Ten at once in one hour, then one in the next. `JoinSet` runs the
        // ten as concurrent tasks (the crate has no `futures` dependency).
        let mut reads = tokio::task::JoinSet::new();
        for _ in 0..10 {
            let (pool, actor) = (db.pool.clone(), actor.clone());
            reads.spawn(async move { record_admin_read(&pool, &actor, "main", now()).await });
        }
        while let Some(read) = reads.join_next().await {
            read.unwrap().unwrap();
        }
        record_admin_read(&db.pool, &actor, "main", now() + TimeDelta::hours(1)).await.unwrap();

        let counts: Vec<i64> = sqlx::query_scalar(
            "SELECT c.requests FROM audit_entries e JOIN audit_access_counts c ON c.entry_id = e.id \
             WHERE e.action = 'catalog.admin_read' ORDER BY e.id",
        )
        .fetch_all(&db.pool)
        .await
        .unwrap();
        assert_eq!(counts, [10, 1]);
    }
```

`src/http/api/browse.rs` (or `catalogs.rs`) tests:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn an_operator_read_is_bucketed_and_a_members_read_is_not() {
        let test = app().await;
        let scoped = keys(&test).await;
        // An every-catalog admin key of a user outside main's owner team.
        crate::store::users::create(test.app.pool(), &NewUser { name: "staff", display_name: "" },
                                    &AuditActor::cli(), Utc::now()).await.unwrap();
        let staff = key_rows::create_with_id(test.app.pool(), &NewKey {
            name: "staff-root", access: Access::Admin, maintenance_report: false, grants: Grants::All,
            default_catalog: None, actor: "staff-root", user: "staff", self_issued: false,
            expires_at: None, levels: &[],
        }, &AuditActor::cli(), Utc::now()).await.unwrap().key;
        test.app.poll().await.unwrap();

        send(&test, "GET", "/api/v1/catalogs/main/tree", Some(&staff), None).await;
        send(&test, "GET", "/api/v1/catalogs/main/document/index.md", Some(&staff), None).await;
        send(&test, "GET", "/api/v1/catalogs/main/tree", Some(&scoped.read), None).await;
        test.app.settle_access().await;

        let entries = crate::store::audit::testing::entries(test.app.pool(), "catalog.admin_read").await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].requests, Some(2));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn every_export_is_recorded_whoever_asks() {
        let test = app().await;
        let scoped = keys(&test).await;

        let (status, _) = send(&test, "GET", "/api/v1/catalogs/main/export", Some(&scoped.read), None).await;

        assert_eq!(status, StatusCode::OK);
        let entries = crate::store::audit::testing::entries(test.app.pool(), "catalog.exported").await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].source, "api");
        assert!(entries[0].details["files"].as_i64().unwrap() > 0);
    }
```

(Use the browse routes `browse::routes()` defines; the document route's path may differ.) And in `src/store/export.rs`:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn the_cli_export_is_recorded_as_the_cli() {
        let db = test_db().await;
        let now = Utc.with_ymd_and_hms(2026, 11, 13, 12, 0, 0).unwrap();
        catalogs::create(&db.pool, &catalogs::NewCatalog { name: "pg", description: "", owner_team: crate::db::TEST_TEAM },
                         &AuditActor::cli(), now).await.unwrap();

        let (_, generation) = export(&db.pool, "pg", Vec::new(), &AuditActor::cli(), now).await.unwrap();

        let entries = crate::store::audit::testing::entries(&db.pool, "catalog.exported").await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].source, "cli");
        assert_eq!(entries[0].target.as_deref(), Some("pg"));
        assert_eq!(entries[0].details, serde_json::json!({"generation": generation, "files": 0}));
    }
```

- [ ] **Step 2: Run them and see them fail**

Run: `tools/agentic/gate.sh narrow --lib auth::tests`, `tools/agentic/gate.sh narrow --lib store::audit::tests::admin_reads`, `tools/agentic/gate.sh narrow --lib http::api::browse::tests::an_operator_read`, `tools/agentic/gate.sh narrow --lib http::api::browse::tests::every_export`, `tools/agentic/gate.sh narrow --lib store::export::tests`
Expected: compile errors.

- [ ] **Step 3: Implement the gates** (`src/auth.rs`, `src/store/gates.rs`)

```rust
/// Who reaches which catalog as a customer, loaded with the key cache and
/// shared by every principal (as M13's suspension map is). Task 9b adds the
/// unlock and maintenance fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccessGates {
    /// By `users.id`: the catalogs owned by the user's teams.
    pub members: HashMap<String, HashSet<String>>,
    /// The `users.id` of every admin user.
    pub admins: HashSet<String>,
}

/// How a content read or write is let in (spec Parts 5 and 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// A member of the owner team (or, Task 9b, an entitled non-admin):
    /// nothing is recorded.
    Customer,
    /// Neither a customer nor an admin read: nothing is recorded (writes
    /// are recorded as publishes).
    Untracked,
    /// An admin credential reading a catalog outside its user's teams, with
    /// `require_unlock` off: an hourly `catalog.admin_read` bucket.
    AdminRead,
}

impl Admission {
    /// Whether this admission writes an audit row or a tally.
    pub fn is_recorded(self) -> bool {
        matches!(self, Admission::AdminRead)
    }
}

impl Principal {
    /// How this principal is let into `catalog`'s contents for `access`.
    /// Call it only on a catalog the principal reaches at that level.
    ///
    /// # Arguments
    /// * `catalog` - the catalog.
    /// * `access` - what the operation needs.
    /// * `now` - the server clock (unlock expiry, Task 9b).
    ///
    /// # Returns
    /// The admission.
    ///
    /// # Errors
    /// None yet; Task 9b adds `UnlockRequired` and `MaintenanceOff`.
    pub fn admission(&self, catalog: &str, access: Access, now: DateTime<Utc>) -> Result<Admission, OkfError> {
        let _ = now;
        if self.is_member_of_owner(catalog) {
            return Ok(Admission::Customer);
        }
        Ok(if self.access == Access::Admin && access == Access::Read {
            Admission::AdminRead
        } else {
            Admission::Untracked
        })
    }

    /// Whether the credential's user is in the catalog's owner team.
    fn is_member_of_owner(&self, catalog: &str) -> bool {
        self.user_id
            .as_deref()
            .and_then(|user_id| self.gates.members.get(user_id))
            .is_some_and(|catalogs| catalogs.contains(catalog))
    }
}
```

`Principal.gates: Arc<AccessGates>` (doc: "The access gates of the cache it came from (shared)"). `KeyCache` gains `gates: Arc<AccessGates>`; `from_records(records, ceilings, suspended, gates, now)`; `principal_of` sets `principal.gates = Arc::clone(gates)`; `KeyCache::read` adds `gates::load(pool)` to its `try_join!`; `Keys::gates() -> Arc<AccessGates>`. `Principal::for_session(user, ceiling, suspended, gates)`; `session_principal` passes `app.keys().gates()`.

`src/store/gates.rs`:

```rust
//! The access gates the key cache loads (spec Parts 5 and 6): who is a
//! customer of which catalog. One query per source; the cache reloads them
//! whenever `admin_generation` moves (every team, user and catalog write
//! bumps it).

/// Loads the gates.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn load(pool: &PgPool) -> Result<AccessGates, OkfError> {
    let (members, admins) = tokio::try_join!(
        sqlx::query_as::<_, (String, String)>(
            "SELECT m.user_id, c.name FROM team_members m \
             JOIN catalogs c ON c.owner_team_id = m.team_id",
        )
        .fetch_all(pool),
        sqlx::query_scalar::<_, String>("SELECT id FROM users WHERE is_admin").fetch_all(pool),
    )?;
    let mut gates = AccessGates { admins: admins.into_iter().collect(), ..AccessGates::default() };
    for (user_id, catalog) in members {
        // `entry(..).or_default()`: Python's `setdefault(key, set())`.
        gates.members.entry(user_id).or_default().insert(catalog);
    }
    Ok(gates)
}
```

- [ ] **Step 4: Noting admissions** (`src/store/audit.rs`, `src/mcp/context.rs`, `src/app.rs`)

```rust
/// What a recorded access did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccessKind {
    /// Read contents.
    Read,
    /// Wrote to a changeset.
    Write,
    /// Exported.
    Export,
}

/// Counts one admin read in its hour's `catalog.admin_read` entry, creating
/// the entry on the hour's first read. Exact under concurrency: the counter
/// row is upserted, so a second first read waits for the first to commit
/// and then increments.
///
/// # Arguments
/// * `pool` - the database.
/// * `actor` - the reading credential.
/// * `catalog` - the catalog read.
/// * `now` - the server clock.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn record_admin_read(
    pool: &PgPool,
    actor: &AuditActor,
    catalog: &str,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    // `DurationRound::duration_trunc`: the start of the clock hour.
    let hour = now.duration_trunc(TimeDelta::hours(1)).unwrap_or(now);
    let who = actor
        .credential
        .clone()
        .or_else(|| actor.user_id.as_ref().map(|id| format!("user:{id}")))
        .unwrap_or_else(|| actor.label.clone());
    // U+001F (unit separator) cannot occur in an id or a catalog name.
    let bucket = format!("{who}\u{1f}{catalog}\u{1f}{}", hour.timestamp());
    let mut tx = pool.begin().await?;
    let entry_id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO audit_access_counts (bucket, requests) VALUES ($1, 1) \
         ON CONFLICT (bucket) DO UPDATE SET requests = audit_access_counts.requests + 1 \
         RETURNING entry_id",
    )
    .bind(&bucket)
    .fetch_one(&mut *tx)
    .await?;
    if entry_id.is_none() {
        let catalog_id: Option<i64> = sqlx::query_scalar("SELECT id FROM catalogs WHERE name = $1")
            .bind(catalog)
            .fetch_optional(&mut *tx)
            .await?;
        let teams = match catalog_id {
            Some(id) => catalog_teams(&mut tx, id).await?,
            None => Vec::new(),
        };
        let id = record(
            &mut tx,
            actor,
            &Entry {
                action: Action::CatalogAdminRead,
                target: Target::Catalog(catalog),
                person: None,
                teams: &teams,
                details: serde_json::json!({"hour": crate::clock::format_timestamp(hour)}),
            },
            now,
        )
        .await?;
        sqlx::query("UPDATE audit_access_counts SET entry_id = $2 WHERE bucket = $1")
            .bind(&bucket)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}
```

`ToolContext` gains `noted: std::sync::Mutex<HashSet<(String, AccessKind)>>` and:

```rust
    /// Notes one access to `catalog`'s contents for the audit log: at most
    /// once per call, catalog and kind (one request, one count), and only
    /// for an admission that is recorded. The write runs in the background
    /// ([`App::note_access`]), off the request path.
    pub(crate) fn admit(&self, catalog: &str, kind: AccessKind) {
        let access = if kind == AccessKind::Write { Access::Write } else { Access::Read };
        let Ok(admission) = self.principal.admission(catalog, access, self.app.clock().now()) else {
            return;
        };
        if !admission.is_recorded() {
            return;
        }
        let fresh = self
            .noted
            .lock()
            .expect("noted lock poisoned")
            .insert((catalog.to_string(), kind));
        if fresh {
            self.app.note_access(self.audit_actor(), catalog.to_string(), kind, admission);
        }
    }
```

`select` calls `self.admit(name, AccessKind::Read)` for each catalog of a successful selection; `changeset` calls `self.admit(state.name(), if access == Access::Write { AccessKind::Write } else { AccessKind::Read })` after its `require`. The REST revision route (`browse.rs:328`) calls `ctx.admit(&catalog, AccessKind::Read)` after building its context.

`src/app.rs`:

```rust
    // Background writes of recorded accesses (admin reads, unlock tallies).
    // A `JoinSet` so tests and shutdown can wait for them.
    access_writes: std::sync::Mutex<tokio::task::JoinSet<()>>,
```

```rust
    /// Records one operator access in the background: an admin-read bucket
    /// (Task 9b: also an unlock's tally). A failure is logged.
    ///
    /// # Arguments
    /// * `actor` - who.
    /// * `catalog` - which catalog.
    /// * `kind` - read, write or export.
    /// * `admission` - how it was let in.
    pub fn note_access(&self, actor: AuditActor, catalog: String, kind: AccessKind, admission: Admission) {
        let pool = self.pool.clone();
        let now = self.clock.now();
        let mut writes = self.access_writes.lock().expect("access writes lock poisoned");
        // Reap the finished ones, so the set holds only running writes.
        while writes.try_join_next().is_some() {}
        writes.spawn(async move {
            let written = match admission {
                Admission::AdminRead => audit::record_admin_read(&pool, &actor, &catalog, now).await,
                Admission::Customer | Admission::Untracked => Ok(()),
            };
            if let Err(error) = written {
                tracing::error!(%error, %catalog, ?kind, "recording an operator access failed");
            }
        });
    }

    /// Waits for every background access write started so far (tests, and
    /// shutdown, so a stopping server loses no count).
    pub async fn settle_access(&self) {
        // Take the set out under the lock, then wait without holding it.
        let mut writes = std::mem::take(&mut *self.access_writes.lock().expect("access writes lock poisoned"));
        while writes.join_next().await.is_some() {}
    }
```

`http::serve` awaits `app.settle_access()` after the server stops.

- [ ] **Step 5: Exports**

`store::export::export(pool, catalog, out, audit: &AuditActor, now)`: after the read transaction commits and before compressing, record `catalog.exported` in a short transaction of its own, `{"generation", "files": rows.len()}`, teams `catalog_teams`. A failed record fails the export (no archive without its entry). The REST handler takes a `requester: Requester` argument and passes `&AuditActor::of(&principal, &requester)`; `Command::Export` passes `&AuditActor::cli()`.

- [ ] **Step 6: Run, gate, commit**

Run the narrow tests, `tools/agentic/gate.sh test`, `fmt`, `lint`. `docs/operations.md`'s audit section: exports, admin reads (one entry per credential, catalog and clock hour, with an exact request count, for an admin credential reading a catalog outside its user's teams; a key with no user always counts).

```bash
git add src docs/operations.md changelog/
git commit -m "feat(audit): access gates; record every export and operators' reads, bucketed hourly (M14 task 7b)" \
           -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8a: Reading the log over REST

**Files:**
- Create: `src/http/api/audit.rs`
- Modify: `src/store/audit.rs` (`Viewer`, `AuditQuery`, `AuditRecord`, `ActorView`, `list`, `team_ref_for_reading`), `src/store/teams.rs` (`owned_by`, `owned_names`), `src/http/api/mod.rs` (`mod audit`, merge its routes), `src/http/api/me.rs` (`AccountView.owned_teams`), `docs/operations.md` (API reference rows)

- [ ] **Step 1: Write the failing tests**

`src/http/api/audit.rs`'s tests:

```rust
#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use serde_json::json;

    use crate::db::{TEST_TEAM, TEST_USER};
    use crate::http::api::testing::{app, every_catalog_admin, keys, send, Browser, Payload};

    #[tokio::test(flavor = "multi_thread")]
    async fn an_admin_reads_everything_newest_first_with_filters_and_a_cursor() {
        let test = app().await;
        let root = every_catalog_admin(&test).await;
        for name in ["alice", "bob", "carol"] {
            send(&test, "POST", "/api/v1/users", Some(&root), Some(json!({"name": name}))).await;
        }

        let (status, page) = send(&test, "GET", "/api/v1/audit?action=user.created&limit=2", Some(&root), None).await;
        let after = page["next_after"].as_i64().unwrap();
        let (_, rest) = send(&test, "GET", &format!("/api/v1/audit?action=user.created&after={after}"), Some(&root), None).await;
        let (bad, _) = send(&test, "GET", "/api/v1/audit?action=no.such", Some(&root), None).await;

        assert_eq!(status, StatusCode::OK);
        let names: Vec<&str> = page["entries"].as_array().unwrap().iter()
            .map(|entry| entry["target"]["person"].as_str().unwrap()).collect();
        assert_eq!(names, ["carol", "bob"]);
        assert_eq!(rest["entries"][0]["target"]["person"], "alice");
        assert_eq!(page["entries"][0]["actor"]["credential"].as_str().is_some(), true);
        assert_eq!(bad, StatusCode::BAD_REQUEST);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_owner_session_sees_its_team_with_operators_redacted_and_a_member_sees_nothing() {
        let test = app().await;
        let root = every_catalog_admin(&test).await;
        send(&test, "POST", "/api/v1/users", Some(&root), Some(json!({"name": "bob"}))).await;
        send(&test, "POST", "/api/v1/teams", Some(&root), Some(json!({"name": "family", "owner": TEST_USER}))).await;
        send(&test, "PUT", "/api/v1/teams/family/members/bob", Some(&root), Some(json!({"role": "member"}))).await;
        send(&test, "POST", "/api/v1/teams", Some(&root), Some(json!({"name": "other", "owner": "bob"}))).await;
        let mut owner = Browser::session_for(&test, TEST_USER).await;
        let mut member = Browser::session_for(&test, "bob").await;

        let mine = owner.send(&test, "GET", "/api/v1/audit?team=family", Payload::Empty).await;
        let not_mine = owner.send(&test, "GET", "/api/v1/audit?team=other", Payload::Empty).await;
        let as_member = member.send(&test, "GET", "/api/v1/audit?team=family", Payload::Empty).await;
        let scoped = keys(&test).await;
        let (by_key, _) = send(&test, "GET", "/api/v1/audit?team=family", Some(&scoped.admin), None).await;

        let (status, body) = mine.parts();
        assert_eq!(status, StatusCode::OK, "{body}");
        let actions: Vec<&str> = body["entries"].as_array().unwrap().iter()
            .map(|entry| entry["action"].as_str().unwrap()).collect();
        assert_eq!(actions, ["team.member_added", "team.created"], "only family's entries");
        assert_eq!(body["entries"][0]["actor"], json!({"kind": "operator"}));
        assert_eq!(body["entries"][0]["ip"], serde_json::Value::Null);
        assert_eq!(not_mine.status, StatusCode::FORBIDDEN);
        assert_eq!(as_member.status, StatusCode::FORBIDDEN);
        assert_eq!(by_key, StatusCode::FORBIDDEN, "owners read through a console session only");
        let _ = TEST_TEAM;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_provisioner_reads_only_its_own_entries() {
        let test = app().await;
        let root = every_catalog_admin(&test).await;
        let provisioner = crate::store::keys::create_provisioner(
            test.app.pool(),
            &crate::store::keys::NewProvisioner { name: "signup", actor: "signup", user: TEST_USER },
            &crate::store::audit::AuditActor::cli(),
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        test.app.poll().await.unwrap();
        send(&test, "POST", "/api/v1/users", Some(&root), Some(json!({"name": "by-root"}))).await;
        send(&test, "POST", "/api/v1/users", Some(&provisioner.key), Some(json!({"name": "by-signup"}))).await;

        let (status, page) = send(&test, "GET", "/api/v1/audit?action=user.created", Some(&provisioner.key), None).await;

        assert_eq!(status, StatusCode::OK);
        let entries = page["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["target"]["person"], "by-signup");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_export_is_json_lines_with_the_same_filters() {
        let test = app().await;
        let root = every_catalog_admin(&test).await;
        send(&test, "POST", "/api/v1/users", Some(&root), Some(json!({"name": "alice"}))).await;
        let request = axum::http::Request::builder()
            .uri("/api/v1/audit/export?action=user.created")
            .header("authorization", format!("Bearer {root}"))
            .body(axum::body::Body::empty())
            .unwrap();

        // `oneshot` sends one request through the whole router (auth included).
        let response = tower::ServiceExt::oneshot(
            crate::http::router(std::sync::Arc::clone(&test.app), &tokio_util::sync::CancellationToken::new()),
            request,
        )
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "application/x-ndjson");
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let lines: Vec<serde_json::Value> = std::str::from_utf8(&bytes).unwrap().lines()
            .map(|line| serde_json::from_str(line).unwrap()).collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["action"], "user.created");
    }
}
```

(The owner test's actor is `root`, an admin credential, so the owner sees "an operator".)

And a store test of the redaction rules (`src/store/audit.rs`):

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn owners_never_see_an_operators_identity_ip_or_unlock_reason() {
        let db = test_db().await;
        let me = user_id(&db.pool, TEST_USER).await;
        let mut tx = db.pool.begin().await.unwrap();
        let team = team_ref(&mut tx, TEST_TEAM).await.unwrap();
        record(
            &mut tx,
            &admin_actor(&me),
            &Entry {
                action: Action::CatalogUnlocked,
                target: Target::Catalog("main"),
                person: Some(&me),
                teams: std::slice::from_ref(&team),
                details: serde_json::json!({"category": "support", "reason": "ticket 12", "unlock": 1}),
            },
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let team_id: String = sqlx::query_scalar("SELECT id FROM teams WHERE name = $1")
            .bind(TEST_TEAM)
            .fetch_one(&db.pool)
            .await
            .unwrap();

        let owner = list(&db.pool, &Viewer::Owner { team_id, team_ref: team.clone() }, &AuditQuery::default())
            .await
            .unwrap();
        let admin = list(&db.pool, &Viewer::Admin, &AuditQuery::default()).await.unwrap();

        assert_eq!(owner[0].actor, ActorView::Operator);
        assert_eq!(owner[0].ip, None);
        assert_eq!(owner[0].details, serde_json::json!({"category": "support", "unlock": 1}));
        assert_eq!(admin[0].details["reason"], "ticket 12");
        assert_eq!(admin[0].ip.as_deref(), Some("192.0.2.7"));
        assert_eq!(admin[0].teams, [TEST_USER], "a personal team reads as its user");
    }
```

- [ ] **Step 2: Run them and see them fail**

Run: `tools/agentic/gate.sh narrow --lib http::api::audit::tests`, then `tools/agentic/gate.sh narrow --lib store::audit::tests::owners_never`
Expected: compile errors; then 404 for the routes.

- [ ] **Step 3: Implement the reader** (`src/store/audit.rs`)

```rust
/// Who is reading the log, which decides what they see (spec "Who reads it").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Viewer {
    /// An every-catalog admin or the CLI: everything, unredacted.
    Admin,
    /// An owner of one team, through a console session: that team's
    /// entries, operators redacted.
    Owner {
        /// The team's `teams.id` (for the member-IP rule).
        team_id: String,
        /// Its reference as entries store it ([`team_ref`]).
        team_ref: String,
    },
    /// A provisioner: only the entries it wrote.
    Credential(String),
}

/// The filters of `GET /audit` and `audit list` (all optional).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditQuery<'a> {
    /// An admin's team filter, by team name.
    pub team: Option<&'a str>,
    /// One action.
    pub action: Option<Action>,
    /// A person's name, a credential id or an actor label.
    pub actor: Option<&'a str>,
    /// A target name, or a target person's name.
    pub target: Option<&'a str>,
    /// From this time on.
    pub since: Option<DateTime<Utc>>,
    /// Before this time.
    pub until: Option<DateTime<Utc>>,
    /// Older than this entry id (the cursor).
    pub after: Option<i64>,
    /// At most this many (1 to 500; 100 when 0).
    pub limit: i64,
}

/// Who an entry shows as acting.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActorView {
    /// An admin credential or a provisioner, as a team owner sees it.
    Operator,
    /// Anyone else, or an operator as an admin sees it.
    Known {
        /// The person, if the actor was one (`deleted user #N` once deleted).
        person: Option<String>,
        /// The key or grant id.
        credential: Option<String>,
        /// The actor label.
        label: String,
        /// Whether it was an operator credential.
        operator: bool,
    },
}

/// One entry as a reader sees it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AuditRecord {
    /// Its id (the cursor).
    pub id: i64,
    /// When (`2026-11-13T12:00:00Z`, `format_timestamp`).
    pub at: String,
    /// The action.
    pub action: String,
    /// `cli`, `api`, `console` or `system`.
    pub source: String,
    /// Who.
    pub actor: ActorView,
    /// The client address, when the reader may see it.
    pub ip: Option<String>,
    /// What: `{kind, name, person}`.
    pub target: TargetView,
    /// The teams it belongs to, personal teams shown as their user.
    pub teams: Vec<String>,
    /// Its details, redacted for owners.
    pub details: serde_json::Value,
    /// For `catalog.admin_read`: the request count.
    pub requests: Option<i64>,
}

/// An entry's target as a reader sees it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TargetView {
    /// `user`, `team`, `catalog`, `key`, `grant`, `code`, `setting` or `unlock`.
    pub kind: String,
    /// Its name (`None` for a user).
    pub name: Option<String>,
    /// The person it is about, if any.
    pub person: Option<String>,
}

/// The `details` keys a team owner never sees, per action.
const OWNER_REDACTED: &[(&str, &[&str])] = &[("catalog.unlocked", &["reason"])];

/// The entries a viewer may read, newest first.
///
/// # Arguments
/// * `pool` - the database.
/// * `viewer` - who reads.
/// * `query` - the filters.
///
/// # Returns
/// At most `query.limit` records.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn list(pool: &PgPool, viewer: &Viewer, query: &AuditQuery<'_>) -> Result<Vec<AuditRecord>, OkfError> {
    let (team, team_id, credential, owner) = match viewer {
        Viewer::Admin => {
            let mut conn = pool.acquire().await?;
            let team = match query.team {
                Some(name) => Some(team_ref_for_reading(&mut conn, name).await?),
                None => None,
            };
            (team, None, None, false)
        }
        Viewer::Owner { team_id, team_ref } => (Some(team_ref.clone()), Some(team_id.as_str()), None, true),
        Viewer::Credential(id) => (None, None, Some(id.as_str()), false),
    };
    let limit = if query.limit <= 0 { 100 } else { query.limit.min(500) };
    let rows = sqlx::query_as::<_, ListedRow>(LIST_SQL)
        .bind(team)
        .bind(query.action.map(Action::as_str))
        .bind(query.actor)
        .bind(query.target)
        .bind(query.since)
        .bind(query.until)
        .bind(query.after)
        .bind(credential)
        .bind(team_id)
        .bind(owner)
        .bind(limit)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(|row| row.into_record(owner)).collect())
}

/// The listing. `$1` team reference, `$2` action, `$3` actor, `$4` target,
/// `$5` since, `$6` until, `$7` cursor, `$8` credential, `$9` the owner's
/// team id, `$10` owner view, `$11` limit.
const LIST_SQL: &str = "\
    SELECT e.id, e.at, e.action, e.source, e.actor_operator, e.actor_credential, e.actor_label, \
           ap.name AS actor_person, host(e.client_ip) AS ip, e.target_kind, e.target_name, \
           tp.name AS target_person, \
           ARRAY(SELECT CASE WHEN r LIKE 'person:%' \
                             THEN COALESCE((SELECT p.name FROM audit_people p \
                                            WHERE p.id = substr(r, 8)::bigint), r) \
                             ELSE r END \
                 FROM unnest(e.teams) AS r) AS teams, \
           e.details, c.requests, \
           ($9::text IS NOT NULL AND EXISTS ( \
               SELECT 1 FROM audit_people p JOIN team_members m ON m.user_id = p.user_id \
               WHERE p.id = e.actor_person_id AND m.team_id = $9)) AS actor_in_team \
    FROM audit_entries e \
    LEFT JOIN audit_people ap ON ap.id = e.actor_person_id \
    LEFT JOIN audit_people tp ON tp.id = e.target_person_id \
    LEFT JOIN audit_access_counts c ON c.entry_id = e.id \
    WHERE ($1::text IS NULL OR e.teams @> ARRAY[$1::text]) \
      AND ($2::text IS NULL OR e.action = $2) \
      AND ($3::text IS NULL OR ((NOT $10 OR NOT e.actor_operator) \
           AND (ap.name = $3 OR e.actor_credential = $3 OR e.actor_label = $3))) \
      AND ($4::text IS NULL OR e.target_name = $4 OR tp.name = $4) \
      AND ($5::timestamptz IS NULL OR e.at >= $5) \
      AND ($6::timestamptz IS NULL OR e.at < $6) \
      AND ($7::bigint IS NULL OR e.id < $7) \
      AND ($8::text IS NULL OR e.actor_credential = $8) \
    ORDER BY e.id DESC LIMIT $11";
```

`ListedRow` is the `sqlx::FromRow` of those columns; `into_record(owner)`:

```rust
impl ListedRow {
    /// The record a reader sees. For an owner: an operator becomes
    /// [`ActorView::Operator`] with no IP, other IPs are shown only for the
    /// team's own members, and [`OWNER_REDACTED`] keys leave `details`. The
    /// stored entry is never changed.
    fn into_record(self, owner: bool) -> AuditRecord {
        let operator_hidden = owner && self.actor_operator;
        let actor = if operator_hidden {
            ActorView::Operator
        } else {
            ActorView::Known {
                person: self.actor_person,
                credential: self.actor_credential,
                label: self.actor_label,
                operator: self.actor_operator,
            }
        };
        let ip = if owner && (self.actor_operator || !self.actor_in_team) { None } else { self.ip };
        let mut details = self.details;
        if owner {
            if let Some((_, keys)) = OWNER_REDACTED.iter().find(|(action, _)| *action == self.action) {
                if let Some(object) = details.as_object_mut() {
                    for key in *keys {
                        object.remove(*key);
                    }
                }
            }
        }
        AuditRecord {
            id: self.id,
            at: crate::clock::format_timestamp(self.at),
            action: self.action,
            source: self.source,
            actor,
            ip,
            target: TargetView { kind: self.target_kind, name: self.target_name, person: self.target_person },
            teams: self.teams,
            details,
            requests: self.requests,
        }
    }
}
```

`team_ref_for_reading(conn, name)`: the reference without creating a person: `SELECT CASE WHEN personal_of IS NULL THEN name ELSE 'person:' || (SELECT id FROM audit_people WHERE user_id = t.personal_of)::text END FROM teams t WHERE t.name = $1`; an unknown team falls back to the name itself (a deleted shared team's entries keep its name).

`src/store/teams.rs`:

```rust
/// The team `team` if `user_id` owns it: its id and its audit reference.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn owned_by(pool: &PgPool, user_id: &str, team: &str) -> Result<Option<(String, String)>, OkfError> {
    Ok(sqlx::query_as(
        "SELECT t.id, audit_team_ref(t.id) FROM teams t \
         JOIN team_members m ON m.team_id = t.id \
         WHERE t.name = $1 AND m.user_id = $2 AND m.role = 'owner'",
    )
    .bind(team)
    .bind(user_id)
    .fetch_optional(pool)
    .await?)
}

/// The names of the teams `user_id` owns, sorted (the console's team picker).
///
/// # Errors
/// [`OkfError::Database`].
pub async fn owned_names(pool: &PgPool, user_id: &str) -> Result<Vec<String>, OkfError>
```

- [ ] **Step 4: The routes** (`src/http/api/audit.rs`)

```rust
/// `GET /audit` and `/audit/export`'s query.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuditParams {
    team: Option<String>,
    action: Option<String>,
    actor: Option<String>,
    target: Option<String>,
    since: Option<String>,
    until: Option<String>,
    after: Option<i64>,
    limit: Option<i64>,
}

/// `GET /audit`'s answer.
#[derive(Debug, Serialize)]
pub(crate) struct AuditPage {
    /// Newest first.
    pub entries: Vec<AuditRecord>,
    /// The cursor for the next page (`?after=`), or null on the last page.
    pub next_after: Option<i64>,
    /// Every action name, for the console's filter.
    pub actions: Vec<&'static str>,
}

/// This area's routes.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/audit", get(list_audit))
        .route("/audit/export", get(export_audit))
}

/// Who may read, as what (spec "Who reads it, and how"): a provisioner its
/// own entries; an every-catalog admin everything; a team's owner, through
/// a console session and naming the team, that team's entries.
///
/// # Errors
/// [`OkfError::Forbidden`] for anyone else.
async fn viewer(app: &App, principal: &Principal, team: Option<&str>) -> Result<Viewer, OkfError> {
    if principal.provisioner {
        return Ok(Viewer::Credential(principal.key_id.clone()));
    }
    if require_every_catalog(principal).is_ok() {
        return Ok(Viewer::Admin);
    }
    let refused = || OkfError::Forbidden {
        reason: "the audit log needs an admin, or a team owner's console session with ?team=".to_string(),
    };
    let (CredentialKind::Session { user_id }, Some(team)) = (&principal.kind, team) else {
        return Err(refused());
    };
    let (team_id, team_ref) = teams::owned_by(app.pool(), user_id, team).await?.ok_or_else(refused)?;
    Ok(Viewer::Owner { team_id, team_ref })
}

/// Parses the query into the store's filters.
///
/// # Errors
/// [`OkfError::InvalidPath`] (400) for an unknown action or a time that is
/// not RFC 3339.
fn query_of(params: &AuditParams) -> Result<AuditQuery<'_>, OkfError> {
    let time = |name: &str, text: &Option<String>| {
        text.as_deref()
            .map(|text| {
                DateTime::parse_from_rfc3339(text)
                    .map(|time| time.with_timezone(&Utc))
                    .map_err(|_| OkfError::InvalidPath {
                        path: name.to_string(),
                        reason: format!("{text:?} is not an RFC 3339 time"),
                    })
            })
            .transpose()
    };
    let action = params
        .action
        .as_deref()
        .map(|name| {
            Action::parse(name).ok_or_else(|| OkfError::InvalidPath {
                path: "action".to_string(),
                reason: format!("{name:?} is not an audit action"),
            })
        })
        .transpose()?;
    Ok(AuditQuery {
        team: params.team.as_deref(),
        action,
        actor: params.actor.as_deref(),
        target: params.target.as_deref(),
        since: time("since", &params.since)?,
        until: time("until", &params.until)?,
        after: params.after,
        limit: params.limit.unwrap_or(100),
    })
}

/// `GET /audit`: entries newest first, cursor-paged.
///
/// # Errors
/// 403 for a caller that may not read (see [`viewer`]); 400 for a bad filter.
async fn list_audit(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Query(params): Query<AuditParams>,
) -> Result<Json<AuditPage>, AppError> {
    let viewer = viewer(&app, &principal, params.team.as_deref()).await?;
    let query = query_of(&params)?;
    let entries = audit::list(app.pool(), &viewer, &query).await?;
    // A full page may have more after it.
    let next_after = (entries.len() as i64 >= query.limit.clamp(1, 500))
        .then(|| entries.last().map(|entry| entry.id))
        .flatten();
    Ok(Json(AuditPage {
        entries,
        next_after,
        actions: Action::ALL.iter().map(|action| action.as_str()).collect(),
    }))
}

/// `GET /audit/export`: every entry the filters select, as JSON Lines (one
/// record per line, newest first), for a compliance request.
///
/// # Errors
/// As [`list_audit`].
async fn export_audit(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Query(params): Query<AuditParams>,
) -> Result<Response, AppError> {
    let viewer = viewer(&app, &principal, params.team.as_deref()).await?;
    let mut query = query_of(&params)?;
    query.limit = 500;
    let mut body = Vec::new();
    // Page through with the cursor; retention bounds the total.
    loop {
        let page = audit::list(app.pool(), &viewer, &query).await?;
        for record in &page {
            serde_json::to_writer(&mut body, record).map_err(|error| OkfError::Io {
                path: "<audit export>".to_string(),
                source: error.into(),
            })?;
            body.push(b'\n');
        }
        match page.last() {
            Some(last) if page.len() == 500 => query.after = Some(last.id),
            _ => break,
        }
    }
    let headers = [
        (header::CONTENT_TYPE, "application/x-ndjson"),
        (header::CONTENT_DISPOSITION, "attachment; filename=\"audit.jsonl\""),
    ];
    Ok((headers, body).into_response())
}
```

`src/http/api/me.rs`: `AccountView.owned_teams: Vec<String>` (doc: "The teams the user owns: whose audit log its console session can read"), filled with `teams::owned_names`.

`docs/operations.md` API reference, a new "### Audit API" table: `GET /api/v1/audit` and `GET /api/v1/audit/export` (who, filters, paging, redaction, the JSON shape of an entry).

- [ ] **Step 5: Run, gate, commit**

```bash
git add src docs/operations.md changelog/
git commit -m "feat(api): read and export the audit log; owners see their team, operators redacted (M14 task 8a)" \
           -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8b: `okf-mcp audit list` and `audit export`

**Files:**
- Create: `src/cli/audit.rs`
- Modify: `src/cli.rs` (`Command::Audit(AuditCommand)`, dispatch), `tests/admin_parity.rs`, `docs/operations.md` (commands, parity rows)

- [ ] **Step 1: Write the failing tests**

`tests/admin_parity.rs`: `"audit list"`, `"audit export"` in `COVERED`; `"audit"` in `SCANNED_GROUPS`.

`src/cli/audit.rs`'s tests:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn audit_list_prints_one_line_per_entry_and_export_prints_json_lines() {
        let db = crate::db::test_db().await;
        crate::store::users::create(&db.pool, &crate::store::users::NewUser { name: "alice", display_name: "" },
                                    &AuditActor::cli(), now()).await.unwrap();
        let filters = AuditArgs { action: Some("user.created".into()), ..AuditArgs::default() };

        let listed = audit(&db.pool, AuditCommand::List(filters.clone())).await.unwrap();
        let exported = audit(&db.pool, AuditCommand::Export(filters)).await.unwrap();

        let line = listed.lines().next().unwrap();
        assert!(line.contains("\tuser.created\t"), "{line}");
        assert!(line.contains("user alice"), "{line}");
        let record: serde_json::Value = serde_json::from_str(exported.lines().next().unwrap()).unwrap();
        assert_eq!(record["source"], "cli");
    }
```

- [ ] **Step 2: Run them and see them fail**

Run: `tools/agentic/gate.sh narrow --test admin_parity`, then `tools/agentic/gate.sh narrow --lib cli::audit::tests`
Expected: "`okf-mcp audit list` is not a subcommand"; compile errors.

- [ ] **Step 3: Implement**

`src/cli.rs`:

```rust
    /// Read the audit log: administrative changes, catalog writes, exports
    /// and operators' reads.
    #[command(subcommand)]
    Audit(AuditCommand),
```

```rust
/// `okf-mcp audit ...`.
#[derive(Debug, Subcommand)]
pub enum AuditCommand {
    /// Entries, newest first, one tab-separated line each.
    List(AuditArgs),
    /// Every matching entry as JSON Lines on stdout.
    Export(AuditArgs),
}

/// The audit filters, as `GET /api/v1/audit` takes them.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct AuditArgs {
    /// Only entries of this team.
    #[arg(long)]
    pub team: Option<String>,
    /// Only this action (`user.created`, ...).
    #[arg(long)]
    pub action: Option<String>,
    /// Only this actor: a person, a credential id or an actor label.
    #[arg(long)]
    pub actor: Option<String>,
    /// Only this target: a name or a person.
    #[arg(long)]
    pub target: Option<String>,
    /// From this RFC 3339 time.
    #[arg(long)]
    pub since: Option<String>,
    /// Before this RFC 3339 time.
    #[arg(long)]
    pub until: Option<String>,
    /// Older than this entry id.
    #[arg(long)]
    pub after: Option<i64>,
    /// At most this many (`list` only; default 100, at most 500).
    #[arg(long, default_value_t = 100)]
    pub limit: i64,
}
```

`src/cli/audit.rs`: `pub async fn audit(pool: &PgPool, command: AuditCommand) -> anyhow::Result<String>` builds an `AuditQuery` (parsing as `query_of` does; share the parse by moving `query_of`'s action and time parsing into `store::audit::parse_filters`) and calls `audit::list(pool, &Viewer::Admin, ..)` (the CLI has operator access). `List` prints `{id}\t{at}\t{action}\t{actor}\t{target}\t{teams}\t{details}` where `actor` is `person NAME`, else `key ID`, else the label, and `target` is `user NAME` or `KIND NAME`; `Export` pages through like `export_audit` and prints JSON Lines.

`docs/operations.md`: the commands in the audit section, and parity rows

```markdown
| `okf-mcp audit list` | `GET /api/v1/audit` | Audit |
| `okf-mcp audit export` | `GET /api/v1/audit/export` | Audit → Export |
```

- [ ] **Step 4: Run, gate, commit**

```bash
git add src/cli.rs src/cli/audit.rs src/store/audit.rs src/http/api/audit.rs tests/admin_parity.rs \
        docs/operations.md changelog/
git commit -m "feat(cli): audit list and audit export, at parity (M14 task 8b)" \
           -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8c: The console's Audit page

**Files:**
- Create: `src/http/ui/js/pages/audit.js`, `tests/ui_e2e/audit.rs`
- Modify: `src/http/ui/index.html` (nav link), `src/http/ui.rs` (manifest), `src/http/ui/js/core/api.js` (`download`), `tests/ui_e2e/main.rs` (`mod audit;`)

- [ ] **Step 1: Write the failing journeys** (`tests/ui_e2e/audit.rs`)

```rust
//! The Audit page (M14 Task 8c): an admin reads and filters everything and
//! exports it; a team owner picks one of its teams and sees operators as
//! "an operator".

use okf_mcp::config::Config;
use okf_mcp::store::keys::Access;
use serde_json::json;

use crate::harness::drive;
use crate::support::server::{files, TestServer, OWNER};

async fn server() -> TestServer {
    TestServer::start(vec![("main", files(&[("index.md", "# Main\n")]))], Config::default()).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admin_filters_the_log_by_action_and_exports_it() {
    let server = server().await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.open("/users");
        ui.click("#user-new");
        ui.fill("#user-form input[name=name]", "alice");
        ui.click("#user-form button[type=submit]");
        ui.wait_visible("[data-test=user-alice]");
        ui.open("/audit");
        ui.select("#audit-action", "user.created");
        ui.wait_text("#audit-table", "alice");
        ui.wait_text_gone("#audit-table", "catalog.created");
        ui.hold_next("GET", "/api/v1/audit/export?action=user.created");
        ui.click("#audit-export");
        assert_eq!(ui.held_matches(), 1);
        ui.release_held();
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_owner_reads_its_team_and_sees_an_operator() {
    let server = server().await;
    let root = server.key("console-admin", Access::Admin, &[], None).await;
    server.user_with_password("olive", "olive@example.com", "a long passphrase 1").await;
    // An operator (an admin key) adds a member to olive's team.
    let client = reqwest::Client::new();
    for (method, path, body) in [
        ("POST", "/api/v1/teams", json!({"name": "family", "owner": "olive"})),
        ("PUT", &format!("/api/v1/teams/family/members/{OWNER}") as &str, json!({"role": "member"})),
    ] {
        client
            .request(method.parse().unwrap(), format!("{}{path}", server.base))
            .bearer_auth(&root)
            .json(&body)
            .send()
            .await
            .unwrap();
    }
    drive(server.base.clone(), move |ui| {
        ui.sign_in_password("olive@example.com", "a long passphrase 1");
        ui.open("/audit");
        ui.select("#audit-team", "family");
        ui.wait_text("#audit-table", "team.member_added");
        ui.wait_text("#audit-table", "an operator");
        ui.wait_text_gone("#audit-table", "console-admin");
    })
    .await;
}
```

(If the harness has no `hold_next`/`held_matches` for a GET, assert the export through `ui.eval` on a spied `fetch` instead; `reqwest` is already a dev-dependency of the server tests. Use the password rules `store::passwords` enforces.)

- [ ] **Step 2: Run them and see them fail**

Run: `tools/agentic/gate.sh test-ui`
Expected: both time out on `#audit-action` / `#audit-team`.

- [ ] **Step 3: Implement**

`api.js`:

```js
// Downloads a GET answer as a file with the console's own credential (the
// bearer key or the session cookie), which a plain link cannot carry.
export async function download(path, filename) {
    const response = await request('GET', path, undefined, { raw: true });
    const url = URL.createObjectURL(await response.blob());
    const link = h('a', { href: url, download: filename });
    document.body.append(link);
    link.click();
    link.remove();
    URL.revokeObjectURL(url);
}
```

(Reuse the module's own request function and its error handling; `raw` returns the `Response` without parsing JSON.)

`pages/audit.js`: the page head with, for an admin (`store.whoami` access `admin` reaching every catalog), a team text filter; for a non-admin console session, `#audit-team`, a `select` of `/me`'s `owned_teams` (the page says "You own no team; the audit log shows your teams' entries to their owners" when empty, through `fill()`); `#audit-action` (from the answer's `actions`), actor and target inputs, since and until (`datetime-local`, sent as UTC RFC 3339), and `#audit-export` calling `api.download('/audit/export?' + params, 'audit.jsonl')`. The table `#audit-table` (`listTable`) shows When (`ago`), Action, Actor (`an operator` for `{kind: "operator"}`; else the person, the key id or the label), Target (`user NAME` or `KIND NAME`), Teams, and Details (`JSON.stringify`, `mono small`), with "Load more" using `next_after`. A `catalog.admin_read` row shows `N requests`; a `catalog.maintenance_run` row reads "maintenance ran". Sign-ins are not in the log (they are in `sign_in_events`): for an admin, a `user.*` row's person links to that user's drawer, whose Recent sign-ins lists them. Every conditional cell goes through `h()`.

`index.html`: an `Audit` link (`data-nav="audit"`, `href="#/audit"`) under the admin links, shown to admins and to sessions that own a team (`shell.js` hides it otherwise); `ui.rs`'s manifest lists `audit`.

- [ ] **Step 4: Run, gate, commit**

```bash
git add src/http/ui src/http/ui.rs tests/ui_e2e changelog/
git commit -m "feat(console): the Audit page for admins and team owners (M14 task 8c)" \
           -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 9a: Unlock storage, settings, errors and the owner email

**Files:**
- Create: `migrations/0015_unlocks.sql`, `src/store/unlocks.rs`
- Modify: `src/store/mod.rs`, `src/store/settings.rs`, `src/error.rs`, `src/http/mod.rs` (status), `src/mcp/mod.rs` (only if it matches variants), `src/mail/templates.rs`, `docs/operations.md` (Settings rows, error rows)

- [ ] **Step 1: Write the migration**

```sql
-- Unlock before access (spec Part 6). An unlock lets one admin user's
-- credentials into one catalog's contents, for a reason, until it expires
-- or is ended; it is never extended. The tallies count what was done
-- under it. The per-catalog maintenance flag lets an owner keep the
-- maintenance runner out (default on).
CREATE TABLE unlocks (
    id         bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    catalog_id bigint NOT NULL REFERENCES catalogs (id) ON DELETE CASCADE,
    -- NULL once the user is deleted; person_id keeps who it was.
    user_id    text REFERENCES users (id) ON DELETE SET NULL,
    person_id  bigint NOT NULL REFERENCES audit_people (id),
    category   text NOT NULL CHECK (category IN ('support', 'deletion_request', 'export_request',
                                                 'security_investigation', 'legal', 'other')),
    reason     text NOT NULL CHECK (char_length(btrim(reason)) BETWEEN 1 AND 500),
    created_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL CHECK (expires_at > created_at),
    ended_at   timestamptz CHECK (ended_at IS NULL OR ended_at >= created_at),
    reads      bigint NOT NULL DEFAULT 0 CHECK (reads >= 0),
    writes     bigint NOT NULL DEFAULT 0 CHECK (writes >= 0),
    exports    bigint NOT NULL DEFAULT 0 CHECK (exports >= 0)
);
-- The key cache loads the live ones at every reload.
CREATE INDEX unlocks_live ON unlocks (expires_at) WHERE ended_at IS NULL;
CREATE INDEX unlocks_catalog ON unlocks (catalog_id, created_at DESC);
CREATE INDEX unlocks_user_id ON unlocks (user_id) WHERE user_id IS NOT NULL;

ALTER TABLE catalogs ADD COLUMN maintenance boolean NOT NULL DEFAULT true;
```

- [ ] **Step 2: Write the failing tests**

`src/error.rs`:

```rust
    #[test]
    fn unlock_required_names_the_catalog_and_says_how() {
        let error = OkfError::UnlockRequired { catalog: "main".to_string(), has_user: true };
        let user_less = OkfError::UnlockRequired { catalog: "main".to_string(), has_user: false };

        let body = error.to_json();

        assert_eq!(body, serde_json::json!({
            "error": "UnlockRequired",
            "message": "catalog \"main\" needs an unlock for operator access: okf-mcp unlock create, \
                        POST /api/v1/unlocks, or Unlock in the console",
            "catalog": "main",
        }));
        assert!(user_less.to_string().contains("has no user"));
    }

    #[test]
    fn maintenance_off_is_a_forbidden_the_runner_can_tell_apart() {
        let body = OkfError::MaintenanceOff { catalog: "main".to_string() }.to_json();
        assert_eq!(body["error"], "Forbidden");
        assert_eq!(body["maintenance"], "off");
        assert_eq!(body["catalog"], "main");
    }
```

and in `src/http/mod.rs`: both are 403.

`src/store/unlocks.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{test_db, TEST_TEAM, TEST_USER};
    use crate::store::audit::{testing, AuditActor};
    use crate::store::catalogs::{self, NewCatalog};
    use crate::store::users::{self, AdminChange, NewUser};

    fn now() -> DateTime<Utc> {
        "2026-11-13T12:00:00Z".parse().unwrap()
    }

    fn policy() -> UnlockPolicy {
        UnlockPolicy { max_hours: 4, notify_owners: true }
    }

    async fn staff_and_main(pool: &PgPool) {
        users::create(pool, &NewUser { name: "staff", display_name: "" }, &AuditActor::cli(), now()).await.unwrap();
        users::set_admin(pool, "staff", AdminChange { on: true, allow_last: true }, &AuditActor::cli(), now())
            .await
            .unwrap();
        users::set_email(pool, TEST_USER, Some("owner@example.com"), &AuditActor::cli(), now()).await.unwrap();
        catalogs::create(pool, &NewCatalog { name: "main", description: "", owner_team: TEST_TEAM },
                         &AuditActor::cli(), now()).await.unwrap();
    }

    fn new(hours: u32) -> NewUnlock<'static> {
        NewUnlock { catalog: "main", user: "staff", category: Category::Support, reason: "ticket 12", hours }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unlock_is_recorded_mails_the_owners_and_can_be_ended_once() {
        let db = test_db().await;
        staff_and_main(&db.pool).await;

        let made = create(&db.pool, &new(2), &policy(), &AuditActor::cli(), now()).await.unwrap();
        let ended = end(&db.pool, made.id, &AuditActor::cli(), now() + TimeDelta::minutes(5)).await.unwrap();
        let again = end(&db.pool, made.id, &AuditActor::cli(), now() + TimeDelta::minutes(6)).await;

        assert_eq!(made.expires_at, now() + TimeDelta::hours(2));
        assert_eq!(made.category, Category::Support);
        assert_eq!(ended.ended_at, Some(now() + TimeDelta::minutes(5)));
        assert!(matches!(again, Err(OkfError::Refused { .. })));
        let unlocked = testing::entries(&db.pool, "catalog.unlocked").await;
        assert_eq!(unlocked[0].details["reason"], "ticket 12");
        assert_eq!(unlocked[0].details["category"], "support");
        assert_eq!(testing::entries(&db.pool, "catalog.unlock_ended").await.len(), 1);
        let mail = crate::mail::queue::list(&db.pool).await.unwrap();
        assert_eq!(mail.len(), 1);
        assert_eq!(mail[0].to, "owner@example.com");
        assert_eq!(mail[0].template, "unlocked");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unlock_is_for_an_admin_user_within_the_limit_with_a_reason() {
        let db = test_db().await;
        staff_and_main(&db.pool).await;

        let too_long = create(&db.pool, &new(5), &policy(), &AuditActor::cli(), now()).await;
        let blank = create(&db.pool, &NewUnlock { reason: "  ", ..new(1) }, &policy(), &AuditActor::cli(), now()).await;
        let not_admin = create(&db.pool, &NewUnlock { user: TEST_USER, ..new(1) }, &policy(), &AuditActor::cli(), now()).await;
        let quiet = create(&db.pool, &new(1), &UnlockPolicy { notify_owners: false, ..policy() },
                           &AuditActor::cli(), now()).await;

        assert!(matches!(too_long, Err(OkfError::LimitExceeded { limit: "unlock_max_hours", .. })));
        assert!(matches!(blank, Err(OkfError::LimitExceeded { limit: "reason", .. })));
        assert!(matches!(not_admin, Err(OkfError::Refused { .. })));
        assert!(quiet.is_ok());
        assert!(crate::mail::queue::list(&db.pool).await.unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn live_unlocks_are_the_unended_unexpired_ones() {
        let db = test_db().await;
        staff_and_main(&db.pool).await;
        let ended = create(&db.pool, &new(1), &policy(), &AuditActor::cli(), now()).await.unwrap();
        end(&db.pool, ended.id, &AuditActor::cli(), now()).await.unwrap();
        let live_one = create(&db.pool, &new(1), &policy(), &AuditActor::cli(), now()).await.unwrap();

        let live = live(&db.pool, now() + TimeDelta::minutes(30)).await.unwrap();
        let later = live(&db.pool, now() + TimeDelta::hours(2)).await.unwrap();

        assert_eq!(live.iter().map(|unlock| unlock.id).collect::<Vec<_>>(), [live_one.id]);
        assert!(later.is_empty());
    }
}
```

`src/store/settings.rs`: `require_unlock` (default `false`, `true|false|on|off`), `unlock_max_hours` (default 4, 1 to 72), `unlock_notify_owners` (default `true`).

- [ ] **Step 3: Run them and see them fail**

Run: `tools/agentic/gate.sh narrow --lib error::tests`, `tools/agentic/gate.sh narrow --lib store::unlocks::tests`, `tools/agentic/gate.sh narrow --lib store::settings::tests`
Expected: compile errors.

- [ ] **Step 4: The error kinds** (`src/error.rs`)

```rust
    /// Operator access to a catalog's contents without an active unlock, on
    /// a server with `require_unlock` on (spec Part 6).
    #[error("{}", unlock_message(.catalog, *.has_user))]
    UnlockRequired {
        /// The catalog asked for.
        catalog: String,
        /// Whether the credential has a user (one without cannot be unlocked).
        has_user: bool,
    },

    /// The maintenance runner on a catalog whose owners turned maintenance
    /// off. Its kind is `Forbidden` (spec Part 6); the body's
    /// `"maintenance": "off"` lets the runner skip the catalog.
    #[error("forbidden: maintenance is off for catalog {catalog:?}; an owner of its team can turn it on")]
    MaintenanceOff {
        /// The catalog.
        catalog: String,
    },
```

```rust
/// The text of [`OkfError::UnlockRequired`].
fn unlock_message(catalog: &str, has_user: bool) -> String {
    if has_user {
        format!(
            "catalog {catalog:?} needs an unlock for operator access: okf-mcp unlock create, \
             POST /api/v1/unlocks, or Unlock in the console"
        )
    } else {
        format!(
            "catalog {catalog:?} needs an unlock for operator access, and this key has no user, so \
             it cannot be unlocked: give it a user (okf-mcp key assign)"
        )
    }
}
```

`kind()`: `UnlockRequired { .. } => "UnlockRequired"`, `MaintenanceOff { .. } => "Forbidden"` (with a comment: the one variant whose kind is not its name, on purpose). `to_json()`: `UnlockRequired { catalog, .. }` sets `catalog`; `MaintenanceOff { catalog }` sets `catalog` and `"maintenance": "off"`. `AppError::status`: both `FORBIDDEN`.

- [ ] **Step 5: `src/store/unlocks.rs`**

```rust
//! Unlocks (spec Part 6, "An unlock"): an admin user's time-limited,
//! reasoned access to one customer catalog's contents. Creating and ending
//! one bump `admin_generation`, so the key cache (which holds the live
//! ones) reloads; expiry is checked at each request.

/// Why an operator opens a catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// Helping the customer.
    #[value(name = "support")]
    Support,
    /// Carrying out a deletion request.
    #[value(name = "deletion_request")]
    DeletionRequest,
    /// Carrying out an export request.
    #[value(name = "export_request")]
    ExportRequest,
    /// Investigating a security problem.
    #[value(name = "security_investigation")]
    SecurityInvestigation,
    /// A legal obligation.
    #[value(name = "legal")]
    Legal,
    /// Anything else (the reason says what).
    #[value(name = "other")]
    Other,
}
// with `as_str()` and `parse()` like `MemberAccess`, and `label()` for mail
// ("support", "a deletion request", "an export request", "a security
// investigation", "a legal obligation", "another reason").

/// The longest reason, in characters (the schema checks it too).
pub const MAX_REASON_CHARS: usize = 500;

/// An unlock to create.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewUnlock<'a> {
    /// The catalog.
    pub catalog: &'a str,
    /// The admin user it is for.
    pub user: &'a str,
    /// Why, as a category owners see.
    pub category: Category,
    /// Why, in words (a ticket number); never shown to owners.
    pub reason: &'a str,
    /// How long, in hours (1 to `unlock_max_hours`).
    pub hours: u32,
}

/// The settings an unlock follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnlockPolicy {
    /// `unlock_max_hours`.
    pub max_hours: u32,
    /// `unlock_notify_owners`.
    pub notify_owners: bool,
}

/// A stored unlock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnlockRecord {
    pub id: i64,
    pub catalog: String,
    /// The catalog's owner team now (`None` for an unowned catalog).
    pub owner_team: Option<String>,
    /// The user's name, while it exists.
    pub user: Option<String>,
    /// Who it was, as the audit log names them (`deleted user #N`).
    pub person: String,
    pub category: Category,
    pub reason: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub reads: i64,
    pub writes: i64,
    pub exports: i64,
}

/// A live unlock, as the key cache holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveUnlock {
    pub id: i64,
    pub user_id: String,
    pub catalog: String,
    pub expires_at: DateTime<Utc>,
}

/// Which unlocks [`list`] returns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UnlockFilter<'a> {
    /// Only this catalog's.
    pub catalog: Option<&'a str>,
    /// Only those of catalogs this team owns.
    pub team: Option<&'a str>,
    /// Only live ones (neither ended nor expired at this time).
    pub live_at: Option<DateTime<Utc>>,
}

/// Creates an unlock, records `catalog.unlocked` in full, queues the owner
/// team's owners an email when `policy.notify_owners`, and bumps the
/// administrative generation, in one transaction.
///
/// # Errors
/// [`OkfError::LimitExceeded`] (`unlock_max_hours`) for 0 hours or more
/// than the limit, (`reason`) for a blank or over-long reason;
/// [`OkfError::UnknownName`] for an unknown user;
/// [`OkfError::CatalogNotSelected`] for an unknown catalog;
/// [`OkfError::Refused`] for a user who is not an admin;
/// [`OkfError::Database`].
pub async fn create(
    pool: &PgPool,
    new: &NewUnlock<'_>,
    policy: &UnlockPolicy,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<UnlockRecord, OkfError> {
    if new.hours == 0 || new.hours > policy.max_hours {
        return Err(OkfError::LimitExceeded {
            limit: "unlock_max_hours",
            reason: format!("an unlock lasts 1 to {} hours", policy.max_hours),
        });
    }
    let reason = new.reason.trim();
    if reason.is_empty() || reason.chars().count() > MAX_REASON_CHARS {
        return Err(OkfError::LimitExceeded {
            limit: "reason",
            reason: format!("an unlock needs a reason of 1 to {MAX_REASON_CHARS} characters"),
        });
    }
    let mut tx = pool.begin().await?;
    // `FOR SHARE`: `set_admin` (FOR UPDATE) cannot clear the flag meanwhile.
    let (user_id, is_admin): (String, bool) =
        sqlx::query_as("SELECT id, is_admin FROM users WHERE name = $1 FOR SHARE")
            .bind(new.user)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| crate::store::users::unknown(new.user))?;
    if !is_admin {
        return Err(OkfError::Refused {
            reason: format!("{:?} is not an admin user; an unlock is for operator staff", new.user),
        });
    }
    let catalog = catalogs::find(&mut *tx, new.catalog).await?;
    let expires_at = now + TimeDelta::hours(i64::from(new.hours));
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO unlocks (catalog_id, user_id, person_id, category, reason, created_at, expires_at) \
         VALUES ($1, $2, audit_person($2), $3, $4, $5, $6) RETURNING id",
    )
    .bind(catalog.id)
    .bind(&user_id)
    .bind(new.category.as_str())
    .bind(reason)
    .bind(now)
    .bind(expires_at)
    .fetch_one(&mut *tx)
    .await?;
    let teams = audit::catalog_teams(&mut tx, catalog.id).await?;
    audit::record(
        &mut tx,
        actor,
        &Entry {
            action: Action::CatalogUnlocked,
            target: Target::Catalog(new.catalog),
            person: Some(&user_id),
            teams: &teams,
            details: serde_json::json!({
                "unlock": id,
                "category": new.category.as_str(),
                "reason": reason,
                "starts": format_timestamp(now),
                "expires": format_timestamp(expires_at),
            }),
        },
        now,
    )
    .await?;
    if policy.notify_owners {
        let owners: Vec<String> = sqlx::query_scalar(
            "SELECT u.email::text FROM catalogs c \
             JOIN team_members m ON m.team_id = c.owner_team_id AND m.role = 'owner' \
             JOIN users u ON u.id = m.user_id \
             WHERE c.id = $1 AND u.email IS NOT NULL ORDER BY u.email",
        )
        .bind(catalog.id)
        .fetch_all(&mut *tx)
        .await?;
        let template = Template::Unlocked {
            catalog: new.catalog.to_string(),
            category: new.category.label().to_string(),
            until: format_timestamp(expires_at),
        };
        for owner in &owners {
            // In the transaction: no mail for an unlock that rolls back.
            queue::enqueue_in(&mut tx, owner, &template, now).await?;
        }
    }
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    find(pool, id).await
}
```

`end(pool, id, actor, now)`: `UPDATE unlocks SET ended_at = $2 WHERE id = $1 AND ended_at IS NULL AND expires_at > $2 RETURNING catalog_id`; none is `Refused` ("unlock N has already ended or expired") or `NotFound` for an unknown id; records `catalog.unlock_ended` (`{"unlock": id}`), bumps the generation. `find(pool, id)`, `list(pool, filter)` (newest first; the `team` filter matches the catalog's current owner team), `live(pool, now) -> Vec<LiveUnlock>` (`ended_at IS NULL AND expires_at > now AND user_id IS NOT NULL`). (`tally` comes in Task 9b, with `AccessKind` from Task 7b.)

`Template::Unlocked { catalog, category, until }` (name `"unlocked"`): subject `An operator opened {catalog}`; body `An operator of {product} opened your team's catalog {catalog} for {category} until {until} (UTC). The operator's reads and writes are counted and shown in the audit log of your team. If you did not expect this, contact {support or "the operator"}.` No staff name, no reason.

`docs/operations.md`: Settings rows for the three unlock settings; error rows for `UnlockRequired` (403, `catalog`) and the maintenance `Forbidden` (403, `catalog`, `maintenance: "off"`).

- [ ] **Step 6: Run, gate, commit**

```bash
git add migrations/0015_unlocks.sql src docs/operations.md changelog/
git commit -m "feat(unlocks): unlock records, settings, error kinds and the owner email (M14 task 9a)" \
           -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 9b: The unlock check and the maintenance exemption

**Files:**
- Modify: `src/auth.rs` (`AccessGates` fields, `Admission` variants, `admission`, `check_open`, `require_contents`), `src/store/gates.rs` (the new sources), `src/selection.rs` (`Sources.now`, `check_open`), `src/mcp/context.rs` (`select`, `changeset`), `src/http/api/search.rs`, `src/mcp/changesets.rs` (`list_changesets`, `open_changeset`), `src/http/api/browse.rs:328`, `src/http/archives.rs` (export, import), `src/mcp/maintenance/work.rs` (`maintenance_close`'s operator override), `src/app.rs` (`note_access` tallies), every `Sources { ... }` literal (`now`), `docs/operations.md`

- [ ] **Step 1: Write the failing tests**

`src/auth.rs`:

```rust
    fn gates(require_unlock: bool) -> AccessGates {
        AccessGates {
            members: HashMap::from([("u-member".to_string(), HashSet::from(["main".to_string()]))]),
            entitled: HashMap::from([
                ("u-entitled".to_string(), HashSet::from(["main".to_string()])),
                ("u-staff".to_string(), HashSet::from(["main".to_string()])),
            ]),
            admins: HashSet::from(["u-staff".to_string()]),
            require_unlock,
            unlocks: HashMap::new(),
            maintenance_off: HashSet::new(),
        }
    }

    fn as_user(user: Option<&str>, access: Access, gates: AccessGates) -> Principal {
        Principal {
            user_id: user.map(str::to_string),
            gates: Arc::new(gates),
            ..writer_on(&["main"], access, Arc::default())
        }
    }

    #[test]
    fn with_require_unlock_operators_are_refused_and_customers_are_not() {
        let now = Utc::now();
        let refused = |principal: Principal| {
            matches!(principal.admission("main", Access::Read, now), Err(OkfError::UnlockRequired { .. }))
        };

        assert!(refused(as_user(Some("u-staff"), Access::Admin, gates(true))), "an admin, even entitled");
        assert!(refused(as_user(Some("u-other"), Access::Read, gates(true))), "an admin-issued key outside its teams");
        assert!(refused(as_user(None, Access::Read, gates(true))), "a key with no user");
        assert_eq!(as_user(Some("u-member"), Access::Admin, gates(true)).admission("main", Access::Read, now).unwrap(),
                   Admission::Customer);
        assert_eq!(as_user(Some("u-entitled"), Access::Read, gates(true)).admission("main", Access::Read, now).unwrap(),
                   Admission::Customer);
        assert_eq!(as_user(Some("u-staff"), Access::Admin, gates(false)).admission("main", Access::Read, now).unwrap(),
                   Admission::AdminRead, "off: nothing changes but the hourly record");
    }

    #[test]
    fn an_unlock_admits_exactly_its_user_to_exactly_its_catalog_until_it_expires() {
        let now = Utc::now();
        let mut with_unlock = gates(true);
        with_unlock.unlocks.insert(
            "u-staff".to_string(),
            vec![LiveUnlock { id: 7, user_id: "u-staff".into(), catalog: "main".into(), expires_at: now + TimeDelta::hours(1) }],
        );
        let staff = as_user(Some("u-staff"), Access::Admin, with_unlock.clone());
        let other = as_user(Some("u-other"), Access::Admin, with_unlock);

        assert_eq!(staff.admission("main", Access::Write, now).unwrap(), Admission::Unlocked { unlock_id: 7 });
        assert!(staff.admission("main", Access::Read, now + TimeDelta::hours(2)).is_err(), "expired");
        assert!(other.admission("main", Access::Read, now).is_err(), "another admin");
    }

    #[test]
    fn the_maintenance_runner_needs_no_unlock_unless_the_catalog_opted_out() {
        let now = Utc::now();
        let runner = |maintenance_off: bool, access| Principal {
            maintenance_report: true,
            ..as_user(Some("u-other"), access, AccessGates {
                maintenance_off: if maintenance_off { HashSet::from(["main".to_string()]) } else { HashSet::new() },
                ..gates(true)
            })
        };

        assert_eq!(runner(false, Access::Read).admission("main", Access::Read, now).unwrap(), Admission::Maintenance);
        assert!(matches!(runner(true, Access::Read).admission("main", Access::Read, now),
                         Err(OkfError::MaintenanceOff { .. })));
        assert!(matches!(runner(false, Access::Admin).admission("main", Access::Read, now),
                         Err(OkfError::UnlockRequired { .. })), "an admin key with the flag is not exempt");
        assert!(matches!(runner(false, Access::Write).admission("main", Access::Write, now),
                         Err(OkfError::UnlockRequired { .. })), "the exemption covers reads and reports only");
        assert!(matches!(
            Principal { maintenance_report: true, ..as_user(Some("u-member"), Access::Read,
                AccessGates { maintenance_off: HashSet::from(["main".to_string()]), ..gates(false) }) }
                .admission("main", Access::Read, now),
            Err(OkfError::MaintenanceOff { .. })
        ), "off keeps the runner out on any server, whatever its grants");
    }
```

`src/http/api/browse.rs` (or a new `src/http/api/unlock_tests.rs`):

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn with_require_unlock_staff_need_an_unlock_for_reads_writes_and_exports() {
        let test = app().await;
        let scoped = keys(&test).await;
        let staff = staff_root(&test).await; // an every-catalog admin key of admin user "staff"
        settings::set(test.app.pool(), SettingKey::RequireUnlock, "on", &AuditActor::cli(), Utc::now()).await.unwrap();
        test.app.poll().await.unwrap();

        let (tree, refused) = send(&test, "GET", "/api/v1/catalogs/main/tree", Some(&staff), None).await;
        let (export, _) = send(&test, "GET", "/api/v1/catalogs/main/export", Some(&staff), None).await;
        let (member, _) = send(&test, "GET", "/api/v1/catalogs/main/tree", Some(&scoped.read), None).await;
        let (listing, _) = send(&test, "GET", "/api/v1/catalogs", Some(&staff), None).await;
        let (users, _) = send(&test, "GET", "/api/v1/users", Some(&staff), None).await;
        let unlock = unlocks::create(test.app.pool(),
            &NewUnlock { catalog: "main", user: "staff", category: Category::Support, reason: "ticket 12", hours: 1 },
            &UnlockPolicy { max_hours: 4, notify_owners: false }, &AuditActor::cli(), Utc::now()).await.unwrap();
        test.app.poll().await.unwrap();
        let (open, _) = send(&test, "GET", "/api/v1/catalogs/main/tree", Some(&staff), None).await;
        send(&test, "GET", "/api/v1/catalogs/main/tree", Some(&staff), None).await;
        let (exported, _) = send(&test, "GET", "/api/v1/catalogs/main/export", Some(&staff), None).await;
        test.app.settle_access().await;

        assert_eq!(tree, StatusCode::FORBIDDEN);
        assert_eq!(refused["error"], "UnlockRequired");
        assert_eq!(refused["catalog"], "main");
        assert_eq!(export, StatusCode::FORBIDDEN, "export needs an unlock too");
        assert_eq!(member, StatusCode::OK);
        assert_eq!(listing, StatusCode::OK, "metadata needs none");
        assert_eq!(users, StatusCode::OK, "administration needs none");
        assert_eq!((open, exported), (StatusCode::OK, StatusCode::OK));
        let tallied = unlocks::find(test.app.pool(), unlock.id).await.unwrap();
        assert_eq!((tallied.reads, tallied.writes, tallied.exports), (2, 0, 1));
        assert!(crate::store::audit::testing::entries(test.app.pool(), "catalog.admin_read").await.is_empty(),
                "the unlock's tallies replace the hourly record");
    }
```

with a `staff_root` helper (create admin user `staff`, an every-catalog admin key for it, poll). And an MCP-side case in `src/selection.rs`: a connection list of `["main", "other"]` where only `other` is unlocked selects `["other"]`, and naming `main` is `UnlockRequired`.

- [ ] **Step 2: Run them and see them fail**

Run: `tools/agentic/gate.sh narrow --lib auth::tests`, `tools/agentic/gate.sh narrow --lib selection::tests`, `tools/agentic/gate.sh narrow --lib with_require_unlock`
Expected: compile errors (`entitled`, `require_unlock`, `Admission::Unlocked`), then 200s where 403s are due.

- [ ] **Step 3: Implement the check** (`src/auth.rs`)

`AccessGates` gains:

```rust
    /// By `users.id`: the catalogs the user is entitled to (`user_catalogs`).
    pub entitled: HashMap<String, HashSet<String>>,
    /// The `require_unlock` setting.
    pub require_unlock: bool,
    /// By `users.id`: the user's live unlocks (expiry checked per request).
    pub unlocks: HashMap<String, Vec<LiveUnlock>>,
    /// Catalogs whose `maintenance` flag is off.
    pub maintenance_off: HashSet<String>,
```

`Admission` gains `Maintenance` (not recorded; its passes are, Task 9c) and `Unlocked { unlock_id: i64 }` (recorded: `is_recorded` matches it). `admission` becomes:

```rust
    pub fn admission(&self, catalog: &str, access: Access, now: DateTime<Utc>) -> Result<Admission, OkfError> {
        let gates = &*self.gates;
        // The maintenance runner (a non-admin reporting key) first: an owner's
        // opt-out keeps it out whatever its grants and whatever require_unlock.
        if self.maintenance_report && self.access != Access::Admin {
            if gates.maintenance_off.contains(catalog) {
                return Err(OkfError::MaintenanceOff { catalog: catalog.to_string() });
            }
            if access == Access::Read {
                return Ok(Admission::Maintenance);
            }
        }
        let user = self.user_id.as_deref();
        let holds = |map: &HashMap<String, HashSet<String>>| {
            user.and_then(|id| map.get(id)).is_some_and(|catalogs| catalogs.contains(catalog))
        };
        let admin_user = user.is_some_and(|id| gates.admins.contains(id));
        // A customer: in the owner team, or a non-admin with an entitlement.
        if holds(&gates.members) || (!admin_user && holds(&gates.entitled)) {
            return Ok(Admission::Customer);
        }
        if !gates.require_unlock {
            return Ok(if self.access == Access::Admin && access == Access::Read {
                Admission::AdminRead
            } else {
                Admission::Untracked
            });
        }
        let unlock = user
            .and_then(|id| gates.unlocks.get(id))
            .and_then(|live| live.iter().find(|unlock| unlock.catalog == catalog && unlock.expires_at > now));
        match unlock {
            Some(unlock) => Ok(Admission::Unlocked { unlock_id: unlock.id }),
            None => Err(OkfError::UnlockRequired { catalog: catalog.to_string(), has_user: user.is_some() }),
        }
    }

    /// The content gate after reach and level: suspension (M13), then the
    /// admission. Used by selection, which has already checked reach.
    ///
    /// # Errors
    /// [`OkfError::Suspended`]; as [`Self::admission`].
    pub fn check_open(&self, catalog: &str, access: Access, now: DateTime<Utc>) -> Result<Admission, OkfError> {
        self.check_not_suspended(catalog)?;
        self.admission(catalog, access, now)
    }

    /// The whole check for a route that reads or writes a catalog's
    /// contents itself: reach, level and suspension ([`Self::require`]),
    /// then the admission (spec "The refusal": unlocks are checked last).
    /// Administrative routes keep [`Self::require`].
    ///
    /// # Errors
    /// As [`Self::require`]; as [`Self::admission`].
    pub fn require_contents(&self, access: Access, catalog: &str, now: DateTime<Utc>) -> Result<Admission, OkfError> {
        self.require(access, catalog)?;
        self.admission(catalog, access, now)
    }
```

`src/store/gates.rs` `load(pool, now)` adds, to its `try_join!`: `SELECT e.user_id, c.name FROM user_catalogs e JOIN catalogs c ON c.id = e.catalog_id`, `SELECT name FROM catalogs WHERE NOT maintenance`, `unlocks::live(pool, now)` (grouped by user id) and `settings::load(pool, &AuthConfig::default())` for `require_unlock` (the unlock settings have no `[auth]` default). `KeyCache::read` passes `now`.

- [ ] **Step 4: The choke points**

- `selection.rs`: `Sources` gains `now: DateTime<Utc>` (`ToolContext::select` passes `self.app.clock().now()`; tests pass `Utc::now()`). M13's `refuse_suspended` and `without_suspended` become `refuse_closed` and `without_closed`, calling `principal.check_open(name, Access::Read, sources.now)`: a named catalog that is locked is refused with `UnlockRequired`; a connection list drops it, and is refused with the first refusal only when nothing is left. Update `resolve`'s `# Errors` and the module doc.
- `http/api/search.rs` and `mcp/changesets.rs` (`list_changesets`): M13's `check_not_suspended(name).is_ok()` filter becomes `check_open(name, Access::Read, now).is_ok()`.
- `ToolContext::changeset`: `self.principal.require_contents(access, state.name(), self.app.clock().now())?` replaces `require`; `open_changeset` (`mcp/changesets.rs:235`) likewise; the revision route (`browse.rs:328`); `maintenance_close`'s operator override (`work.rs:172`) uses `require_contents(Access::Write, ..).is_ok()`.
- `http/archives.rs`: export keeps M13's `require_export` (reach and read level, suspension ignored), then `principal.admission(&name, Access::Read, now)?`; with `Admission::Unlocked`, `app.note_access(.., AccessKind::Export, ..)`. Import: `require(Access::Admin, ..)` then `admission(&name, Access::Write, now)?`, noted as a write.
- `App::note_access`: `Admission::Unlocked { unlock_id } => unlocks::tally(&pool, unlock_id, kind).await`, with, in `src/store/unlocks.rs`:

```rust
/// Counts one request under an unlock.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn tally(pool: &PgPool, id: i64, kind: AccessKind) -> Result<(), OkfError> {
    // One fixed statement per column: a column name cannot be a bind parameter.
    let sql = match kind {
        AccessKind::Read => "UPDATE unlocks SET reads = reads + 1 WHERE id = $1",
        AccessKind::Write => "UPDATE unlocks SET writes = writes + 1 WHERE id = $1",
        AccessKind::Export => "UPDATE unlocks SET exports = exports + 1 WHERE id = $1",
    };
    sqlx::query(sql).bind(id).execute(pool).await?;
    Ok(())
}
```

- The CLI is unchanged: it works on the database directly (spec: its export and import are audited, `source = cli`, and need no unlock).

- [ ] **Step 5: Run, gate, commit**

Run the narrow tests and `tools/agentic/gate.sh test` (every existing test runs with `require_unlock` off, where nothing changes). `docs/operations.md`: a section **"Unlock before access"** (who needs one, what "contents" covers, metadata and administration that do not, the refusal, the maintenance runner's exemption).

```bash
git add src docs/operations.md changelog/
git commit -m "feat(auth): operator access to customer catalogs needs an unlock when require_unlock is on (M14 task 9b)" \
           -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 9c: The per-catalog maintenance flag and maintenance passes

**Files:**
- Create: `migrations/0016_maintenance_audit.sql`
- Modify: `src/store/catalogs.rs` (`Catalog.maintenance`, `set_maintenance`), `src/store/teams.rs` (`owns_catalog`), `src/http/api/catalogs.rs` (route, `CatalogSummary.maintenance`), `src/cli.rs` (`CatalogCommand::SetMaintenance`), `src/investigator/{error,contract,supervisor,types}.rs`, `tests/admin_parity.rs`, `docs/operations.md`

- [ ] **Step 1: Write the migration**

```sql
-- One catalog.maintenance_run entry per maintenance runner pass (spec Part
-- 6): when a run leaves 'running', however it ends (closed by the runner,
-- completed, or expired by recovery), in the same transaction. The
-- runner's individual reads are not recorded.
CREATE FUNCTION audit_maintenance_run() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO audit_entries (at, action, source, actor_credential, actor_label,
                               actor_operator, target_kind, target_name, teams, details)
    SELECT COALESCE(NEW.ended_at, NEW.deadline), 'catalog.maintenance_run', 'system',
           NEW.credential_id, NEW.actor, false, 'catalog', c.name,
           CASE WHEN c.owner_team_id IS NULL THEN '{}'::text[]
                ELSE ARRAY[audit_team_ref(c.owner_team_id)] END,
           jsonb_build_object(
               'run', NEW.id,
               'state', NEW.state,
               'findings', (SELECT count(*) FROM maintenance_findings f
                            WHERE f.catalog_id = NEW.catalog_id AND f.run_id = NEW.id))
    FROM catalogs c WHERE c.id = NEW.catalog_id;
    RETURN NEW;
END
$$;
CREATE TRIGGER maintenance_runs_audit AFTER UPDATE OF state ON maintenance_runs
    FOR EACH ROW WHEN (OLD.state = 'running' AND NEW.state <> 'running')
    EXECUTE FUNCTION audit_maintenance_run();
```

- [ ] **Step 2: Write the failing tests**

`src/store/maintenance/tests.rs` (its `setup`, `actor` and `t0` fixtures):

```rust
#[tokio::test(flavor = "multi_thread")]
async fn each_run_that_ends_writes_one_maintenance_run_entry() {
    let db = test_db().await;
    let catalog = setup(&db.pool, &["a.md"]).await;
    let runner = actor("runner");
    let run = runs::claim(&db.pool, catalog, &runner, "claim-1", 1, Duration::days(30), t0())
        .await
        .unwrap();

    runs::close(&db.pool, catalog, &runner, &run.run_id, RunState::Cancelled, "stop", false, "close-1", t0())
        .await
        .unwrap();
    // The same close again is a replay: the run already ended.
    runs::close(&db.pool, catalog, &runner, &run.run_id, RunState::Cancelled, "stop", false, "close-1", t0())
        .await
        .unwrap();

    let entries = crate::store::audit::testing::entries(&db.pool, "catalog.maintenance_run").await;
    assert_eq!(entries.len(), 1, "one entry per pass");
    assert_eq!(entries[0].details["run"], run.run_id.as_str());
    assert_eq!(entries[0].details["state"], "cancelled");
    assert_eq!(entries[0].details["findings"], 0);
    assert_eq!(entries[0].source, "system");
}
```

`src/http/api/catalogs.rs`:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn an_owner_or_an_admin_switches_maintenance_and_a_member_cannot() {
        let test = app().await;
        let root = every_catalog_admin(&test).await;
        send(&test, "POST", "/api/v1/users", Some(&root), Some(json!({"name": "bob"}))).await;
        send(&test, "PUT", &format!("/api/v1/teams/{TEST_TEAM}/members/bob"), Some(&root),
             Some(json!({"role": "member"}))).await;
        let mut owner = Browser::session_for(&test, TEST_USER).await;
        let mut member = Browser::session_for(&test, "bob").await;

        let off = owner.send(&test, "PUT", "/api/v1/catalogs/main/maintenance",
                             Payload::Json(json!({"maintenance": "off"}))).await;
        let refused = member.send(&test, "PUT", "/api/v1/catalogs/main/maintenance",
                                  Payload::Json(json!({"maintenance": "on"}))).await;
        let (on, _) = send(&test, "PUT", "/api/v1/catalogs/main/maintenance", Some(&root),
                           Some(json!({"maintenance": "on"}))).await;
        let (_, listing) = send(&test, "GET", "/api/v1/catalogs", Some(&root), None).await;

        assert_eq!(off.status, StatusCode::OK);
        assert_eq!(off.json()["previous"], "on");
        assert_eq!(refused.status, StatusCode::FORBIDDEN);
        assert_eq!(on, StatusCode::OK);
        assert_eq!(listing["catalogs"][0]["maintenance"], "on");
        let changes = crate::store::audit::testing::entries(test.app.pool(), "catalog.maintenance_changed").await;
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].details, json!({"from": "off", "to": "on"}));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_runner_is_refused_a_catalog_whose_owners_turned_maintenance_off() {
        let test = app().await;
        // The runner's user is not in main's owner team: the maintenance exemption, not membership.
        crate::store::users::create(test.app.pool(), &crate::store::users::NewUser { name: "runner-user", display_name: "" },
                                    &AuditActor::cli(), Utc::now()).await.unwrap();
        let catalogs = ["main".to_string()];
        let runner = key_rows::create_with_id(test.app.pool(), &NewKey {
            name: "runner", access: Access::Read, maintenance_report: true,
            grants: Grants::Catalogs(&catalogs), default_catalog: Some("main"), actor: "runner",
            user: "runner-user", self_issued: false, expires_at: None, levels: &[],
        }, &AuditActor::cli(), Utc::now()).await.unwrap().key;
        let root = every_catalog_admin(&test).await;
        test.app.poll().await.unwrap();
        let (before, _) = send(&test, "GET", "/api/v1/catalogs/main/tree", Some(&runner), None).await;
        send(&test, "PUT", "/api/v1/catalogs/main/maintenance", Some(&root), Some(json!({"maintenance": "off"}))).await;

        let (status, body) = send(&test, "GET", "/api/v1/catalogs/main/tree", Some(&runner), None).await;

        assert_eq!(before, StatusCode::OK, "with the flag on, no unlock or membership is needed");

        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"], "Forbidden");
        assert_eq!(body["maintenance"], "off");
    }
```

`src/investigator/supervisor_tests.rs`:

```rust
#[tokio::test]
async fn investigator_supervisor_skips_a_catalog_with_maintenance_off() {
    let f = Fixture::new();
    let mut c = f.client(1);
    c.fail = Some((AllowedTool::Claim, RunnerError::MaintenanceOff));
    let s = run(&f, &mut c, FakeInference::new(80)).await.unwrap();
    assert_eq!(s.reason, "maintenance_off");
    assert_eq!(s.exit_code(), 0, "a catalog that opted out is skipped, not failed");
    assert_eq!(s.invocations, 0);
}
```

and in `src/investigator/contract.rs`'s tests: an `isError` result whose structured content is `{"error": "Forbidden", "maintenance": "off", ...}` maps to `RunnerError::MaintenanceOff`; any other `isError` still maps to `Permission`.

`tests/admin_parity.rs`: `"catalog set-maintenance"` in `COVERED`.

- [ ] **Step 3: Run them and see them fail**

Run: `tools/agentic/gate.sh narrow --lib store::maintenance::tests::each_run`, `tools/agentic/gate.sh narrow --lib http::api::catalogs::tests::an_owner_or_an_admin`, `tools/agentic/gate.sh narrow --lib http::api::catalogs::tests::the_runner_is_refused`, `tools/agentic/gate.sh narrow --lib investigator`, `tools/agentic/gate.sh narrow --test admin_parity`
Expected: no entries; 404 for the route; compile errors.

- [ ] **Step 4: Implement**

`src/store/catalogs.rs`: `Catalog.maintenance: bool` (`c.maintenance` in `select_catalogs!` and `create`'s `RETURNING`), and

```rust
/// Turns the maintenance runner's access to a catalog on or off, records
/// `catalog.maintenance_changed` when it changes, and bumps the
/// administrative generation (the key cache holds the flag).
///
/// # Arguments
/// * `pool` - the database.
/// * `name` - the catalog.
/// * `on` - the new flag.
/// * `actor` - who.
/// * `now` - the server clock.
///
/// # Returns
/// The flag before.
///
/// # Errors
/// [`OkfError::CatalogNotSelected`]; [`OkfError::Database`].
pub async fn set_maintenance(
    pool: &PgPool,
    name: &str,
    on: bool,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<bool, OkfError> {
    let mut tx = pool.begin().await?;
    let (id, previous): (i64, bool) = sqlx::query_as(
        "UPDATE catalogs c SET maintenance = $2 \
         FROM (SELECT id, maintenance FROM catalogs WHERE name = $1 FOR UPDATE) old \
         WHERE c.id = old.id RETURNING c.id, old.maintenance",
    )
    .bind(name)
    .bind(on)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| OkfError::CatalogNotSelected { catalog: name.to_string() })?;
    if previous != on {
        let teams = audit::catalog_teams(&mut tx, id).await?;
        let word = |flag: bool| if flag { "on" } else { "off" };
        audit::record(
            &mut tx,
            actor,
            &Entry {
                action: Action::CatalogMaintenanceChanged,
                target: Target::Catalog(name),
                person: None,
                teams: &teams,
                details: serde_json::json!({"from": word(previous), "to": word(on)}),
            },
            now,
        )
        .await?;
        bump_admin_generation(&mut tx).await?;
    }
    tx.commit().await?;
    Ok(previous)
}
```

`src/store/teams.rs`: `owns_catalog(pool, user_id, catalog) -> bool` (the user is an `owner` of the catalog's owner team).

`src/http/api/catalogs.rs`:

```rust
/// `on` or `off`, as the body and the listing spell it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Switch {
    /// On.
    On,
    /// Off.
    Off,
}

/// `PUT /catalogs/{catalog}/maintenance`'s body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MaintenanceBody {
    maintenance: Switch,
}

/// `PUT /catalogs/{catalog}/maintenance`: lets the maintenance runner in,
/// or keeps it out (spec Part 6). An every-catalog admin, or an owner of the
/// catalog's owner team through a console session: the one owner-managed
/// control of this milestone.
///
/// # Errors
/// 403 for anyone else; 404 for an unknown catalog; 422 without
/// `maintenance` or with another value.
async fn set_maintenance(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    actor: AuditActor,
    Path(catalog): Path<String>,
    Json(body): Json<MaintenanceBody>,
) -> Result<Json<MaintenanceChanged>, AppError> {
    let owner = match &principal.kind {
        CredentialKind::Session { user_id } => teams::owns_catalog(app.pool(), user_id, &catalog).await?,
        CredentialKind::ApiKey | CredentialKind::OAuth { .. } => false,
    };
    if !owner && require_every_catalog(&principal).is_err() {
        return Err(OkfError::Forbidden {
            reason: "this needs an admin that reaches every catalog, or an owner of the catalog's \
                     team signed in to the console"
                .to_string(),
        }
        .into());
    }
    let on = body.maintenance == Switch::On;
    let previous = catalog_rows::set_maintenance(app.pool(), &catalog, on, &actor, app.clock().now()).await?;
    settle(&app).await;
    Ok(Json(MaintenanceChanged {
        name: catalog,
        previous: if previous { Switch::On } else { Switch::Off },
        maintenance: body.maintenance,
    }))
}
```

routed as `.route("/catalogs/{catalog}/maintenance", put(set_maintenance))`. `CatalogSummary.maintenance: Switch`, from `principal.gates.maintenance_off.contains(name)` (the cache already holds it).

`src/cli.rs`: `CatalogCommand::SetMaintenance { name: String, #[arg(value_enum)] state: Switch }` ("Let the maintenance runner check a catalog (on, the default) or keep it out (off)"), printing `catalog C: maintenance OLD -> NEW`.

The runner (`src/investigator`): `RunnerError::MaintenanceOff` (`"runner skipped: maintenance is off for this catalog"`); in `contract::result`, before the generic `isError` mapping:

```rust
    if response.is_error == Some(true) {
        // The server's maintenance opt-out (a Forbidden with
        // "maintenance": "off") is a skip, not a permission failure.
        let off = response
            .structured_content
            .as_ref()
            .is_some_and(|body| body["error"] == "Forbidden" && body["maintenance"] == "off");
        return Err(if off { RunnerError::MaintenanceOff } else { RunnerError::Permission });
    }
```

`kind(MaintenanceOff) = "maintenance_off"`, `failure_kind` lists it, and `RunSummary::exit_code` treats `"maintenance_off"` as success beside `"completed"` and `"empty"`. The runner's per-catalog config means "skips that catalog" is "claims nothing and exits 0 with that reason".

`docs/operations.md`: the maintenance section (the flag, who sets it, its default, the runner's exemption and skip, the `maintenance_run` entries); the parity row

```markdown
| `okf-mcp catalog set-maintenance` | `PUT /api/v1/catalogs/{catalog}/maintenance` | Catalogs → catalog → Maintenance switch |
```

and the API row; "Report-only runner" gains the `maintenance_off` reason.

- [ ] **Step 5: Run, gate, commit**

```bash
git add migrations/0016_maintenance_audit.sql src tests/admin_parity.rs docs/operations.md changelog/
git commit -m "feat(maintenance): a per-catalog maintenance switch; runner passes are audited and skipped when off (M14 task 9c)" \
           -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 9d: Unlocks over REST and the CLI

**Files:**
- Create: `src/http/api/unlocks.rs`, `src/cli/unlock.rs`
- Modify: `src/http/api/mod.rs`, `src/cli.rs` (`Command::Unlock(UnlockCommand)`), `tests/admin_parity.rs`, `docs/operations.md`

- [ ] **Step 1: Write the failing tests**

`src/http/api/unlocks.rs`:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn an_admin_unlocks_lists_and_ends_and_an_owner_sees_the_redacted_record() {
        let test = app().await;
        let staff = staff_root(&test).await; // as in Task 9b's tests: an every-catalog admin key of admin user "staff"

        let (made, unlock) = send(&test, "POST", "/api/v1/unlocks", Some(&staff), Some(json!({
            "catalog": "main", "category": "support", "reason": "ticket 12", "hours": 1
        }))).await;
        let id = unlock["id"].as_i64().unwrap();
        let (_, listed) = send(&test, "GET", "/api/v1/unlocks?catalog=main", Some(&staff), None).await;
        let mut owner = Browser::session_for(&test, TEST_USER).await;
        let owner_view = owner.send(&test, "GET", &format!("/api/v1/unlocks?team={TEST_TEAM}"), Payload::Empty).await;
        let (too_long, _) = send(&test, "POST", "/api/v1/unlocks", Some(&staff), Some(json!({
            "catalog": "main", "category": "support", "reason": "x", "hours": 9
        }))).await;
        let (ended, after) = send(&test, "DELETE", &format!("/api/v1/unlocks/{id}"), Some(&staff), None).await;

        assert_eq!(made, StatusCode::CREATED);
        assert_eq!(unlock["user"], "staff");
        assert_eq!(unlock["reason"], "ticket 12");
        assert_eq!(listed["unlocks"][0]["id"], id);
        let (status, body) = owner_view.parts();
        assert_eq!(status, StatusCode::OK);
        let seen = &body["unlocks"][0];
        assert_eq!(seen["category"], "support");
        assert_eq!(seen["by"], "an operator");
        assert!(seen.get("reason").is_none() && seen.get("user").is_none());
        assert_eq!(too_long, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(ended, StatusCode::OK);
        assert_eq!(after["state"], "ended");
    }
```

`src/cli/unlock.rs`: `unlock create --catalog main --user staff --category support --reason "ticket 12" --hours 2` prints `unlocked main for staff until T (unlock N)`; `unlock list` prints one line per unlock (`N\tcatalog\tuser\tcategory\tstate\treads R\twrites W\texports E\tuntil T`); `unlock end N` prints `ended unlock N`.

`tests/admin_parity.rs`: `"unlock create"`, `"unlock list"`, `"unlock end"` in `COVERED`; `"unlock"` in `SCANNED_GROUPS`.

- [ ] **Step 2: Run them and see them fail**

Run: `tools/agentic/gate.sh narrow --lib http::api::unlocks::tests`, `tools/agentic/gate.sh narrow --lib cli::unlock::tests`, `tools/agentic/gate.sh narrow --test admin_parity`
Expected: 404; compile errors; the parity failure.

- [ ] **Step 3: Implement**

`POST /unlocks` body `{catalog, category, reason, hours?, user?}` (`deny_unknown_fields`; `hours` defaults to `unlock_max_hours`): `require_admin` (a provisioner is refused by the gate); `user` defaults to the caller's own user (403 for a credential with no user: "a key with no user cannot be unlocked; give it a user"); naming another user needs `require_every_catalog`. Calls `unlocks::create` with `UnlockPolicy` from `app.settings()`, then `settle`. 201 with the full `UnlockView`.

`GET /unlocks?catalog=&team=&live=true`: an admin (`require_admin`) sees full views; a console session that owns `team` sees `OwnerUnlockView { id, catalog, category, starts, ends, state, reads, writes, exports, by: "an operator" }` for that team's catalogs (spec: "an operator opened <catalog>: <category>, <start> to <end>, N reads, M writes"); anyone else 403.

`DELETE /unlocks/{id}`: `require_admin`; ends it (its admin or any admin), `settle`, 200 with the view. 404 for an unknown id; 409 for one already ended or expired.

```rust
/// An unlock as an admin sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct UnlockView {
    pub id: i64,
    pub catalog: String,
    pub owner_team: Option<String>,
    /// The admin user's name, or `deleted user #N`.
    pub user: String,
    pub category: Category,
    pub reason: String,
    pub starts: String,
    pub expires: String,
    pub ended: Option<String>,
    /// `live`, `expired` or `ended`.
    pub state: &'static str,
    pub reads: i64,
    pub writes: i64,
    pub exports: i64,
}
```

`src/cli.rs`:

```rust
/// `okf-mcp unlock ...`: operator access to a customer catalog's contents
/// on a server with `require_unlock` on.
#[derive(Debug, Subcommand)]
pub enum UnlockCommand {
    /// Unlock a catalog for an admin user, for a reason, for a while.
    Create {
        /// The catalog.
        #[arg(long)]
        catalog: String,
        /// The admin user it is for.
        #[arg(long)]
        user: String,
        /// Why, as a category its owners see.
        #[arg(long, value_enum)]
        category: Category,
        /// Why, in words (a ticket number); owners never see it.
        #[arg(long)]
        reason: String,
        /// How long (default and maximum: `unlock_max_hours`).
        #[arg(long)]
        hours: Option<u32>,
    },
    /// List unlocks, newest first.
    List {
        /// Only this catalog's.
        #[arg(long)]
        catalog: Option<String>,
        /// Only live ones.
        #[arg(long)]
        live: bool,
    },
    /// End an unlock before it expires.
    End {
        /// Its id.
        id: i64,
    },
}
```

`docs/operations.md`: the unlock commands in "Unlock before access", the parity rows

```markdown
| `okf-mcp unlock create` | `POST /api/v1/unlocks` | Unlock (on a locked catalog), or Unlocks → Unlock a catalog |
| `okf-mcp unlock list` | `GET /api/v1/unlocks` | Unlocks |
| `okf-mcp unlock end` | `DELETE /api/v1/unlocks/{id}` | Unlocks → End |
```

and a "### Unlocks API" table.

- [ ] **Step 4: Run, gate, commit**

```bash
git add src tests/admin_parity.rs docs/operations.md changelog/
git commit -m "feat(unlocks): create, list and end unlocks over REST and the CLI, at parity (M14 task 9d)" \
           -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 9e: Console: unlocking, the Unlocks page and the maintenance switch

**Files:**
- Create: `src/http/ui/js/components/unlock.js`, `src/http/ui/js/pages/unlocks.js`, `tests/ui_e2e/unlocks.rs`
- Modify: `src/http/ui/js/core/api.js`, `src/http/ui/js/pages/browse.js`, `src/http/ui/js/pages/catalogs.js`, `src/http/ui/js/pages/settings.js` (boolean settings), `src/http/ui/index.html`, `src/http/ui.rs`, `tests/ui_e2e/main.rs`

- [ ] **Step 1: Write the failing journeys** (`tests/ui_e2e/unlocks.rs`)

```rust
//! Unlock before access in the console (M14 Task 9e): a refused admin
//! unlocks from Browse, reads, and ends the unlock on the Unlocks page; an
//! owner switches maintenance off.

use chrono::Utc;
use okf_mcp::config::Config;
use okf_mcp::store::audit::AuditActor;
use okf_mcp::store::settings::{self, SettingKey};
use okf_mcp::store::users::{self, AdminChange};

use crate::harness::drive;
use crate::support::server::{files, TestServer};

const PASSWORD: &str = "a long passphrase 1";

async fn server() -> TestServer {
    TestServer::start(vec![("main", files(&[("index.md", "# Main\n")]))], Config::default()).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_admin_unlocks_main_reads_it_and_ends_the_unlock() {
    let server = server().await;
    server.user_with_password("staff", "staff@example.com", PASSWORD).await;
    users::set_admin(server.app.pool(), "staff", AdminChange { on: true, allow_last: true },
                     &AuditActor::cli(), Utc::now()).await.unwrap();
    settings::set(server.app.pool(), SettingKey::RequireUnlock, "on", &AuditActor::cli(), Utc::now())
        .await
        .unwrap();
    server.app.poll().await.unwrap();
    drive(server.base.clone(), move |ui| {
        ui.sign_in_password("staff@example.com", PASSWORD);
        ui.open("/browse/main");
        ui.wait_visible("#browse-locked");
        ui.click("#browse-unlock");
        ui.select("#unlock-form select[name=category]", "support");
        ui.fill("#unlock-form input[name=reason]", "ticket 12");
        ui.fill("#unlock-form input[name=hours]", "1");
        ui.click("#unlock-form button[type=submit]");
        ui.wait_text("#toasts", "Unlocked main");
        ui.wait_text("#browse-tree", "index.md");
        ui.open("/unlocks");
        ui.wait_text("#unlocks-table", "ticket 12");
        ui.click("[data-test=unlock-end-main]");
        ui.click("#confirm-ok");
        ui.wait_text("#unlocks-table", "ended");
        ui.open("/browse/main");
        ui.wait_visible("#browse-locked");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_owner_switches_maintenance_off_for_its_catalog() {
    let server = server().await;
    // OWNER's team owns main; give OWNER a password to sign in with.
    let owner_email = server.owner_with_password(PASSWORD).await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_password(&owner_email, PASSWORD);
        ui.open("/catalogs");
        ui.click("[data-catalog=main]");
        ui.wait_visible("#catalog-maintenance:checked");
        ui.click("#catalog-maintenance");
        ui.wait_text("#toasts", "Maintenance is off for main");
        ui.wait_visible("#catalog-maintenance:not(:checked)");
    })
    .await;
}
```

(`owner_with_password` is a small `TestServer` helper to add: set the fixture owner's email and password, return the email. Use the selectors the pages already use for the catalog drawer and the Browse tree.)

- [ ] **Step 2: Run them and see them fail**

Run: `tools/agentic/gate.sh test-ui`
Expected: timeouts on `#browse-locked` and `#catalog-maintenance`.

- [ ] **Step 3: Implement**

`components/unlock.js`: `openUnlock(catalog, { onDone })` opens a drawer with `#unlock-form`: a category `select` (the six categories, labelled), a required `reason` text input (`maxlength 500`), `hours` (`type=number`, `min 1`, `max` from the `unlock_max_hours` setting via `GET /settings` when readable, else 4) and Unlock; it posts `/unlocks` without `user` (the caller's own), toasts `Unlocked ${catalog} until ${when(view.expires)}.`, closes and calls `onDone`. All children through `h()`.

`api.js`: a 403 whose body is `UnlockRequired` toasts its own message, and for an admin session shows an Unlock action that calls `openUnlock(body.catalog)` (the generic "cannot do this" stays for other 403s); a 403 with `maintenance: "off"` shows its message.

`browse.js`: in the tree fetch's `catch`, beside M13's `Suspended` branch:

```js
        if (error?.body?.error === 'UnlockRequired') { showLocked(view, where.catalog, error.body); return; }
```

```js
// A catalog an operator must unlock first: the reason, and the Unlock button.
function showLocked(view, catalog, body) {
    fill(view, h('div', { id: 'browse-locked', class: 'notice' },
        h('h2', {}, `${catalog} is locked`),
        h('p', {}, body.message),
        store.whoami?.admin_user
            ? h('button', { type: 'button', id: 'browse-unlock',
                            onclick: () => openUnlock(catalog, { onDone: () => render(view, catalog) }) }, 'Unlock')
            : null));
}
```

(Use the whoami field that says the session is an admin user; `fill()` drops the `null`.)

`pages/unlocks.js`: `#unlocks-table` with Catalog, User, Category, Reason, From, Until, State, Reads, Writes, Exports, and an End button `[data-test=unlock-end-<catalog>]` for live ones (`confirmDialog`, then `DELETE /unlocks/{id}`); an "Unlock a catalog" button opening `openUnlock` with a catalog picker. For a non-admin session it shows its owned teams' unlocks (`?team=`), redacted, with no End.

`catalogs.js`: in the catalog drawer, a `Maintenance` section with `#catalog-maintenance` (`input type=checkbox role=switch`, checked when `row.maintenance === 'on'`), enabled for an every-catalog admin and for the owner of the catalog's team (`/me`'s `owned_teams` includes `row.owner_team`); toggling `PUT /catalogs/{c}/maintenance` and toasting `Maintenance is on|off for ${name}.`; a muted line: "Lets the maintenance runner check this catalog for stale, bloated and broken notes. Turn it off to keep the runner out."

`settings.js`: a `BOOLEAN_SETTINGS` set (`password_sign_in`, `require_unlock`, `unlock_notify_owners`) replaces the `password_sign_in` special case.

`index.html` and `ui.rs`: the `Unlocks` nav link and manifest entry (admins, and owners of a team).

- [ ] **Step 4: Run, gate, commit**

```bash
git add src/http/ui src/http/ui.rs tests/ui_e2e tests/support changelog/
git commit -m "feat(console): unlock a locked catalog, the Unlocks page and the maintenance switch (M14 task 9e)" \
           -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 10: Documentation and close-out

**Files:**
- Modify: `docs/operations.md`, `README.md`, `docs/superpowers/specs/2026-11-13-okf-operator-api-design.md`
- Create: `changelog/<timestamp>-milestone-14-operator-api.md`

- [ ] **Step 1: `docs/operations.md`**
  - "Users and teams": a lead paragraph on running okf-mcp for other people (branding, provisioner, audit, unlocks), linking each section.
  - "### Audit log": what is recorded (the full action table, grouped as the spec groups it), what is not (members' reads, sign-ins, runner reads), people in entries (`audit_people`, `deleted user #N`, `person:` team references), append-only and retention, who reads it (admins, owners through a console session, provisioners their own), the redaction rules, the CLI, REST and console surfaces, and a sample entry.
  - "Upgrades": migrations `0012` to `0016`; `0013` changes three foreign keys to `CASCADE` (deleting a user now deletes its credentials); all others are additive; `require_unlock` is off until an operator turns it on.
  - "Troubleshooting": `UnlockRequired` on a key with no user (assign it a user); `maintenance_off` from the runner.
  - Check every new route has an API reference row and every new subcommand a parity row (`tools/agentic/gate.sh narrow --test admin_parity`).
- [ ] **Step 2: `README.md`** status paragraph: "Milestone 14 added the operator API: branding, finding and deleting users, provisioner keys, a stored audit log readable by team owners, and unlock-before-access for operator staff (`docs/operations.md`; design: `docs/superpowers/specs/2026-11-13-okf-operator-api-design.md`)."
- [ ] **Step 3: The spec**: set its status to "Delivered" and add "What shipped differently", listing this plan's Decisions 1 to 12 that the spec did not already state, and every dated correction the tasks recorded.
- [ ] **Step 4: The milestone changelog**, in the format of the milestone 12 one.
- [ ] **Step 5: Issues**: close #297 (deletion meets its goal); file follow-ups for anything deferred during review.
- [ ] **Step 6: Gates and commit**

Run: `tools/agentic/gate.sh fmt`, `tools/agentic/gate.sh lint`, `tools/agentic/gate.sh test`, `tools/agentic/gate.sh test-ui`.

```bash
git add docs/ README.md changelog/
git commit -m "docs: operator API operations, API reference and close-out (M14 task 10)" \
           -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

## Decisions taken while planning (2026-11-13)

1. **The actor is an argument**, `actor: &AuditActor`, on every instrumented store function (spec "Left to the builder"). A function that would take more than five arguments takes an input struct (`AdminChange`, `NewEntitlement`, `Membership`, `NewCatalog`, `PublishInput`). In `src/store/keys.rs` the argument is named `audit`, because `actor` there is a key's actor label.
2. **Personal teams are written as `person:<id>`** in `teams` and in `details`. A personal team's name is its user's name, so writing the name would keep identifying a deleted person, against the founder's decision that entries must not identify deleted people. The reader shows the person's current name, or `deleted user #N`.
3. **Users are targets only through `audit_people`** (`target_name` is NULL for a user), and a console session's credential id (`session:<name>`) is never stored, for the same reason.
4. **Admin-read counts live in `audit_access_counts`**, beside an immutable entry, because the trigger forbids `UPDATE` of entries. The first read of an hour creates the entry; the upsert keeps the count exact under concurrency.
5. **Access accounting runs in the background** (`App::note_access` on a `JoinSet`), off the request path, at most once per tool call, catalog and kind. A crash between a request and its write can lose that one count; shutdown waits for every write (`settle_access`).
6. **An all-catalogs key's events belong to no team** (operator only): "the owner teams of the catalogs it is granted" would otherwise put every operator key into every customer's log. Its uses of a customer catalog are still recorded there (admin reads, unlocks).
7. **`MaintenanceOff` is its own variant with kind `Forbidden`**: the spec says the runner is refused with `Forbidden`, and the runner must tell that refusal from a revoked key to skip the catalog. The body's `"maintenance": "off"` is the marker; it is the one variant whose kind is not its name.
8. **The maintenance exemption covers reads only** (and the reporting tools, which need only read): a reporting key with write access gets no exemption for writes. The opt-out applies whatever the key's grants and whatever `require_unlock`.
9. **Provisioner admin-target refusals are in the store**, under the row locks `users::set_admin` conflicts with, not in the handlers, so a promotion racing a provisioner's email change cannot let it take over an admin. A provisioner's lookup by email hides admin users rather than answering 403, so it cannot probe who is an admin by email.
10. **The provisioner is its own store function** (`create_provisioner`), not a `NewKey` field: it skips grant resolution, which refuses an empty grant list, and 46 `NewKey` literals stay as they are.
11. **The Users page gets a filter box.** The spec says "the Users page's filter box matches emails too (no new control)", but the page has no filter box today; the plan adds one (name, display name and email), the smallest control that meets the goal.
12. **Retention runs daily with an hour's margin**, and the trigger compares against the database clock: the margin keeps a small skew between the two clocks from refusing the prune.
13. **`unlock_max_hours` is 1 to 72**, `audit_retention_days` 30 to 3650 (the spec gives only the defaults and the minimum).
14. **Owners read unlocks** both as `catalog.unlocked` entries (reason removed) and through `GET /api/v1/unlocks?team=` (with the tallies), both through a console session only.
15. **The parity test learns flag entries** (`user list --email`, `key create --provisioner`), so the two new flags the spec lists under "Surfaces and parity" are enforced like subcommands. `settings set` already covers the new settings keys.
