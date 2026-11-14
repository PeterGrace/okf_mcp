//! A user's ceiling (spec "The ceiling"): the most a self-issued credential
//! may reach. It is the catalogs owned by the user's teams, at each one's
//! `member_access`, merged with the user's entitlements (`user_catalogs`);
//! the higher level wins. Every write here that changes a ceiling bumps
//! `admin_generation`, so running servers re-cap within one poll.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::{PgConnection, PgExecutor, PgPool};

use crate::auth::{Principal, Reach};
use crate::error::OkfError;
use crate::store::audit::{self, Action, AuditActor, Entry, People, RowLock};
use crate::store::catalogs::{self, bump_admin_generation, CatalogEvent};
use crate::store::keys::Access;
use crate::store::users;

/// Catalog name to the highest level a user's self-issued credentials may
/// have there: `Read` or `Write`, never `Admin`.
pub type Ceiling = BTreeMap<String, Access>;

/// What members of a catalog's owning team may reach through self-issued
/// credentials.
// `clap::ValueEnum` parses `--access none|read|write`; `serde::Deserialize`
// with `rename_all = "lowercase"` reads `"none"`/`"read"`/`"write"` from REST
// bodies. A variant named `None` is fine: `Option`'s `None` is a different
// item, and this one is always written `MemberAccess::None`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum MemberAccess {
    /// Team members get nothing through the team.
    None,
    /// Read only.
    Read,
    /// Read and write (the default).
    Write,
}

impl MemberAccess {
    /// The stored spelling.
    ///
    /// # Returns
    /// `"none"`, `"read"` or `"write"`.
    pub fn as_str(self) -> &'static str {
        match self {
            MemberAccess::None => "none",
            MemberAccess::Read => "read",
            MemberAccess::Write => "write",
        }
    }

    /// Parses the stored spelling.
    ///
    /// # Arguments
    /// * `text` - a `catalogs.member_access` value.
    ///
    /// # Returns
    /// The value, or `None` for anything else.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "none" => Some(MemberAccess::None),
            "read" => Some(MemberAccess::Read),
            "write" => Some(MemberAccess::Write),
            _ => None,
        }
    }

    /// The level it gives a member.
    ///
    /// # Returns
    /// `Read` or `Write`, or `None` for [`MemberAccess::None`].
    pub fn level(self) -> Option<Access> {
        match self {
            MemberAccess::None => None,
            MemberAccess::Read => Some(Access::Read),
            MemberAccess::Write => Some(Access::Write),
        }
    }
}

/// One entitlement: a catalog given to a user beyond their teams' catalogs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entitlement {
    /// The catalog's name.
    pub catalog: String,
    /// `Read` or `Write`.
    pub access: Access,
    /// When it was first given.
    pub created_at: DateTime<Utc>,
}

/// One catalog of one user's ceiling, as the query below returns it.
#[derive(sqlx::FromRow)]
struct CeilingRow {
    user_id: String,
    catalog: String,
    // `true` when any source gives write: the higher level wins.
    write: bool,
}

/// Every user's ceiling (or one user's, with `$1`), one row per user and
/// catalog. The union is the two sources; `bool_or` merges them.
const CEILING_ROWS: &str = "\
    SELECT s.user_id, c.name AS catalog, bool_or(s.access = 'write') AS write \
    FROM (SELECT m.user_id, c.id AS catalog_id, c.member_access AS access \
          FROM team_members m JOIN catalogs c ON c.owner_team_id = m.team_id \
          WHERE c.member_access <> 'none' \
          UNION ALL \
          SELECT e.user_id, e.catalog_id, e.access FROM user_catalogs e) s \
    JOIN catalogs c ON c.id = s.catalog_id \
    WHERE $1::text IS NULL OR s.user_id = $1 \
    GROUP BY s.user_id, c.name";

/// The ceiling rows of every user, or of the user with this id.
///
/// # Arguments
/// * `executor` - a `&PgPool`, or `&mut *tx` to read inside a transaction.
/// * `user_id` - one user's id, or `None` for every user.
///
/// # Errors
/// [`OkfError::Database`].
// `impl PgExecutor<'_>` accepts either a pool or a connection, as
// `user_sessions::revoke_all` does.
async fn ceiling_rows(
    executor: impl PgExecutor<'_>,
    user_id: Option<&str>,
) -> Result<Vec<CeilingRow>, OkfError> {
    Ok(sqlx::query_as::<_, CeilingRow>(CEILING_ROWS)
        .bind(user_id)
        .fetch_all(executor)
        .await?)
}

/// The level a row stands for.
fn row_level(write: bool) -> Access {
    if write {
        Access::Write
    } else {
        Access::Read
    }
}

/// One user's rows as a ceiling.
fn one_ceiling(rows: Vec<CeilingRow>) -> Ceiling {
    rows.into_iter()
        .map(|row| (row.catalog, row_level(row.write)))
        .collect()
}

/// The refusal for a catalog name that names no catalog.
fn unknown_catalog(catalog: &str) -> OkfError {
    OkfError::CatalogNotSelected {
        catalog: catalog.to_string(),
    }
}

