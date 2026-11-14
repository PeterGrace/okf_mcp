# OKF MCP Server, Milestone 13 (team suspension): Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. In this project each task is one GitHub sub-issue and one PR (`github-issue-to-pr`).

**Goal:** An operator can suspend a team. Every catalog the team owns then stops answering non-admin credentials at once (MCP, REST, console), except export, until the suspension is lifted. Nothing is deleted.

**Architecture:**
- **One additive migration (`0011`):** `teams.suspended_at` and `teams.suspension_notice`.
- **Suspension in the key cache.** `KeyCache` loads a map of suspended catalogs (catalog name to `{team, notice}`), and every `Principal` it builds, including console sessions, shares it through an `Arc`. Suspending and lifting bump `admin_generation`, and the route settles (polls) before answering, so the next request sees it when that poll succeeds; if the poll or reload fails, the committed change takes effect at the poll loop's next successful pass (within `poll_interval`), as for every other team write. *(Qualified, PR #360 review, 2026-11-13.)*
- **Two choke points.** `Principal::require` (every write, every pinned changeset, every REST route that checks access itself) and `selection::resolve` (every MCP read, and every REST route that runs a tool through a `ToolContext`). Admin credentials are exempt. A new `Principal::require_export` skips the suspension check, and the export handler uses it.
- **Listing stays honest.** `list_catalogs` and `GET /api/v1/catalogs` mark a suspended catalog instead of hiding it.

**Tech Stack:** Rust 2021, axum 0.8, sqlx 0.9 (Postgres), clap, the vanilla-JS console, rustwright browser tests. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-13-okf-team-suspension-design.md`.

---

## Conventions for every task

- Branch per task from `main`, one PR per task, linked to its sub-issue. Follow `AGENTS.md` (review rounds, keeping branches current, `tools/agentic/`).
- TDD: write the failing test, run it, implement, run it, commit.
- Gates only through `tools/agentic/gate.sh`: `fmt`, `lint` and `test` before every commit, plus `test-ui` for Task 7. Use `gate.sh narrow <cargo test args>` while iterating; it passes its arguments straight to `cargo test`, which takes **one** test-name filter per run, so run several filters as several commands.
- Comments for a Python-expert, Rust-novice reader on Rust-specific constructs, as in the existing code. Every public item gets a doc comment with `# Arguments`, `# Returns` and `# Errors`. No `.unwrap()` outside tests.
- One changelog entry per task, `changelog/<UTC ts>-m13-task-<NN>-<slug>.md` (timestamp from `date -u +%Y-%m-%dT%H-%M-%SZ`), with `## Added`, `## Changed` and `## Notes`. Task 8 also writes the milestone changelog.
- The code in this plan has not been compiled. If an implementation has to differ, fix this plan in the same PR (inline, or with a dated `> **Corrected (issue #N).**` note), then grep later tasks for the changed symbol.
- Placeholders only in tests and docs: `alice`, `family`, `example.com`. No real hostnames, domains or names.
- Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Verified facts the code relies on

Checked against `origin/main` at `9a84af6` (v0.15.0) on 2026-11-13.

- `Principal::require(&self, access, catalog)` is the single level check: unreached is `CatalogNotSelected`, too low is `Forbidden` (`src/auth.rs:140`). Callers: 39, all through `require` or the two below.
- MCP reads never call `require`: they pick catalogs with `selection::resolve`, whose `available` closure is `exists(name) && principal.reaches(name)` (`src/selection.rs:182-188`). `ToolContext::select` and `states` wrap it (`src/mcp/context.rs`), and REST browse, health and report routes run tools through a `ToolContext`.
- Changeset writes and publish go through `ToolContext::pinned`, which calls `principal.require(access, ..)` (`src/mcp/context.rs:384`); `open_changeset` calls `require(Access::Write, ..)` (`src/mcp/changesets.rs:235`).
- Export checks `principal.require(Access::Read, &name)` (`src/http/archives.rs:110`); `/api/v1/catalogs/{c}/export` uses the same handler.
- `list_changesets` without a `catalog` enumerates catalogs with `ctx.offers` (reach and connection scope only) after `ctx.check_connection()` (`src/mcp/changesets.rs:114-125`, `src/mcp/context.rs:144-145`); `GET /api/v1/changesets` runs the same function (`src/http/api/changesets.rs:52-60`). `ctx.one(Some(name))` resolves through `resolve`.
- The REST search over every reached catalog filters with `principal.reaches` and passes the names as the argument (`src/http/api/search.rs:72-79`), so without Task 4's filter one suspended catalog would refuse the whole search.
- The key cache is rebuilt whole by `KeyCache::read`, a `tokio::try_join!` of three loads (`src/auth.rs:415-427`), when `admin_generation` moves (`src/app.rs:428`). Team writes bump it with `bump_admin_generation(&mut tx)` (`src/store/catalogs.rs:346`), and the REST team handlers then call `settle(&app)`, which polls at once (`src/http/api/mod.rs:159`).
- Console session principals are built per request by `Principal::for_session(&user, ceiling)` (`src/http/console_session.rs:267-277`).
- `OkfError::kind`, `to_json` and `AppError::status` match every variant with no `_` arm (`src/error.rs:286`, `:337-403`; `src/http/mod.rs:96`), so the compiler lists each place a new variant needs.
- thiserror 2.0.6 accepts field references in extra format arguments: the crate already uses `#[error("import refused: {}", .problems.join("; "))]` (`src/error.rs:94`).
- `catalogs::create(pool, name, description, owner_team, now)` and `catalogs::set_owner(pool, name, team)` (`src/store/catalogs.rs:76`, `:138`). Test apps create every catalog owned by `db::TEST_TEAM`, the personal team of `db::TEST_USER` (`src/mcp/testing.rs:77`, `src/db.rs:100-103`).
- The console's API client toasts a generic "cannot do this" for every 403 (`src/http/ui/js/core/api.js:53-54`); its `ApiError` keeps the JSON body as `error.body` (`:9-14`).
- Hash routes load `js/pages/<page>.js` (`src/http/ui/js/core/router.js:1`), so Import & export for a catalog is `#/transfer/<catalog>`. The confirm dialog's button is `#confirm-ok` (`src/http/ui/js/components/confirm.js:10`).
- `TestServer` exposes `app: Arc<App>` (`tests/support/server.rs:90`): journeys reach the pool with `server.app.pool()` and poll with `server.app.poll()`.
- `tests/admin_parity.rs` fails until every `team` subcommand is in `COVERED` and has a parity row whose REST cell is a route documented in `docs/operations.md`.

## Task dependencies

```
1 -> 2 -> 3 -> 4 -> 5 -> 6
                    5 -> 7
               6, 7 -> 8
```

- Task 2 needs Task 1 only for order (both are small); Task 3 needs both (`SuspendedCatalog`, `OkfError::Suspended`).
- Task 5 needs Task 4 although they share no files: its member test expects `/catalogs/main/tree` to answer `Suspended` (selection, Task 4), the catalog listing's `suspended` field (Task 4) and the REST changeset filtering (Task 4 Step 5b). *(Corrected, PR #360 review, 2026-11-13: the graph first had 4 and 5 in parallel.)*
- Tasks 6 and 7 can run in parallel after Task 5. Task 6 edits `docs/operations.md` (parity and API rows); Task 8 edits other sections of it, so it goes last.

---

## Scope

| Task | Delivers |
|---|---|
| 1 | Migration `0011`; `store::teams` suspend, lift, and the suspended-catalogs map |
| 2 | The `Suspended` error kind (403) |
| 3 | `Principal` and `KeyCache` enforcement; export left open |
| 4 | Selection rules, REST search, `list_catalogs` and `GET /catalogs` marking |
| 5 | `PUT /api/v1/teams/{name}/suspension` and the team views |
| 6 | `okf-mcp team suspend` and `unsuspend`; parity table |
| 7 | Console: team controls, pills, Browse notice, the 403 message |
| 8 | Documentation, README status line, close-out |

### Deliberately deferred

