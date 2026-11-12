//! Teams: the owners of catalogs (spec "Data model"). Every user has a
//! personal team, made with it; shared teams have owners and members.
//! Teams are addressed by name; ids stay inside SQL. Every write locks the
//! team row `FOR UPDATE`, so changes to one team's membership run one at a
//! time and the last-owner check cannot race.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};

use crate::error::OkfError;
use crate::store::catalogs::bump_admin_generation;
use crate::store::users;

/// A member's role in a team.
// `clap::ValueEnum` parses `--role owner|member`; `serde::Deserialize` with
// `rename_all = "lowercase"` reads `"owner"`/`"member"` from REST bodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// May be the team's administrative contact; every team keeps one.
    Owner,
    /// A plain member.
    Member,
}

impl Role {
    /// The stored spelling.
    ///
    /// # Returns
    /// `"owner"` or `"member"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Member => "member",
        }
    }

    /// Parses the stored spelling.
    ///
    /// # Arguments
    /// * `text` - a `team_members.role` value.
    ///
    /// # Returns
    /// The role, or `None` for anything other than [`Role::as_str`]'s spellings.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "owner" => Some(Role::Owner),
            "member" => Some(Role::Member),
            _ => None,
        }
    }
}

/// A team to create.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewTeam<'a> {
    /// `[a-z0-9-]+`, not taken by a user or team.
    pub name: &'a str,
    /// Free text shown beside the name.
    pub display_name: &'a str,
    /// The user who becomes its first owner.
    pub owner: &'a str,
}

/// One membership of a team.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// The user's name.
    pub user: String,
    /// Its role.
    pub role: Role,
}

/// A stored team.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamRecord {
    /// Its name.
    pub name: String,
    /// Its display name (may be empty).
    pub display_name: String,
    /// The user whose personal team this is.
    pub personal_of: Option<String>,
    /// When it was created (server clock).
    pub created_at: DateTime<Utc>,
    /// Sorted by user name.
    pub members: Vec<Member>,
    /// Names of the catalogs it owns, sorted.
    pub catalogs: Vec<String>,
}

/// What [`set_member`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberChange {
    /// The user was not a member and now is.
    Added,
    /// The user was a member with the other role.
    RoleChanged,
    /// The user already had that role; nothing was written.
    Unchanged,
}

/// A team row without its members (they come from a second query).
#[derive(sqlx::FromRow)]
struct TeamRow {
    name: String,
    display_name: String,
    personal_of: Option<String>,
    created_at: DateTime<Utc>,
    catalogs: Vec<String>,
}

/// A membership row, with its team's name for grouping.
#[derive(sqlx::FromRow)]
struct MemberRow {
    team: String,
    user: String,
    role: String,
}

// The row and member SELECTs shared by `list` and `find` (macros so
// `concat!` can append each query's tail at compile time).
macro_rules! select_teams {
    () => {
        "SELECT t.name, t.display_name, p.name AS personal_of, t.created_at, \
                COALESCE((SELECT array_agg(c.name ORDER BY c.name) FROM catalogs c \
                          WHERE c.owner_team_id = t.id), '{}'::text[]) AS catalogs \
         FROM teams t LEFT JOIN users p ON p.id = t.personal_of"
    };
}
macro_rules! select_members {
    () => {
        "SELECT t.name AS team, u.name AS user, m.role \
         FROM team_members m \
         JOIN teams t ON t.id = m.team_id \
         JOIN users u ON u.id = m.user_id"
    };
}

/// Joins team rows with their membership rows (two queries, so there is no
/// query per team).
fn assemble(rows: Vec<TeamRow>, member_rows: Vec<MemberRow>) -> Vec<TeamRecord> {
    // The member rows arrive sorted by user, so each team's list stays sorted.
    let mut members: HashMap<String, Vec<Member>> = HashMap::with_capacity(rows.len());
    for row in member_rows {
        members.entry(row.team).or_default().push(Member {
            user: row.user,
            role: Role::parse(&row.role).expect("the schema checks team_members.role"),
        });
    }
    rows.into_iter()
        .map(|row| TeamRecord {
            members: members.remove(&row.name).unwrap_or_default(),
            name: row.name,
            display_name: row.display_name,
            personal_of: row.personal_of,
            created_at: row.created_at,
            catalogs: row.catalogs,
        })
        .collect()
}