/// Refuses, before any query, a user or catalog name that cannot name
/// one (not `[a-z0-9-]+`: a NUL, which Postgres refuses as a database
/// error, among others), as the unknown name it is.
///
/// # Errors
/// [`OkfError::UnknownName`] or [`OkfError::CatalogNotSelected`].
fn check_names(user: Option<&str>, catalog: &str) -> Result<(), OkfError> {
    if let Some(user) = user.filter(|user| !catalogs::is_valid_name(user)) {
        return Err(users::unknown(user));
    }
    if !catalogs::is_valid_name(catalog) {
        return Err(unknown_catalog(catalog));
    }
    Ok(())
}

/// Every user's ceiling, for the key cache.
///
/// # Arguments
/// * `pool` - the database.
///
/// # Returns
/// The ceilings by user id; a user with an empty ceiling is absent.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn ceilings(pool: &PgPool) -> Result<HashMap<String, Ceiling>, OkfError> {
    let mut by_user: HashMap<String, Ceiling> = HashMap::new();
    for row in ceiling_rows(pool, None).await? {
        // `entry(..).or_default()`: Python's `setdefault(key, {})`.
        by_user
            .entry(row.user_id)
            .or_default()
            .insert(row.catalog, row_level(row.write));
    }
    Ok(by_user)
}

/// One user's ceiling.
///
/// # Arguments
/// * `pool` - the database.
/// * `user` - the user's name.
///
/// # Returns
/// The ceiling (empty when the user reaches nothing).
///
/// # Errors
/// [`OkfError::UnknownName`] for an unknown user; [`OkfError::Database`].
pub async fn ceiling_of(pool: &PgPool, user: &str) -> Result<Ceiling, OkfError> {
    let user_id = users::id_by_name(pool, user).await?;
    Ok(one_ceiling(ceiling_rows(pool, Some(&user_id)).await?))
}

/// One user's ceiling, by id, read inside the caller's transaction: what
/// `store::oauth::grants::create_self_issued` checks a consent against
/// while it holds the user's row.
///
/// # Arguments
/// * `conn` - the open transaction's connection.
/// * `user_id` - the user's id.
///
/// # Returns
/// The ceiling (empty for an unknown user or one who reaches nothing).
///
/// # Errors
/// [`OkfError::Database`].
pub(crate) async fn ceiling_in(
    conn: &mut PgConnection,
    user_id: &str,
) -> Result<Ceiling, OkfError> {
    Ok(one_ceiling(ceiling_rows(conn, Some(user_id)).await?))
}

/// Sets what members of a catalog's owning team may reach, and records
/// `catalog.member_access_changed` and bumps `admin_generation` when it
/// changes.
///
/// # Arguments
/// * `pool` - the database.
/// * `catalog` - the catalog's name.
/// * `access` - the new value.
/// * `actor` - who, for the audit log.
/// * `now` - the server clock.
///
/// # Returns
/// The value it had before.
///
/// # Errors
/// [`OkfError::CatalogNotSelected`] for an unknown (or malformed) catalog;
/// [`OkfError::Database`], a failed audit write included.
pub async fn set_member_access(
    pool: &PgPool,
    catalog: &str,
    access: MemberAccess,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<MemberAccess, OkfError> {
    check_names(None, catalog)?;
    let mut tx = pool.begin().await?;
    audit::lock_people(
        &mut tx,
        actor,
        &People {
            catalogs: &[catalog],
            ..People::default()
        },
    )
    .await?;
    // As in `catalogs::set_owner`: the subquery reads the row as it was,
    // under its row lock, so this returns the value it replaced.
    let previous: Option<(i64, String)> = sqlx::query_as(
        "UPDATE catalogs c SET member_access = $2 \
         FROM (SELECT id, member_access FROM catalogs WHERE name = $1 FOR UPDATE) old \
         WHERE c.id = old.id RETURNING c.id, old.member_access",
    )
    .bind(catalog)
    .bind(access.as_str())
    .fetch_optional(&mut *tx)
    .await?;
    let (catalog_id, previous) = previous.ok_or_else(|| unknown_catalog(catalog))?;
    let previous =
        MemberAccess::parse(&previous).expect("the schema checks catalogs.member_access");
    if previous != access {
        catalogs::record_catalog(
            &mut tx,
            actor,
            CatalogEvent {
                action: Action::CatalogMemberAccessChanged,
                catalog_id,
                catalog,
                details: serde_json::json!({"from": previous.as_str(), "to": access.as_str()}),
            },
            now,
        )
        .await?;
        bump_admin_generation(&mut tx).await?;
    }
    tx.commit().await?;
    Ok(previous)
}

/// A catalog's member access, for the console's catalog drawer.
///
/// # Arguments
/// * `pool` - the database.
/// * `catalog` - the catalog's name.
///
/// # Returns
/// Its `member_access`.
///
/// # Errors
/// [`OkfError::CatalogNotSelected`] for an unknown or malformed catalog
/// name; [`OkfError::Database`].
pub async fn member_access(pool: &PgPool, catalog: &str) -> Result<MemberAccess, OkfError> {
    check_names(None, catalog)?;
    let stored: Option<String> =
        sqlx::query_scalar("SELECT member_access FROM catalogs WHERE name = $1")
            .bind(catalog)
            .fetch_optional(pool)
            .await?;
    let stored = stored.ok_or_else(|| unknown_catalog(catalog))?;
    Ok(MemberAccess::parse(&stored).expect("the schema checks catalogs.member_access"))
}

/// An entitlement to give: what [`entitle`] is asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewEntitlement<'a> {
    /// The user's name.
    pub user: &'a str,
    /// The catalog's name.
    pub catalog: &'a str,
    /// `Read` or `Write`.
    pub access: Access,
}

