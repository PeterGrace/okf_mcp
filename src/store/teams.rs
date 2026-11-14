//! Teams: the owners of catalogs (spec "Data model"). Every user has a
//! personal team, made with it; shared teams have owners and members.
//! Teams are addressed by name; ids stay inside SQL. Every write locks the
//! team row `FOR UPDATE`, so changes to one team's membership run one at a
//! time and the last-owner check cannot race.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};

use crate::error::OkfError;
use crate::store::audit::{self, Action, AuditActor, Entry, Target};
use crate::store::catalogs::{bump_admin_generation, is_valid_name};
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

/// A membership to set: [`set_member`]'s input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Membership<'a> {
    /// The team's name.
    pub team: &'a str,
    /// The user's name.
    pub user: &'a str,
    /// The role it should have.
    pub role: Role,
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
    /// Its suspension; `None` while it is active.
    pub suspension: Option<Suspension>,
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

/// A team row without its members (they come from a second query).
#[derive(sqlx::FromRow)]
struct TeamRow {
    name: String,
    display_name: String,
    personal_of: Option<String>,
    created_at: DateTime<Utc>,
    suspended_at: Option<DateTime<Utc>>,
    suspension_notice: Option<String>,
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
                t.suspended_at, t.suspension_notice, \
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
            // `map` runs only for `Some`: a team with a time is suspended.
            // Struct-literal fields are evaluated in the order written, so
            // this moves `suspension_notice` out of `row` before the
            // fields below move the rest.
            suspension: row.suspended_at.map(|since| Suspension {
                since,
                notice: row.suspension_notice,
            }),
            name: row.name,
            display_name: row.display_name,
            personal_of: row.personal_of,
            created_at: row.created_at,
            catalogs: row.catalogs,
        })
        .collect()
}

