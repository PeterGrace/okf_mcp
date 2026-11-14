//! `okf-mcp user ...` (accounts, the admin flag, email, setup invites,
//! entitlements, console sessions and sign-ins), `team ...`, `unassigned`,
//! the assignment subcommands (`catalog set-owner`, `key assign`, `oauth
//! code assign`) and `catalog set-member-access`.
//!
//! Each subcommand calls the same store function as its REST route
//! (`src/http/api/people.rs`, `catalogs.rs`), so the two cannot disagree.
//! The CLI has operator access: `user set-admin --off` may clear the last
//! admin user, which the API refuses (plan Decision 16).
//!
//! Each handler returns the text to print (one `\n`-terminated line per
//! row) instead of printing it, so the tests can compare it exactly. A
//! store write already bumps the administrative generation, which is how a
//! running server notices the change, so the handlers do nothing else.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use super::{labelled, tab_row, TeamCommand, UserCommand};
use crate::clock::format_timestamp;
use crate::config::Config;
use crate::mail::invite;
use crate::store::audit::AuditActor;
use crate::store::catalogs;
use crate::store::ceiling::{self, MemberAccess, NewEntitlement};
use crate::store::keys::{self, KeyKind};
use crate::store::oauth::codes;
use crate::store::sign_in_events::{self, SignInEvent};
use crate::store::teams::{self, MemberChange, Membership, NewTeam, Role, TeamRecord};
use crate::store::unassigned;
use crate::store::user_sessions::{self, SessionInfo};
use crate::store::users::{self, AdminChange, NewUser, UserRecord};

/// Runs `okf-mcp user ...`.
///
/// # Arguments
/// * `pool` - the migrated database.
/// * `config` - for `user invite`: `public_url` (the link's origin) and the
///   `[auth]` defaults under the stored settings (`setup_link_ttl`).
/// * `command` - which user subcommand.
/// * `now` - the server clock: `create`'s creation time, an entitlement's
///   and a setup link's start, and every audit entry's time (each change is
///   recorded as the operating-system user, `AuditActor::cli`).
///
/// # Returns
/// The text to print. `user invite` names the address, never the link.
///
/// # Errors
/// The store's refusal (an invalid or taken name, a user whose personal
/// team owns a catalog or who is a shared team's last owner, an email
/// another user has or that is not one, an `admin` entitlement, an invite
/// without an email or `public_url`), an unknown
/// user or catalog, or a database error.
pub(super) async fn user(
    pool: &PgPool,
    config: &Config,
    command: UserCommand,
    now: DateTime<Utc>,
) -> anyhow::Result<String> {
    Ok(match command {
        UserCommand::Create { name, display_name } => {
            let new = NewUser {
                name: &name,
                display_name: &display_name,
            };
            let created = users::create(pool, &new, &AuditActor::cli(), now).await?;
            // The personal team shares the user's name.
            format!(
                "created user {} (personal team {})\n",
                created.name, created.name
            )
        }
        // `--email`: zero or one user; `Option::iter` yields it or nothing,
        // so both arms print through the same `lines(.. format_user)`.
        UserCommand::List { email: Some(email) } => lines(
            users::find_record_by_email(pool, &email)
                .await?
                .iter()
                .map(format_user),
        ),
        UserCommand::List { email: None } => {
            lines(users::list(pool).await?.iter().map(format_user))
        }
        UserCommand::Delete { name } => {
            // `allow_last_admin: true`: operator access may delete the last
            // admin user, as it may clear the last admin flag.
            let deleted = users::delete(pool, &name, true, &AuditActor::cli(), now).await?;
            format!(
                "deleted user {name} ({} key(s) and grant(s), {} sign-in code(s), \
                 {} open changeset(s) discarded)\n",
                deleted.keys, deleted.codes, deleted.discarded
            )
        }
        // `off: _` ignores the field: clap made `--on` and `--off` exclusive
        // and required one, so `on` alone decides.
        UserCommand::SetAdmin { name, on, off: _ } => {
            // `allow_last: true`: operator access may clear the last admin
            // (Decision 16).
            let change = AdminChange {
                on,
                allow_last: true,
            };
            let previous = users::set_admin(pool, &name, change, &AuditActor::cli(), now).await?;
            describe_admin_change(&name, previous, on)
        }
        UserCommand::SetEmail {
            name,
            email,
            clear: _,
        } => {
            users::set_email(pool, &name, email.as_deref(), &AuditActor::cli(), now).await?;
            match email {
                Some(email) => format!("{name}: email {email}\n"),
                None => format!("{name}: email cleared\n"),
            }
        }
        UserCommand::Invite { name } => {
            // Only queues the mail (no mailer, so no mail secret, is needed);
            // a running server's worker sends it.
            let email = invite::send_setup(pool, config, &name, &AuditActor::cli(), now).await?;
            format!("queued a setup email for {name} to {email}\n")
        }
        UserCommand::Entitle {
            name,
            catalog,
            access,
        } => {
            let new = NewEntitlement {
                user: &name,
                catalog: &catalog,
                access,
            };
            ceiling::entitle(pool, &new, &AuditActor::cli(), now).await?;
            format!("{name} may now use {catalog} ({})\n", access.as_str())
        }
        UserCommand::Unentitle { name, catalog } => {
            ceiling::unentitle(pool, &name, &catalog, &AuditActor::cli(), now).await?;
            format!("{name} may no longer use {catalog} through an entitlement\n")
        }
        UserCommand::Sessions { name } => {
            let id = users::id_by_name(pool, &name).await?;
            let sessions = user_sessions::list(pool, &id, None).await?;
            if sessions.is_empty() {
                format!("{name} has no sessions\n")
            } else {
                lines(sessions.iter().map(format_session))
            }
        }
        UserCommand::RevokeSessions { name } => {
            // `end_sessions` locks the user's row first, so a sign-in still
            // committing is ended too (corrected, issue #322).
            let revoked = users::end_sessions(pool, &name, &AuditActor::cli(), now).await?;
            format!("revoked {revoked} sessions of {name}\n")
        }
        UserCommand::SignIns { name, limit } => {
            let events = sign_in_events::recent(pool, &name, limit).await?;
            if events.is_empty() {
                format!("{name} has no sign-ins\n")
            } else {
                lines(events.iter().map(format_sign_in))
            }
        }
    })
}