/// Creates a shared team with `owner` as its first owner, and bumps the
/// administrative generation, in one transaction.
///
/// # Arguments
/// * `pool` - the database.
/// * `new` - its name, display name and owner.
/// * `now` - the creation time (server clock).
///
/// # Returns
/// The new team.
///
/// # Errors
/// [`OkfError::Refused`] for an invalid name or one a user or team already
/// has (including one taken by a concurrent create);
/// [`OkfError::UnknownName`] for an unknown owner; [`OkfError::Database`].
pub async fn create(
    pool: &PgPool,
    new: &NewTeam<'_>,
    now: DateTime<Utc>,
) -> Result<TeamRecord, OkfError> {
    users::check_name("team", new.name)?;
    let mut tx = pool.begin().await?;
    users::check_free(&mut tx, new.name).await?;
    let owner_id = users::id_of(&mut tx, new.owner).await?;
    let inserted = sqlx::query_scalar::<_, String>(
        "INSERT INTO teams (name, display_name, created_at) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(new.name)
    .bind(new.display_name)
    .bind(now)
    .fetch_one(&mut *tx)
    .await;
    let team_id = users::or_name_taken(inserted, new.name)?;
    insert_member(&mut tx, &team_id, &owner_id, Role::Owner, now).await?;
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(TeamRecord {
        name: new.name.to_string(),
        display_name: new.display_name.to_string(),
        personal_of: None,
        created_at: now,
        members: vec![Member {
            user: new.owner.to_string(),
            role: Role::Owner,
        }],
        catalogs: Vec::new(),
    })
}

/// Every team, by name, with its members and catalogs.
///
/// # Arguments
/// * `pool` - the database.
///
/// # Returns
/// The teams, sorted by name.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn list(pool: &PgPool) -> Result<Vec<TeamRecord>, OkfError> {
    let rows = sqlx::query_as::<_, TeamRow>(concat!(select_teams!(), " ORDER BY t.name"))
        .fetch_all(pool)
        .await?;
    let member_rows =
        sqlx::query_as::<_, MemberRow>(concat!(select_members!(), " ORDER BY u.name"))
            .fetch_all(pool)
            .await?;
    Ok(assemble(rows, member_rows))
}

/// The team called `name`, with its members and catalogs.
///
/// # Arguments
/// * `pool` - the database.
/// * `name` - the team's name.
///
/// # Returns
/// The team.
///
/// # Errors
/// [`OkfError::UnknownName`] if there is none; [`OkfError::Database`].
pub async fn find(pool: &PgPool, name: &str) -> Result<TeamRecord, OkfError> {
    let row = sqlx::query_as::<_, TeamRow>(concat!(select_teams!(), " WHERE t.name = $1"))
        .bind(name)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| unknown(name))?;
    let member_rows = sqlx::query_as::<_, MemberRow>(concat!(
        select_members!(),
        " WHERE t.name = $1 ORDER BY u.name"
    ))
    .bind(name)
    .fetch_all(pool)
    .await?;
    // `assemble` returns exactly one record for one row.
    Ok(assemble(vec![row], member_rows)
        .pop()
        .expect("one team row gives one record"))
}

/// Adds `user` to `team` with `role`, or changes its role, and bumps the
/// administrative generation when anything changed.
///
/// # Arguments
/// * `pool` - the database.
/// * `team` - the team's name.
/// * `user` - the user's name.
/// * `role` - the role it should have.
/// * `now` - the membership's creation time, when added (server clock).
///
/// # Returns
/// Whether the user was added, its role changed, or nothing changed.
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
    let locked = lock_team(&mut tx, team).await?;
    let user_id = users::id_of(&mut tx, user).await?;
    let change = match current_role(&mut tx, &locked.id, &user_id).await? {
        None => {
            insert_member(&mut tx, &locked.id, &user_id, role, now).await?;
            MemberChange::Added
        }
        // Dropping `tx` here rolls back the (read-only) transaction.
        Some(existing) if existing == role => return Ok(MemberChange::Unchanged),
        Some(existing) => {
            if existing == Role::Owner {
                refuse_last_owner(&mut tx, &locked.id, team, user).await?;
            }
            sqlx::query("UPDATE team_members SET role = $3 WHERE team_id = $1 AND user_id = $2")
                .bind(&locked.id)
                .bind(&user_id)
                .bind(role.as_str())
                .execute(&mut *tx)
                .await?;
            MemberChange::RoleChanged
        }
    };
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(change)
}