/// Gives a user a catalog at `access`, or changes the level of one already
/// given; when anything changed, bumps `admin_generation` and records
/// `user.entitlement_set` (in the catalog's owner team's log). An
/// unchanged level writes and records nothing.
///
/// # Arguments
/// * `pool` - the database.
/// * `new` - the user, the catalog and the level.
/// * `actor` - who, for the audit log.
/// * `now` - when it is given (server clock); kept when only the level changes.
///
/// # Errors
/// [`OkfError::Refused`] for `Admin`; [`OkfError::UnknownName`] for an
/// unknown user; [`OkfError::CatalogNotSelected`] for an unknown catalog;
/// [`OkfError::Database`].
pub async fn entitle(
    pool: &PgPool,
    new: &NewEntitlement<'_>,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    // Destructuring copies the fields out (`NewEntitlement` is `Copy`).
    let NewEntitlement {
        user,
        catalog,
        access,
    } = *new;
    if access == Access::Admin {
        return Err(OkfError::Refused {
            reason: "an entitlement is read or write; admin comes only from an admin key"
                .to_string(),
        });
    }
    check_names(Some(user), catalog)?;
    let mut tx = pool.begin().await?;
    let user_id = lock_entitlement_users(&mut tx, user, catalog, actor).await?;
    let catalog_id = catalogs::find(&mut *tx, catalog).await?.id;
    // The `WHERE` on the update makes an unchanged level write nothing, so
    // no row comes back. A row that does: `xmax = 0` is Postgres' mark of a
    // row this statement inserted (an updated row carries this transaction
    // as its `xmax`). Read in the same statement, it cannot race another
    // entitle the way a separate SELECT of the old level could.
    let inserted: Option<bool> = sqlx::query_scalar(
        "INSERT INTO user_catalogs (user_id, catalog_id, access, created_at) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (user_id, catalog_id) DO UPDATE SET access = EXCLUDED.access \
         WHERE user_catalogs.access <> EXCLUDED.access \
         RETURNING xmax = 0",
    )
    .bind(&user_id)
    .bind(catalog_id)
    .bind(access.as_str())
    .bind(now)
    .fetch_optional(&mut *tx)
    .await?;
    // `let ... else`: unchanged (no row) commits nothing and records nothing.
    let Some(inserted) = inserted else {
        return Ok(());
    };
    bump_admin_generation(&mut tx).await?;
    // There are two levels, so a changed level was the other one.
    let previous = match (inserted, access) {
        (true, _) => None,
        (false, Access::Write) => Some(Access::Read.as_str()),
        (false, Access::Read | Access::Admin) => Some(Access::Write.as_str()),
    };
    let details = json!({
        "catalog": catalog,
        "access": access.as_str(),
        "previous": previous,
    });
    // The entry belongs to the catalog's owner team, as a catalog event does.
    let teams = audit::catalog_teams(&mut tx, catalog_id).await?;
    let entry = Entry {
        teams: &teams,
        ..users::user_entry(Action::UserEntitlementSet, &user_id, details)
    };
    audit::record(&mut tx, actor, &entry, now).await?;
    tx.commit().await?;
    Ok(())
}