/// Runs `okf-mcp team ...`.
///
/// # Arguments
/// * `pool` - the migrated database.
/// * `command` - which team subcommand.
/// * `now` - the creation time for `create` and new memberships, the
///   suspension time for `suspend` of an active team, and every audit
///   entry's time (the actor is the CLI, [`AuditActor::cli`]).
///
/// # Returns
/// The text to print.
///
/// # Errors
/// The store's refusal (an invalid or taken name, the last owner, a
/// personal team, a team that owns a catalog, a notice over 2000 characters
/// or holding a NUL), an unknown or malformed team name, an unknown user,
/// or a database error.
pub(super) async fn team(
    pool: &PgPool,
    command: TeamCommand,
    now: DateTime<Utc>,
) -> anyhow::Result<String> {
    Ok(match command {
        TeamCommand::Create {
            name,
            display_name,
            owner,
        } => {
            let new = NewTeam {
                name: &name,
                display_name: &display_name,
                owner: &owner,
            };
            let created = teams::create(pool, &new, &AuditActor::cli(), now).await?;
            format!("created team {} (owner {owner})\n", created.name)
        }
        TeamCommand::List => lines(teams::list(pool).await?.iter().map(format_team)),
        // `|` matches either variant and binds the same names from both,
        // like two `case` patterns sharing one body.
        TeamCommand::AddMember { team, user, role } | TeamCommand::SetRole { team, user, role } => {
            let membership = Membership {
                team: &team,
                user: &user,
                role,
            };
            let change = teams::set_member(pool, &membership, &AuditActor::cli(), now).await?;
            describe_member_change(change, &team, &user, role)
        }
        TeamCommand::RemoveMember { team, user } => {
            teams::remove_member(pool, &team, &user, &AuditActor::cli(), now).await?;
            format!("removed {user} from team {team}\n")
        }
        TeamCommand::Delete { name } => {
            teams::delete(pool, &name, &AuditActor::cli(), now).await?;
            format!("deleted team {name}\n")
        }
        TeamCommand::Suspend { name, notice } => {
            // `as_deref` turns `Option<String>` into `Option<&str>`, the
            // borrowed form the store takes, without copying the text.
            teams::suspend(pool, &name, notice.as_deref(), &AuditActor::cli(), now).await?;
            format!("suspended team {name}\n")
        }
        TeamCommand::Unsuspend { name } => {
            teams::unsuspend(pool, &name, &AuditActor::cli(), now).await?;
            format!("lifted the suspension of team {name}\n")
        }
    })
}

/// Runs `okf-mcp unassigned`: one line per key, grant, sign-in code or
/// catalog that predates users and teams and has no user or owner yet.
///
/// # Arguments
/// * `pool` - the migrated database.
///
/// # Returns
/// `key NAME (id ID, actor ACTOR)`, `grant NAME (id ID)`, `code NAME (id
/// ID)` and `catalog NAME` lines, or `nothing is unassigned`.
///
/// # Errors
/// A database error.
pub(super) async fn unassigned(pool: &PgPool) -> anyhow::Result<String> {
    let found = unassigned::list(pool).await?;
    if found.is_empty() {
        return Ok("nothing is unassigned\n".to_string());
    }
    // `chain` joins the three iterators end to end, like `itertools.chain`.
    let keys = found.keys.iter().map(|key| match key.kind {
        KeyKind::Key => format!("key {} (id {}, actor {})", key.name, key.id, key.actor),
        KeyKind::OAuth => format!("grant {} (id {})", key.name, key.id),
    });
    let codes = found
        .codes
        .iter()
        .map(|code| format!("code {} (id {})", code.name, code.id));
    let catalogs = found
        .catalogs
        .iter()
        .map(|catalog| format!("catalog {catalog}"));
    Ok(lines(keys.chain(codes).chain(catalogs)))
}