| Item | Where it goes |
|---|---|
| Suspending one user or one catalog | Not planned; suspend the owner team (spec, Non-goals) |
| Scheduled or automatic suspension | Not planned; an operator or an outside system calls the route |
| A narrower "provisioner" admin scope for automation | A later milestone (file as an issue at close-out) |
| Deleting a user whose credentials still name them (#297) | Its own issue, unchanged by this milestone |
| Multiple replicas | Unchanged: single replica (`docs/operations.md`) |

## File map

| File | Change | Tasks |
|---|---|---|
| `migrations/0011_team_suspension.sql` | Create | 1 |
| `src/store/teams.rs` | `Suspension`, `SuspendedCatalog`, `suspend`, `unsuspend`, `suspended_catalogs`, `TeamRecord.suspension` | 1 |
| `src/error.rs` | `OkfError::Suspended`, its kind and JSON | 2 |
| `src/http/mod.rs` | 403 for `Suspended` | 2 |
| `src/mcp/mod.rs` | Tool-error mapping, only if it matches variants | 2 |
| `src/auth.rs` | `Suspensions`, `Principal.suspended`, `suspension`, `check_not_suspended`, `require_export`, cache wiring, `for_session` | 3 |
| `src/http/console_session.rs` | Pass the cache's suspensions to `for_session` | 3 |
| `src/http/archives.rs` | Export uses `require_export` | 3 |
| `src/store/keys.rs`, `src/mcp/changesets.rs` (tests) | New `from_records` / `for_session` argument | 3 |
| Every `Principal { ... }` literal (`grep -rn "Principal {" src tests`) | `suspended: Default::default(),` | 3 |
| `src/selection.rs` | Suspension rules in `resolve` | 4 |
| `src/http/api/search.rs` | Leave suspended catalogs out of "every catalog" | 4 |
| `src/mcp/catalogs.rs` | `SuspensionInfo`, `CatalogInfo.suspended` | 4 |
| `src/http/api/catalogs.rs` | `CatalogSummary.suspended` | 4 |
| `src/mcp/changesets.rs` | `list_changesets` leaves suspended catalogs out | 4 |
| `src/http/api/people.rs` | The suspension route; `TeamView` fields | 1 (pattern), 5 |
| `src/cli.rs`, `src/cli/people.rs` | `team suspend`, `team unsuspend`; list column | 6 |
| `tests/admin_parity.rs` | Two covered subcommands | 6 |
| `docs/operations.md` | Parity and API rows, team list format (6); everything else (8) | 6, 8 |
| `src/http/ui/js/core/api.js`, `pages/teams.js`, `pages/catalogs.js`, `pages/browse.js` | Console | 7 |
| `tests/ui_e2e/teams.rs` | Journeys | 7 |
| `README.md`, the spec, the milestone changelog | Close-out | 8 |
| `changelog/` | One entry per task | all |

---

### Task 1: Migration and team store

**Files:**
- Create: `migrations/0011_team_suspension.sql`
- Modify: `src/store/teams.rs`

- [ ] **Step 1: Write the migration**

```sql
-- Team suspension (spec 2026-11-13-okf-team-suspension-design.md). A
-- suspended team's catalogs stop answering non-admin credentials until the
-- suspension is lifted; nothing is deleted. The notice is shown to members.
ALTER TABLE teams
    ADD COLUMN suspended_at timestamptz,
    ADD COLUMN suspension_notice text
        CHECK (char_length(suspension_notice) <= 2000),
    ADD CONSTRAINT teams_notice_needs_suspension
        CHECK (suspension_notice IS NULL OR suspended_at IS NOT NULL);
```

- [ ] **Step 2: Write the failing store tests** (append to `mod tests` in `src/store/teams.rs`)

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn suspend_keeps_the_first_time_and_replaces_the_notice() {
        let db = test_db().await;
        family(&db.pool).await;
        let later = now() + chrono::TimeDelta::hours(1);

        let first = suspend(&db.pool, "family", Some("Contact the operator."), now())
            .await
            .unwrap();
        let again = suspend(&db.pool, "family", Some("Still paused."), later)
            .await
            .unwrap();

        assert_eq!(
            first.suspension,
            Some(Suspension {
                since: now(),
                notice: Some("Contact the operator.".to_string()),
            })
        );
        let again = again.suspension.unwrap();
        assert_eq!(again.since, now(), "a second suspend keeps the first time");
        assert_eq!(again.notice.as_deref(), Some("Still paused."));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_blank_notice_is_none_and_a_long_one_is_refused() {
        let db = test_db().await;
        family(&db.pool).await;

        let blank = suspend(&db.pool, "family", Some("   "), now()).await.unwrap();
        let long = "x".repeat(MAX_NOTICE_CHARS + 1);
        let refused = suspend(&db.pool, "family", Some(&long), now()).await;

        assert_eq!(blank.suspension.unwrap().notice, None);
        assert!(matches!(refused, Err(OkfError::Refused { .. })));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unsuspend_lifts_and_is_a_no_op_when_active() {
        let db = test_db().await;
        family(&db.pool).await;
        suspend(&db.pool, "family", None, now()).await.unwrap();
        let before = crate::store::catalogs::admin_generation(&db.pool).await.unwrap();

        let lifted = unsuspend(&db.pool, "family").await.unwrap();
        let after_lift = crate::store::catalogs::admin_generation(&db.pool).await.unwrap();
        let unchanged = unsuspend(&db.pool, "family").await.unwrap();
        let after_noop = crate::store::catalogs::admin_generation(&db.pool).await.unwrap();

        assert_eq!(lifted.suspension, None);
        assert_eq!(unchanged.suspension, None);
        assert!(after_lift > before, "lifting bumps the generation");
        assert_eq!(after_noop, after_lift, "lifting nothing does not");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn suspending_an_unknown_team_is_unknown_name() {
        let db = test_db().await;

        let suspended = suspend(&db.pool, "nobody", None, now()).await;
        let lifted = unsuspend(&db.pool, "nobody").await;

        assert!(matches!(suspended, Err(OkfError::UnknownName { noun: "team", .. })));
        assert!(matches!(lifted, Err(OkfError::UnknownName { noun: "team", .. })));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn suspended_catalogs_names_each_catalog_of_a_suspended_team() {
        let db = test_db().await;
        family(&db.pool).await;
        crate::store::catalogs::create(&db.pool, "shared", "", "family", now())
            .await
            .unwrap();
        crate::store::catalogs::create(&db.pool, "mine", "", TEST_USER, now())
            .await
            .unwrap();
        suspend(&db.pool, "family", Some("Paused."), now()).await.unwrap();

        let map = suspended_catalogs(&db.pool).await.unwrap();

        assert_eq!(map.len(), 1, "only the suspended team's catalog: {map:?}");
        assert_eq!(
            map.get("shared"),
            Some(&SuspendedCatalog {
                team: "family".to_string(),
                notice: Some("Paused.".to_string()),
            })
        );
    }
```

If `catalogs::create`'s signature differs from `(pool, name, description, owner_team, now)`, use the one `src/mcp/testing.rs::app_at` calls.

- [ ] **Step 3: Run them and see them fail**

Run: `tools/agentic/gate.sh narrow --lib store::teams::tests`
Expected: compile errors (`suspend`, `Suspension`, `SuspendedCatalog`, `MAX_NOTICE_CHARS` not found).

- [ ] **Step 4: Implement** in `src/store/teams.rs`

Add after `MemberChange`:

```rust
/// The longest suspension notice, in characters (the schema checks it too).
pub const MAX_NOTICE_CHARS: usize = 2000;

/// A team's suspension: since when, and what members are told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suspension {
    /// When it was first suspended (server clock); a second suspend keeps it.
    pub since: DateTime<Utc>,
    /// The notice members see, if the operator gave one.
    pub notice: Option<String>,
}

/// One catalog whose owner team is suspended, as the key cache holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuspendedCatalog {
    /// The owner team's name.
    pub team: String,
    /// Its notice, if any.
    pub notice: Option<String>,
}
```

Add a field to `TeamRecord` (after `catalogs`):

```rust
    /// Its suspension; `None` while it is active.
    pub suspension: Option<Suspension>,
```

Add two columns to `TeamRow`:

```rust
    suspended_at: Option<DateTime<Utc>>,
    suspension_notice: Option<String>,
```

Extend `select_teams!` (keep the rest of the query as it is):

```rust
macro_rules! select_teams {
    () => {
        "SELECT t.name, t.display_name, p.name AS personal_of, t.created_at, \
                t.suspended_at, t.suspension_notice, \
                COALESCE((SELECT array_agg(c.name ORDER BY c.name) FROM catalogs c \
                          WHERE c.owner_team_id = t.id), '{}'::text[]) AS catalogs \
         FROM teams t LEFT JOIN users p ON p.id = t.personal_of"
    };
}
```

In `assemble`, build the field:

```rust
            // `map` runs only for `Some`: a team with a time is suspended.
            suspension: row.suspended_at.map(|since| Suspension {
                since,
                notice: row.suspension_notice,
            }),
```

(Put it before `name: row.name` moves out of `row`; `suspension_notice` is moved here, so build `suspension` first in the struct literal.)

In `create`'s returned literal add `suspension: None,`.

Add the three functions after `delete`:

```rust
/// A notice as stored: trimmed, `None` when blank.
///
/// # Errors
/// [`OkfError::Refused`] for one longer than [`MAX_NOTICE_CHARS`].
fn clean_notice(notice: Option<&str>) -> Result<Option<&str>, OkfError> {
    // `filter` turns a blank (after trimming) notice into `None`.
    let notice = notice.map(str::trim).filter(|text| !text.is_empty());
    if let Some(text) = notice {
        // `chars().count()`: characters, as the schema's `char_length`
        // counts, not bytes.
        if text.chars().count() > MAX_NOTICE_CHARS {
            return Err(OkfError::Refused {
                reason: format!("a suspension notice is at most {MAX_NOTICE_CHARS} characters"),
            });
        }
    }
    Ok(notice)
}

/// Suspends a team, or replaces the notice of one already suspended
/// (keeping its first `suspended_at`), and bumps the administrative
/// generation.
///
/// # Arguments
/// * `pool` - the database.
/// * `name` - the team's name.
/// * `notice` - what members are told; blank or `None` for no notice.
/// * `now` - the suspension time, if it was not suspended (server clock).
///
/// # Returns
/// The team as it now is.
///
/// # Errors
/// [`OkfError::UnknownName`]; [`OkfError::Refused`] for a notice over
/// [`MAX_NOTICE_CHARS`]; [`OkfError::Database`].
pub async fn suspend(
    pool: &PgPool,
    name: &str,
    notice: Option<&str>,
    now: DateTime<Utc>,
) -> Result<TeamRecord, OkfError> {
    let notice = clean_notice(notice)?;
    let mut tx = pool.begin().await?;
    let locked = lock_team(&mut tx, name).await?;
    // `COALESCE` keeps the first suspension time on a repeat.
    sqlx::query(
        "UPDATE teams SET suspended_at = COALESCE(suspended_at, $2), suspension_notice = $3 \
         WHERE id = $1",
    )
    .bind(&locked.id)
    .bind(now)
    .bind(notice)
    .execute(&mut *tx)
    .await?;
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    find(pool, name).await
}

/// Lifts a team's suspension, and bumps the administrative generation when
/// there was one.
///
/// # Arguments
/// * `pool` - the database.
/// * `name` - the team's name.
///
/// # Returns
/// The team as it now is.
///
/// # Errors
/// [`OkfError::UnknownName`]; [`OkfError::Database`].
pub async fn unsuspend(pool: &PgPool, name: &str) -> Result<TeamRecord, OkfError> {
    let mut tx = pool.begin().await?;
    let locked = lock_team(&mut tx, name).await?;
    let lifted = sqlx::query(
        "UPDATE teams SET suspended_at = NULL, suspension_notice = NULL \
         WHERE id = $1 AND suspended_at IS NOT NULL",
    )
    .bind(&locked.id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if lifted > 0 {
        bump_admin_generation(&mut tx).await?;
    }
    tx.commit().await?;
    find(pool, name).await
}

/// Every catalog whose owner team is suspended, by catalog name: what the
/// key cache gives every principal.
///
/// # Arguments
/// * `pool` - the database.
///
/// # Returns
/// The map; empty when no team is suspended.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn suspended_catalogs(
    pool: &PgPool,
) -> Result<HashMap<String, SuspendedCatalog>, OkfError> {
    let rows = sqlx::query_as::<_, (String, String, Option<String>)>(
        "SELECT c.name, t.name, t.suspension_notice \
         FROM catalogs c JOIN teams t ON t.id = c.owner_team_id \
         WHERE t.suspended_at IS NOT NULL",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(catalog, team, notice)| (catalog, SuspendedCatalog { team, notice }))
        .collect())
}
```

Fix every other `TeamRecord { ... }` literal the compiler reports (tests, `src/http/api/people.rs` conversions use destructuring: add `suspension` to the pattern; Task 5 uses it).

> **Corrected (issue #363, 2026-11-13).** The only other `TeamRecord` pattern is `From<TeamRecord> for TeamView` in `src/http/api/people.rs`; Task 1 adds `suspension: _` there (the field is ignored until Task 5 shows it). The store tests also check that `suspend` bumps the generation (the spec's test list asks for it) and add `the_schema_refuses_a_notice_without_a_suspension_or_over_the_limit`, which checks the migration's two constraints directly. `clean_notice` also refuses a notice holding a NUL character (`Refused`), since Postgres text cannot store one and it would otherwise be a database error (a 500) when Task 5's route passes a REST body through; `a_blank_notice_is_none_and_a_long_one_is_refused` covers that and a notice of 2000 two-byte characters. Read `src/store/teams.rs` for the shipped `clean_notice`.

- [ ] **Step 5: Run the tests**

Run: `tools/agentic/gate.sh narrow --lib store::teams::tests`
Expected: PASS. If the project uses `sqlx` offline query data, regenerate it as `docs/operations.md` ("Development") describes.

- [ ] **Step 6: Gates and commit**

Run: `tools/agentic/gate.sh fmt`, then `tools/agentic/gate.sh lint`.

```bash
git add migrations/0011_team_suspension.sql src/store/teams.rs changelog/
git commit -m "feat(store): suspend and lift teams; the suspended-catalogs map (M13 task 1)"
```

---

### Task 2: The `Suspended` error kind

**Files:**
- Modify: `src/error.rs`, `src/http/mod.rs`

- [ ] **Step 1: Write the failing tests** (in `src/error.rs`'s `mod tests`)

```rust
    #[test]
    fn suspended_names_the_catalog_team_and_notice() {
        let error = OkfError::Suspended {
            catalog: "shared".to_string(),
            team: "family".to_string(),
            notice: Some("Contact the operator.".to_string()),
        };

        let body = error.to_json();

        assert_eq!(error.kind(), "Suspended");
        assert_eq!(body["error"], "Suspended");
        assert_eq!(body["catalog"], "shared");
        assert_eq!(body["team"], "family");
        assert_eq!(body["notice"], "Contact the operator.");
        assert_eq!(
            body["message"],
            "catalog \"shared\" is unavailable: its team \"family\" is suspended. \
             Contact the operator."
        );
    }

    #[test]
    fn suspended_without_a_notice_has_a_null_notice() {
        let error = OkfError::Suspended {
            catalog: "shared".to_string(),
            team: "family".to_string(),
            notice: None,
        };

        let body = error.to_json();

        // `get`, not `body["notice"]`: indexing a missing key also yields
        // `Null`, so only `get` proves the field is present.
        assert_eq!(body.get("notice"), Some(&serde_json::Value::Null));
        assert_eq!(
            body["message"],
            "catalog \"shared\" is unavailable: its team \"family\" is suspended"
        );
    }
```

And in `src/http/mod.rs`'s tests, next to the existing status checks:

```rust
    #[test]
    fn suspended_is_403() {
        let suspended = AppError(OkfError::Suspended {
            catalog: "shared".to_string(),
            team: "family".to_string(),
            notice: None,
        });
        assert_eq!(suspended.status(), StatusCode::FORBIDDEN);
    }
```

- [ ] **Step 2: Run and see them fail**

Run: `tools/agentic/gate.sh narrow --lib error::tests`, then `tools/agentic/gate.sh narrow --lib http::tests` (`cargo test` takes one filter per run)
Expected: compile error, no variant `Suspended`.

- [ ] **Step 3: Implement**

In `OkfError`, after `Forbidden`:

```rust
    /// The key reaches the catalog, but its owner team is suspended: only
    /// export works until the operator lifts it.
    // `.catalog`, `.team` and `.notice` name the variant's fields inside
    // `#[error]`; `.as_deref()` turns `&Option<String>` into `Option<&str>`.
    #[error("{}", suspended_message(.catalog, .team, .notice.as_deref()))]
    Suspended {
        /// The catalog asked for.
        catalog: String,
        /// Its owner team.
        team: String,
        /// The operator's notice, if any.
        notice: Option<String>,
    },
```

Add the helper below the `impl OkfError` block:

```rust
/// The text of [`OkfError::Suspended`]: the catalog, the team, then the
/// notice as its own sentence.
fn suspended_message(catalog: &str, team: &str, notice: Option<&str>) -> String {
    let mut message = format!("catalog {catalog:?} is unavailable: its team {team:?} is suspended");
    if let Some(notice) = notice {
        message.push_str(". ");
        message.push_str(notice);
    }
    message
}
```

In `kind()`: `OkfError::Suspended { .. } => "Suspended",`.

In `to_json()`, add an arm:

```rust
            OkfError::Suspended {
                catalog,
                team,
                notice,
            } => {
                body["catalog"] = catalog.as_str().into();
                body["team"] = team.as_str().into();
                // `Option<&str>` into JSON: a string, or `null` for `None`.
                body["notice"] = notice.as_deref().into();
            }
```

In `src/http/mod.rs` `status()`: `OkfError::Forbidden { .. } | OkfError::Suspended { .. } => StatusCode::FORBIDDEN,` and update the doc comment ("403 for an access level that is not enough or a suspended team's catalog").

Check the MCP tool-error mapping (`src/mcp/mod.rs`, which turns `OkfError` into a tool error with `structured_content`) needs no arm; if it matches variants exhaustively, add `Suspended` beside `Forbidden`.

> **Corrected (issue #364, 2026-11-13).** Shipped as above, with two inline fixes: `suspended_message` takes `Option<&str>` (the `#[error]` passes `.notice.as_deref()`) rather than `&Option<String>`, and the no-notice test checks `body.get("notice")`, because `body["notice"]` is `Null` for a missing key too, so the plan's assertion passed with the field dropped (found by reverting it). The MCP mapping (`tool_error` in `src/mcp/mod.rs`) calls `to_json` and matches no variants, so it needs no arm and is not changed. Read `src/error.rs` for the shipped code.

- [ ] **Step 4: Run the tests**

Run: `tools/agentic/gate.sh narrow --lib error::tests`, then `tools/agentic/gate.sh narrow --lib http::tests`
Expected: PASS.

- [ ] **Step 5: Gates and commit**

```bash
git add src/error.rs src/http/mod.rs changelog/
git commit -m "feat(error): a Suspended error kind, 403 (M13 task 2)"
```

---

### Task 3: Enforcement in `Principal` and the key cache; export left open

**Files:**
- Modify: `src/auth.rs`, `src/http/console_session.rs`, `src/http/archives.rs`, `src/store/keys.rs` (its `from_records` call), every `Principal { ... }` literal.

- [ ] **Step 1: Write the failing tests** (in `src/auth.rs`'s `mod tests`)

```rust
    use crate::store::teams::SuspendedCatalog;

    fn suspended_on(catalog: &str) -> Arc<Suspensions> {
        Arc::new(Suspensions::from([(
            catalog.to_string(),
            SuspendedCatalog {
                team: "family".to_string(),
                notice: Some("Paused.".to_string()),
            },
        )]))
    }

    fn writer_on(catalogs: &[&str], access: Access, suspended: Arc<Suspensions>) -> Principal {
        Principal {
            key_id: "k".to_string(),
            access,
            maintenance_report: false,
            reach: Reach::Catalogs(
                catalogs.iter().map(|name| (name.to_string(), access)).collect(),
            ),
            default_catalog: None,
            actor: "a".to_string(),
            kind: CredentialKind::ApiKey,
            user: None,
            suspended,
        }
    }

    #[test]
    fn require_refuses_a_suspended_catalog_after_reach_and_level() {
        let principal = writer_on(&["shared", "mine"], Access::Write, suspended_on("shared"));

        let suspended = principal.require(Access::Read, "shared");
        let unreached = principal.require(Access::Read, "other");
        let too_low = principal.require(Access::Admin, "shared");
        let fine = principal.require(Access::Write, "mine");

        assert!(matches!(suspended, Err(OkfError::Suspended { ref team, .. }) if team == "family"));
        assert!(matches!(unreached, Err(OkfError::CatalogNotSelected { .. })));
        assert!(matches!(too_low, Err(OkfError::Forbidden { .. })));
        assert!(fine.is_ok());
    }

    #[test]
    fn an_admin_is_not_suspended_and_export_ignores_suspension() {
        let admin = writer_on(&["shared"], Access::Admin, suspended_on("shared"));
        let reader = writer_on(&["shared"], Access::Read, suspended_on("shared"));

        assert!(admin.require(Access::Write, "shared").is_ok());
        assert!(reader.require_export("shared").is_ok());
        assert!(matches!(
            reader.require_export("other"),
            Err(OkfError::CatalogNotSelected { .. })
        ));
        assert_eq!(
            reader.suspension("shared").map(|s| s.team.as_str()),
            Some("family"),
            "the fact is visible to anyone who reaches it"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_cache_gives_every_principal_the_suspended_catalogs() {
        let db = crate::db::test_db().await;
        crate::store::catalogs::create(&db.pool, "main", "", crate::db::TEST_TEAM, Utc::now())
            .await
            .unwrap();
        crate::store::teams::suspend(&db.pool, crate::db::TEST_TEAM, None, Utc::now())
            .await
            .unwrap();

        let keys = Keys::load(&db.pool, Arc::new(crate::clock::SystemClock)).await.unwrap();

        assert!(keys.suspensions().contains_key("main"));
    }
```

Adjust the imports in the test module if `Utc`, `Keys` or `SystemClock` are named differently there; reuse whatever the module's other async tests import.

- [ ] **Step 2: Run and see them fail**

Run: `tools/agentic/gate.sh narrow --lib auth::tests`
Expected: compile errors (`Suspensions`, field `suspended`, `require_export`, `suspension`, `Keys::suspensions`).

- [ ] **Step 3: Implement in `src/auth.rs`**

Imports: `use std::collections::HashMap;` (if not there), `use crate::store::teams::{self, SuspendedCatalog};`.

Add after `CredentialKind`:

```rust
/// Every catalog whose owner team is suspended, by catalog name. One map per
/// key-cache load, shared by every principal built from it.
pub type Suspensions = HashMap<String, SuspendedCatalog>;
```

Add the field to `Principal` (last):

```rust
    /// The catalogs suspended when the cache was loaded (shared). Read by
    /// [`Principal::require`] and selection; an admin credential ignores it.
    pub suspended: Arc<Suspensions>,
```

Replace `require` and add the helpers:

```rust
    /// The suspension of `catalog`, if its owner team is suspended: a fact
    /// shown to anyone who reaches it (listing), whatever their level.
    ///
    /// # Arguments
    /// * `catalog` - a catalog name.
    ///
    /// # Returns
    /// The owner team and notice, or `None` for an active catalog.
    pub fn suspension(&self, catalog: &str) -> Option<&SuspendedCatalog> {
        self.suspended.get(catalog)
    }

    /// Refuses a suspended catalog for every credential but an admin.
    ///
    /// # Arguments
    /// * `catalog` - a catalog the principal reaches.
    ///
    /// # Errors
    /// [`OkfError::Suspended`] when its owner team is suspended and the
    /// principal is not an admin.
    pub fn check_not_suspended(&self, catalog: &str) -> Result<(), OkfError> {
        if self.access == Access::Admin {
            return Ok(());
        }
        match self.suspension(catalog) {
            None => Ok(()),
            Some(suspended) => Err(OkfError::Suspended {
                catalog: catalog.to_string(),
                team: suspended.team.clone(),
                notice: suspended.notice.clone(),
            }),
        }
    }

    /// Checks that the key may do something needing `access` to `catalog`,
    /// and that the catalog is not suspended.
    ///
    /// # Arguments
    /// * `access` - the level the operation needs.
    /// * `catalog` - the catalog it acts on.
    ///
    /// # Errors
    /// As [`Self::require_level`]; then [`OkfError::Suspended`] (see
    /// [`Self::check_not_suspended`]).
    pub fn require(&self, access: Access, catalog: &str) -> Result<(), OkfError> {
        self.require_level(access, catalog)?;
        self.check_not_suspended(catalog)
    }

    /// Checks the key may export `catalog`: read access, suspended or not
    /// (spec: export stays open so members can take their data).
    ///
    /// # Errors
    /// As [`Self::require_level`] with [`Access::Read`].
    pub fn require_export(&self, catalog: &str) -> Result<(), OkfError> {
        self.require_level(Access::Read, catalog)
    }
```

Rename the old `require` body to a private `fn require_level(&self, access: Access, catalog: &str) -> Result<(), OkfError>` with its existing doc comment (errors: `CatalogNotSelected`, `Forbidden`).

`impl From<KeyRecord> for Principal`: add `suspended: Arc::default(),`.

`for_session` takes the map:

```rust
    pub fn for_session(user: &UserAuth, ceiling: Ceiling, suspended: Arc<Suspensions>) -> Self {
```

and sets `suspended,` in its literal; document the new argument.

`principal_of(record, ceilings, suspended: &Arc<Suspensions>)`: after building `principal`, set `principal.suspended = Arc::clone(suspended);` (`Arc::clone` copies the pointer, not the map).

`KeyCache`: add a field

```rust
    // The suspended catalogs this cache's principals share.
    suspended: Arc<Suspensions>,
```

`from_records(records, ceilings, suspended: Arc<Suspensions>, now)`: store it (`suspended,` in the `Self { .. }` literal) and pass `&cache.suspended` to `principal_of`. Update its doc comment.

`KeyCache::read`: load the map with the others:

```rust
        let (records, ceilings, users, suspended) = tokio::try_join!(
            key_rows::list(pool),
            ceiling::ceilings(pool),
            users::auth_by_id(pool),
            teams::suspended_catalogs(pool)
        )?;
        Ok(Self::from_records(records, ceilings, Arc::new(suspended), now).with_users(users))
```

`Keys`: add

```rust
    /// The suspended catalogs, from the current cache.
    ///
    /// # Returns
    /// The shared map (a pointer copy).
    pub fn suspensions(&self) -> Arc<Suspensions> {
        Arc::clone(&self.cache.read().expect("key cache lock poisoned").suspended)
    }
```

- [ ] **Step 4: Wire the callers**

- `src/http/console_session.rs` `session_principal`: pass `app.keys().suspensions()` as the third argument in both `Principal::for_session` calls. For the not-yet-cached user, the current cache's map is right: suspension is per catalog, not per user.
- `src/http/archives.rs` `export_catalog`: `principal.require(Access::Read, &name)?;` becomes `principal.require_export(&name)?;`, with a comment: `// Export stays open while the owner team is suspended (M13 spec).`. Leave import (`require(Access::Admin, ..)`) as it is: importing into a suspended catalog is refused for non-admins, and admins are exempt.
- Every other `from_records(...)` call (`src/auth.rs` tests, `src/store/keys.rs`): add `Arc::default()` before `now`.
- Every other `for_session(...)` call (`src/auth.rs` tests, `src/mcp/changesets.rs` test): add `Default::default()`.
- Every `Principal { ... }` literal outside these: add `suspended: Default::default(),`. Find them with `grep -rn "Principal {" src tests`.

- [ ] **Step 5: Run the tests**

Run: `tools/agentic/gate.sh narrow --lib auth::tests`
Expected: PASS. Then `tools/agentic/gate.sh test` for the whole suite: every existing test must still pass (nothing is suspended in them).

- [ ] **Step 6: Gates and commit**

```bash
git add -A src/ changelog/
git commit -m "feat(auth): suspended teams' catalogs refuse non-admin credentials; export stays open (M13 task 3)"
```

> **Corrected (issue #365, 2026-11-13).** Shipped as above; the code compiled as written. Differences: the unit tests use `key_on` (for `writer_on`) and a `suspended_on(&[..])` taking several catalogs, so the unreached case asks for a catalog that *is* suspended (`theirs`) and must still get `CatalogNotSelected`; `require_maintenance_report` is checked too, and a session test (`a_member_session_is_suspended_and_an_admin_session_is_not`) was added. The cache test stores a real key, checks that its principal shares the cache's map (`Arc::ptr_eq`) and that `unsuspend` plus `reload` restores it. Every path that reaches `Principal::require` has its own test of the whole class (non-admin `Suspended`, unreached 404 or `UnknownChangeset`, admin unchanged): `mcp::changesets::suspension_tests` (open, pinned read, write, revert, pending, discard, publish), `mcp::maintenance::work::suspension_tests` (the five work tools), `http::api::maintenance` (every route), `http::api::changesets` (diff, delete), `http::api::browse` (the revision route, also as member and admin console sessions, and export as the member), `http::api::catalogs` (update, member access and delete: below admin still `Forbidden`), `http::api::keys` (create with catalogs, grant, ungrant) and `http::archives` (export open, import admin only). New test helpers later tasks can reuse: `mcp::testing::{suspend_catalogs, lift_suspension, stored_secret, cached_principal, SUSPENDED_TEAM, SUSPENSION_NOTICE}` (`suspend_catalogs` moves the named catalogs to a new team `family`, suspends it and polls; every other catalog stays active with `TEST_TEAM`) and `http::api::testing::{Expect, assert_answers}` (a status and error kind per request). Paths that pick catalogs through `selection::resolve` or `ctx.offers` (browse tree, concept, links, history, document, health, reports, search, `list_changesets`) still answer for a suspended catalog until Task 4.

---

### Task 4: Selection, REST search and listing

**Files:**
- Modify: `src/selection.rs`, `src/http/api/search.rs`, `src/mcp/catalogs.rs`, `src/http/api/catalogs.rs`, `src/mcp/changesets.rs`

- [ ] **Step 1: Write the failing selection tests** (in `src/selection.rs`'s `mod tests`; the module's `principal` helper gains `suspended`)

Change the helper to take the suspended names:

```rust
    fn principal_with(
        reach: &[&str],
        default: Option<&str>,
        suspended: &[&str],
    ) -> Principal {
        Principal {
            key_id: "k".to_string(),
            access: Access::Read,
            maintenance_report: false,
            reach: if reach.is_empty() {
                Reach::All
            } else {
                Reach::Catalogs(
                    reach
                        .iter()
                        .map(|name| (name.to_string(), Access::Read))
                        .collect(),
                )
            },
            default_catalog: default.map(str::to_string),
            actor: "a".to_string(),
            kind: CredentialKind::ApiKey,
            user: None,
            suspended: std::sync::Arc::new(
                suspended
                    .iter()
                    .map(|name| {
                        (
                            name.to_string(),
                            crate::store::teams::SuspendedCatalog {
                                team: "family".to_string(),
                                notice: None,
                            },
                        )
                    })
                    .collect(),
            ),
        }
    }

    fn principal(reach: &[&str], default: Option<&str>) -> Principal {
        principal_with(reach, default, &[])
    }
```

Tests:

```rust
    #[test]
    fn a_named_suspended_catalog_is_refused() {
        let key = principal_with(&[], Some("pg"), &["work"]);
        let list = names(&["pg", "work"]);
        let sources = Sources {
            argument: Some(&list),
            connection: None,
            server_default: None,
        };

        let refused = resolve(&key, sources, exists);

        assert!(matches!(refused, Err(OkfError::Suspended { ref catalog, .. }) if catalog == "work"));
    }

    #[test]
    fn a_suspended_default_is_refused() {
        let key = principal_with(&[], Some("work"), &["work"]);
        let sources = Sources {
            argument: None,
            connection: None,
            server_default: None,
        };

        assert!(matches!(resolve(&key, sources, exists), Err(OkfError::Suspended { .. })));
    }

    #[test]
    fn the_connection_list_drops_suspended_catalogs() {
        let key = principal_with(&[], None, &["pg"]);
        let connection = names(&["pg", "work"]);
        let sources = Sources {
            argument: None,
            connection: Some(&connection),
            server_default: None,
        };

        let selection = resolve(&key, sources, exists).unwrap();

        assert_eq!(selection.catalogs(), ["work"]);
        assert_eq!(selection.write_target(), "work");
    }

    #[test]
    fn a_connection_list_of_only_suspended_catalogs_is_refused() {
        let key = principal_with(&[], None, &["pg", "work"]);
        let connection = names(&["pg", "work"]);
        let sources = Sources {
            argument: None,
            connection: Some(&connection),
            server_default: None,
        };

        let refused = resolve(&key, sources, exists);

        assert!(matches!(refused, Err(OkfError::Suspended { ref catalog, .. }) if catalog == "pg"));
    }
```

- [ ] **Step 2: Run and see them fail**

Run: `tools/agentic/gate.sh narrow --lib selection::tests`
Expected: the four new tests fail (suspended catalogs are selected).

- [ ] **Step 3: Implement in `src/selection.rs`**

Add below `checked`:

```rust
/// `selection` refused whole if any of its catalogs is suspended: for a
/// call that named them (its argument or the key's default).
///
/// # Errors
/// [`OkfError::Suspended`] for the first suspended catalog.
fn refuse_suspended(principal: &Principal, selection: Selection) -> Result<Selection, OkfError> {
    for name in &selection.catalogs {
        principal.check_not_suspended(name)?;
    }
    Ok(selection)
}

/// `selection` without its suspended catalogs: for a call that falls back to
/// the connection's list, so one suspended team does not stop the rest.
///
/// # Errors
/// [`OkfError::Suspended`] for the first one, when every catalog is
/// suspended (the selection is never empty).
fn without_suspended(principal: &Principal, selection: Selection) -> Result<Selection, OkfError> {
    let mut kept = Vec::with_capacity(selection.catalogs.len());
    let mut first_refusal = None;
    for name in selection.catalogs {
        match principal.check_not_suspended(&name) {
            Ok(()) => kept.push(name),
            // `get_or_insert` keeps the first refusal only.
            Err(error) => {
                first_refusal.get_or_insert(error);
            }
        }
    }
    match first_refusal {
        Some(error) if kept.is_empty() => Err(error),
        _ => Ok(Selection { catalogs: kept }),
    }
}
```

In `resolve`, route each return through them:

```rust
    if let Some(connection) = set(sources.connection) {
        let scope = checked(connection.iter().map(String::as_str), available)?;
        return match argument {
            None => without_suspended(principal, scope),
            Some(list) => checked(list.iter().map(String::as_str), |name| {
                scope.catalogs.iter().any(|listed| listed == name)
            })
            .and_then(|selection| refuse_suspended(principal, selection)),
        };
    }
    if let Some(list) = argument {
        return checked(list.iter().map(String::as_str), available)
            .and_then(|selection| refuse_suspended(principal, selection));
    }
    match principal
        .default_catalog
        .as_deref()
        .or(sources.server_default)
    {
        Some(name) => {
            checked([name], available).and_then(|selection| refuse_suspended(principal, selection))
        }
        None => Err(OkfError::Refused { /* unchanged */ }),
    }
```

Update `resolve`'s `# Errors` to add `[`OkfError::Suspended`]` and a sentence on the connection-list rule; update the module doc comment with one line: "A suspended team's catalogs are refused when named and left out of a connection's list (milestone 13)."

- [ ] **Step 4: REST search** (`src/http/api/search.rs`): the "every catalog the key reaches" list leaves suspended ones out:

```rust
        app.catalogs()
            .keys()
            // A suspended team's catalogs are not searched (M13); naming one
            // explicitly is refused by the selection instead.
            .filter(|name| principal.reaches(name) && principal.check_not_suspended(name).is_ok())
            .cloned()
            .collect()
```

- [ ] **Step 5: Listing**

`src/mcp/catalogs.rs`: add

```rust
/// Why a listed catalog is unavailable: its owner team is suspended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SuspensionInfo {
    /// The owner team.
    pub team: String,
    /// The operator's notice, if any.
    pub notice: Option<String>,
}
```

and a field on `CatalogInfo`:

```rust
    /// Set when its owner team is suspended: then only export works.
    pub suspended: Option<SuspensionInfo>,
```

filled in `list` with

```rust
                suspended: ctx.principal().suspension(name).map(|s| SuspensionInfo {
                    team: s.team.clone(),
                    notice: s.notice.clone(),
                }),
```

Mention suspension in the tool's `description` (one clause: "a catalog whose team is suspended is listed with `suspended` set, and only export works on it").

`src/http/api/catalogs.rs`: add the same field to `CatalogSummary` (reuse `crate::mcp::catalogs::SuspensionInfo`; it already derives `Serialize`), set to `None` in `CatalogSummary::of`, and in `list_catalogs`:

```rust
        .map(|(name, state)| {
            let mut summary = CatalogSummary::of(name, state);
            summary.suspended = principal.suspension(name).map(|s| SuspensionInfo {
                team: s.team.clone(),
                notice: s.notice.clone(),
            });
            summary
        })
```

If the conversion appears twice, add `impl From<&SuspendedCatalog> for SuspensionInfo` in `src/mcp/catalogs.rs` and use `.map(SuspensionInfo::from)` in both places.

- [ ] **Step 5b: Changeset listing** (`src/mcp/changesets.rs` `list_changesets`, which `GET /api/v1/changesets` also runs)

Without a `catalog` argument it enumerates every catalog `ctx.offers`, which checks only reach and the connection list, so it would bypass both choke points (review of PR #360). Leave suspended catalogs out there too; with `catalog` named, `ctx.one` already goes through `resolve` and refuses it:

```rust
        None => {
            // A list naming a bad catalog fails here as it does elsewhere.
            ctx.check_connection()?;
            ctx.catalogs()
                .values()
                // A suspended team's catalog shows no changesets (M13): its
                // handles and paths are part of what suspension withholds.
                .filter(|state| {
                    ctx.offers(state.name())
                        && principal.check_not_suspended(state.name()).is_ok()
                })
                .map(|state| state.id())
                .collect()
        }
```

Tests, in `src/mcp/changesets.rs`'s tests (built like Step 6's: `main` owned by the suspended `TEST_TEAM`, `other` moved to an active team `family`, and a `write` principal given the cache's map). Open one changeset in each catalog before suspending, then:

```rust
        // No connection list: only the active catalog's changeset is listed.
        let listed = list_changesets(&context(&test, Arc::clone(&writer), None), ListParams { catalog: None })
            .await
            .unwrap();
        assert_eq!(catalogs_of(&listed), ["other"]);

        // A mixed connection list: the same.
        let mixed = list_changesets(
            &context(&test, Arc::clone(&writer), Some(&["main", "other"])),
            ListParams { catalog: None },
        )
        .await
        .unwrap();
        assert_eq!(catalogs_of(&mixed), ["other"]);

        // Naming the suspended catalog is refused.
        let named = list_changesets(
            &context(&test, Arc::clone(&writer), None),
            ListParams { catalog: Some("main".to_string()) },
        )
        .await;
        assert!(matches!(named, Err(OkfError::Suspended { .. })));
```

where `catalogs_of` collects each listed changeset's catalog name (use the field `Listed`'s rows carry; see the existing `list_changesets` tests). Task 5's member test adds the REST case: with the `write` key from `keys()`, open a changeset on `main` before suspending (as the existing REST changeset tests do), then `GET /api/v1/changesets` lists none and `GET /api/v1/changesets?catalog=main` is 403 `Suspended`.

- [ ] **Step 6: A tool-level test** (in `src/mcp/catalogs.rs`'s tests, or create a `mod tests` there following `src/mcp/search.rs`'s):

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn a_suspended_catalog_is_listed_marked_and_the_others_still_select() {
        let test = crate::mcp::testing::app_with(
            &[("main", &[("index.md", "# Main\n")]), ("other", &[("index.md", "# Other\n")])],
            crate::config::Config::default(),
        )
        .await;
        // `main` and `other` both belong to TEST_TEAM in test apps; move
        // `other` to a team that is not suspended.
        crate::store::teams::create(
            test.app.pool(),
            &crate::store::teams::NewTeam {
                name: "family",
                display_name: "",
                owner: crate::db::TEST_USER,
            },
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        crate::store::catalogs::set_owner(test.app.pool(), "other", "family")
            .await
            .unwrap();
        crate::store::teams::suspend(test.app.pool(), crate::db::TEST_TEAM, Some("Paused."), chrono::Utc::now())
            .await
            .unwrap();
        test.app.poll().await.unwrap();
        let reader = crate::mcp::testing::principal_as("reader", crate::store::keys::Access::Read, &[], None);
        let principal = std::sync::Arc::new(crate::auth::Principal {
            suspended: test.app.keys().suspensions(),
            ..(*reader).clone()
        });
        let ctx = crate::mcp::testing::context(&test, principal, Some(&["main", "other"]));

        let listed = list(&ctx);

        let main = listed.catalogs.iter().find(|c| c.name == "main").unwrap();
        assert_eq!(main.suspended.as_ref().map(|s| s.team.as_str()), Some(crate::db::TEST_TEAM));
        assert_eq!(listed.selection, ["other"], "the connection keeps the active catalog");
    }
```

(Test principals are built by hand rather than from the cache, so the test gives one the cache's map, as the cache would.)

> **Corrected (issue #366, 2026-11-13).** `resolve`, `refuse_suspended`, `without_suspended`, the REST search filter, the `list_changesets` filter, `SuspensionInfo` and `CatalogInfo.suspended` shipped as above, and the four selection tests passed as written. Differences:
> - `CatalogSummary::of(name, state, principal)` takes the caller and fills `suspended` itself (with `.map(SuspensionInfo::from)`; the `From<&SuspendedCatalog>` impl is in `src/mcp/catalogs.rs`), rather than `list_catalogs` setting it after. `POST /api/v1/catalogs`' answer reads `suspended` from `app.keys().suspensions()` after `settle`, so a catalog created for a suspended team is marked (null if that poll failed, until the next).
> - `ToolContext::check_connection` goes through `select(None)`, so a connection list whose every catalog is suspended is now refused with `Suspended` there too (`list_changesets` without `catalog`, and a changeset handle on such a connection); a mixed list passes.
> - Step 6's test and Step 5b's use `mcp::testing::{suspend_catalogs, stored_secret, cached_principal}` (Task 3's helpers: `main` or `pg` moved to the suspended team `family`, keys from the cache) instead of suspending `TEST_TEAM` and building principals by hand. Step 5b's test is `mcp::changesets::suspension_tests::list_changesets_leaves_a_suspended_catalog_out_but_to_an_admin`, and also covers a connection list of only suspended catalogs, an unreached key (`CatalogNotSelected`), an admin and the lift.
> - Added for the whole class (each path refuses a named suspended catalog, drops it from a connection's list, answers `CatalogNotSelected` to a key that does not reach it, and leaves an admin alone): `selection::tests::{an_unreached_suspended_catalog_is_not_selected_rather_than_suspended, an_admin_selects_suspended_catalogs}`; `mcp::context::tests::every_selection_path_refuses_or_drops_a_suspended_catalog` (`select`, `states`, `read_set`, `one`, `report_catalog`, `check_connection` and the `search` tool); `http::api::browse::tests::every_selection_backed_read_route_refuses_a_suspended_catalog_to_members` (tree, concept, links, history, document, health and the five reports, for keys and a member session); `http::api::search::tests::search_leaves_out_or_refuses_a_suspended_catalog`; `http::api::catalogs::tests::the_list_marks_a_suspended_catalog_and_never_hides_it`; and `http::api::changesets::tests::the_changeset_list_leaves_out_a_suspended_catalog_but_to_an_admin`, which is the REST changeset case Step 5b gave Task 5 (Task 5's member test may keep it or rely on this one).
> - Tools that select their catalog before checking their own level (`open_changeset`, `maintenance_dispose`, the maintenance work tools) now refuse a suspended catalog at the selection, so a key that reaches it below the level the tool needs gets 403 `Suspended` rather than 403 `Forbidden`. Reach is still checked first, so Decision 2's guarantee holds; Task 3's `http::api::maintenance` test now expects `Suspended` for the read key's disposition.
> - Searching every reached catalog when every one is suspended falls back to the key's default, as for a key that reaches nothing, and that default is then refused as named (403 `Suspended`).

- [ ] **Step 7: Run, gate, commit**

Run: `tools/agentic/gate.sh narrow --lib selection::tests`, `tools/agentic/gate.sh narrow --lib mcp::catalogs` and `tools/agentic/gate.sh narrow --lib mcp::changesets` (one filter per run), then `tools/agentic/gate.sh test`.

```bash
git add src/selection.rs src/http/api/search.rs src/mcp/catalogs.rs src/http/api/catalogs.rs src/mcp/changesets.rs changelog/
git commit -m "feat(selection): refuse named suspended catalogs, drop them from a connection's list; mark them in listings (M13 task 4)"
```

---

### Task 5: The REST route and team views

**Files:**
- Modify: `src/http/api/people.rs`

- [ ] **Step 1: Write the failing tests** (in `src/http/api/people.rs`'s tests)

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn suspension_route_suspends_lifts_and_needs_an_every_catalog_admin() {
        let test = app().await;
        let root = every_catalog_admin(&test).await;
        let scoped = keys(&test).await;

        let (denied, _) = send(
            &test,
            "PUT",
            &format!("/api/v1/teams/{TEST_TEAM}/suspension"),
            Some(&scoped.admin),
            Some(json!({"suspended": true})),
        )
        .await;
        let (suspended, team) = send(
            &test,
            "PUT",
            &format!("/api/v1/teams/{TEST_TEAM}/suspension"),
            Some(&root),
            Some(json!({"suspended": true, "notice": "Contact the operator."})),
        )
        .await;
        let (missing, _) = send(
            &test,
            "PUT",
            &format!("/api/v1/teams/{TEST_TEAM}/suspension"),
            Some(&root),
            Some(json!({"notice": "x"})),
        )
        .await;
        let (lifted, after) = send(
            &test,
            "PUT",
            &format!("/api/v1/teams/{TEST_TEAM}/suspension"),
            Some(&root),
            Some(json!({"suspended": false})),
        )
        .await;

        assert_eq!(denied, StatusCode::FORBIDDEN);
        assert_eq!(suspended, StatusCode::OK);
        assert_eq!(team["suspension_notice"], "Contact the operator.");
        assert!(team["suspended_at"].is_string());
        assert_eq!(missing, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(lifted, StatusCode::OK);
        // `get`: `after["suspended_at"]` is `Null` for a missing key too.
        // (Corrected, issue #364, 2026-11-13.)
        assert_eq!(after.get("suspended_at"), Some(&serde_json::Value::Null));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_member_of_a_suspended_team_may_export_but_not_browse_or_search() {
        let test = app().await;
        let root = every_catalog_admin(&test).await;
        let scoped = keys(&test).await;
        send(
            &test,
            "PUT",
            &format!("/api/v1/teams/{TEST_TEAM}/suspension"),
            Some(&root),
            Some(json!({"suspended": true, "notice": "Paused."})),
        )
        .await;

        let (tree, refused) =
            send(&test, "GET", "/api/v1/catalogs/main/tree", Some(&scoped.read), None).await;
        let (export, _) =
            send(&test, "GET", "/api/v1/catalogs/main/export", Some(&scoped.read), None).await;
        let (listed, listing) = send(&test, "GET", "/api/v1/catalogs", Some(&scoped.read), None).await;
        let (admin_tree, _) = send(&test, "GET", "/api/v1/catalogs/main/tree", Some(&root), None).await;

        assert_eq!(tree, StatusCode::FORBIDDEN);
        assert_eq!(refused["error"], "Suspended");
        assert_eq!(refused["notice"], "Paused.");
        assert_eq!(export, StatusCode::OK);
        assert_eq!(listed, StatusCode::OK);
        assert_eq!(listing["catalogs"][0]["suspended"]["team"], TEST_TEAM);
        assert_eq!(admin_tree, StatusCode::OK, "admins are not suspended");
    }
```

Import `TEST_TEAM` from `crate::db` and `keys` from `crate::http::api::testing` in the test module if they aren't already. The `catalogs[0]` index assumes `main` sorts first; it does (`main` < `other`).

- [ ] **Step 2: Run and see them fail**

Run: `tools/agentic/gate.sh narrow --lib http::api::people::tests::suspension`, then `tools/agentic/gate.sh narrow --lib http::api::people::tests::a_member_of_a_suspended` (one filter per run)
Expected: 404/405 for the new route.

- [ ] **Step 3: Implement**

`TeamView` gains (after `catalogs`):

```rust
    /// When it was suspended; null while active.
    pub suspended_at: Option<String>,
    /// The notice its members see; null without one.
    pub suspension_notice: Option<String>,
```

In `From<TeamRecord>`, bind `suspension` (Task 1 left the pattern ignoring it as `suspension: _`; change that to `suspension`) and set:

```rust
            // `as_ref` borrows inside the `Option`, so both fields can read it.
            suspended_at: suspension.as_ref().map(|s| format_timestamp(s.since)),
            suspension_notice: suspension.and_then(|s| s.notice),
```

The body:

```rust
/// `PUT /teams/{name}/suspension`'s body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SuspensionBody {
    /// `true` suspends (or replaces the notice); `false` lifts.
    suspended: bool,
    /// What members are told; ignored when lifting.
    #[serde(default)]
    notice: Option<String>,
}
```

The handler:

```rust
/// `PUT /teams/{name}/suspension`: suspends the team (or replaces its
/// notice), or lifts its suspension.
///
/// # Returns
/// The team as it now is.
///
/// # Errors
/// 403 unless the key is an admin reaching every catalog; 404 for an unknown
/// team; 409 for a notice over 2000 characters; 422 without `suspended`; 500
/// for a database failure.
async fn set_suspension(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(name): Path<String>,
    Json(body): Json<SuspensionBody>,
) -> Result<Json<TeamView>, AppError> {
    require_every_catalog(&principal)?;
    let team = if body.suspended {
        teams::suspend(app.pool(), &name, body.notice.as_deref(), app.clock().now()).await?
    } else {
        teams::unsuspend(app.pool(), &name).await?
    };
    settle(&app).await;
    Ok(Json(team.into()))
}
```

Route, beside the other team routes:

```rust
        .route("/teams/{name}/suspension", put(set_suspension))
```

Add the route to the module doc comment's access list ("suspending and lifting a team need an admin key that reaches every catalog").

> **Corrected (issue #367, 2026-11-14).** The view fields, body, handler and route shipped as above (`TeamView.suspended_at` and `suspension_notice`, body `{suspended, notice?}` with `deny_unknown_fields`, answer the team; Tasks 6 and 7 can rely on these names). Differences:
> - The tests import `axum::http::StatusCode` and `teams::MAX_NOTICE_CHARS` (the test module has no `use super::*`). The route test also checks every key below an every-catalog admin (read, write, scoped admin, stranger) gets 403 `Forbidden` and changed nothing, 422 for an unknown field, 409 `Refused` for a notice over the limit and for one holding a NUL, 404 for an unknown team, and that a repeat suspend keeps `suspended_at` while a blank notice clears the old one. A helper `put_suspension` sends the request.
> - The member test suspends `TEST_TEAM` through the route (so the listing's team is `TEST_TEAM`, as written), sends no `poll` of its own (the route settles), and also covers search and `GET /api/v1/changesets?catalog=main` (403 `Suspended` with the notice), the unfiltered changeset list (empty), the scoped admin (exempt), and then lifts and checks the member is served again and the listing's `suspended` is null.
> - `store::teams::find`, `lock_team` and `id_of` refuse a name that is not `[a-z0-9-]+` with `UnknownName` before SQL, so a NUL in a team path (`/teams/a%00b/...`) is a 404, never a 500 (part of #339, for every team route, including this one and the CLI). `a_nul_or_malformed_name_in_a_path_is_a_404_never_a_500` now covers the team routes. Read `src/http/api/people.rs` for the shipped code.

- [ ] **Step 4: Run, gate, commit**

Run the two tests, then `tools/agentic/gate.sh test`.

```bash
git add src/http/api/people.rs src/store/teams.rs changelog/
git commit -m "feat(api): PUT /teams/{name}/suspension; teams show their suspension (M13 task 5)"
```

---

### Task 6: CLI and parity

**Files:**
- Modify: `src/cli.rs`, `src/cli/people.rs`, `tests/admin_parity.rs`, `docs/operations.md` (parity table and team list format only; the rest is Task 8)

- [ ] **Step 1: Write the failing tests**

In `tests/admin_parity.rs` `COVERED`, after `"team delete",`:

```rust
    "team suspend",
    "team unsuspend",
```

In `src/cli/people.rs`'s tests, following `code_assign_gives_a_code_a_user`'s pattern for a CLI test against the test database:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn team_suspend_and_unsuspend_print_and_list_the_state() {
        let db = crate::db::test_db().await;

        let suspended = team(
            &db.pool,
            TeamCommand::Suspend {
                name: crate::db::TEST_TEAM.to_string(),
                notice: Some("Paused.".to_string()),
            },
            now(),
        )
        .await
        .unwrap();
        let listed = team(&db.pool, TeamCommand::List, now()).await.unwrap();
        let lifted = team(
            &db.pool,
            TeamCommand::Unsuspend {
                name: crate::db::TEST_TEAM.to_string(),
            },
            now(),
        )
        .await
        .unwrap();

        assert_eq!(suspended, format!("suspended team {}\n", crate::db::TEST_TEAM));
        assert!(listed.contains("suspended 2026-"), "{listed}");
        assert_eq!(lifted, format!("lifted the suspension of team {}\n", crate::db::TEST_TEAM));
    }
```

(Use the module's own `now()` helper if it has one; otherwise a fixed `2026-11-13T12:00:00Z` as `src/store/teams.rs` does.)

- [ ] **Step 2: Run and see them fail**

Run: `tools/agentic/gate.sh narrow --test admin_parity` and `tools/agentic/gate.sh narrow --lib cli::people::tests::team_suspend`
Expected: "`okf-mcp team suspend` is not a subcommand"; compile error for the variants.

- [ ] **Step 3: Implement**

`src/cli.rs` `TeamCommand`, after `Delete`:

```rust
    /// Suspend a team: its catalogs stop answering non-admin credentials
    /// (export still works) until `team unsuspend`. Repeating it replaces
    /// the notice.
    Suspend {
        /// The team.
        name: String,
        /// What its members are told (at most 2000 characters).
        #[arg(long)]
        notice: Option<String>,
    },
    /// Lift a team's suspension.
    Unsuspend {
        /// The team.
        name: String,
    },
```

`src/cli/people.rs` `team()`:

```rust
        TeamCommand::Suspend { name, notice } => {
            teams::suspend(pool, &name, notice.as_deref(), now).await?;
            format!("suspended team {name}\n")
        }
        TeamCommand::Unsuspend { name } => {
            teams::unsuspend(pool, &name).await?;
            format!("lifted the suspension of team {name}\n")
        }
```

`format_team`: add a cell after `catalogs`, before the display name:

```rust
        &labelled(
            "suspended",
            team.suspension
                .as_ref()
                .map(|s| format_timestamp(s.since))
                .as_deref(),
        ),
```

(import `crate::clock::format_timestamp` if the module lacks it). Update `team()`'s doc comment and any test that checks the team list's columns.

`docs/operations.md`: in "### CLI, API and console parity", after the `team delete` row:

```markdown
| `okf-mcp team suspend` | `PUT /api/v1/teams/{name}/suspension` | Teams → team → Suspension → Suspend |
| `okf-mcp team unsuspend` | `PUT /api/v1/teams/{name}/suspension` | Teams → team → Suspension → Lift suspension |
```

and in the API reference table, after the `DELETE /api/v1/teams/{name}/members/{user}` row:

```markdown
| `PUT /api/v1/teams/{name}/suspension` | admin, every catalog | Body `{suspended: bool, notice?}`. `true` suspends the team, or replaces the notice of a suspended one (its `suspended_at` is kept); `false` lifts it (no change when active). Answers the team. Its catalogs then refuse every non-admin credential with 403 `Suspended`, except export. 404 for an unknown or malformed team name, 409 for a notice over 2000 characters or holding a NUL, 422 without `suspended` or with an unknown field. *(Corrected, issue #367, 2026-11-14.)* |
```

Update the `okf-mcp team list` line's documented columns to include `suspended TIME` (`-` when active).

> **Corrected (issue #368, 2026-11-14).** The subcommands, the `team()` arms, the `format_team` cell, the parity rows and the API row shipped as written (the API row was not yet in `docs/operations.md`; this task added it). Differences: the tests run the commands through the test module's `okf(&db, &[...])` helper, which parses them with clap as the binary does (the module has no `now()` helper), and the list check asserts the exact cell `suspended <the stored since>` rather than `suspended 2026-`, then that the lifted team lists `suspended -`. A second test, `team_suspend_without_a_notice_and_its_refusals`, covers a missing notice, unknown and NUL names for both subcommands (`UnknownName`), and a long or NUL notice (`Refused`, leaving the suspension unchanged). `team_membership_round_trip` expects the new `suspended -` cell. Read `src/cli/people.rs` for the shipped code.

- [ ] **Step 4: Run, gate, commit**

Run both tests, then `tools/agentic/gate.sh lint`.

```bash
git add src/cli.rs src/cli/people.rs tests/admin_parity.rs docs/operations.md changelog/
git commit -m "feat(cli): team suspend and unsuspend, at parity (M13 task 6)"
```

---

### Task 7: Console

**Files:**
- Modify: `src/http/ui/js/core/api.js`, `src/http/ui/js/pages/teams.js`, `src/http/ui/js/pages/catalogs.js`, `src/http/ui/js/pages/browse.js`
- Test: `tests/ui_e2e/teams.rs`

- [ ] **Step 1: Write the failing journey** (append to `tests/ui_e2e/teams.rs`)

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_team_is_suspended_with_a_notice_shown_on_its_catalogs_then_lifted() {
    let server = server().await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.open("/teams");
        ui.click(&format!("[data-test=team-{OWNER}]"));
        ui.fill("#team-suspend-form textarea[name=notice]", "Paused for review.");
        ui.click("#team-suspend");
        ui.click("#confirm-ok");
        ui.wait_text("#toasts", &format!("Suspended team {OWNER}."));
        ui.wait_text("#team-suspension", "Paused for review.");
        ui.wait_visible(&format!("[data-test=team-suspended-{OWNER}]"));
        ui.click("#drawer-close");
        ui.open("/catalogs");
        ui.wait_visible("[data-test=catalog-suspended-main]");
        ui.open("/teams");
        ui.click(&format!("[data-test=team-{OWNER}]"));
        ui.click("#team-unsuspend");
        ui.wait_text("#toasts", &format!("Lifted the suspension of team {OWNER}."));
        ui.wait_hidden(&format!("[data-test=team-suspended-{OWNER}]"));
        // Suspended without a notice: no notice paragraph, and never the
        // text "null" (core/dom.js, issue #302).
        ui.click("#team-suspend");
        ui.click("#confirm-ok");
        ui.wait_text("#team-suspension", "Suspended");
        ui.wait_hidden("#team-suspension-notice");
        ui.wait_text_gone("#team-suspension", "null");
    })
    .await;
}
```

The admin is exempt, so the admin's own Browse still works; the Browse notice is covered by the member case below.

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_sees_the_notice_on_browse_with_a_link_to_export() {
    let server = server().await;
    let reader = server.key("member-reader", Access::Read, &["main"], Some("main")).await;
    teams::suspend(server.app.pool(), OWNER, Some("Paused for review."), Utc::now())
        .await
        .unwrap();
    server.app.poll().await.unwrap();
    drive(server.base.clone(), move |ui| {
        ui.login(&reader);
        ui.open("/browse/main");
        ui.wait_text("#browse-suspended", "Paused for review.");
        ui.wait_visible("#browse-suspended a[href='#/transfer/main']");
    })
    .await;
}
```


- [ ] **Step 2: Run and see them fail**

Run: `tools/agentic/gate.sh test-ui`
Expected: the two journeys time out on `#team-suspend-form` and `#browse-suspended`.

- [ ] **Step 3: `api.js`**: show a suspension's own message on a 403:

```js
        // A suspended team's 403 carries the operator's notice: show it, not
        // the generic "cannot do this".
        const suspended = errorBody?.error === 'Suspended';
        if (!quiet) toast(response.status === 403 && !suspended ? denied : error.message, 'error');
```

- [ ] **Step 4: `teams.js`**

Name column: add the pill after the personal one:

```js
          render: team => h('code', { dataset: { test: `team-${team.name}` } }, team.name,
              team.personal_of ? [' ', pill('personal', 'muted')] : null,
              team.suspended_at ? [' ', h('span', { dataset: { test: `team-suspended-${team.name}` } },
                                         pill('suspended', 'danger'))] : null) },
```

In `openDetail`, before `const reason = ...`, build the section:

```js
        // Suspension: its catalogs refuse every non-admin credential except
        // export until it is lifted. Same rule as the other team writes.
        // Every conditional child goes through h(), never the DOM's own
        // append, which prints null as "null" (core/dom.js, issue #302).
        const heading = h('h3', {}, 'Suspension');
        let suspension;
        if (team.suspended_at) {
            suspension = h('section', { id: 'team-suspension' }, heading,
                h('p', {}, `Suspended ${when(team.suspended_at)}.`),
                team.suspension_notice
                    ? h('p', { class: 'muted', id: 'team-suspension-notice' }, team.suspension_notice)
                    : null,
                h('button', {
                    type: 'button', id: 'team-unsuspend', disabled: !manager,
                    onclick: async () => {
                        let updated;
                        try { updated = await api.put(`/teams/${enc(team.name)}/suspension`, { suspended: false }); } catch { return; }
                        await changed(updated, `Lifted the suspension of team ${team.name}.`, ticket);
                    },
                }, 'Lift suspension'));
        } else {
            const form = h('form', { id: 'team-suspend-form' },
                h('label', {}, 'Notice for its members (optional)',
                    h('textarea', { name: 'notice', maxlength: 2000, rows: 3, disabled: !manager })),
                h('button', { type: 'submit', id: 'team-suspend', class: 'danger', disabled: !manager }, 'Suspend'));
            form.addEventListener('submit', async event => {
                event.preventDefault();
                const ok = await confirmDialog({
                    title: `Suspend team ${team.name}?`,
                    message: 'Its catalogs stop answering everyone but admins until you lift it. Members can still export them. Nothing is deleted.',
                    action: 'Suspend',
                });
                if (!ok) return;
                const notice = String(new FormData(form).get('notice') ?? '');
                let updated;
                try { updated = await api.put(`/teams/${enc(team.name)}/suspension`, { suspended: true, notice }); } catch { return; }
                await changed(updated, `Suspended team ${team.name}.`, ticket);
            });
            suspension = h('section', { id: 'team-suspension' }, heading, form);
        }
```

`h()` drops `null` children (`core/dom.js:6-10`); the native `append` would print them as `"null"`, so the section is built with `h()` in both branches.

Add `suspension` to `ticket.open(...)` after the Members section and before the `manager ? null : ...` note.

- [ ] **Step 5: `catalogs.js`**: the Name column:

```js
          render: row => h('code', { dataset: { catalog: row.name } }, row.name,
              row.suspended ? [' ', h('span', { dataset: { test: `catalog-suspended-${row.name}` },
                                               title: row.suspended.notice ?? `Team ${row.suspended.team} is suspended.` },
                                      pill('suspended', 'danger'))] : null) },
```

Import `pill` from `../core/dom.js` if the page does not already.

- [ ] **Step 6: `browse.js`**: in `render`, replace the tree fetch's `catch`:

```js
    } catch (error) {
        clear(side);
        // A suspended team's catalog: say so, and point at export (still open).
        if (error?.body?.error === 'Suspended') showSuspended(view, where.catalog, error.body);
        return;
    }
```

and add:

```js
// A catalog whose team is suspended: the notice, and the one thing that works.
function showSuspended(view, catalog, body) {
    clear(view).append(h('div', { id: 'browse-suspended', class: 'notice' },
        h('h2', {}, `${catalog} is unavailable`),
        h('p', {}, `Its team ${body.team} is suspended.`),
        body.notice ? h('p', {}, body.notice) : h('p', { class: 'muted' }, 'Ask the server operator for details.'),
        h('p', {}, h('a', { href: `#/transfer/${enc(catalog)}` }, 'Export a copy'))));
}
```


> **Corrected (issue #369, 2026-11-14).** Steps 3, 5 and 6 shipped as written in substance (Step 3 names its test `generic`; Step 6 adds `role="status"`). Differences:
> - `core/dom.js`'s `pill` has no `danger` kind (only `ok`, `warn`, `error`, `info`, `muted`), so both "suspended" pills use `error`.
> - Step 4's section is a function, `suspensionSection(team, refusal)` inside `render` in `pages/teams.js`, and follows `tools/agentic/lessons.md`'s console rules, which the plan's code did not: the Suspend button, the notice and Lift are `guard`ed by the pending-write key `team:<name>:suspension` and the PUT runs under `once`, so they stay disabled while it runs, in a drawer closed and opened again meanwhile too; a refused write shows its reason in `#team-suspension-error` until the next write. To make that work the drawer took `pages/users.js`'s shape: `openDetail(name, ticket, { known, refusal, last })` (was `openDetail(name, known, ticket)`) numbers its reads and draws only the newest, and settles the team (a write's answer or a read) and the users (for Add member) one by one with `Promise.allSettled`: a part that fails is drawn from the last answer with `#team-stale` ("may be out of date"), so the reason and the controls still come back, and a write's answer is never thrown away for a failed users read (PR #384 review); `changed(name, team, { message, refusal })` (was `changed(team, message, ticket)`) redraws whichever drawer shows that team now, through that drawer's own ticket, and the membership controls use it too; it takes the drawer's read number as the write settles, before reading the list again, so an older write whose list read answers late never redraws over a newer one (PR #384 review, journey `an_older_writes_late_list_read_never_redraws_over_a_newer_write`). It takes that number only while the drawer shows that team, so a write of one team never cancels the loading drawer of another (journey `a_write_settling_for_one_team_never_cancels_opening_another`). The list's `load()` numbers its reads as well. The suspended state reads "Suspended since <when>."; the active one says what suspending does. Read `src/http/ui/js/pages/teams.js` for the shipped code.
> - Step 6's notice uses a `.notice` rule added to `src/http/ui/styles.css` (it existed only in `signin.css`), with `role="status"`.
> - Step 1's first journey waits for "Suspended since", also checks the catalog pill's `title` is the notice, and checks the stored suspension (no notice) afterwards. The member journey also checks the toast is the 403's own message (not "cannot do this") and follows the link to export a copy. Two more journeys cover the lessons: `a_suspension_write_runs_once_and_settles_in_a_reopened_drawer` (`hold_next`, `click_twice`, a redrawn Lift) and `a_refused_lift_keeps_its_reason_and_a_late_read_never_draws_over_a_newer_one` (`fail_next` on the PUT and the follow-up GET, then `hold_answer` on a refused lift's read that settles after a successful one), and `a_written_suspension_shows_when_reading_the_users_again_fails` (PR #384 review).

- [ ] **Step 7: Run, gate, commit**

Run: `tools/agentic/gate.sh test-ui`, then `tools/agentic/gate.sh lint`.

```bash
git add src/http/ui/js tests/ui_e2e/teams.rs changelog/
git commit -m "feat(console): suspend and lift teams; suspended pills and the Browse notice (M13 task 7)"
```

---

### Task 8: Documentation and close-out

**Files:**
- Modify: `docs/operations.md`, `README.md`, `docs/superpowers/specs/2026-11-13-okf-team-suspension-design.md`
- Create: `changelog/<timestamp>-milestone-13-team-suspension.md`

- [ ] **Step 1: `docs/operations.md`**
  - In "Users and teams", a subsection **"Suspending a team"**: what it does, what stays open (export, sign-in, listing), that admins are exempt, the connection-list rule, that it takes effect on the next request on a single replica, and the CLI and REST forms.
  - In the error table, a `Suspended` row: 403, when, and its body fields (`catalog`, `team`, `notice`).
  - In the MCP tools table, note `list_catalogs`' `suspended` field; in the API reference, note `GET /api/v1/catalogs`' `suspended` field and `GET /api/v1/teams`' `suspended_at` and `suspension_notice`.
  - In "Upgrades", one line: migration `0011` is additive.
  - In "Users and teams"' command table, rows for `okf-mcp team suspend NAME [--notice TEXT]` (prints `suspended team NAME`; repeating it replaces the notice and keeps the first time) and `okf-mcp team unsuspend NAME` (prints `lifted the suspension of team NAME`; no change when active). *(Corrected, issue #368, 2026-11-14: Task 6 added the parity rows, the API row and the `team list` column only.)*
- [ ] **Step 2: `README.md`** status paragraph: "Milestone 13 added team suspension: an operator can suspend a team, whose catalogs then refuse every non-admin credential except export until it is lifted (`docs/operations.md`, "Suspending a team"; design: `docs/superpowers/specs/2026-11-13-okf-team-suspension-design.md`)."
- [ ] **Step 3: The spec**: add a "What shipped differently" section listing any deviation the tasks recorded (or "None").
- [ ] **Step 4: The changelog entry** for the milestone, in the format of `changelog/2026-11-13T15-30-24Z-milestone-12-user-login.md`.
- [ ] **Step 5: Gates and commit**

Run: `tools/agentic/gate.sh fmt`, `tools/agentic/gate.sh lint`, `tools/agentic/gate.sh test`.

```bash
git add docs/ README.md changelog/
git commit -m "docs: team suspension operations, API reference and close-out (M13 task 8)"
```

---

## Decisions taken while planning (2026-11-13)

1. **Admins are exempt** (`Principal.access == Admin`), including admin console sessions: the operator must still be able to inspect, export and repair a suspended team's catalogs.
2. **Order of checks:** reach, then level, then suspension. A credential that doesn't reach a catalog never learns it is suspended. *(Qualified, issue #366, 2026-11-13: `Principal::require` keeps this order; a tool that selects its catalog first refuses a suspended one at the selection, after reach but before the tool's own level check.)*
3. **Connection lists drop, explicit names refuse.** One suspended team doesn't break a connection that also reads other teams' catalogs, while a call that names a suspended catalog gets a clear `Suspended`.
4. **The suspension map lives in the key cache**, not in each grant's levels, so lifting a suspension restores every credential exactly as it was, with no recomputation of ceilings.
5. **Import into a suspended catalog** stays behind `require(Admin, ..)`, so only admins (who are exempt) can import; no special case.
6. **Notice length 2000 characters**, checked in the store (409 `Refused`) and the schema.
