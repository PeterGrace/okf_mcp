//! The audit log (spec "Part 5"): one row per administrative change,
//! catalog write, export and admin read, written by [`record`] inside the
//! transaction of the change it records. Immutable columns never hold a
//! person's name or a reusable name: people are `audit_people` ids, teams
//! `audit_teams` ids (one per `teams.id`, never reused), credentials
//! `audit_credentials` ids; [`forget_person`] clears what identifies a
//! deleted user in those rows. Never a secret: no key, token, password,
//! link, session id or email address goes into an entry.

use std::net::IpAddr;

use chrono::{DateTime, Utc};
use sqlx::PgConnection;

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

/// What kind of credential acted: what an entry's immutable `actor_label`
/// says, so it never needs a person's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// A console session.
    Session,
    /// An API key.
    Key,
    /// An OAuth grant.
    Grant,
    /// The command line.
    Cli,
    /// The server itself.
    System,
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
    /// The credential's actor, the OS user, or a system label. For a
    /// credential it goes to the mutable `audit_credentials.label`, never
    /// into the entry ([`AuditActor::stored_label`]).
    pub label: String,
    /// What kind of credential acted.
    pub via: Via,
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
            via: Via::Cli,
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
            via: Via::System,
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
        let (credential, via) = match principal.kind {
            CredentialKind::Session { .. } => (None, Via::Session),
            CredentialKind::ApiKey => (Some(principal.key_id.clone()), Via::Key),
            CredentialKind::OAuth { .. } => (Some(principal.key_id.clone()), Via::Grant),
        };
        Self {
            source: requester.source,
            credential,
            user_id: principal.user_id.clone(),
            // A session's `actor` is its user's name: it is kept here for
            // `settings.updated_by`, never written into an entry.
            label: principal.actor.clone(),
            via,
            // Task 5 adds `|| principal.provisioner`.
            operator: principal.access == Access::Admin,
            // Task 5: `principal.provisioner`.
            provisioner: false,
            ip: requester.ip,
        }
    }

    /// The entry's immutable `actor_label`: never a person's name. Every
    /// key and grant is named by its kind, whether or not it has a user now:
    /// a key with no user can be assigned one later, and its own label (a
    /// legacy key's is often a person's device or name) must stay where
    /// [`forget_person`] can clear it, in the mutable `audit_credentials`.
    /// Only the CLI's operating-system user (operator staff, not an okf-mcp
    /// user) and the system keep their label.
    ///
    /// # Returns
    /// The label to store.
    pub fn stored_label(&self) -> String {
        match self.via {
            Via::Session => "console session".to_string(),
            Via::Key => "api key".to_string(),
            Via::Grant => "oauth grant".to_string(),
            Via::Cli => format!("cli:{}", self.label),
            Via::System => format!("system:{}", self.label),
        }
    }
}