/// Removes `user` from `team`, and bumps the administrative generation.
///
/// # Arguments
/// * `pool` - the database.
/// * `team` - the team's name.
/// * `user` - the user's name.
///
/// # Errors
/// [`OkfError::UnknownName`] for the team or user; [`OkfError::Refused`]
/// when the user is not a member, is the team's last owner, or is the user
/// whose personal team this is; [`OkfError::Database`].
pub async fn remove_member(pool: &PgPool, team: &str, user: &str) -> Result<(), OkfError> {
    let mut tx = pool.begin().await?;
    let locked = lock_team(&mut tx, team).await?;
    let user_id = users::id_of(&mut tx, user).await?;
    if locked.personal_of.as_deref() == Some(user_id.as_str()) {
        return Err(OkfError::Refused {
            reason: format!("{team:?} is {user:?}'s personal team; it cannot leave it"),
        });
    }
    match current_role(&mut tx, &locked.id, &user_id).await? {
        None => {
            return Err(OkfError::Refused {
                reason: format!("{user:?} is not a member of team {team:?}"),
            })
        }
        Some(Role::Owner) => refuse_last_owner(&mut tx, &locked.id, team, user).await?,
        Some(Role::Member) => {}
    }
    sqlx::query("DELETE FROM team_members WHERE team_id = $1 AND user_id = $2")
        .bind(&locked.id)
        .bind(&user_id)
        .execute(&mut *tx)
        .await?;
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Deletes a shared team that owns no catalog (its memberships cascade),
/// and bumps the administrative generation (spec "Deletion").
///
/// # Arguments
/// * `pool` - the database.
/// * `name` - the team's name.
///
/// # Errors
/// [`OkfError::UnknownName`]; [`OkfError::Refused`] for a personal team
/// (it goes with its user) or one that owns a catalog; [`OkfError::Database`].
pub async fn delete(pool: &PgPool, name: &str) -> Result<(), OkfError> {
    let mut tx = pool.begin().await?;
    let locked = lock_team(&mut tx, name).await?;
    if locked.personal_of.is_some() {
        return Err(OkfError::Refused {
            reason: format!("{name:?} is a personal team; it is deleted with its user"),
        });
    }
    let owned: i64 = sqlx::query_scalar("SELECT count(*) FROM catalogs WHERE owner_team_id = $1")
        .bind(&locked.id)
        .fetch_one(&mut *tx)
        .await?;
    if owned > 0 {
        return Err(OkfError::Refused {
            reason: format!("team {name:?} owns {owned} catalog(s); give them another owner first"),
        });
    }
    sqlx::query("DELETE FROM teams WHERE id = $1")
        .bind(&locked.id)
        .execute(&mut *tx)
        .await?;
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(())
}

/// A team row held `FOR UPDATE` by the caller's transaction.
struct LockedTeam {
    /// Its `teams.id`.
    id: String,
    /// The `users.id` whose personal team it is, if any.
    personal_of: Option<String>,
}

/// Locks the team row `FOR UPDATE`, so concurrent membership changes to one
/// team run one at a time (the last-owner check cannot race), and so a
/// concurrent catalog assignment to it waits for a delete.
///
/// # Errors
/// [`OkfError::UnknownName`]; [`OkfError::Database`].
async fn lock_team(conn: &mut PgConnection, name: &str) -> Result<LockedTeam, OkfError> {
    let (id, personal_of) = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT id, personal_of FROM teams WHERE name = $1 FOR UPDATE",
    )
    .bind(name)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| unknown(name))?;
    Ok(LockedTeam { id, personal_of })
}