/// Runs `okf-mcp catalog set-owner`.
///
/// # Arguments
/// * `pool` - the migrated database.
/// * `catalog` - the catalog's name.
/// * `team` - the team that owns it from now on.
/// * `now` - the audit entry's time (server clock).
///
/// # Returns
/// `catalog C: owner OLD -> NEW`, with `(none)` for a catalog that had no
/// owner.
///
/// # Errors
/// An unknown catalog or team, or a database error.
pub(super) async fn set_owner(
    pool: &PgPool,
    catalog: &str,
    team: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<String> {
    let previous = catalogs::set_owner(pool, catalog, team, &AuditActor::cli(), now).await?;
    Ok(format!(
        "catalog {catalog}: owner {} -> {team}\n",
        previous.as_deref().unwrap_or("(none)")
    ))
}

/// Runs `okf-mcp catalog set-member-access`.
///
/// # Arguments
/// * `pool` - the migrated database.
/// * `catalog` - the catalog's name.
/// * `access` - what the owning team's members may reach from now on.
/// * `now` - the audit entry's time (server clock).
///
/// # Returns
/// `catalog C: member access OLD -> NEW`.
///
/// # Errors
/// An unknown catalog, or a database error.
pub(super) async fn set_member_access(
    pool: &PgPool,
    catalog: &str,
    access: MemberAccess,
    now: DateTime<Utc>,
) -> anyhow::Result<String> {
    let previous =
        ceiling::set_member_access(pool, catalog, access, &AuditActor::cli(), now).await?;
    Ok(format!(
        "catalog {catalog}: member access {} -> {}\n",
        previous.as_str(),
        access.as_str()
    ))
}

/// Runs `okf-mcp key assign`: gives a key or OAuth grant a user.
///
/// # Arguments
/// * `pool` - the migrated database.
/// * `target` - the key's or grant's id, or an unambiguous active name.
/// * `actor` - narrows a name to one actor.
/// * `user` - the user it belongs to from now on.
///
/// # Returns
/// `key ID now belongs to USER`.
///
/// # Errors
/// An unknown, revoked (by name) or ambiguous target, an unknown user, or
/// a database error.
pub(super) async fn assign_key(
    pool: &PgPool,
    target: &str,
    actor: Option<&str>,
    user: &str,
) -> anyhow::Result<String> {
    let resolved = keys::assign_user_target(pool, target, actor, user).await?;
    Ok(format!("key {resolved} now belongs to {user}\n"))
}

/// Runs `okf-mcp oauth code assign`: gives a sign-in code a user.
///
/// # Arguments
/// * `pool` - the migrated database.
/// * `id` - the code's id.
/// * `user` - the user it belongs to from now on.
///
/// # Returns
/// `sign-in code ID now belongs to USER`.
///
/// # Errors
/// An unknown code or user, or a database error.
pub(super) async fn assign_code(pool: &PgPool, id: &str, user: &str) -> anyhow::Result<String> {
    codes::assign_user(pool, id, user).await?;
    Ok(format!("sign-in code {id} now belongs to {user}\n"))
}

/// What `team add-member` or `team set-role` did, as one line.
///
/// # Arguments
/// * `change` - the store's answer.
/// * `team`, `user`, `role` - the command's arguments.
///
/// # Returns
/// The line, with its newline.
fn describe_member_change(change: MemberChange, team: &str, user: &str, role: Role) -> String {
    let role = role.as_str();
    match change {
        MemberChange::Added => format!("added {user} to team {team} as {role}\n"),
        MemberChange::RoleChanged => format!("{user} is now {role} of team {team}\n"),
        MemberChange::Unchanged => format!("{user} is already {role} of team {team}\n"),
    }
}

/// What `user set-admin` did, as one line.
///
/// # Arguments
/// * `name` - the user.
/// * `previous` - the flag before.
/// * `on` - the flag asked for.
///
/// # Returns
/// The line, with its newline.
fn describe_admin_change(name: &str, previous: bool, on: bool) -> String {
    match (previous, on) {
        (false, true) => format!("{name} is now an admin\n"),
        (true, false) => format!("{name} is no longer an admin\n"),
        (true, true) => format!("{name} is already an admin\n"),
        (false, false) => format!("{name} is already not an admin\n"),
    }
}

/// One `user list` line, tab-separated: name, `credentials N`, `email E`
/// (`-` when unset), `admin yes|no`, `teams T1,T2`, then the free-text
/// display name last.
///
/// # Arguments
/// * `user` - the user.
///
/// # Returns
/// The line, without a newline.
fn format_user(user: &UserRecord) -> String {
    tab_row(&[
        &user.name,
        &format!("credentials {}", user.credentials),
        &labelled("email", user.email.as_deref()),
        if user.is_admin {
            "admin yes"
        } else {
            "admin no"
        },
        &labelled("teams", comma_list(&user.teams).as_deref()),
        &user.display_name,
    ])
}

/// One `user sessions` line, tab-separated: the id prefix (never the id),
/// `created T`, `seen T`, `ip A` (`-` when unknown), then the free-text
/// user agent last.
///
/// # Arguments
/// * `session` - the session.
///
/// # Returns
/// The line, without a newline.
fn format_session(session: &SessionInfo) -> String {
    let ip = session.ip.map(|address| address.to_string());
    tab_row(&[
        &session.id_prefix,
        &format!("created {}", format_timestamp(session.created_at)),
        &format!("seen {}", format_timestamp(session.last_seen_at)),
        &labelled("ip", ip.as_deref()),
        &session.user_agent,
    ])
}

/// One `user sign-ins` line, tab-separated: time, method, outcome, `ip A`
/// (`-` when unknown).
///
/// # Arguments
/// * `event` - the attempt.
///
/// # Returns
/// The line, without a newline.
fn format_sign_in(event: &SignInEvent) -> String {
    let ip = event.ip.map(|address| address.to_string());
    tab_row(&[
        &format_timestamp(event.at),
        event.method.as_str(),
        event.outcome.as_str(),
        &labelled("ip", ip.as_deref()),
    ])
}

/// One `team list` line, tab-separated: name, `personal U` (`-` for a
/// shared team), `members U(role),...`, `catalogs C1,C2`, `suspended TIME`
/// (`-` when active), then the free-text display name last.
///
/// # Arguments
/// * `team` - the team.
///
/// # Returns
/// The line, without a newline.
fn format_team(team: &TeamRecord) -> String {
    let members: Vec<String> = team
        .members
        .iter()
        .map(|member| format!("{}({})", member.user, member.role.as_str()))
        .collect();
    tab_row(&[
        &team.name,
        &labelled("personal", team.personal_of.as_deref()),
        &labelled("members", comma_list(&members).as_deref()),
        &labelled("catalogs", comma_list(&team.catalogs).as_deref()),
        &labelled(
            "suspended",
            // `map` formats the time only when there is a suspension;
            // `as_deref` then lends the `String` out as the `&str` cell.
            team.suspension
                .as_ref()
                .map(|suspension| format_timestamp(suspension.since))
                .as_deref(),
        ),
        &team.display_name,
    ])
}

/// Comma-joins `items`, or `None` when there are none (a `-` cell).
///
/// # Arguments
/// * `items` - the names.
///
/// # Returns
/// `"a,b"`, or `None` for an empty slice.
fn comma_list(items: &[String]) -> Option<String> {
    // `then` runs the closure only when the condition holds: `x if c else None`.
    (!items.is_empty()).then(|| items.join(","))
}

/// Joins listing lines, each followed by a newline (nothing for no lines).
///
/// # Arguments
/// * `rows` - the lines, without newlines.
///
/// # Returns
/// The text to print.
fn lines(rows: impl Iterator<Item = String>) -> String {
    rows.map(|row| row + "\n").collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{run, CatalogCommand, Cli, CodeCommand, Command, KeyCommand, OauthCommand};
    use crate::clock::FixedClock;
    use crate::db::pg_support::TestDb;
    use crate::db::{test_db, TEST_USER};
    use clap::Parser;
    use std::sync::Arc;

    /// The config the tests run commands with: `public_url` set, so
    /// `user invite` can build its link.
    fn test_config() -> Config {
        Config {
            public_url: Some("https://okf.example.com".to_string()),
            ..Config::default()
        }
    }

    /// Runs `okf-mcp <args>` against `db` and returns what it prints. The
    /// commands this module and `admin` handle return their text; any other
    /// runs through `cli::run` (printing to the real stdout) and returns
    /// `""`.
    async fn okf(db: &TestDb, args: &[&str]) -> anyhow::Result<String> {
        let parsed = Cli::try_parse_from(std::iter::once(&"okf-mcp").chain(args))?;
        let now = Utc::now();
        let pool = &db.pool;
        let config = test_config();
        match parsed.command.expect("a subcommand") {
            Command::User(command) => user(pool, &config, command, now).await,
            Command::Team(command) => team(pool, command, now).await,
            Command::Unassigned => unassigned(pool).await,
            Command::Settings(command) => {
                crate::cli::admin::settings(pool, &config.auth, command, now).await
            }
            Command::Mail(command) => crate::cli::admin::mail(pool, command, now).await,
            Command::Catalog(CatalogCommand::SetOwner { name, team }) => {
                set_owner(pool, &name, &team, now).await
            }
            Command::Catalog(CatalogCommand::SetMemberAccess { name, access }) => {
                set_member_access(pool, &name, access, now).await
            }
            Command::Key(KeyCommand::Assign { id, actor, user }) => {
                assign_key(pool, &id, actor.as_deref(), &user).await
            }
            Command::Oauth(OauthCommand::Code(CodeCommand::Assign { id, user })) => {
                assign_code(pool, &id, &user).await
            }
            other => {
                let cli = Cli {
                    database_url: Some(db.url.clone()),
                    config: None,
                    command: Some(other),
                };
                run(cli, Arc::new(FixedClock(now))).await?;
                Ok(String::new())
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn user_create_list_and_delete() {
        let db = test_db().await;

        let created = okf(
            &db,
            &["user", "create", "alice", "--display-name", "Alice Example"],
        )
        .await
        .unwrap();
        let listed = okf(&db, &["user", "list"]).await.unwrap();
        let deleted = okf(&db, &["user", "delete", "alice"]).await.unwrap();
        let after = okf(&db, &["user", "list"]).await.unwrap();

        assert_eq!(created, "created user alice (personal team alice)\n");
        // The fixture user sorts after alice.
        assert_eq!(
            listed,
            "alice\tcredentials 0\temail -\tadmin no\tteams alice\tAlice Example\n\
             test-user\tcredentials 0\temail -\tadmin no\tteams test-user\t\n"
        );
        assert_eq!(
            deleted,
            "deleted user alice (0 key(s) and grant(s), 0 sign-in code(s), \
             0 open changeset(s) discarded)\n"
        );
        assert!(!after.contains("alice"), "{after}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn user_list_with_an_email_prints_only_that_user() {
        let db = test_db().await;
        okf(&db, &["user", "create", "alice"]).await.unwrap();
        okf(&db, &["user", "create", "bob"]).await.unwrap();
        okf(&db, &["user", "set-email", "alice", "alice@example.com"])
            .await
            .unwrap();

        let listed = okf(&db, &["user", "list", "--email", "ALICE@example.com"])
            .await
            .unwrap();
        let none = okf(&db, &["user", "list", "--email", "x@example.com"])
            .await
            .unwrap();

        assert_eq!(
            listed,
            "alice\tcredentials 0\temail alice@example.com\tadmin no\tteams alice\t\n"
        );
        assert_eq!(none, "");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn team_membership_round_trip() {
        let db = test_db().await;
        okf(&db, &["user", "create", "alice"]).await.unwrap();
        okf(&db, &["user", "create", "bob"]).await.unwrap();

        let created = okf(&db, &["team", "create", "family", "--owner", "alice"])
            .await
            .unwrap();
        let added = okf(&db, &["team", "add-member", "family", "bob"])
            .await
            .unwrap();
        let again = okf(&db, &["team", "add-member", "family", "bob"])
            .await
            .unwrap();
        let promoted = okf(
            &db,
            &["team", "set-role", "family", "bob", "--role", "owner"],
        )
        .await
        .unwrap();
        let removed = okf(&db, &["team", "remove-member", "family", "alice"])
            .await
            .unwrap();
        let listed = okf(&db, &["team", "list"]).await.unwrap();
        let deleted = okf(&db, &["team", "delete", "family"]).await.unwrap();

        assert_eq!(created, "created team family (owner alice)\n");
        assert_eq!(added, "added bob to team family as member\n");
        assert_eq!(again, "bob is already member of team family\n");
        assert_eq!(promoted, "bob is now owner of team family\n");
        assert_eq!(removed, "removed alice from team family\n");
        let family_row = "family\tpersonal -\tmembers bob(owner)\tcatalogs -\tsuspended -\t\n";
        let alice_row = "alice\tpersonal alice\tmembers alice(owner)\tcatalogs -\tsuspended -\t\n";
        assert!(listed.contains(family_row), "{listed}");
        assert!(listed.contains(alice_row), "{listed}");
        assert_eq!(deleted, "deleted team family\n");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn team_suspend_and_unsuspend_print_and_list_the_state() {
        let db = test_db().await;
        let team_name = crate::db::TEST_TEAM;

        let suspended = okf(&db, &["team", "suspend", team_name, "--notice", "Paused."])
            .await
            .unwrap();
        let record = teams::find(&db.pool, team_name).await.unwrap();
        let listed = okf(&db, &["team", "list"]).await.unwrap();
        let lifted = okf(&db, &["team", "unsuspend", team_name]).await.unwrap();
        let listed_after = okf(&db, &["team", "list"]).await.unwrap();
        let lifted_again = okf(&db, &["team", "unsuspend", team_name]).await.unwrap();

        assert_eq!(suspended, format!("suspended team {team_name}\n"));
        let suspension = record.suspension.expect("the team is suspended");
        assert_eq!(suspension.notice.as_deref(), Some("Paused."));
        assert!(
            listed.contains(&format!(
                "\tcatalogs -\tsuspended {}\t",
                format_timestamp(suspension.since)
            )),
            "{listed}"
        );
        assert_eq!(
            lifted,
            format!("lifted the suspension of team {team_name}\n")
        );
        assert!(listed_after.contains("\tsuspended -\t"), "{listed_after}");
        assert!(!listed_after.contains("suspended 2"), "{listed_after}");
        // Lifting an active team changes nothing and still succeeds, as
        // over REST.
        assert_eq!(lifted_again, lifted);
        let after = teams::find(&db.pool, team_name).await.unwrap();
        assert_eq!(after.suspension, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn team_suspend_without_a_notice_and_its_refusals() {
        let db = test_db().await;
        let team_name = crate::db::TEST_TEAM;
        let too_long = "x".repeat(teams::MAX_NOTICE_CHARS + 1);

        let bare = okf(&db, &["team", "suspend", team_name]).await.unwrap();
        let unknown = okf(&db, &["team", "suspend", "nobody"]).await;
        let malformed = okf(&db, &["team", "suspend", "a\0b"]).await;
        let unknown_lift = okf(&db, &["team", "unsuspend", "nobody"]).await;
        let malformed_lift = okf(&db, &["team", "unsuspend", "a\0b"]).await;
        let long = okf(&db, &["team", "suspend", team_name, "--notice", &too_long]).await;
        let nul = okf(&db, &["team", "suspend", team_name, "--notice", "a\0b"]).await;

        assert_eq!(bare, format!("suspended team {team_name}\n"));
        let record = teams::find(&db.pool, team_name).await.unwrap();
        assert_eq!(record.suspension.map(|s| s.notice), Some(None));
        // A malformed name takes the unknown name's path, never a database
        // error; the store's error says which name it did not find.
        for refused in [unknown, malformed, unknown_lift, malformed_lift] {
            let error = refused.unwrap_err();
            assert!(
                matches!(
                    error.downcast_ref::<crate::error::OkfError>(),
                    Some(crate::error::OkfError::UnknownName { .. })
                ),
                "{error}"
            );
        }
        for refused in [long, nul] {
            let error = refused.unwrap_err();
            assert!(
                matches!(
                    error.downcast_ref::<crate::error::OkfError>(),
                    Some(crate::error::OkfError::Refused { .. })
                ),
                "{error}"
            );
        }
        // The refused notices left the earlier suspension as it was.
        let after = teams::find(&db.pool, team_name).await.unwrap();
        assert_eq!(after.suspension.map(|s| s.notice), Some(None));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn removing_the_last_owner_is_refused() {
        let db = test_db().await;
        okf(&db, &["user", "create", "alice"]).await.unwrap();
        okf(&db, &["team", "create", "family", "--owner", "alice"])
            .await
            .unwrap();

        let refused = okf(&db, &["team", "remove-member", "family", "alice"]).await;

        let message = refused.unwrap_err().to_string();
        assert!(message.contains("last owner"), "{message}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn catalog_create_needs_an_owner_and_set_owner_moves_it() {
        let db = test_db().await;
        okf(&db, &["user", "create", "alice"]).await.unwrap();
        okf(&db, &["team", "create", "family", "--owner", "alice"])
            .await
            .unwrap();

        let missing = okf(&db, &["catalog", "create", "notes"]).await;
        okf(&db, &["catalog", "create", "notes", "--owner", "alice"])
            .await
            .unwrap();
        let moved = okf(&db, &["catalog", "set-owner", "notes", "--team", "family"])
            .await
            .unwrap();
        let unknown = okf(&db, &["catalog", "set-owner", "notes", "--team", "nobody"]).await;
        let teams = okf(&db, &["team", "list"]).await.unwrap();

        assert!(missing.is_err(), "clap requires --owner");
        assert_eq!(moved, "catalog notes: owner alice -> family\n");
        assert!(unknown.is_err());
        assert!(teams.contains("catalogs notes"), "{teams}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_owner_names_no_previous_owner_for_a_legacy_catalog() {
        let db = test_db().await;
        sqlx::query(
            "INSERT INTO catalogs (name, description, created_at) VALUES ('old', '', now())",
        )
        .execute(&db.pool)
        .await
        .unwrap();

        let unassigned_before = okf(&db, &["unassigned"]).await.unwrap();
        let moved = okf(&db, &["catalog", "set-owner", "old", "--team", "test-user"])
            .await
            .unwrap();

        assert_eq!(unassigned_before, "catalog old\n");
        assert_eq!(moved, "catalog old: owner (none) -> test-user\n");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn key_assign_and_unassigned() {
        let db = test_db().await;
        okf(&db, &["user", "create", "alice"]).await.unwrap();
        keys::insert_legacy_test_key(&db.pool, "legacy", keys::Access::Read, "legacy", Utc::now())
            .await;

        let before = okf(&db, &["unassigned"]).await.unwrap();
        let assigned = okf(&db, &["key", "assign", "legacy", "--user", "alice"])
            .await
            .unwrap();
        let after = okf(&db, &["unassigned"]).await.unwrap();
        let users = okf(&db, &["user", "list"]).await.unwrap();

        assert_eq!(before, "key legacy (id legacy, actor legacy)\n");
        assert_eq!(assigned, "key legacy now belongs to alice\n");
        assert_eq!(after, "nothing is unassigned\n");
        assert!(users.starts_with("alice\tcredentials 1\t"), "{users}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn code_assign_gives_a_code_a_user() {
        let db = test_db().await;
        okf(&db, &["user", "create", "alice"]).await.unwrap();
        okf(
            &db,
            &[
                "oauth", "code", "create", "laptop", "--access", "read", "--all", "--user",
                TEST_USER,
            ],
        )
        .await
        .unwrap();
        let code = codes::list(&db.pool).await.unwrap().remove(0);

        let assigned = okf(
            &db,
            &["oauth", "code", "assign", &code.id, "--user", "alice"],
        )
        .await
        .unwrap();
        let unknown = okf(&db, &["oauth", "code", "assign", "nope", "--user", "alice"]).await;

        assert_eq!(
            assigned,
            format!("sign-in code {} now belongs to alice\n", code.id)
        );
        let after = codes::list(&db.pool).await.unwrap().remove(0);
        assert_eq!(after.user.as_deref(), Some("alice"));
        // `--actor` was left out: the actor is the code's name.
        assert_eq!(after.actor, "laptop");
        assert!(unknown.is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn key_assign_reaches_an_unassigned_grant_and_its_code() {
        use crate::store::oauth::clients::ClientKind;
        use crate::store::oauth::codes::insert_test_client;

        let db = test_db().await;
        okf(&db, &["user", "create", "alice"]).await.unwrap();
        okf(
            &db,
            &[
                "oauth", "code", "create", "phone", "--access", "read", "--all", "--user",
                TEST_USER,
            ],
        )
        .await
        .unwrap();
        let code = codes::list(&db.pool).await.unwrap().remove(0);
        let client = insert_test_client(
            &db.pool,
            "dcr-client",
            ClientKind::Dcr,
            "https://client.example.com/callback",
        )
        .await;
        let grant = codes::redeem(&db.pool, &code.id, &client, Utc::now())
            .await
            .unwrap();
        // Make both rows look as if they predate migration 0009.
        sqlx::query("UPDATE api_keys SET user_id = NULL")
            .execute(&db.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE sign_in_codes SET user_id = NULL")
            .execute(&db.pool)
            .await
            .unwrap();
        let grant_name = keys::list(&db.pool).await.unwrap().remove(0).name;

        let before = okf(&db, &["unassigned"]).await.unwrap();
        let assigned = okf(&db, &["key", "assign", &grant, "--user", "alice"])
            .await
            .unwrap();
        okf(
            &db,
            &["oauth", "code", "assign", &code.id, "--user", "alice"],
        )
        .await
        .unwrap();
        let after = okf(&db, &["unassigned"]).await.unwrap();

        assert_eq!(
            before,
            format!(
                "grant {grant_name} (id {grant})\ncode phone (id {})\n",
                code.id
            )
        );
        assert_eq!(assigned, format!("key {grant} now belongs to alice\n"));
        assert_eq!(after, "nothing is unassigned\n");
        let record = keys::list(&db.pool).await.unwrap().remove(0);
        assert_eq!(record.user.as_deref(), Some("alice"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deleting_a_user_takes_its_keys_and_may_delete_the_last_admin() {
        let db = test_db().await;
        okf(&db, &["user", "create", "alice"]).await.unwrap();
        okf(
            &db,
            &[
                "key", "create", "k", "--access", "read", "--all", "--user", "alice",
            ],
        )
        .await
        .unwrap();
        okf(&db, &["user", "set-admin", "alice", "--on"])
            .await
            .unwrap();

        let deleted = okf(&db, &["user", "delete", "alice"]).await.unwrap();
        let keys = okf(&db, &["key", "list"]).await.unwrap();

        assert_eq!(
            deleted,
            "deleted user alice (1 key(s) and grant(s), 0 sign-in code(s), \
             0 open changeset(s) discarded)\n"
        );
        assert!(!keys.contains("alice"), "{keys}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_admin_turns_the_flag_on_and_off_and_may_clear_the_last_admin() {
        let db = test_db().await;
        okf(&db, &["user", "create", "alice"]).await.unwrap();

        let on = okf(&db, &["user", "set-admin", "alice", "--on"])
            .await
            .unwrap();
        let again = okf(&db, &["user", "set-admin", "alice", "--on"])
            .await
            .unwrap();
        let listed = okf(&db, &["user", "list"]).await.unwrap();
        let admins_before = users::list(&db.pool)
            .await
            .unwrap()
            .iter()
            .filter(|user| user.is_admin)
            .count();
        // alice is the only admin: the CLI may still clear it (Decision 16),
        // which the API refuses (`users::set_admin` with `allow_last: false`).
        let off = okf(&db, &["user", "set-admin", "alice", "--off"])
            .await
            .unwrap();
        let admins_after = users::list(&db.pool)
            .await
            .unwrap()
            .iter()
            .filter(|user| user.is_admin)
            .count();
        let already_off = okf(&db, &["user", "set-admin", "alice", "--off"])
            .await
            .unwrap();
        let neither = okf(&db, &["user", "set-admin", "alice"]).await;
        let both = okf(&db, &["user", "set-admin", "alice", "--on", "--off"]).await;
        let unknown = okf(&db, &["user", "set-admin", "nobody", "--on"]).await;

        assert_eq!(on, "alice is now an admin\n");
        assert_eq!(again, "alice is already an admin\n");
        assert!(
            listed.starts_with("alice\tcredentials 0\temail -\tadmin yes\t"),
            "{listed}"
        );
        assert_eq!(admins_before, 1, "alice was the last admin");
        assert_eq!(off, "alice is no longer an admin\n");
        assert_eq!(admins_after, 0, "the CLI cleared the last admin");
        assert_eq!(already_off, "alice is already not an admin\n");
        assert!(neither.is_err(), "clap requires --on or --off");
        assert!(both.is_err(), "clap refuses both");
        assert!(unknown.is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_email_sets_clears_and_refuses_a_duplicate() {
        let db = test_db().await;
        okf(&db, &["user", "create", "alice"]).await.unwrap();

        let set = okf(&db, &["user", "set-email", "alice", "alice@example.com"])
            .await
            .unwrap();
        let duplicate = okf(&db, &["user", "set-email", TEST_USER, "ALICE@example.com"]).await;
        let malformed = okf(&db, &["user", "set-email", TEST_USER, "not an address"]).await;
        let listed = okf(&db, &["user", "list"]).await.unwrap();
        let cleared = okf(&db, &["user", "set-email", "alice", "--clear"])
            .await
            .unwrap();
        let neither = okf(&db, &["user", "set-email", "alice"]).await;
        let both = okf(
            &db,
            &["user", "set-email", "alice", "alice@example.com", "--clear"],
        )
        .await;
        let after = okf(&db, &["user", "list"]).await.unwrap();

        assert_eq!(set, "alice: email alice@example.com\n");
        assert!(duplicate.is_err(), "emails are unique, case-insensitively");
        assert!(malformed.is_err(), "not an address");
        assert!(
            listed.contains("alice\tcredentials 0\temail alice@example.com\t"),
            "{listed}"
        );
        assert_eq!(cleared, "alice: email cleared\n");
        assert!(neither.is_err(), "clap requires EMAIL or --clear");
        assert!(both.is_err(), "clap refuses both");
        assert!(
            after.starts_with("alice\tcredentials 0\temail -\t"),
            "{after}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn invite_queues_a_setup_mail_and_never_prints_the_link() {
        let db = test_db().await;
        okf(&db, &["user", "create", "alice"]).await.unwrap();
        let without = okf(&db, &["user", "invite", "alice"]).await;
        okf(&db, &["user", "set-email", "alice", "alice@example.com"])
            .await
            .unwrap();

        let invited = okf(&db, &["user", "invite", "alice"]).await.unwrap();

        assert!(without.is_err(), "no email yet");
        assert_eq!(
            invited,
            "queued a setup email for alice to alice@example.com\n"
        );
        assert!(!invited.contains("token") && !invited.contains("https://"));
        let queued = crate::mail::queue::list(&db.pool).await.unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].template, "setup");
        assert_eq!(queued[0].to, "alice@example.com");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn entitle_and_unentitle_change_the_ceiling() {
        let db = test_db().await;
        okf(&db, &["user", "create", "alice"]).await.unwrap();
        okf(&db, &["catalog", "create", "notes", "--owner", TEST_USER])
            .await
            .unwrap();

        let entitled = okf(
            &db,
            &["user", "entitle", "alice", "notes", "--access", "read"],
        )
        .await
        .unwrap();
        let ceiling = ceiling::ceiling_of(&db.pool, "alice").await.unwrap();
        let admin = okf(
            &db,
            &["user", "entitle", "alice", "notes", "--access", "admin"],
        )
        .await;
        let unknown = okf(
            &db,
            &["user", "entitle", "alice", "nowhere", "--access", "read"],
        )
        .await;
        let removed = okf(&db, &["user", "unentitle", "alice", "notes"])
            .await
            .unwrap();
        let after = ceiling::ceiling_of(&db.pool, "alice").await.unwrap();

        assert_eq!(entitled, "alice may now use notes (read)\n");
        assert_eq!(ceiling.get("notes"), Some(&keys::Access::Read));
        assert!(admin.is_err(), "entitlements are read or write");
        assert!(unknown.is_err(), "an unknown catalog");
        assert_eq!(
            removed,
            "alice may no longer use notes through an entitlement\n"
        );
        assert!(!after.contains_key("notes"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sessions_revoke_sessions_and_sign_ins_list_a_users_activity() {
        use crate::store::sign_in_events::{Method, Outcome};

        let db = test_db().await;
        okf(&db, &["user", "create", "alice"]).await.unwrap();
        let id = users::id_by_name(&db.pool, "alice").await.unwrap();
        let day = std::time::Duration::from_secs(24 * 3600);
        let mut raw_ids = Vec::new();
        for agent in ["Browser One", "Browser Two"] {
            let started = user_sessions::create(&db.pool, &id, None, agent, day, Utc::now())
                .await
                .unwrap();
            raw_ids.push(started.id);
        }
        sign_in_events::record(
            &db.pool,
            Some(&id),
            Method::Password,
            Outcome::Failed,
            None,
            Utc::now(),
        )
        .await
        .unwrap();

        let sessions = okf(&db, &["user", "sessions", "alice"]).await.unwrap();
        let sign_ins = okf(&db, &["user", "sign-ins", "alice"]).await.unwrap();
        let revoked = okf(&db, &["user", "revoke-sessions", "alice"])
            .await
            .unwrap();
        let none = okf(&db, &["user", "sessions", "alice"]).await.unwrap();
        let unknown = okf(&db, &["user", "sessions", "nobody"]).await;

        assert_eq!(sessions.lines().count(), 2, "{sessions}");
        assert!(sessions.contains("\tip -\tBrowser One\n"), "{sessions}");
        for line in sessions.lines() {
            // The first cell is the 12-character prefix of the stored hash.
            let prefix = line.split('\t').next().unwrap_or_default();
            assert_eq!(prefix.len(), user_sessions::ID_PREFIX_LENGTH, "{line}");
        }
        for raw in &raw_ids {
            assert!(!sessions.contains(raw.as_str()), "a session id is printed");
        }
        assert_eq!(sign_ins.lines().count(), 1, "{sign_ins}");
        assert!(
            sign_ins.ends_with("\tpassword\tfailed\tip -\n"),
            "{sign_ins}"
        );
        assert_eq!(revoked, "revoked 2 sessions of alice\n");
        assert_eq!(none, "alice has no sessions\n");
        assert!(unknown.is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_member_access_reports_the_previous_level() {
        let db = test_db().await;
        okf(&db, &["catalog", "create", "notes", "--owner", TEST_USER])
            .await
            .unwrap();

        let read = okf(
            &db,
            &["catalog", "set-member-access", "notes", "--access", "read"],
        )
        .await
        .unwrap();
        let none = okf(
            &db,
            &["catalog", "set-member-access", "notes", "--access", "none"],
        )
        .await
        .unwrap();
        let unknown = okf(
            &db,
            &[
                "catalog",
                "set-member-access",
                "nowhere",
                "--access",
                "read",
            ],
        )
        .await;
        let stored = ceiling::member_access(&db.pool, "notes").await.unwrap();

        assert_eq!(read, "catalog notes: member access write -> read\n");
        assert_eq!(none, "catalog notes: member access read -> none\n");
        assert!(unknown.is_err());
        assert_eq!(stored, MemberAccess::None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn invite_and_mail_test_queue_without_any_mail_secret() {
        // A Postmark config whose token variable is unset: building a mailer
        // would fail, so success proves the CLI only queues.
        const UNSET: &str = "OKF_CLI_TEST_POSTMARK_TOKEN_UNSET";
        assert!(std::env::var(UNSET).is_err(), "{UNSET} must be unset");
        let db = test_db().await;
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("okf.toml");
        std::fs::write(
            &config_path,
            format!(
                "public_url = \"https://okf.example.com\"\n\
                 allowed_hosts = [\"okf.example.com\"]\n\
                 [mail]\nprovider = \"postmark\"\n\
                 [mail.postmark]\ntoken_env = \"{UNSET}\"\n"
            ),
        )
        .unwrap();
        okf(&db, &["user", "create", "alice"]).await.unwrap();
        okf(&db, &["user", "set-email", "alice", "alice@example.com"])
            .await
            .unwrap();
        // Through `cli::run`, as the binary runs them, with that config file.
        let through_run = |args: &[&str]| {
            let parsed =
                Cli::try_parse_from(std::iter::once(&"okf-mcp").chain(args)).expect("parses");
            Cli {
                database_url: Some(db.url.clone()),
                config: Some(config_path.clone()),
                command: parsed.command,
            }
        };

        run(
            through_run(&["user", "invite", "alice"]),
            Arc::new(FixedClock(Utc::now())),
        )
        .await
        .unwrap();
        run(
            through_run(&["mail", "test", "ops@example.com"]),
            Arc::new(FixedClock(Utc::now())),
        )
        .await
        .unwrap();

        let mut templates: Vec<String> = crate::mail::queue::list(&db.pool)
            .await
            .unwrap()
            .into_iter()
            .map(|mail| mail.template)
            .collect();
        templates.sort();
        assert_eq!(templates, ["setup", "test"]);
    }

    #[test]
    fn admin_changes_are_described() {
        assert_eq!(
            describe_admin_change("alice", false, false),
            "alice is already not an admin\n"
        );
        assert_eq!(
            describe_admin_change("alice", true, false),
            "alice is no longer an admin\n"
        );
    }

    #[test]
    fn member_changes_are_described() {
        assert_eq!(
            describe_member_change(MemberChange::Added, "family", "bob", Role::Owner),
            "added bob to team family as owner\n"
        );
        assert_eq!(
            describe_member_change(MemberChange::RoleChanged, "family", "bob", Role::Member),
            "bob is now member of team family\n"
        );
    }
}