/// Creates a shared team with `owner` as its first owner, records
/// `team.created`, and bumps the administrative generation, in one
/// transaction.
///
/// # Arguments
/// * `pool` - the database.
/// * `new` - its name, display name and owner.
/// * `actor` - who, for the audit log; a provisioner is refused an admin owner.
/// * `now` - the creation time (server clock).
///
/// # Returns
/// The new team.
///
/// # Errors
/// [`OkfError::Refused`] for an invalid name or one a user or team already
/// has (including one taken by a concurrent create);
/// [`OkfError::UnknownName`] for an unknown (or malformed) owner;
/// [`OkfError::Forbidden`] for a provisioner and an admin owner;
/// [`OkfError::Database`], a failed audit write included.
pub async fn create(
    pool: &PgPool,
    new: &NewTeam<'_>,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<TeamRecord, OkfError> {
    users::check_name("team", new.name)?;
    let mut tx = pool.begin().await?;
    audit::lock_actor(&mut tx, actor).await?;
    users::check_free(&mut tx, new.name).await?;
    // `FOR KEY SHARE` on the owner conflicts with `users::set_admin`'s
    // `FOR UPDATE`, so the guard below cannot race a promotion.
    let (owner_id, owner_is_admin) = users::id_and_admin_of(&mut tx, new.owner).await?;
    audit::refuse_admin_target(actor, owner_is_admin, new.owner)?;
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
    record_team(
        &mut tx,
        actor,
        TeamEvent {
            action: Action::TeamCreated,
            team: new.name,
            person: Some(&owner_id),
            details: serde_json::json!({}),
        },
        now,
    )
    .await?;
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
        suspension: None,
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
/// [`OkfError::UnknownName`] if there is none (a malformed name included);
/// [`OkfError::Database`].
pub async fn find(pool: &PgPool, name: &str) -> Result<TeamRecord, OkfError> {
    // A malformed name (a NUL, which Postgres refuses) names no team: the
    // same 404 as an unknown one, never a database error (issue #339).
    if !is_valid_name(name) {
        return Err(unknown(name));
    }
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

/// Adds a user to a team with a role, or changes its role, records
/// `team.member_added` or `team.role_changed`, and bumps the administrative
/// generation, when anything changed.
///
/// # Arguments
/// * `pool` - the database.
/// * `membership` - the team, the user and the role it should have.
/// * `actor` - who, for the audit log; a provisioner is refused an admin user.
/// * `now` - the membership's creation time, when added (server clock).
///
/// # Returns
/// Whether the user was added, its role changed, or nothing changed.
///
/// # Errors
/// [`OkfError::UnknownName`] for the team or user (a malformed name
/// included); [`OkfError::Forbidden`] for a provisioner and an admin user;
/// [`OkfError::Refused`] for demoting the team's last owner;
/// [`OkfError::Database`], a failed audit write included.
pub async fn set_member(
    pool: &PgPool,
    membership: &Membership<'_>,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<MemberChange, OkfError> {
    let Membership { team, user, role } = *membership;
    let mut tx = pool.begin().await?;
    // Users before the team: `users::delete` locks the user and then its
    // teams, so taking them in the other order could deadlock.
    audit::lock_actor(&mut tx, actor).await?;
    let (user_id, is_admin) = users::id_and_admin_of(&mut tx, user).await?;
    audit::refuse_admin_target(actor, is_admin, user)?;
    let locked = lock_team(&mut tx, team).await?;
    let (change, action, from) = match current_role(&mut tx, &locked.id, &user_id).await? {
        None => {
            insert_member(&mut tx, &locked.id, &user_id, role, now).await?;
            (MemberChange::Added, Action::TeamMemberAdded, None)
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
            (
                MemberChange::RoleChanged,
                Action::TeamRoleChanged,
                Some(existing.as_str()),
            )
        }
    };
    record_team(
        &mut tx,
        actor,
        TeamEvent {
            action,
            team,
            person: Some(&user_id),
            // `from` is `None` (JSON null) for a new member.
            details: serde_json::json!({"role": role.as_str(), "from": from}),
        },
        now,
    )
    .await?;
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(change)
}

/// Removes a user from a team, records `team.member_removed`, and bumps
/// the administrative generation.
///
/// # Arguments
/// * `pool` - the database.
/// * `team` - the team's name.
/// * `user` - the user's name.
/// * `actor` - who, for the audit log; a provisioner is refused an admin user.
/// * `now` - the server clock.
///
/// # Errors
/// [`OkfError::UnknownName`] for the team or user (a malformed name
/// included); [`OkfError::Forbidden`] for a provisioner and an admin user;
/// [`OkfError::Refused`] when the user is not a member, is the team's last
/// owner, or is the user whose personal team this is;
/// [`OkfError::Database`], a failed audit write included.
pub async fn remove_member(
    pool: &PgPool,
    team: &str,
    user: &str,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    let mut tx = pool.begin().await?;
    // Users before the team, as in `set_member`.
    audit::lock_actor(&mut tx, actor).await?;
    let (user_id, is_admin) = users::id_and_admin_of(&mut tx, user).await?;
    audit::refuse_admin_target(actor, is_admin, user)?;
    let locked = lock_team(&mut tx, team).await?;
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
    record_team(
        &mut tx,
        actor,
        TeamEvent {
            action: Action::TeamMemberRemoved,
            team,
            person: Some(&user_id),
            details: serde_json::json!({}),
        },
        now,
    )
    .await?;
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Deletes a shared team that owns no catalog (its memberships cascade),
/// records `team.deleted`, and bumps the administrative generation (spec
/// "Deletion").
///
/// # Arguments
/// * `pool` - the database.
/// * `name` - the team's name.
/// * `actor` - who, for the audit log.
/// * `now` - the server clock.
///
/// # Errors
/// [`OkfError::UnknownName`] (a malformed name included);
/// [`OkfError::Refused`] for a personal team (it goes with its user) or one
/// that owns a catalog; [`OkfError::Database`], a failed audit write
/// included.
pub async fn delete(
    pool: &PgPool,
    name: &str,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    let mut tx = pool.begin().await?;
    audit::lock_actor(&mut tx, actor).await?;
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
    // Recorded while the row exists: the entry's team reference is
    // resolved from it.
    record_team(
        &mut tx,
        actor,
        TeamEvent {
            action: Action::TeamDeleted,
            team: name,
            person: None,
            details: serde_json::json!({}),
        },
        now,
    )
    .await?;
    sqlx::query("DELETE FROM teams WHERE id = $1")
        .bind(&locked.id)
        .execute(&mut *tx)
        .await?;
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(())
}

/// A notice as stored: trimmed, `None` when blank.
///
/// # Arguments
/// * `notice` - the notice as given.
///
/// # Returns
/// The trimmed notice, or `None` for a missing or blank one.
///
/// # Errors
/// [`OkfError::Refused`] for one longer than [`MAX_NOTICE_CHARS`], or one
/// holding a NUL character (Postgres text cannot store it, so it would
/// otherwise be a database error, a 500).
fn clean_notice(notice: Option<&str>) -> Result<Option<&str>, OkfError> {
    // `filter` turns a blank (after trimming) notice into `None`.
    let notice = notice.map(str::trim).filter(|text| !text.is_empty());
    if let Some(text) = notice {
        if text.contains('\0') {
            return Err(OkfError::Refused {
                reason: "a suspension notice cannot contain a NUL character".to_string(),
            });
        }
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
/// (keeping its first `suspended_at`), records `team.suspended`, and bumps
/// the administrative generation. Suspending a team already suspended with
/// the same notice changes nothing, so it writes and records nothing.
///
/// # Arguments
/// * `pool` - the database.
/// * `name` - the team's name.
/// * `notice` - what members are told; blank or `None` for no notice.
/// * `actor` - who, for the audit log.
/// * `now` - the suspension time, if it was not suspended (server clock).
///
/// # Returns
/// The team as it now is.
///
/// # Errors
/// [`OkfError::UnknownName`] (a malformed name included);
/// [`OkfError::Refused`] for a notice over [`MAX_NOTICE_CHARS`] or holding
/// a NUL; [`OkfError::Database`], a failed audit write included.
pub async fn suspend(
    pool: &PgPool,
    name: &str,
    notice: Option<&str>,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<TeamRecord, OkfError> {
    let notice = clean_notice(notice)?;
    let mut tx = pool.begin().await?;
    audit::lock_actor(&mut tx, actor).await?;
    let locked = lock_team(&mut tx, name).await?;
    // The state before the write, under the team's lock.
    let (already, previous_notice): (bool, Option<String>) = sqlx::query_as(
        "SELECT suspended_at IS NOT NULL, suspension_notice FROM teams WHERE id = $1",
    )
    .bind(&locked.id)
    .fetch_one(&mut *tx)
    .await?;
    if already && previous_notice.as_deref() == notice {
        // Dropping `tx` rolls back the read-only transaction.
        drop(tx);
        return find(pool, name).await;
    }
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
    record_team(
        &mut tx,
        actor,
        TeamEvent {
            action: Action::TeamSuspended,
            team: name,
            person: None,
            // Whether there is a notice, never its text.
            details: serde_json::json!({"notice": notice.is_some(), "already": already}),
        },
        now,
    )
    .await?;
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    find(pool, name).await
}

/// Lifts a team's suspension, and records `team.unsuspended` and bumps
/// the administrative generation when there was one.
///
/// # Arguments
/// * `pool` - the database.
/// * `name` - the team's name.
/// * `actor` - who, for the audit log.
/// * `now` - the server clock.
///
/// # Returns
/// The team as it now is.
///
/// # Errors
/// [`OkfError::UnknownName`] (a malformed name included);
/// [`OkfError::Database`], a failed audit write included.
pub async fn unsuspend(
    pool: &PgPool,
    name: &str,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<TeamRecord, OkfError> {
    let mut tx = pool.begin().await?;
    audit::lock_actor(&mut tx, actor).await?;
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
        record_team(
            &mut tx,
            actor,
            TeamEvent {
                action: Action::TeamUnsuspended,
                team: name,
                person: None,
                details: serde_json::json!({}),
            },
            now,
        )
        .await?;
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
/// Lock order: a caller that also needs a user resolves it first
/// ([`users::id_of`], `FOR KEY SHARE`), because `users::delete` locks the
/// user and then its personal team. For the same reason a personal team's
/// user is locked here before the team ([`lock_team_person`]): recording an
/// entry about the team resolves that person under `FOR KEY SHARE`.
///
/// # Errors
/// [`OkfError::UnknownName`]; [`OkfError::Database`].
async fn lock_team(conn: &mut PgConnection, name: &str) -> Result<LockedTeam, OkfError> {
    // As in `find`: a malformed name names no team (issue #339).
    if !is_valid_name(name) {
        return Err(unknown(name));
    }
    let person = lock_team_person(conn, name).await?;
    let (id, personal_of) = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT id, personal_of FROM teams WHERE name = $1 FOR UPDATE",
    )
    .bind(name)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| unknown(name))?;
    // A team's `personal_of` never changes; a different one means the team
    // was deleted and another made under its name meanwhile.
    if personal_of != person {
        return Err(OkfError::Busy {
            reason: format!("team {name:?} was replaced while it was being locked"),
        });
    }
    Ok(LockedTeam { id, personal_of })
}

/// Locks the user whose personal team `name` is, if it is one, `FOR KEY
/// SHARE`, before the caller locks the team: the order `users::delete`
/// takes them in (the user, then its personal team). Without it, an audit
/// entry naming the team (whose `audit_teams` row resolves that person)
/// would lock the user after the team and could deadlock with the user's
/// deletion.
///
/// # Arguments
/// * `conn` - the caller's transaction.
/// * `name` - a valid team name.
///
/// # Returns
/// The `users.id` whose personal team it is, or `None` for a shared team
/// (or no team; the caller's lock then finds none).
///
/// # Errors
/// [`OkfError::Database`].
async fn lock_team_person(conn: &mut PgConnection, name: &str) -> Result<Option<String>, OkfError> {
    Ok(sqlx::query_scalar(
        "SELECT u.id FROM users u WHERE u.id = (SELECT personal_of FROM teams WHERE name = $1) \
         FOR KEY SHARE",
    )
    .bind(name)
    .fetch_optional(&mut *conn)
    .await?)
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

/// The id of the team called `name`, inside a caller's transaction.
///
/// The row is held `FOR KEY SHARE` until that transaction ends, so a
/// concurrent [`delete`] (which locks the team `FOR UPDATE`) waits for the
/// caller and then sees what it wrote (a catalog the team now owns); one that
/// got there first leaves no row, which is `UnknownName` here rather than a
/// foreign-key failure at the caller's write.
///
/// # Arguments
/// * `conn` - the caller's transaction (pass `&mut tx`).
/// * `name` - the team's name.
///
/// # Returns
/// Its `teams.id`.
///
/// # Errors
/// [`OkfError::UnknownName`]; [`OkfError::Database`].
pub(crate) async fn id_of(conn: &mut PgConnection, name: &str) -> Result<String, OkfError> {
    // As in `find`: a malformed name names no team (issue #339).
    if !is_valid_name(name) {
        return Err(unknown(name));
    }
    // The person before the team, as in `lock_team`: a catalog entry's
    // teams resolve a personal team's person.
    lock_team_person(conn, name).await?;
    sqlx::query_scalar("SELECT id FROM teams WHERE name = $1 FOR KEY SHARE")
        .bind(name)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| unknown(name))
}

/// One team event to record: [`record_team`]'s input.
struct TeamEvent<'a> {
    /// What happened.
    action: Action,
    /// The team's name; it must still exist.
    team: &'a str,
    /// The `users.id` the event is also about (a member, the owner).
    person: Option<&'a str>,
    /// The entry's details.
    details: serde_json::Value,
}

/// Records an entry about a team in the change's transaction: the team is
/// both the target and the one team the entry belongs to.
///
/// # Errors
/// [`OkfError::Database`] (which fails the change).
async fn record_team(
    conn: &mut PgConnection,
    actor: &AuditActor,
    event: TeamEvent<'_>,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    let team_ref = audit::team_ref(conn, event.team).await?;
    audit::record(
        conn,
        actor,
        &Entry {
            action: event.action,
            target: Target::Team(event.team),
            person: event.person,
            teams: &[team_ref],
            details: event.details,
        },
        now,
    )
    .await?;
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
            &AuditActor::cli(),
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
        set_member(
            &db.pool,
            &crate::store::teams::Membership {
                team: "family",
                user: TEST_USER,
                role: Role::Member,
            },
            &AuditActor::cli(),
            now(),
        )
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
            &AuditActor::cli(),
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

        let added = set_member(
            &db.pool,
            &crate::store::teams::Membership {
                team: "family",
                user: TEST_USER,
                role: Role::Member,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let changed = set_member(
            &db.pool,
            &crate::store::teams::Membership {
                team: "family",
                user: TEST_USER,
                role: Role::Owner,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let same = set_member(
            &db.pool,
            &crate::store::teams::Membership {
                team: "family",
                user: TEST_USER,
                role: Role::Owner,
            },
            &AuditActor::cli(),
            now(),
        )
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

        let no_team = set_member(
            &db.pool,
            &crate::store::teams::Membership {
                team: "nowhere",
                user: TEST_USER,
                role: Role::Member,
            },
            &AuditActor::cli(),
            now(),
        )
        .await;
        let no_user = set_member(
            &db.pool,
            &crate::store::teams::Membership {
                team: "family",
                user: "nobody",
                role: Role::Member,
            },
            &AuditActor::cli(),
            now(),
        )
        .await;

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

        let removed =
            remove_member(&db.pool, "family", "alice", &AuditActor::cli(), Utc::now()).await;
        let demoted = set_member(
            &db.pool,
            &crate::store::teams::Membership {
                team: "family",
                user: "alice",
                role: Role::Member,
            },
            &AuditActor::cli(),
            now(),
        )
        .await;

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
        set_member(
            &db.pool,
            &crate::store::teams::Membership {
                team: "family",
                user: TEST_USER,
                role: Role::Owner,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();

        let actor = AuditActor::cli();
        let (first, second) = tokio::join!(
            remove_member(&db.pool, "family", "alice", &actor, now()),
            remove_member(&db.pool, "family", TEST_USER, &actor, now()),
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
        set_member(
            &db.pool,
            &crate::store::teams::Membership {
                team: "alice",
                user: TEST_USER,
                role: Role::Owner,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();

        let removed =
            remove_member(&db.pool, "alice", "alice", &AuditActor::cli(), Utc::now()).await;

        assert!(
            matches!(removed, Err(OkfError::Refused { .. })),
            "{removed:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_member_is_removed_but_a_non_member_is_refused() {
        let db = test_db().await;
        family(&db.pool).await;
        set_member(
            &db.pool,
            &crate::store::teams::Membership {
                team: "family",
                user: TEST_USER,
                role: Role::Member,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();

        remove_member(
            &db.pool,
            "family",
            TEST_USER,
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        let again = remove_member(
            &db.pool,
            "family",
            TEST_USER,
            &AuditActor::cli(),
            Utc::now(),
        )
        .await;

        assert_eq!(find(&db.pool, "family").await.unwrap().members.len(), 1);
        assert!(matches!(again, Err(OkfError::Refused { .. })), "{again:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_personal_team_is_never_deleted_but_a_shared_one_is() {
        let db = test_db().await;
        family(&db.pool).await;

        let personal = delete(&db.pool, "alice", &AuditActor::cli(), Utc::now()).await;
        delete(&db.pool, "family", &AuditActor::cli(), Utc::now())
            .await
            .unwrap();

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

        let refused = delete(&db.pool, "family", &AuditActor::cli(), Utc::now()).await;

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

    /// Which membership change races the user's deletion.
    #[derive(Debug, Clone, Copy)]
    enum Change {
        SetMember,
        RemoveMember,
    }

    /// Deletes `alice` the way `users::delete` takes its locks (the user
    /// `FOR UPDATE`, then her personal team `FOR UPDATE`) while `change` on
    /// her personal team runs between the two, and returns that change's
    /// outcome.
    async fn race_membership_with_deleting_alice(change: Change) -> Result<(), OkfError> {
        let db = test_db().await;
        users::create(
            &db.pool,
            &NewUser {
                name: "alice",
                display_name: "",
            },
            now(),
        )
        .await
        .unwrap();
        let mut deleting = db.pool.begin().await.unwrap();
        sqlx::query("SELECT id FROM users WHERE name = 'alice' FOR UPDATE")
            .execute(&mut *deleting)
            .await
            .unwrap();
        // `tokio::spawn` runs the change concurrently, like a thread. It must
        // wait on the user without holding alice's team: with the team taken
        // first, the next statement and the change would deadlock.
        let pool = db.pool.clone();
        let changing = tokio::spawn(async move {
            match change {
                Change::SetMember => set_member(
                    &pool,
                    &crate::store::teams::Membership {
                        team: "alice",
                        user: "alice",
                        role: Role::Owner,
                    },
                    &AuditActor::cli(),
                    now(),
                )
                .await
                .map(|_| ()),
                Change::RemoveMember => {
                    remove_member(&pool, "alice", "alice", &AuditActor::cli(), Utc::now()).await
                }
            }
        });
        crate::store::catalogs::tests::until_a_lock_is_awaited(&db.pool).await;

        // A deadlock makes Postgres abort one side (after `deadlock_timeout`);
        // the timeout only bounds a hang.
        let team_lock = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            sqlx::query(
                "SELECT id FROM teams WHERE personal_of = \
                 (SELECT id FROM users WHERE name = 'alice') FOR UPDATE",
            )
            .execute(&mut *deleting),
        )
        .await
        .expect("the team lock does not hang");
        assert!(team_lock.is_ok(), "{change:?}: {team_lock:?}");
        sqlx::query(
            "DELETE FROM teams WHERE personal_of = (SELECT id FROM users WHERE name = 'alice')",
        )
        .execute(&mut *deleting)
        .await
        .unwrap();
        sqlx::query("DELETE FROM users WHERE name = 'alice'")
            .execute(&mut *deleting)
            .await
            .unwrap();
        deleting.commit().await.unwrap();
        changing.await.unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_membership_change_racing_a_user_delete_waits_instead_of_deadlocking() {
        for change in [Change::SetMember, Change::RemoveMember] {
            let outcome = race_membership_with_deleting_alice(change).await;

            // The change waited on the user, then found it gone: not a
            // deadlock or foreign-key error (a 500).
            assert!(
                matches!(&outcome, Err(OkfError::UnknownName { noun: "user", .. })),
                "{change:?}: {outcome:?}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn suspend_keeps_the_first_time_and_replaces_the_notice() {
        let db = test_db().await;
        family(&db.pool).await;
        let later = now() + chrono::TimeDelta::hours(1);
        let before = crate::store::catalogs::admin_generation(&db.pool)
            .await
            .unwrap();

        let first = suspend(
            &db.pool,
            "family",
            Some("Contact the operator."),
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let after_first = crate::store::catalogs::admin_generation(&db.pool)
            .await
            .unwrap();
        let again = suspend(
            &db.pool,
            "family",
            Some("Still paused."),
            &AuditActor::cli(),
            later,
        )
        .await
        .unwrap();

        assert!(after_first > before, "suspending bumps the generation");

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

        let blank = suspend(&db.pool, "family", Some("   "), &AuditActor::cli(), now())
            .await
            .unwrap();
        let long = "x".repeat(MAX_NOTICE_CHARS + 1);
        let refused = suspend(&db.pool, "family", Some(&long), &AuditActor::cli(), now()).await;

        let nul = suspend(&db.pool, "family", Some("a\0b"), &AuditActor::cli(), now()).await;
        let at_limit = "\u{e9}".repeat(MAX_NOTICE_CHARS);
        let multibyte = suspend(
            &db.pool,
            "family",
            Some(&at_limit),
            &AuditActor::cli(),
            now(),
        )
        .await;

        assert_eq!(blank.suspension.unwrap().notice, None);
        assert!(
            matches!(refused, Err(OkfError::Refused { .. })),
            "{refused:?}"
        );
        assert!(matches!(nul, Err(OkfError::Refused { .. })), "{nul:?}");
        // 2000 two-byte characters: the limit counts characters, not bytes.
        let multibyte = multibyte.unwrap().suspension.unwrap().notice.unwrap();
        assert_eq!(multibyte.chars().count(), MAX_NOTICE_CHARS);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_schema_refuses_a_notice_without_a_suspension_or_over_the_limit() {
        // The store never writes either; the constraints guard hand edits.
        let db = test_db().await;
        family(&db.pool).await;
        let long = "x".repeat(MAX_NOTICE_CHARS + 1);

        let orphan = sqlx::query("UPDATE teams SET suspension_notice = 'x' WHERE name = 'family'")
            .execute(&db.pool)
            .await;
        let too_long = sqlx::query(
            "UPDATE teams SET suspended_at = now(), suspension_notice = $1 WHERE name = 'family'",
        )
        .bind(&long)
        .execute(&db.pool)
        .await;

        assert!(orphan.is_err(), "a notice needs a suspension");
        assert!(
            too_long.is_err(),
            "a notice is at most {MAX_NOTICE_CHARS} characters"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unsuspend_lifts_and_is_a_no_op_when_active() {
        let db = test_db().await;
        family(&db.pool).await;
        suspend(&db.pool, "family", None, &AuditActor::cli(), now())
            .await
            .unwrap();
        let before = crate::store::catalogs::admin_generation(&db.pool)
            .await
            .unwrap();

        let lifted = unsuspend(&db.pool, "family", &AuditActor::cli(), Utc::now())
            .await
            .unwrap();
        let after_lift = crate::store::catalogs::admin_generation(&db.pool)
            .await
            .unwrap();
        let unchanged = unsuspend(&db.pool, "family", &AuditActor::cli(), Utc::now())
            .await
            .unwrap();
        let after_noop = crate::store::catalogs::admin_generation(&db.pool)
            .await
            .unwrap();

        assert_eq!(lifted.suspension, None);
        assert_eq!(unchanged.suspension, None);
        assert!(after_lift > before, "lifting bumps the generation");
        assert_eq!(after_noop, after_lift, "lifting nothing does not");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn suspending_an_unknown_team_is_unknown_name() {
        let db = test_db().await;

        let suspended = suspend(&db.pool, "nobody", None, &AuditActor::cli(), now()).await;
        let lifted = unsuspend(&db.pool, "nobody", &AuditActor::cli(), Utc::now()).await;

        assert!(matches!(
            suspended,
            Err(OkfError::UnknownName { noun: "team", .. })
        ));
        assert!(matches!(
            lifted,
            Err(OkfError::UnknownName { noun: "team", .. })
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn suspended_catalogs_names_each_catalog_of_a_suspended_team() {
        let db = test_db().await;
        family(&db.pool).await;
        crate::store::catalogs::create(
            &db.pool,
            &crate::store::catalogs::NewCatalog {
                name: "shared",
                description: "",
                owner_team: "family",
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        crate::store::catalogs::create(
            &db.pool,
            &crate::store::catalogs::NewCatalog {
                name: "mine",
                description: "",
                owner_team: TEST_USER,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        suspend(
            &db.pool,
            "family",
            Some("Paused."),
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();

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

    // ---- The audit log (M14 Task 6b) ----

    use crate::store::audit::testing::entries;
    use crate::store::audit::{Source, Via};
    use serde_json::json;

    /// How long a race test waits for a held lock before it fails.
    const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

    /// Creates user `name` (and its personal team of the same name).
    async fn seed_user(pool: &PgPool, name: &str) {
        users::create(
            pool,
            &NewUser {
                name,
                display_name: "",
            },
            now(),
        )
        .await
        .unwrap();
    }

    /// Makes `name` an admin user directly, without `users::set_admin`.
    async fn make_admin(pool: &PgPool, name: &str) {
        sqlx::query("UPDATE users SET is_admin = true WHERE name = $1")
            .bind(name)
            .execute(pool)
            .await
            .unwrap();
    }

    /// A provisioner, as Task 5's keys will set it.
    fn provisioner() -> AuditActor {
        AuditActor {
            provisioner: true,
            ..AuditActor::cli()
        }
    }

    /// The `users.id` of `name`.
    async fn user_id(pool: &PgPool, name: &str) -> String {
        sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// A console session of user `name`, as `AuditActor::of` builds it.
    async fn session_of(pool: &PgPool, name: &str) -> AuditActor {
        AuditActor {
            source: Source::Console,
            user_id: Some(user_id(pool, name).await),
            label: name.to_string(),
            via: Via::Session,
            operator: true,
            ..AuditActor::cli()
        }
    }

    fn membership<'a>(team: &'a str, user: &'a str, role: Role) -> Membership<'a> {
        Membership { team, user, role }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_records_team_created_with_its_owner() {
        let db = test_db().await;

        family(&db.pool).await;

        let recorded = entries(&db.pool, "team.created").await;
        assert_eq!(recorded.len(), 1, "{recorded:?}");
        assert_eq!(recorded[0].target_kind, "team");
        assert_eq!(recorded[0].target.as_deref(), Some("family"));
        assert_eq!(recorded[0].person.as_deref(), Some("alice"));
        assert_eq!(recorded[0].teams, ["family"]);
        assert_eq!(recorded[0].details, json!({}));
        assert_eq!(recorded[0].source, "cli");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_member_records_an_addition_and_a_role_change_and_none_for_no_change() {
        let db = test_db().await;
        family(&db.pool).await;
        let actor = AuditActor::cli();

        set_member(
            &db.pool,
            &membership("family", TEST_USER, Role::Member),
            &actor,
            now(),
        )
        .await
        .unwrap();
        set_member(
            &db.pool,
            &membership("family", TEST_USER, Role::Owner),
            &actor,
            now(),
        )
        .await
        .unwrap();
        let unchanged = set_member(
            &db.pool,
            &membership("family", TEST_USER, Role::Owner),
            &actor,
            now(),
        )
        .await
        .unwrap();

        assert_eq!(unchanged, MemberChange::Unchanged);
        let added = entries(&db.pool, "team.member_added").await;
        assert_eq!(added.len(), 1, "{added:?}");
        assert_eq!(added[0].target.as_deref(), Some("family"));
        assert_eq!(added[0].person.as_deref(), Some(TEST_USER));
        assert_eq!(added[0].teams, ["family"]);
        assert_eq!(added[0].details, json!({"role": "member", "from": null}));
        let changed = entries(&db.pool, "team.role_changed").await;
        assert_eq!(changed.len(), 1, "no entry for no change: {changed:?}");
        assert_eq!(changed[0].person.as_deref(), Some(TEST_USER));
        assert_eq!(
            changed[0].details,
            json!({"role": "owner", "from": "member"})
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn remove_member_records_one_entry_and_none_when_refused() {
        let db = test_db().await;
        family(&db.pool).await;
        let actor = AuditActor::cli();
        set_member(
            &db.pool,
            &membership("family", TEST_USER, Role::Member),
            &actor,
            now(),
        )
        .await
        .unwrap();

        remove_member(&db.pool, "family", TEST_USER, &actor, now())
            .await
            .unwrap();
        let not_a_member = remove_member(&db.pool, "family", TEST_USER, &actor, now()).await;
        let last_owner = remove_member(&db.pool, "family", "alice", &actor, now()).await;

        assert!(matches!(not_a_member, Err(OkfError::Refused { .. })));
        assert!(matches!(last_owner, Err(OkfError::Refused { .. })));
        let removed = entries(&db.pool, "team.member_removed").await;
        assert_eq!(removed.len(), 1, "{removed:?}");
        assert_eq!(removed[0].target.as_deref(), Some("family"));
        assert_eq!(removed[0].person.as_deref(), Some(TEST_USER));
        assert_eq!(removed[0].teams, ["family"]);
        assert_eq!(removed[0].details, json!({}));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_records_the_team_before_it_goes_and_nothing_when_refused() {
        let db = test_db().await;
        family(&db.pool).await;
        let actor = AuditActor::cli();

        let personal = delete(&db.pool, "alice", &actor, now()).await;
        delete(&db.pool, "family", &actor, now()).await.unwrap();

        assert!(matches!(personal, Err(OkfError::Refused { .. })));
        let deleted = entries(&db.pool, "team.deleted").await;
        assert_eq!(deleted.len(), 1, "{deleted:?}");
        // The team row is gone; its `audit_teams` row keeps the name.
        assert_eq!(deleted[0].target.as_deref(), Some("family"));
        assert_eq!(deleted[0].teams, ["family"]);
        assert_eq!(deleted[0].details, json!({}));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn suspend_records_each_change_and_nothing_for_a_repeat_or_a_refusal() {
        let db = test_db().await;
        family(&db.pool).await;
        let actor = AuditActor::cli();
        let long = "x".repeat(MAX_NOTICE_CHARS + 1);

        suspend(&db.pool, "family", Some("Paused."), &actor, now())
            .await
            .unwrap();
        let generation = crate::store::catalogs::admin_generation(&db.pool)
            .await
            .unwrap();
        let repeat = suspend(&db.pool, "family", Some(" Paused. "), &actor, now())
            .await
            .unwrap();
        let after_repeat = crate::store::catalogs::admin_generation(&db.pool)
            .await
            .unwrap();
        suspend(&db.pool, "family", Some("Still paused."), &actor, now())
            .await
            .unwrap();
        suspend(&db.pool, "family", Some("   "), &actor, now())
            .await
            .unwrap();
        let nul = suspend(&db.pool, "family", Some("a\0b"), &actor, now()).await;
        let too_long = suspend(&db.pool, "family", Some(&long), &actor, now()).await;
        let bad_name = suspend(&db.pool, "fam\0ily", None, &actor, now()).await;

        assert_eq!(
            repeat.suspension.unwrap().notice.as_deref(),
            Some("Paused.")
        );
        assert_eq!(after_repeat, generation, "a repeat writes nothing");
        assert!(matches!(nul, Err(OkfError::Refused { .. })), "{nul:?}");
        assert!(matches!(too_long, Err(OkfError::Refused { .. })));
        assert!(matches!(bad_name, Err(OkfError::UnknownName { .. })));
        let suspended = entries(&db.pool, "team.suspended").await;
        let details: Vec<_> = suspended.iter().map(|entry| &entry.details).collect();
        assert_eq!(
            details,
            [
                &json!({"notice": true, "already": false}),
                &json!({"notice": true, "already": true}),
                &json!({"notice": false, "already": true}),
            ]
        );
        assert_eq!(suspended[0].target.as_deref(), Some("family"));
        assert_eq!(suspended[0].teams, ["family"]);
        assert!(
            !suspended
                .iter()
                .any(|e| e.details.to_string().contains("aused")),
            "never the notice's text"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unsuspend_records_a_lift_and_nothing_for_an_active_team() {
        let db = test_db().await;
        family(&db.pool).await;
        let actor = AuditActor::cli();
        suspend(&db.pool, "family", None, &actor, now())
            .await
            .unwrap();

        unsuspend(&db.pool, "family", &actor, now()).await.unwrap();
        unsuspend(&db.pool, "family", &actor, now()).await.unwrap();
        let bad_name = unsuspend(&db.pool, "fam\0ily", &actor, now()).await;

        assert!(matches!(bad_name, Err(OkfError::UnknownName { .. })));
        let lifted = entries(&db.pool, "team.unsuspended").await;
        assert_eq!(lifted.len(), 1, "{lifted:?}");
        assert_eq!(lifted[0].target.as_deref(), Some("family"));
        assert_eq!(lifted[0].teams, ["family"]);
        assert_eq!(lifted[0].details, json!({}));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_audit_write_fails_the_team_change() {
        let db = test_db().await;
        family(&db.pool).await;
        crate::http::api::testing::fail_on(&db.pool, "BEFORE INSERT ON audit_entries FOR EACH ROW")
            .await;
        let actor = AuditActor::cli();

        let added = set_member(
            &db.pool,
            &membership("family", TEST_USER, Role::Member),
            &actor,
            now(),
        )
        .await;
        let suspended = suspend(&db.pool, "family", None, &actor, now()).await;
        let deleted = delete(&db.pool, "family", &actor, now()).await;

        assert!(added.is_err() && suspended.is_err() && deleted.is_err());
        let team = find(&db.pool, "family").await.unwrap();
        assert_eq!(team.members.len(), 1, "the addition rolled back");
        assert_eq!(team.suspension, None, "the suspension rolled back");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_provisioner_cannot_make_an_admin_an_owner_or_change_an_admins_membership() {
        let db = test_db().await;
        family(&db.pool).await;
        seed_user(&db.pool, "staff").await;
        set_member(
            &db.pool,
            &membership("family", "staff", Role::Member),
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        make_admin(&db.pool, "staff").await;
        let provisioner = provisioner();
        let owned_by_staff = NewTeam {
            name: "ops",
            display_name: "",
            owner: "staff",
        };

        let created = create(&db.pool, &owned_by_staff, &provisioner, now()).await;
        let promoted = set_member(
            &db.pool,
            &membership("family", "staff", Role::Owner),
            &provisioner,
            now(),
        )
        .await;
        let removed = remove_member(&db.pool, "family", "staff", &provisioner, now()).await;
        // A plain user is still the provisioner's to manage.
        let plain = set_member(
            &db.pool,
            &membership("family", TEST_USER, Role::Member),
            &provisioner,
            now(),
        )
        .await;

        for refused in [created.map(|_| ()), promoted.map(|_| ()), removed] {
            assert!(
                matches!(refused, Err(OkfError::Forbidden { .. })),
                "{refused:?}"
            );
        }
        assert_eq!(plain.unwrap(), MemberChange::Added);
        assert!(matches!(
            find(&db.pool, "ops").await,
            Err(OkfError::UnknownName { .. })
        ));
        let members = find(&db.pool, "family").await.unwrap().members;
        assert!(members.contains(&Member {
            user: "staff".to_string(),
            role: Role::Member,
        }));
        assert_eq!(
            entries(&db.pool, "team.created").await.len(),
            1,
            "family only"
        );
        assert_eq!(entries(&db.pool, "team.role_changed").await.len(), 0);
        assert_eq!(entries(&db.pool, "team.member_removed").await.len(), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_provisioners_membership_change_waits_for_a_promotion_in_flight() {
        let db = test_db().await;
        family(&db.pool).await;
        seed_user(&db.pool, "staff").await;
        // A promotion in flight: `users::set_admin` holds the row `FOR UPDATE`.
        let mut promoting = db.pool.begin().await.unwrap();
        sqlx::query("SELECT id FROM users WHERE name = 'staff' FOR UPDATE")
            .execute(&mut *promoting)
            .await
            .unwrap();
        sqlx::query("UPDATE users SET is_admin = true WHERE name = 'staff'")
            .execute(&mut *promoting)
            .await
            .unwrap();
        let pool = db.pool.clone();
        let adding = tokio::spawn(async move {
            let staff = membership("family", "staff", Role::Member);
            set_member(&pool, &staff, &provisioner(), now()).await
        });
        tokio::time::timeout(
            LOCK_WAIT,
            crate::store::catalogs::tests::until_a_lock_is_awaited(&db.pool),
        )
        .await
        .expect("the provisioner's change waits on the promotion's row lock");

        promoting.commit().await.unwrap();
        let added = tokio::time::timeout(LOCK_WAIT, adding)
            .await
            .expect("the change finishes once the promotion commits")
            .unwrap();

        // It read the flag after the promotion: refused, not added.
        assert!(
            matches!(added, Err(OkfError::Forbidden { .. })),
            "{added:?}"
        );
        assert_eq!(find(&db.pool, "family").await.unwrap().members.len(), 1);
    }

    /// Which change races a user's deletion (the user's row, then its teams).
    #[derive(Debug, Clone, Copy)]
    enum Recorded {
        /// Suspending alice's personal team: its entry resolves alice.
        SuspendPersonalTeam,
        /// Alice's own session changing a membership of `family`, a team she
        /// is in: its entry resolves her as the actor.
        MembershipBySession,
        /// A catalog created for alice's personal team.
        CatalogForPersonalTeam,
    }

    /// Holds alice's row `FOR UPDATE`, as `users::delete` takes it first,
    /// while `change` runs, then takes `team` `FOR UPDATE`, as the deletion
    /// takes her teams next. A change that locked the team before the user
    /// would deadlock here; one that waits on the user first lets the
    /// deletion through.
    async fn race_with_deleting_alice(change: Recorded) {
        let db = test_db().await;
        family(&db.pool).await;
        let session = session_of(&db.pool, "alice").await;
        let mut deleting = db.pool.begin().await.unwrap();
        sqlx::query("SELECT id FROM users WHERE name = 'alice' FOR UPDATE")
            .execute(&mut *deleting)
            .await
            .unwrap();
        let pool = db.pool.clone();
        let changing = tokio::spawn(async move {
            match change {
                Recorded::SuspendPersonalTeam => {
                    suspend(&pool, "alice", None, &AuditActor::cli(), now())
                        .await
                        .map(|_| ())
                }
                Recorded::MembershipBySession => {
                    let member = membership("family", TEST_USER, Role::Member);
                    set_member(&pool, &member, &session, now())
                        .await
                        .map(|_| ())
                }
                Recorded::CatalogForPersonalTeam => {
                    let new = crate::store::catalogs::NewCatalog {
                        name: "notes",
                        description: "",
                        owner_team: "alice",
                    };
                    crate::store::catalogs::create(&pool, &new, &AuditActor::cli(), now())
                        .await
                        .map(|_| ())
                }
            }
        });
        tokio::time::timeout(
            LOCK_WAIT,
            crate::store::catalogs::tests::until_a_lock_is_awaited(&db.pool),
        )
        .await
        .expect("the change waits on alice's row");
        let team = match change {
            Recorded::MembershipBySession => "family",
            Recorded::SuspendPersonalTeam | Recorded::CatalogForPersonalTeam => "alice",
        };

        // A deadlock makes Postgres abort one side; the timeout bounds a hang.
        let team_lock = tokio::time::timeout(
            LOCK_WAIT,
            sqlx::query("SELECT id FROM teams WHERE name = $1 FOR UPDATE")
                .bind(team)
                .execute(&mut *deleting),
        )
        .await
        .expect("the team lock does not hang");
        assert!(team_lock.is_ok(), "{change:?}: {team_lock:?}");
        // The deletion gives up; the change then goes ahead and records.
        deleting.rollback().await.unwrap();
        let changed = tokio::time::timeout(LOCK_WAIT, changing)
            .await
            .expect("the change finishes")
            .unwrap();
        assert!(changed.is_ok(), "{change:?}: {changed:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_recorded_change_locks_the_users_it_names_before_the_team() {
        for change in [
            Recorded::SuspendPersonalTeam,
            Recorded::MembershipBySession,
            Recorded::CatalogForPersonalTeam,
        ] {
            race_with_deleting_alice(change).await;
        }
    }
}