/// The user's role in the team, or `None` if it is not a member.
///
/// # Errors
/// [`OkfError::Database`].
async fn current_role(
    conn: &mut PgConnection,
    team_id: &str,
    user_id: &str,
) -> Result<Option<Role>, OkfError> {
    let role: Option<String> =
        sqlx::query_scalar("SELECT role FROM team_members WHERE team_id = $1 AND user_id = $2")
            .bind(team_id)
            .bind(user_id)
            .fetch_optional(&mut *conn)
            .await?;
    // `Option::map` applies the closure only to `Some`, like a None-safe call.
    Ok(role.map(|text| Role::parse(&text).expect("the schema checks team_members.role")))
}

/// Inserts one membership row.
///
/// # Errors
/// [`OkfError::Database`].
async fn insert_member(
    conn: &mut PgConnection,
    team_id: &str,
    user_id: &str,
    role: Role,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    sqlx::query(
        "INSERT INTO team_members (team_id, user_id, role, created_at) VALUES ($1, $2, $3, $4)",
    )
    .bind(team_id)
    .bind(user_id)
    .bind(role.as_str())
    .bind(now)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Refuses when `user` is the only owner of the team. The caller holds the
/// team row from [`lock_team`], so the count cannot change under it.
///
/// # Errors
/// [`OkfError::Refused`] for the last owner; [`OkfError::Database`].
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

/// The error for a team name that names no team.
fn unknown(name: &str) -> OkfError {
    OkfError::UnknownName {
        noun: "team",
        name: name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{test_db, TEST_USER};
    use crate::store::users::{self, NewUser};

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-11T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    async fn family(pool: &PgPool) {
        users::create(
            pool,
            &NewUser {
                name: "alice",
                display_name: "",
            },
            now(),
        )
        .await
        .unwrap();
        create(
            pool,
            &NewTeam {
                name: "family",
                display_name: "The family",
                owner: "alice",
            },
            now(),
        )
        .await
        .unwrap();
    }

    #[test]
    fn role_spellings_round_trip() {
        for role in [Role::Owner, Role::Member] {
            assert_eq!(Role::parse(role.as_str()), Some(role));
        }
        assert_eq!(Role::parse("admin"), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_team_starts_with_its_owner() {
        let db = test_db().await;
        family(&db.pool).await;

        let team = find(&db.pool, "family").await.unwrap();

        assert_eq!(team.personal_of, None);
        assert_eq!(team.display_name, "The family");
        assert_eq!(
            team.members
                .iter()
                .map(|m| (m.user.as_str(), m.role))
                .collect::<Vec<_>>(),
            [("alice", Role::Owner)]
        );
        assert!(team.catalogs.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_groups_members_by_team() {
        let db = test_db().await;
        family(&db.pool).await;
        set_member(&db.pool, "family", TEST_USER, Role::Member, now())
            .await
            .unwrap();

        let teams = list(&db.pool).await.unwrap();

        let summary: Vec<(&str, Vec<&str>)> = teams
            .iter()
            .map(|t| {
                let members = t.members.iter().map(|m| m.user.as_str()).collect();
                (t.name.as_str(), members)
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("alice", vec!["alice"]),
                ("family", vec!["alice", TEST_USER]),
                (crate::db::TEST_TEAM, vec![TEST_USER]),
            ]
        );
        assert_eq!(find(&db.pool, "family").await.unwrap(), teams[1]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unknown_owner_is_unknown_name() {
        let db = test_db().await;
        let made = create(
            &db.pool,
            &NewTeam {
                name: "x",
                display_name: "",
                owner: "nobody",
            },
            now(),
        )
        .await;
        assert!(matches!(
            made,
            Err(OkfError::UnknownName { noun: "user", .. })
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_member_adds_then_changes_the_role_then_is_unchanged() {
        let db = test_db().await;
        family(&db.pool).await;

        let added = set_member(&db.pool, "family", TEST_USER, Role::Member, now())
            .await
            .unwrap();
        let changed = set_member(&db.pool, "family", TEST_USER, Role::Owner, now())
            .await
            .unwrap();
        let same = set_member(&db.pool, "family", TEST_USER, Role::Owner, now())
            .await
            .unwrap();

        assert_eq!(
            (added, changed, same),
            (
                MemberChange::Added,
                MemberChange::RoleChanged,
                MemberChange::Unchanged
            )
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_member_on_an_unknown_team_or_user_is_unknown_name() {
        let db = test_db().await;
        family(&db.pool).await;

        let no_team = set_member(&db.pool, "nowhere", TEST_USER, Role::Member, now()).await;
        let no_user = set_member(&db.pool, "family", "nobody", Role::Member, now()).await;

        assert!(matches!(
            no_team,
            Err(OkfError::UnknownName { noun: "team", .. })
        ));
        assert!(matches!(
            no_user,
            Err(OkfError::UnknownName { noun: "user", .. })
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_last_owner_cannot_be_removed_or_demoted() {
        let db = test_db().await;
        family(&db.pool).await;

        let removed = remove_member(&db.pool, "family", "alice").await;
        let demoted = set_member(&db.pool, "family", "alice", Role::Member, now()).await;

        assert!(
            matches!(removed, Err(OkfError::Refused { .. })),
            "{removed:?}"
        );
        assert!(
            matches!(demoted, Err(OkfError::Refused { .. })),
            "{demoted:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_removal_of_two_owners_leaves_one() {
        // Both removals would pass an unlocked "more than one owner" check;
        // the team-row lock makes the second see the first's delete.
        let db = test_db().await;
        family(&db.pool).await;
        set_member(&db.pool, "family", TEST_USER, Role::Owner, now())
            .await
            .unwrap();

        let (first, second) = tokio::join!(
            remove_member(&db.pool, "family", "alice"),
            remove_member(&db.pool, "family", TEST_USER),
        );

        assert_eq!(
            [first.is_ok(), second.is_ok()]
                .iter()
                .filter(|ok| **ok)
                .count(),
            1,
            "{first:?} {second:?}"
        );
        let owners = find(&db.pool, "family").await.unwrap().members;
        assert_eq!(owners.len(), 1);
        assert_eq!(owners[0].role, Role::Owner);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_personal_teams_own_user_cannot_be_removed() {
        let db = test_db().await;
        family(&db.pool).await;
        set_member(&db.pool, "alice", TEST_USER, Role::Owner, now())
            .await
            .unwrap();

        let removed = remove_member(&db.pool, "alice", "alice").await;

        assert!(
            matches!(removed, Err(OkfError::Refused { .. })),
            "{removed:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_member_is_removed_but_a_non_member_is_refused() {
        let db = test_db().await;
        family(&db.pool).await;
        set_member(&db.pool, "family", TEST_USER, Role::Member, now())
            .await
            .unwrap();

        remove_member(&db.pool, "family", TEST_USER).await.unwrap();
        let again = remove_member(&db.pool, "family", TEST_USER).await;

        assert_eq!(find(&db.pool, "family").await.unwrap().members.len(), 1);
        assert!(matches!(again, Err(OkfError::Refused { .. })), "{again:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_personal_team_is_never_deleted_but_a_shared_one_is() {
        let db = test_db().await;
        family(&db.pool).await;

        let personal = delete(&db.pool, "alice").await;
        delete(&db.pool, "family").await.unwrap();

        assert!(matches!(personal, Err(OkfError::Refused { .. })));
        assert!(matches!(
            find(&db.pool, "family").await,
            Err(OkfError::UnknownName { .. })
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_team_that_owns_a_catalog_is_not_deleted() {
        let db = test_db().await;
        family(&db.pool).await;
        users::tests::seed_catalog_owned_by(&db.pool, "family").await;
        let generation = crate::store::catalogs::admin_generation(&db.pool)
            .await
            .unwrap();

        let refused = delete(&db.pool, "family").await;

        assert!(
            matches!(&refused, Err(OkfError::Refused { reason }) if reason.contains("catalog")),
            "{refused:?}"
        );
        let team = find(&db.pool, "family").await.unwrap();
        assert_eq!(team.catalogs, ["shared"]);
        assert_eq!(team.members.len(), 1);
        assert_eq!(
            crate::store::catalogs::admin_generation(&db.pool)
                .await
                .unwrap(),
            generation
        );
    }
}