/// Takes a catalog entitlement from a user (not given is a no-op, which
/// records nothing); when one was removed, bumps `admin_generation` and
/// records `user.entitlement_removed`.
///
/// # Arguments
/// * `pool` - the database.
/// * `user` - the user's name.
/// * `catalog` - the catalog's name.
/// * `actor` - who, for the audit log.
/// * `now` - the server clock.
///
/// # Errors
/// [`OkfError::UnknownName`] for an unknown user;
/// [`OkfError::CatalogNotSelected`] for an unknown catalog;
/// [`OkfError::Database`].
pub async fn unentitle(
    pool: &PgPool,
    user: &str,
    catalog: &str,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    check_names(Some(user), catalog)?;
    let mut tx = pool.begin().await?;
    let user_id = lock_entitlement_users(&mut tx, user, catalog, actor).await?;
    let catalog_id = catalogs::find(&mut *tx, catalog).await?.id;
    let removed = sqlx::query("DELETE FROM user_catalogs WHERE user_id = $1 AND catalog_id = $2")
        .bind(&user_id)
        .bind(catalog_id)
        .execute(&mut *tx)
        .await?;
    if removed.rows_affected() != 0 {
        bump_admin_generation(&mut tx).await?;
        let details = json!({ "catalog": catalog });
        // The entry belongs to the catalog's owner team, as a catalog event does.
        let teams = audit::catalog_teams(&mut tx, catalog_id).await?;
        let entry = Entry {
            teams: &teams,
            ..users::user_entry(Action::UserEntitlementRemoved, &user_id, details)
        };
        audit::record(&mut tx, actor, &entry, now).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Locks the user rows an entitlement change and its entry take: the user
/// (`FOR KEY SHARE` until commit, so a concurrent `users::delete` waits),
/// the actor's rows and the catalog's personal owner, if any (its entry's
/// team), in `audit::lock_people`' order, before `server_state`.
///
/// # Arguments
/// * `conn` - the change's transaction.
/// * `user` - the user's name.
/// * `catalog` - the catalog's name.
/// * `actor` - who acts.
///
/// # Returns
/// The user's id.
///
/// # Errors
/// [`OkfError::UnknownName`]; [`OkfError::Database`].
async fn lock_entitlement_users(
    conn: &mut PgConnection,
    user: &str,
    catalog: &str,
    actor: &AuditActor,
) -> Result<String, OkfError> {
    let people = People {
        target: Some((user, RowLock::KeyShare)),
        catalogs: &[catalog],
        ..People::default()
    };
    // `lock_people` answers `Some` whenever it is given a target.
    let (user_id, _) = audit::lock_people(conn, actor, &people)
        .await?
        .ok_or_else(|| users::unknown(user))?;
    Ok(user_id)
}

/// A user's entitlements, by catalog name.
///
/// # Arguments
/// * `pool` - the database.
/// * `user` - the user's name.
///
/// # Returns
/// Each entitlement, sorted by catalog.
///
/// # Errors
/// [`OkfError::UnknownName`] for an unknown user; [`OkfError::Database`].
pub async fn entitlements(pool: &PgPool, user: &str) -> Result<Vec<Entitlement>, OkfError> {
    let mut conn = pool.acquire().await?;
    entitlements_in(&mut conn, user).await
}

/// Reads entitlements on the caller's connection, so a guarded detail read
/// never borrows another pool slot while holding a user row lock.
///
/// # Errors
/// As [`entitlements`].
pub(crate) async fn entitlements_in(
    conn: &mut PgConnection,
    user: &str,
) -> Result<Vec<Entitlement>, OkfError> {
    let user_id = users::id_by_name(&mut *conn, user).await?;
    let rows: Vec<(String, String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT c.name, e.access, e.created_at FROM user_catalogs e \
         JOIN catalogs c ON c.id = e.catalog_id WHERE e.user_id = $1 ORDER BY c.name",
    )
    .bind(&user_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(catalog, access, created_at)| Entitlement {
            catalog,
            access: Access::parse(&access).expect("the schema checks user_catalogs.access"),
            created_at,
        })
        .collect())
}

/// May `principal` set `catalog`'s member access (or a user's
/// entitlement to it)? The one permission check the CLI, the REST routes
/// and the console use, so team owners can be allowed later here alone.
///
/// # Arguments
/// * `principal` - the caller.
/// * `catalog` - the catalog.
///
/// # Returns
/// `true` for a provisioner or an admin that reaches every catalog.
/// Entitlement routes separately require an every-catalog admin.
pub fn may_set_member_access(principal: &Principal, catalog: &str) -> bool {
    principal.provisioner
        || (matches!(principal.reach, Reach::All)
            && principal.level(catalog) == Some(Access::Admin))
}

/// Caps a credential's levels by a ceiling.
///
/// # Arguments
/// * `grant` - the credential's catalogs and levels.
/// * `ceiling` - the user's ceiling.
///
/// # Returns
/// The catalogs in both, each at the lower of the two levels.
pub fn cap(grant: &BTreeMap<String, Access>, ceiling: &Ceiling) -> BTreeMap<String, Access> {
    grant
        .iter()
        // `filter_map` keeps the `Some`s: a catalog the ceiling lacks drops out.
        .filter_map(|(catalog, level)| {
            ceiling
                .get(catalog)
                .map(|limit| (catalog.clone(), (*level).min(*limit)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{test_db, TEST_TEAM, TEST_USER};
    use crate::store::audit::AuditActor;
    use crate::store::teams::{self, NewTeam, Role};
    use crate::store::users::NewUser;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 12, 12, 0, 0).unwrap()
    }

    /// The map `{catalog: level}` of these pairs.
    fn ceiling(pairs: &[(&str, Access)]) -> Ceiling {
        pairs
            .iter()
            .map(|(catalog, level)| (catalog.to_string(), *level))
            .collect()
    }

    /// `users.id` of `name`.
    async fn id(pool: &PgPool, name: &str) -> String {
        sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// Users alice and bob; team `family` (alice owner, bob member) owns
    /// catalog `shared`; alice's personal team owns `solo`; the fixture
    /// user's team owns `other`.
    async fn family(pool: &PgPool) {
        for name in ["alice", "bob"] {
            users::create(
                pool,
                &NewUser {
                    name,
                    display_name: "",
                },
                &AuditActor::cli(),
                now(),
            )
            .await
            .unwrap();
        }
        let team = NewTeam {
            name: "family",
            display_name: "",
            owner: "alice",
        };
        teams::create(pool, &team, &AuditActor::cli(), now())
            .await
            .unwrap();
        teams::set_member(
            pool,
            &crate::store::teams::Membership {
                team: "family",
                user: "bob",
                role: Role::Member,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        for (catalog, owner) in [
            ("shared", "family"),
            ("solo", "alice"),
            ("other", TEST_TEAM),
        ] {
            catalogs::create(
                pool,
                &crate::store::catalogs::NewCatalog {
                    name: catalog,
                    description: "",
                    owner_team: owner,
                },
                &AuditActor::cli(),
                now(),
            )
            .await
            .unwrap();
        }
    }

    #[test]
    fn cap_keeps_the_lower_level_and_only_catalogs_in_both() {
        let grant = ceiling(&[
            ("pg", Access::Write),
            ("work", Access::Read),
            ("mine", Access::Write),
        ]);
        let limit = ceiling(&[
            ("pg", Access::Read),
            ("work", Access::Write),
            ("docs", Access::Write),
        ]);

        let capped = cap(&grant, &limit);

        assert_eq!(
            capped,
            ceiling(&[("pg", Access::Read), ("work", Access::Read)])
        );
        assert!(cap(&grant, &Ceiling::new()).is_empty());
    }

    #[hegel::test]
    fn cap_never_raises_a_level_or_adds_a_catalog(tc: hegel::TestCase) {
        use hegel::generators;
        let names = ["a", "b", "c", "d"];
        let levels = [Access::Read, Access::Write];
        let draw = || -> Ceiling {
            let drawn: BTreeMap<&str, Access> = tc.draw(generators::btree_maps(
                generators::sampled_from(&names),
                generators::sampled_from(&levels),
            ));
            drawn
                .into_iter()
                .map(|(name, level)| (name.to_string(), level))
                .collect()
        };
        let grant = draw();
        let limit = draw();

        let capped = cap(&grant, &limit);

        for (catalog, level) in &capped {
            assert!(Some(level) <= grant.get(catalog), "never above the grant");
            assert!(Some(level) <= limit.get(catalog), "never above the ceiling");
        }
        for catalog in grant.keys().filter(|catalog| limit.contains_key(*catalog)) {
            assert!(capped.contains_key(catalog), "a catalog in both stays");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_ceiling_is_the_teams_catalogs_at_member_access_and_the_users_entitlements() {
        let db = test_db().await;
        family(&db.pool).await;

        let alice = ceiling_of(&db.pool, "alice").await.unwrap();
        let bob = ceiling_of(&db.pool, "bob").await.unwrap();
        let everyone = ceilings(&db.pool).await.unwrap();

        assert_eq!(
            alice,
            ceiling(&[("shared", Access::Write), ("solo", Access::Write)])
        );
        assert_eq!(bob, ceiling(&[("shared", Access::Write)]));
        assert_eq!(everyone.get(&id(&db.pool, "alice").await), Some(&alice));
        assert_eq!(everyone.get(&id(&db.pool, "bob").await), Some(&bob));
        assert_eq!(
            everyone.get(&id(&db.pool, TEST_USER).await),
            Some(&ceiling(&[("other", Access::Write)]))
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn member_access_none_leaves_a_catalog_out_and_the_higher_level_wins() {
        let db = test_db().await;
        family(&db.pool).await;

        set_member_access(
            &db.pool,
            "shared",
            MemberAccess::Read,
            &AuditActor::cli(),
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        set_member_access(
            &db.pool,
            "solo",
            MemberAccess::None,
            &AuditActor::cli(),
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        entitle(
            &db.pool,
            &NewEntitlement {
                user: "bob",
                catalog: "shared",
                access: Access::Write,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        entitle(
            &db.pool,
            &NewEntitlement {
                user: "bob",
                catalog: "other",
                access: Access::Read,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        entitle(
            &db.pool,
            &NewEntitlement {
                user: "alice",
                catalog: "shared",
                access: Access::Read,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();

        assert_eq!(
            ceiling_of(&db.pool, "alice").await.unwrap(),
            ceiling(&[("shared", Access::Read)])
        );
        assert_eq!(
            ceiling_of(&db.pool, "bob").await.unwrap(),
            ceiling(&[("other", Access::Read), ("shared", Access::Write)])
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_member_access_returns_the_previous_value_and_bumps_only_on_a_change() {
        let db = test_db().await;
        family(&db.pool).await;
        let before = catalogs::admin_generation(&db.pool).await.unwrap();

        let first = set_member_access(
            &db.pool,
            "shared",
            MemberAccess::Read,
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        let again = set_member_access(
            &db.pool,
            "shared",
            MemberAccess::Read,
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        let unknown = set_member_access(
            &db.pool,
            "nope",
            MemberAccess::Read,
            &AuditActor::cli(),
            Utc::now(),
        )
        .await;

        assert_eq!((first, again), (MemberAccess::Write, MemberAccess::Read));
        assert_eq!(
            catalogs::admin_generation(&db.pool).await.unwrap(),
            before + 1
        );
        assert!(
            matches!(unknown, Err(OkfError::CatalogNotSelected { .. })),
            "{unknown:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn entitlements_are_listed_replaced_and_removed_and_each_change_bumps() {
        let db = test_db().await;
        family(&db.pool).await;
        let generation = || catalogs::admin_generation(&db.pool);
        let before = generation().await.unwrap();

        entitle(
            &db.pool,
            &NewEntitlement {
                user: "bob",
                catalog: "other",
                access: Access::Read,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        entitle(
            &db.pool,
            &NewEntitlement {
                user: "bob",
                catalog: "other",
                access: Access::Read,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let after_entitle = generation().await.unwrap();
        entitle(
            &db.pool,
            &NewEntitlement {
                user: "bob",
                catalog: "other",
                access: Access::Write,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let listed = entitlements(&db.pool, "bob").await.unwrap();
        unentitle(&db.pool, "bob", "other", &AuditActor::cli(), now())
            .await
            .unwrap();
        let after_unentitle = generation().await.unwrap();
        unentitle(&db.pool, "bob", "other", &AuditActor::cli(), now())
            .await
            .unwrap();

        assert_eq!(
            after_entitle,
            before + 1,
            "the same entitlement twice is one change"
        );
        assert_eq!(
            listed,
            [Entitlement {
                catalog: "other".to_string(),
                access: Access::Write,
                created_at: now(),
            }]
        );
        assert_eq!(after_unentitle, before + 3);
        assert_eq!(
            generation().await.unwrap(),
            after_unentitle,
            "removing nothing is a no-op"
        );
        assert!(entitlements(&db.pool, "bob").await.unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_admin_entitlement_unknown_users_and_unknown_catalogs_are_refused() {
        let db = test_db().await;
        family(&db.pool).await;

        let admin = entitle(
            &db.pool,
            &NewEntitlement {
                user: "bob",
                catalog: "other",
                access: Access::Admin,
            },
            &AuditActor::cli(),
            now(),
        )
        .await;
        let nobody = entitle(
            &db.pool,
            &NewEntitlement {
                user: "carol",
                catalog: "other",
                access: Access::Read,
            },
            &AuditActor::cli(),
            now(),
        )
        .await;
        let nowhere = entitle(
            &db.pool,
            &NewEntitlement {
                user: "bob",
                catalog: "nope",
                access: Access::Read,
            },
            &AuditActor::cli(),
            now(),
        )
        .await;
        let no_ceiling = ceiling_of(&db.pool, "carol").await;
        let no_list = entitlements(&db.pool, "carol").await;
        let no_removal = unentitle(&db.pool, "bob", "nope", &AuditActor::cli(), now()).await;

        assert!(matches!(admin, Err(OkfError::Refused { .. })), "{admin:?}");
        for refused in [nobody, no_ceiling.map(|_| ()), no_list.map(|_| ())] {
            assert!(
                matches!(refused, Err(OkfError::UnknownName { noun: "user", .. })),
                "{refused:?}"
            );
        }
        for refused in [nowhere, no_removal] {
            assert!(
                matches!(refused, Err(OkfError::CatalogNotSelected { .. })),
                "{refused:?}"
            );
        }
    }

    /// Checks one step of a ceiling change: `admin_generation` moved past
    /// `last` (which becomes the new value) and the user is at `expected`.
    async fn moved(pool: &PgPool, step: &str, last: &mut i64, user_id: &str, expected: Ceiling) {
        let generation = catalogs::admin_generation(pool).await.unwrap();
        assert!(generation > *last, "{step}: the generation moved");
        *last = generation;
        let found = ceilings(pool)
            .await
            .unwrap()
            .remove(user_id)
            .unwrap_or_default();
        assert_eq!(found, expected, "{step}");
    }

    /// Every write that can change a ceiling's inputs (team membership,
    /// catalog creation, ownership and deletion, member access,
    /// entitlements and user deletion) changes `ceilings()` and bumps
    /// `admin_generation`, so the key cache re-caps within one poll.
    #[tokio::test(flavor = "multi_thread")]
    async fn every_write_that_moves_a_ceiling_bumps_the_generation() {
        let db = test_db().await;
        family(&db.pool).await;
        let pool = &db.pool;
        let bob = id(pool, "bob").await;
        let mut last = catalogs::admin_generation(pool).await.unwrap();
        let shared_write = ceiling(&[("shared", Access::Write)]);

        teams::remove_member(pool, "family", "bob", &AuditActor::cli(), Utc::now())
            .await
            .unwrap();
        moved(pool, "leaving the team", &mut last, &bob, Ceiling::new()).await;

        teams::set_member(
            pool,
            &crate::store::teams::Membership {
                team: "family",
                user: "bob",
                role: Role::Member,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        moved(
            pool,
            "joining the team",
            &mut last,
            &bob,
            shared_write.clone(),
        )
        .await;

        catalogs::create(
            pool,
            &crate::store::catalogs::NewCatalog {
                name: "extra",
                description: "",
                owner_team: "family",
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let both = ceiling(&[("extra", Access::Write), ("shared", Access::Write)]);
        moved(pool, "a new catalog of the team", &mut last, &bob, both).await;

        catalogs::set_owner(pool, "extra", TEST_TEAM, &AuditActor::cli(), Utc::now())
            .await
            .unwrap();
        moved(pool, "a new owner", &mut last, &bob, shared_write.clone()).await;

        entitle(
            pool,
            &NewEntitlement {
                user: "bob",
                catalog: "extra",
                access: Access::Read,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let entitled = ceiling(&[("extra", Access::Read), ("shared", Access::Write)]);
        moved(pool, "an entitlement", &mut last, &bob, entitled).await;

        set_member_access(
            pool,
            "shared",
            MemberAccess::Read,
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        let lowered = ceiling(&[("extra", Access::Read), ("shared", Access::Read)]);
        moved(pool, "member access", &mut last, &bob, lowered).await;

        // The entitlement to `extra` cascades with the catalog.
        catalogs::delete(pool, "extra", false, &AuditActor::cli(), Utc::now())
            .await
            .unwrap();
        let shared_read = ceiling(&[("shared", Access::Read)]);
        moved(
            pool,
            "deleting a catalog",
            &mut last,
            &bob,
            shared_read.clone(),
        )
        .await;

        entitle(
            pool,
            &NewEntitlement {
                user: "bob",
                catalog: "other",
                access: Access::Write,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        last = catalogs::admin_generation(pool).await.unwrap();
        unentitle(pool, "bob", "other", &AuditActor::cli(), now())
            .await
            .unwrap();
        moved(
            pool,
            "removing an entitlement",
            &mut last,
            &bob,
            shared_read,
        )
        .await;

        // A deleted user's entitlements go with it.
        users::create(
            pool,
            &NewUser {
                name: "carol",
                display_name: "",
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        entitle(
            pool,
            &NewEntitlement {
                user: "carol",
                catalog: "other",
                access: Access::Read,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let carol = id(pool, "carol").await;
        assert!(ceilings(pool).await.unwrap().contains_key(&carol));
        last = catalogs::admin_generation(pool).await.unwrap();
        users::delete(pool, "carol", false, &AuditActor::cli(), now())
            .await
            .unwrap();
        moved(pool, "deleting a user", &mut last, &carol, Ceiling::new()).await;
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM user_catalogs WHERE user_id = $1")
            .bind(&carol)
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(left, 0, "the entitlement cascaded");
    }

    #[test]
    fn member_access_round_trips_its_spelling() {
        for access in [MemberAccess::None, MemberAccess::Read, MemberAccess::Write] {
            assert_eq!(MemberAccess::parse(access.as_str()), Some(access));
        }
        assert_eq!(MemberAccess::parse("admin"), None);
        assert_eq!(MemberAccess::None.level(), None);
        assert_eq!(MemberAccess::Write.level(), Some(Access::Write));
    }

    #[test]
    fn only_an_admin_that_reaches_every_catalog_may_set_member_access() {
        let every = crate::mcp::testing::principal_as("root", Access::Admin, &[], None);
        let scoped = crate::mcp::testing::principal_as("scoped", Access::Admin, &["pg"], None);
        let writer = crate::mcp::testing::principal_as("writer", Access::Write, &[], None);

        assert!(may_set_member_access(&every, "pg"));
        assert!(!may_set_member_access(&scoped, "pg"));
        assert!(!may_set_member_access(&writer, "pg"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_member_access_records_a_change_and_none_for_no_change() {
        let db = test_db().await;
        family(&db.pool).await;
        let actor = AuditActor::cli();

        set_member_access(&db.pool, "shared", MemberAccess::Read, &actor, now())
            .await
            .unwrap();
        set_member_access(&db.pool, "shared", MemberAccess::Read, &actor, now())
            .await
            .unwrap();
        let bad_name =
            set_member_access(&db.pool, "sh\0ared", MemberAccess::None, &actor, now()).await;

        assert!(matches!(bad_name, Err(OkfError::CatalogNotSelected { .. })));
        let changed =
            crate::store::audit::testing::entries(&db.pool, "catalog.member_access_changed").await;
        assert_eq!(changed.len(), 1, "{changed:?}");
        assert_eq!(changed[0].target.as_deref(), Some("shared"));
        assert_eq!(changed[0].teams, ["family"]);
        assert_eq!(
            changed[0].details,
            serde_json::json!({"from": "write", "to": "read"})
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_audit_write_fails_the_member_access_change() {
        let db = test_db().await;
        family(&db.pool).await;
        crate::http::api::testing::fail_on(&db.pool, "BEFORE INSERT ON audit_entries FOR EACH ROW")
            .await;

        let refused = set_member_access(
            &db.pool,
            "shared",
            MemberAccess::None,
            &AuditActor::cli(),
            now(),
        )
        .await;

        assert!(refused.is_err());
        assert_eq!(
            member_access(&db.pool, "shared").await.unwrap(),
            MemberAccess::Write,
            "the change rolled back"
        );
    }

    /// Gives `user` catalog `catalog` at `access`, as the CLI.
    async fn give(pool: &PgPool, user: &str, catalog: &str, access: Access) {
        let new = NewEntitlement {
            user,
            catalog,
            access,
        };
        entitle(pool, &new, &AuditActor::cli(), now())
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn entitle_records_the_level_and_the_one_before_and_nothing_unchanged() {
        let db = test_db().await;
        family(&db.pool).await;

        give(&db.pool, "bob", "shared", Access::Read).await;
        give(&db.pool, "bob", "shared", Access::Read).await;
        give(&db.pool, "bob", "shared", Access::Write).await;
        give(&db.pool, "bob", "shared", Access::Read).await;

        let entries = crate::store::audit::testing::entries(&db.pool, "user.entitlement_set").await;
        assert_eq!(entries.len(), 3, "an unchanged level records nothing");
        assert!(entries
            .iter()
            .all(|entry| entry.person.as_deref() == Some("bob")));
        assert_eq!(entries[0].target_kind, "user");
        assert_eq!(entries[0].teams, ["family"], "the catalog's owner team");
        assert_eq!(
            entries[0].details,
            json!({"catalog": "shared", "access": "read", "previous": null})
        );
        assert_eq!(
            entries[1].details,
            json!({"catalog": "shared", "access": "write", "previous": "read"})
        );
        assert_eq!(
            entries[2].details,
            json!({"catalog": "shared", "access": "read", "previous": "write"})
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unentitle_records_a_removal_and_nothing_when_none_was_given() {
        let db = test_db().await;
        family(&db.pool).await;
        let cli = AuditActor::cli();

        unentitle(&db.pool, "bob", "other", &cli, now())
            .await
            .unwrap();
        give(&db.pool, "bob", "other", Access::Write).await;
        unentitle(&db.pool, "bob", "other", &cli, now())
            .await
            .unwrap();
        unentitle(&db.pool, "bob", "other", &cli, now())
            .await
            .unwrap();

        let entries =
            crate::store::audit::testing::entries(&db.pool, "user.entitlement_removed").await;
        assert_eq!(entries.len(), 1, "removing nothing records nothing");
        assert_eq!(entries[0].person.as_deref(), Some("bob"));
        assert_eq!(entries[0].teams, [TEST_TEAM], "the catalog's owner team");
        assert_eq!(entries[0].details, json!({"catalog": "other"}));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_audit_write_fails_the_entitlement_change() {
        let db = test_db().await;
        family(&db.pool).await;
        give(&db.pool, "bob", "other", Access::Read).await;
        crate::http::api::testing::fail_on(&db.pool, "BEFORE INSERT ON audit_entries FOR EACH ROW")
            .await;
        let cli = AuditActor::cli();
        let new = NewEntitlement {
            user: "bob",
            catalog: "shared",
            access: Access::Read,
        };

        let given = entitle(&db.pool, &new, &cli, now()).await;
        let taken = unentitle(&db.pool, "bob", "other", &cli, now()).await;

        assert!(given.is_err() && taken.is_err());
        let left: Vec<String> = entitlements(&db.pool, "bob")
            .await
            .unwrap()
            .into_iter()
            .map(|entitlement| entitlement.catalog)
            .collect();
        assert_eq!(left, ["other"], "both changes rolled back");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_admins_entitlement_change_and_a_concurrent_set_admin_both_finish() {
        use crate::store::users::tests::{admin_session_actor, queued_with_set_admin};
        let db = test_db().await;
        family(&db.pool).await;
        let staff = admin_session_actor(&db.pool, "staff").await;
        let pool = db.pool.clone();
        let actor = staff.clone();
        // `solo` belongs to alice's personal team: its entry locks alice too.
        let given = async move {
            let new = NewEntitlement {
                user: "bob",
                catalog: "solo",
                access: Access::Read,
            };
            entitle(&pool, &new, &actor, now()).await
        };
        let pool = db.pool.clone();
        let taken = async move { unentitle(&pool, "bob", "solo", &staff, now()).await };

        let (entitled, first) = queued_with_set_admin(&db.pool, "alice", given).await;
        let (unentitled, second) = queued_with_set_admin(&db.pool, "bob", taken).await;

        entitled.expect("the entitlement");
        first.expect("the first promotion");
        unentitled.expect("the removal");
        second.expect("the second promotion");
        let set = crate::store::audit::testing::entries(&db.pool, "user.entitlement_set").await;
        assert_eq!(
            set[0].teams,
            ["alice"],
            "the personal team reads as its person"
        );
    }
}