// One list of actions, so the enum, `as_str` and `ALL` cannot drift apart.
// `macro_rules!` writes the three from it at compile time.
macro_rules! actions {
    ($($variant:ident => $name:literal),+ $(,)?) => {
        /// What an entry records (spec "What is recorded").
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Action { $(
            #[doc = concat!("`", $name, "`.")]
            $variant
        ),+ }

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
            /// # Arguments
            /// * `name` - a stored name such as `"user.created"`.
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
    /// A team, by name (stored as its `audit_teams` id).
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

/// Where a target is stored: its kind, and at most one of a name, a team
/// (by name, resolved to its `audit_teams` id) or a credential id (resolved
/// to its `audit_credentials` id); a user is stored as a person only.
struct TargetParts<'a> {
    kind: &'static str,
    name: Option<String>,
    team: Option<&'a str>,
    credential: Option<&'a str>,
}

impl<'a> Target<'a> {
    /// The stored parts.
    fn parts(&self) -> TargetParts<'a> {
        let only = |kind| TargetParts {
            kind,
            name: None,
            team: None,
            credential: None,
        };
        let plain = |kind, name: String| TargetParts {
            name: Some(name),
            ..only(kind)
        };
        let credential = |kind, id: &'a str| TargetParts {
            credential: Some(id),
            ..only(kind)
        };
        // `*self` copies the `Copy` enum out of the reference, so each arm
        // binds the borrowed `&'a str` itself rather than a `&&str`.
        match *self {
            Target::User(_) => only("user"),
            Target::Team(name) => TargetParts {
                team: Some(name),
                ..only("team")
            },
            Target::Catalog(name) => plain("catalog", name.to_string()),
            Target::Key(id) => credential("key", id),
            Target::Grant(id) => credential("grant", id),
            Target::Code(id) => credential("code", id),
            Target::Setting(key) => plain("setting", key.to_string()),
            Target::Unlock(id) => plain("unlock", id.to_string()),
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
    /// `audit_teams` ids ([`team_ref`], [`catalog_teams`], [`key_teams`]).
    pub teams: &'a [i64],
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
/// [`OkfError::Database`], which fails the caller's change with it
/// (including a [`Target::Team`] that names no team, which the table's
/// target-shape check refuses).
pub async fn record(
    conn: &mut PgConnection,
    actor: &AuditActor,
    entry: &Entry<'_>,
    now: DateTime<Utc>,
) -> Result<i64, OkfError> {
    let target = entry.target.parts();
    let person = match entry.target {
        Target::User(user_id) => Some(user_id),
        _ => entry.person,
    };
    // The subquery resolves both people once (`audit_person` creates or
    // refreshes a row; NULL in, NULL out); the credentials and the target
    // team resolve to their audit ids the same way. The actor's credential
    // label goes to `audit_credentials` (mutable), the entry gets only
    // `stored_label`. `target_operator`: the target person, or the owner of
    // a target key, grant or code (which `audit_credential` resolves the
    // same way), is an admin user now, so owners never see who it is.
    Ok(sqlx::query_scalar(
        "INSERT INTO audit_entries (at, action, source, actor_credential_ref, actor_person_id, \
             actor_label, actor_operator, client_ip, target_kind, target_name, target_team_ref, \
             target_credential_ref, target_person_id, target_operator, teams, details) \
         SELECT $1, $2, $3, audit_credential($4, p.actor, $5), p.actor, $6, $7, $8::inet, $9, \
                $10, audit_team_ref((SELECT id FROM teams WHERE name = $11)), \
                audit_credential($12, p.target, NULL), p.target, \
                COALESCE((SELECT bool_or(is_admin) FROM users WHERE id IN ($13, \
                    (SELECT user_id FROM api_keys WHERE id = $12), \
                    (SELECT user_id FROM sign_in_codes WHERE id = $12))), false), $14, $15 \
         FROM (SELECT audit_person($16) AS actor, audit_person($13) AS target) p \
         RETURNING id",
    )
    .bind(now)
    .bind(entry.action.as_str())
    .bind(actor.source.as_str())
    .bind(&actor.credential)
    .bind(&actor.label)
    .bind(actor.stored_label())
    .bind(actor.operator)
    .bind(actor.ip.map(|ip| ip.to_string()))
    .bind(target.kind)
    .bind(target.name)
    .bind(target.team)
    .bind(target.credential)
    .bind(person)
    .bind(entry.teams)
    .bind(&entry.details)
    .bind(&actor.user_id)
    // `&mut *conn` reborrows the connection, so `conn` stays usable after.
    .fetch_one(&mut *conn)
    .await?)
}

/// How entries name a team: its `audit_teams` id, which follows the team's
/// immutable `teams.id`, so a later team with the same name is another id.
///
/// # Arguments
/// * `conn` - a connection or transaction.
/// * `team` - the team's current name.
///
/// # Returns
/// Its `audit_teams` id.
///
/// # Errors
/// [`OkfError::UnknownName`] (a malformed name included); [`OkfError::Database`].
pub async fn team_ref(conn: &mut PgConnection, team: &str) -> Result<i64, OkfError> {
    let unknown = || OkfError::UnknownName {
        noun: "team",
        name: team.to_string(),
    };
    // As `teams::find`, `lock_team` and `id_of` do (#339): a malformed name
    // (a NUL, which Postgres refuses) names no team, so it is never a
    // database error.
    if !crate::store::catalogs::is_valid_name(team) {
        return Err(unknown());
    }
    sqlx::query_scalar("SELECT audit_team_ref(id) FROM teams WHERE name = $1")
        .bind(team)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(unknown)
}

/// The `audit_teams` id of a catalog's owner team: what a catalog event
/// belongs to.
///
/// # Arguments
/// * `conn` - a connection or transaction.
/// * `catalog_id` - the catalog.
///
/// # Returns
/// One reference, or none for a catalog with no owner yet (or no catalog).
///
/// # Errors
/// [`OkfError::Database`].
pub async fn catalog_teams(conn: &mut PgConnection, catalog_id: i64) -> Result<Vec<i64>, OkfError> {
    let team: Option<i64> = sqlx::query_scalar(
        "SELECT audit_team_ref(owner_team_id) FROM catalogs \
         WHERE id = $1 AND owner_team_id IS NOT NULL",
    )
    .bind(catalog_id)
    .fetch_optional(&mut *conn)
    .await?;
    // `into_iter().collect()` turns `Option<T>` into a zero- or one-item Vec.
    Ok(team.into_iter().collect())
}

/// The `audit_teams` ids of the catalogs a key or grant is granted: what a
/// credential event belongs to. An all-catalogs credential belongs to no
/// team (operator only; plan Decision 6).
///
/// # Arguments
/// * `conn` - a connection or transaction.
/// * `key_id` - the key or grant.
///
/// # Returns
/// The owner teams of its granted catalogs, once each, in id order.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn key_teams(conn: &mut PgConnection, key_id: &str) -> Result<Vec<i64>, OkfError> {
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

/// Stops the log identifying a user that is being deleted, in the
/// deletion's transaction: the person row becomes `deleted user #<id>`, and
/// the user's credentials lose their id (a legacy key's id was its name)
/// and their label (a self-issued key's defaults to a name the user chose),
/// showing as `credential #<id>`. Entries themselves hold no name to clear.
/// Call it before deleting the user row (its credentials are still there).
///
/// # Arguments
/// * `conn` - the deletion's transaction.
/// * `user_id` - the user's `users.id`.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn forget_person(conn: &mut PgConnection, user_id: &str) -> Result<(), OkfError> {
    // Credentials first, found two ways: linked to the person, or (a row
    // whose person was never resolved) still named by the user's keys,
    // grants and codes. So it must run before the user row is deleted,
    // which cascades those.
    sqlx::query(
        "UPDATE audit_credentials SET credential_id = NULL, label = NULL \
         WHERE person_id = (SELECT id FROM audit_people WHERE user_id = $1) \
            OR credential_id IN (SELECT id FROM api_keys WHERE user_id = $1 \
                                 UNION ALL SELECT id FROM sign_in_codes WHERE user_id = $1)",
    )
    .bind(user_id)
    .execute(&mut *conn)
    .await?;
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
        /// The target's name: a catalog, setting or unlock name, a team's
        /// name (or its person's), or a credential id.
        pub target: Option<String>,
        /// The target person's name (a user target, or `Entry.person`).
        pub person: Option<String>,
        /// The teams' display names, personal teams as their person.
        pub teams: Vec<String>,
        pub details: serde_json::Value,
        /// A `catalog.admin_read` entry's request count.
        pub requests: Option<i64>,
    }

    /// Every entry with this action, oldest first.
    pub(crate) async fn entries(pool: &PgPool, action: &str) -> Vec<RecordedEntry> {
        sqlx::query_as::<_, RecordedEntry>(
            "SELECT e.source, e.actor_label, e.actor_operator, ap.name AS actor_person, \
                    e.target_kind, \
                    COALESCE(e.target_name, tt.name, ttp.name, tc.credential_id) AS target, \
                    tp.name AS person, \
                    ARRAY(SELECT COALESCE(t.name, p.name) \
                          FROM unnest(e.teams) WITH ORDINALITY AS r (id, n) \
                          JOIN audit_teams t ON t.id = r.id \
                          LEFT JOIN audit_people p ON p.id = t.person_id ORDER BY r.n) AS teams, \
                    e.details, c.requests \
             FROM audit_entries e \
             LEFT JOIN audit_people ap ON ap.id = e.actor_person_id \
             LEFT JOIN audit_people tp ON tp.id = e.target_person_id \
             LEFT JOIN audit_teams tt ON tt.id = e.target_team_ref \
             LEFT JOIN audit_people ttp ON ttp.id = tt.person_id \
             LEFT JOIN audit_credentials tc ON tc.id = e.target_credential_ref \
             LEFT JOIN audit_access_counts c ON c.entry_id = e.id \
             WHERE e.action = $1 ORDER BY e.id",
        )
        .bind(action)
        .fetch_all(pool)
        .await
        .expect("read the audit entries")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{test_db, TEST_TEAM, TEST_USER};
    use crate::http::requester::Requester;
    use crate::store::teams::{self, NewTeam};
    use crate::store::users::{self, NewUser};
    use chrono::TimeDelta;
    use sqlx::PgPool;

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
            via: Via::Key,
            operator: true,
            provisioner: false,
            ip: Some("192.0.2.7".parse().unwrap()),
        }
    }

    /// An entry about `target` with no person, teams or details.
    fn entry(action: Action, target: Target<'_>) -> Entry<'_> {
        Entry {
            action,
            target,
            person: None,
            teams: &[],
            details: serde_json::json!({}),
        }
    }

    /// Stores a legacy-style admin key `id` (its id is also its name) of
    /// `user_id`, or of no user, with no grants, so a credential naming it
    /// has a row to resolve.
    async fn store_key(pool: &PgPool, id: &str, user_id: Option<&str>) {
        sqlx::query(
            "INSERT INTO api_keys \
                 (id, name, sha256, access, all_catalogs, actor, created_at, user_id) \
             VALUES ($1, $1, sha256($1::bytea), 'admin', true, 'root', now(), $2)",
        )
        .bind(id)
        .bind(user_id)
        .execute(pool)
        .await
        .unwrap();
    }

    /// Stores an unredeemed sign-in code `id` of `user_id`.
    async fn store_code(pool: &PgPool, id: &str, user_id: &str) {
        sqlx::query(
            "INSERT INTO sign_in_codes (id, name, sha256, access, all_catalogs, actor, \
                 created_at, expires_at, user_id) \
             VALUES ($1, $1, sha256($1::bytea), 'read', true, 'root', now(), \
                     now() + interval '1 day', $2)",
        )
        .bind(id)
        .bind(user_id)
        .execute(pool)
        .await
        .unwrap();
    }

    /// The `(credential_id, label, person_id)` of every `audit_credentials`
    /// row, in id order.
    /// Takes a connection, so it also reads inside an open transaction.
    async fn audit_credentials(
        conn: &mut PgConnection,
    ) -> Vec<(Option<String>, Option<String>, Option<i64>)> {
        sqlx::query_as("SELECT credential_id, label, person_id FROM audit_credentials ORDER BY id")
            .fetch_all(conn)
            .await
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn record_writes_one_entry_with_its_actor_target_and_teams() {
        let db = test_db().await;
        let me = user_id(&db.pool, TEST_USER).await;
        store_key(&db.pool, "k1", Some(&me)).await;
        let mut tx = db.pool.begin().await.unwrap();
        let team = team_ref(&mut tx, TEST_TEAM).await.unwrap();

        let id = record(
            &mut tx,
            &admin_actor(&me),
            &Entry {
                teams: std::slice::from_ref(&team),
                ..entry(Action::CatalogCreated, Target::Catalog("main"))
            },
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        // A ten-column row: sqlx decodes a tuple of up to 16 columns.
        #[allow(clippy::type_complexity)]
        let row: (
            String,
            String,
            Option<String>,
            Option<String>,
            String,
            String,
            bool,
            Option<String>,
            String,
            Vec<i64>,
        ) = sqlx::query_as(
            "SELECT e.action, e.source, c.credential_id, c.label, e.actor_label, p.name, \
                    e.actor_operator, e.client_ip::text, e.target_name, e.teams \
             FROM audit_entries e JOIN audit_people p ON p.id = e.actor_person_id \
             JOIN audit_credentials c ON c.id = e.actor_credential_ref \
             WHERE e.id = $1",
        )
        .bind(id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(row.0, "catalog.created");
        assert_eq!(row.1, "api");
        assert_eq!(row.2.as_deref(), Some("k1"));
        assert_eq!(
            row.3.as_deref(),
            Some("root"),
            "the key's own label, in the mutable table"
        );
        assert_eq!(row.4, "api key", "the immutable label names no one");
        assert_eq!(row.5, TEST_USER);
        assert!(row.6);
        assert_eq!(row.7.as_deref(), Some("192.0.2.7/32"));
        assert_eq!(row.8, "main");
        assert_eq!(row.9, [team]);
        // The shared reader later tasks assert with sees the same entry.
        assert_eq!(
            testing::entries(&db.pool, "catalog.created").await,
            [testing::RecordedEntry {
                source: "api".to_string(),
                actor_label: "api key".to_string(),
                actor_operator: true,
                actor_person: Some(TEST_USER.to_string()),
                target_kind: "catalog".to_string(),
                target: Some("main".to_string()),
                person: None,
                teams: vec![TEST_USER.to_string()],
                details: serde_json::json!({}),
                requests: None,
            }]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_team_created_again_under_its_old_name_gets_a_new_audit_id() {
        let db = test_db().await;
        let family = NewTeam {
            name: "family",
            display_name: "",
            owner: TEST_USER,
        };
        teams::create(&db.pool, &family, now()).await.unwrap();
        let mut tx = db.pool.begin().await.unwrap();
        let first = team_ref(&mut tx, "family").await.unwrap();
        tx.commit().await.unwrap();
        teams::delete(&db.pool, "family").await.unwrap();
        teams::create(&db.pool, &family, now()).await.unwrap();

        let mut tx = db.pool.begin().await.unwrap();
        let second = team_ref(&mut tx, "family").await.unwrap();
        let again = team_ref(&mut tx, "family").await.unwrap();

        assert_ne!(
            first, second,
            "scope follows teams.id, never the reusable name"
        );
        assert_eq!(second, again, "one row per team");
        assert!(matches!(
            team_ref(&mut tx, "no\0such").await,
            Err(OkfError::UnknownName { .. })
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_session_actor_leaves_no_name_in_the_entry_once_its_user_is_forgotten() {
        let db = test_db().await;
        let new = NewUser {
            name: "alice",
            display_name: "Alice Example",
        };
        users::create(&db.pool, &new, now()).await.unwrap();
        let alice = users::find_auth(&db.pool, &user_id(&db.pool, "alice").await)
            .await
            .unwrap()
            .unwrap();
        // A real console-session principal: its `actor` is the user's name.
        // (M13's three arguments; Task 7b adds the gates: pass
        // `Default::default()` once more.)
        let principal = Principal::for_session(&alice, Default::default(), Default::default());
        let requester = Requester {
            ip: None,
            source: Source::Console,
        };
        let mut tx = db.pool.begin().await.unwrap();
        record(
            &mut tx,
            &AuditActor::of(&principal, &requester),
            &entry(Action::CatalogCreated, Target::Catalog("main")),
            now(),
        )
        .await
        .unwrap();

        forget_person(&mut tx, &alice.id).await.unwrap();
        tx.commit().await.unwrap();

        // Every stored column of the entry, and every row it refers to.
        let stored: String = sqlx::query_scalar(
            "SELECT row_to_json(e)::text || coalesce(row_to_json(p)::text, '') \
                    || coalesce(row_to_json(c)::text, '') \
             FROM audit_entries e LEFT JOIN audit_people p ON p.id = e.actor_person_id \
             LEFT JOIN audit_credentials c ON c.id = e.actor_credential_ref",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert!(
            !stored.contains("alice") && !stored.contains("Alice"),
            "{stored}"
        );
        assert!(stored.contains("console session"), "{stored}");
        assert!(stored.contains("deleted user #"), "{stored}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forgetting_a_user_clears_its_credentials_and_keeps_every_entry() {
        let db = test_db().await;
        let new = NewUser {
            name: "alice",
            display_name: "Alice Example",
        };
        users::create(&db.pool, &new, now()).await.unwrap();
        let alice = user_id(&db.pool, "alice").await;
        // A legacy key's id is its name, and a code's name is the user's
        // choice: both identify alice until they are forgotten.
        store_key(&db.pool, "alice-laptop", Some(&alice)).await;
        store_code(&db.pool, "alice-phone", &alice).await;
        let actor = AuditActor {
            credential: Some("alice-laptop".to_string()),
            user_id: Some(alice.clone()),
            label: "alice-laptop".to_string(),
            ..admin_actor(&alice)
        };
        let mut tx = db.pool.begin().await.unwrap();
        // The target names no person: its owner comes from `sign_in_codes`.
        let id = record(
            &mut tx,
            &actor,
            &entry(Action::OauthCodeRevoked, Target::Code("alice-phone")),
            now(),
        )
        .await
        .unwrap();
        let person: i64 = sqlx::query_scalar("SELECT id FROM audit_people WHERE user_id = $1")
            .bind(&alice)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(
            audit_credentials(&mut tx).await,
            [
                (
                    Some("alice-laptop".to_string()),
                    Some("alice-laptop".to_string()),
                    Some(person)
                ),
                (Some("alice-phone".to_string()), None, Some(person)),
            ],
            "both credentials resolved to alice's person"
        );

        // As user deletion will: forget first, then remove the user's rows.
        forget_person(&mut tx, &alice).await.unwrap();
        tx.commit().await.unwrap();
        for table in ["api_keys", "sign_in_codes"] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM {table} WHERE user_id = $1"
            )))
            .bind(&alice)
            .execute(&db.pool)
            .await
            .unwrap();
        }
        users::delete(&db.pool, "alice", false, &AuditActor::cli(), now())
            .await
            .unwrap();

        assert_eq!(
            audit_credentials(&mut db.pool.acquire().await.unwrap()).await,
            [(None, None, Some(person)), (None, None, Some(person))],
            "the ids and labels are SQL NULL; the rows stay"
        );
        let stored: String = sqlx::query_scalar(
            "SELECT row_to_json(e)::text || row_to_json(a)::text || row_to_json(t)::text \
                    || row_to_json(p)::text \
             FROM audit_entries e \
             JOIN audit_credentials a ON a.id = e.actor_credential_ref \
             JOIN audit_credentials t ON t.id = e.target_credential_ref \
             JOIN audit_people p ON p.id = e.actor_person_id \
             WHERE e.id = $1",
        )
        .bind(id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert!(
            !stored.contains("alice") && !stored.contains("Alice"),
            "{stored}"
        );
        assert!(stored.contains("deleted user #"), "{stored}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forgetting_a_user_clears_a_key_assigned_to_it_after_it_was_recorded() {
        let db = test_db().await;
        let new = NewUser {
            name: "alice",
            display_name: "",
        };
        users::create(&db.pool, &new, now()).await.unwrap();
        let alice = user_id(&db.pool, "alice").await;
        // A legacy key with no user yet: its audit row has no person.
        store_key(&db.pool, "alice-legacy", None).await;
        let mut tx = db.pool.begin().await.unwrap();
        record(
            &mut tx,
            &AuditActor::cli(),
            &entry(Action::KeyRevoked, Target::Key("alice-legacy")),
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            audit_credentials(&mut db.pool.acquire().await.unwrap()).await,
            [(Some("alice-legacy".to_string()), None, None)]
        );
        sqlx::query("UPDATE api_keys SET user_id = $1 WHERE id = 'alice-legacy'")
            .bind(&alice)
            .execute(&db.pool)
            .await
            .unwrap();

        let mut tx = db.pool.begin().await.unwrap();
        forget_person(&mut tx, &alice).await.unwrap();
        tx.commit().await.unwrap();

        assert_eq!(
            audit_credentials(&mut db.pool.acquire().await.unwrap()).await,
            [(None, None, None)],
            "found through the live key, though the audit row names no person"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_key_that_acted_before_it_was_assigned_leaves_no_name_once_its_user_is_forgotten() {
        let db = test_db().await;
        let new = NewUser {
            name: "alice",
            display_name: "Alice Example",
        };
        users::create(&db.pool, &new, now()).await.unwrap();
        let alice = user_id(&db.pool, "alice").await;
        // A legacy key with no user acts: its label names a person.
        store_key(&db.pool, "alice-laptop", None).await;
        let unassigned = AuditActor {
            credential: Some("alice-laptop".to_string()),
            user_id: None,
            label: "alice-laptop".to_string(),
            ..admin_actor(&alice)
        };
        let mut tx = db.pool.begin().await.unwrap();
        record(
            &mut tx,
            &unassigned,
            &entry(Action::SettingChanged, Target::Setting("product_name")),
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        // Then the key is assigned to alice, and alice is forgotten.
        sqlx::query("UPDATE api_keys SET user_id = $1 WHERE id = 'alice-laptop'")
            .bind(&alice)
            .execute(&db.pool)
            .await
            .unwrap();
        let mut tx = db.pool.begin().await.unwrap();
        forget_person(&mut tx, &alice).await.unwrap();
        tx.commit().await.unwrap();

        // Every stored column of the entry, and every row it refers to.
        let stored: String = sqlx::query_scalar(
            "SELECT row_to_json(e)::text || coalesce(row_to_json(c)::text, '') \
             FROM audit_entries e \
             LEFT JOIN audit_credentials c ON c.id = e.actor_credential_ref",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert!(!stored.contains("alice"), "{stored}");
        assert!(stored.contains("\"actor_label\":\"api key\""), "{stored}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_credential_target_of_an_admin_is_marked_operator_without_a_person() {
        let db = test_db().await;
        for name in ["staff", "alice"] {
            let new = NewUser {
                name,
                display_name: "",
            };
            users::create(&db.pool, &new, now()).await.unwrap();
        }
        sqlx::query("UPDATE users SET is_admin = true WHERE name = 'staff'")
            .execute(&db.pool)
            .await
            .unwrap();
        let staff = user_id(&db.pool, "staff").await;
        let alice = user_id(&db.pool, "alice").await;
        store_key(&db.pool, "staff-laptop", Some(&staff)).await;
        store_code(&db.pool, "staff-phone", &staff).await;
        store_key(&db.pool, "alice-laptop", Some(&alice)).await;
        let mut tx = db.pool.begin().await.unwrap();
        // `Entry.person` is `None` for each: only the credential names its
        // owner.
        for target in [
            Target::Key("staff-laptop"),
            Target::Code("staff-phone"),
            Target::Key("alice-laptop"),
        ] {
            record(
                &mut tx,
                &AuditActor::cli(),
                &entry(Action::KeyRevoked, target),
                now(),
            )
            .await
            .unwrap();
        }
        tx.commit().await.unwrap();

        let marked: Vec<(Option<String>, bool)> = sqlx::query_as(
            "SELECT c.credential_id, e.target_operator FROM audit_entries e \
             JOIN audit_credentials c ON c.id = e.target_credential_ref ORDER BY e.id",
        )
        .fetch_all(&db.pool)
        .await
        .unwrap();
        assert_eq!(
            marked,
            [
                (Some("staff-laptop".to_string()), true),
                (Some("staff-phone".to_string()), true),
                (Some("alice-laptop".to_string()), false),
            ],
            "an admin's key or code hides from owners; a member's does not"
        );
    }

    /// Creates catalog `name` owned by team `owner` (none for `None`).
    async fn store_catalog(pool: &PgPool, name: &str, owner: Option<&str>) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO catalogs (name, created_at, owner_team_id) \
             VALUES ($1, now(), (SELECT id FROM teams WHERE name = $2)) RETURNING id",
        )
        .bind(name)
        .bind(owner)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    /// Grants key `key` the catalogs `catalogs`.
    async fn grant(pool: &PgPool, key: &str, catalogs: &[i64]) {
        sqlx::query(
            "INSERT INTO key_catalogs (key_id, catalog_id) SELECT $1, unnest($2::bigint[])",
        )
        .bind(key)
        .bind(catalogs)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn catalog_teams_is_the_owner_teams_audit_id_or_nothing() {
        let db = test_db().await;
        let owned = store_catalog(&db.pool, "owned", Some(TEST_TEAM)).await;
        let orphan = store_catalog(&db.pool, "orphan", None).await;
        let mut tx = db.pool.begin().await.unwrap();
        let team = team_ref(&mut tx, TEST_TEAM).await.unwrap();

        let of_owned = catalog_teams(&mut tx, owned).await.unwrap();
        let of_orphan = catalog_teams(&mut tx, orphan).await.unwrap();
        let of_unknown = catalog_teams(&mut tx, orphan + 1000).await.unwrap();

        assert_eq!(of_owned, [team]);
        assert_eq!(of_orphan, Vec::<i64>::new(), "no owner, no team");
        assert_eq!(of_unknown, Vec::<i64>::new(), "no catalog, no team");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn key_teams_is_the_distinct_owners_of_its_grants_and_nothing_else() {
        let db = test_db().await;
        let family = NewTeam {
            name: "family",
            display_name: "",
            owner: TEST_USER,
        };
        teams::create(&db.pool, &family, now()).await.unwrap();
        let staff = NewTeam {
            name: "staff",
            ..family
        };
        teams::create(&db.pool, &staff, now()).await.unwrap();
        let mine = store_catalog(&db.pool, "mine", Some(TEST_TEAM)).await;
        let mine_too = store_catalog(&db.pool, "mine-too", Some(TEST_TEAM)).await;
        let theirs = store_catalog(&db.pool, "theirs", Some("family")).await;
        let orphan = store_catalog(&db.pool, "orphan", None).await;
        let me = user_id(&db.pool, TEST_USER).await;
        // Two grants with one owner; grants across owners (and one with no
        // owner); an all-catalogs key with no grants. The key's user also
        // owns `staff`, which no grant reaches.
        store_key(&db.pool, "shared", Some(&me)).await;
        grant(&db.pool, "shared", &[mine, mine_too]).await;
        store_key(&db.pool, "spread", Some(&me)).await;
        grant(&db.pool, "spread", &[theirs, mine, orphan]).await;
        store_key(&db.pool, "everything", Some(&me)).await;
        let mut tx = db.pool.begin().await.unwrap();
        let personal = team_ref(&mut tx, TEST_TEAM).await.unwrap();
        let family_ref = team_ref(&mut tx, "family").await.unwrap();
        let mut both = vec![personal, family_ref];
        both.sort_unstable();

        let shared = key_teams(&mut tx, "shared").await.unwrap();
        let spread = key_teams(&mut tx, "spread").await.unwrap();
        let everything = key_teams(&mut tx, "everything").await.unwrap();

        assert_eq!(shared, [personal], "one owner, listed once");
        assert_eq!(spread, both, "each owner once, in id order; no staff");
        assert_eq!(
            everything,
            Vec::<i64>::new(),
            "an all-catalogs key belongs to no team"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_user_target_is_a_person_and_forgetting_renames_it() {
        let db = test_db().await;
        let new = NewUser {
            name: "alice",
            display_name: "Alice",
        };
        users::create(&db.pool, &new, now()).await.unwrap();
        let alice = user_id(&db.pool, "alice").await;
        let mut tx = db.pool.begin().await.unwrap();
        record(
            &mut tx,
            &AuditActor::cli(),
            &entry(Action::UserCreated, Target::User(&alice)),
            now(),
        )
        .await
        .unwrap();

        forget_person(&mut tx, &alice).await.unwrap();
        tx.commit().await.unwrap();

        let (name, display, linked, target_name): (String, String, Option<String>, Option<String>) =
            sqlx::query_as(
                "SELECT p.name, p.display_name, p.user_id, e.target_name FROM audit_entries e \
                 JOIN audit_people p ON p.id = e.target_person_id",
            )
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert!(name.starts_with("deleted user #"), "{name}");
        assert_eq!(display, "");
        assert_eq!(linked, None);
        assert_eq!(
            target_name, None,
            "a user target is stored only as a person"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_log_refuses_update_and_early_delete_and_allows_an_old_delete() {
        let db = test_db().await;
        let mut tx = db.pool.begin().await.unwrap();
        let setting = Target::Setting("product_name");
        let recent = record(
            &mut tx,
            &AuditActor::cli(),
            &entry(Action::SettingChanged, setting),
            Utc::now(),
        )
        .await
        .unwrap();
        let old_at = Utc::now() - TimeDelta::days(401);
        let old = record(
            &mut tx,
            &AuditActor::cli(),
            &entry(Action::SettingCleared, setting),
            old_at,
        )
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
        let truncated = sqlx::query("TRUNCATE audit_entries CASCADE")
            .execute(&db.pool)
            .await;

        assert!(updated.is_err(), "UPDATE is refused");
        assert!(early.is_err(), "a recent entry cannot be deleted");
        assert_eq!(late.rows_affected(), 1, "an entry past 400 days can");
        assert!(truncated.is_err(), "TRUNCATE is refused");
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_entries")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(left, 1, "the recent entry survived every attempt");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_entry_rolls_back_with_the_change_it_records() {
        let db = test_db().await;
        let mut tx = db.pool.begin().await.unwrap();
        record(
            &mut tx,
            &AuditActor::cli(),
            &Entry {
                details: serde_json::json!({"to": "x"}),
                ..entry(Action::SettingChanged, Target::Setting("product_name"))
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
        let names: std::collections::HashSet<&str> =
            Action::ALL.iter().map(|action| action.as_str()).collect();
        assert_eq!(names.len(), Action::ALL.len());
        // The same pattern as the table's CHECK on `action`.
        let pattern = regex::Regex::new(r"^[a-z]+\.[a-z_]+$").unwrap();
        assert!(names.iter().all(|name| pattern.is_match(name)), "{names:?}");
        assert!(Action::ALL
            .iter()
            .all(|action| Action::parse(action.as_str()) == Some(*action)));
        assert_eq!(Action::parse("user.unknown"), None);
    }
}
