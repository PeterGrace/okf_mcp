//! `okf-mcp user ...`, `team ...`, `unassigned`, and the assignment
//! subcommands (`catalog set-owner`, `key assign`, `oauth code assign`).
//!
//! Each handler returns the text to print (one `\n`-terminated line per
//! row) instead of printing it, so the tests can compare it exactly. A
//! store write already bumps the administrative generation, which is how a
//! running server notices the change, so the handlers do nothing else.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use super::{labelled, tab_row, TeamCommand, UserCommand};
use crate::store::catalogs;
use crate::store::keys::{self, KeyKind};
use crate::store::oauth::codes;
use crate::store::teams::{self, MemberChange, NewTeam, Role, TeamRecord};
use crate::store::unassigned;
use crate::store::users::{self, NewUser, UserRecord};

/// Runs `okf-mcp user ...`.
///
/// # Arguments
/// * `pool` - the migrated database.
/// * `command` - which user subcommand.
/// * `now` - the creation time for `create` (server clock).
///
/// # Returns
/// The text to print.
///
/// # Errors
/// The store's refusal (an invalid or taken name, a user something still
/// names), an unknown user, or a database error.
pub(super) async fn user(
    pool: &PgPool,
    command: UserCommand,
    now: DateTime<Utc>,
) -> anyhow::Result<String> {
    Ok(match command {
        UserCommand::Create { name, display_name } => {
            let new = NewUser {
                name: &name,
                display_name: &display_name,
            };
            let created = users::create(pool, &new, now).await?;
            // The personal team shares the user's name.
            format!(
                "created user {} (personal team {})\n",
                created.name, created.name
            )
        }
        UserCommand::List => lines(users::list(pool).await?.iter().map(format_user)),
        UserCommand::Delete { name } => {
            users::delete(pool, &name).await?;
            format!("deleted user {name}\n")
        }
    })
}

/// Runs `okf-mcp team ...`.
///
/// # Arguments
/// * `pool` - the migrated database.
/// * `command` - which team subcommand.
/// * `now` - the creation time for `create` and new memberships.
///
/// # Returns
/// The text to print.
///
/// # Errors
/// The store's refusal (an invalid or taken name, the last owner, a
/// personal team, a team that owns a catalog), an unknown team or user, or
/// a database error.
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
            let created = teams::create(pool, &new, now).await?;
            format!("created team {} (owner {owner})\n", created.name)
        }
        TeamCommand::List => lines(teams::list(pool).await?.iter().map(format_team)),
        // `|` matches either variant and binds the same names from both,
        // like two `case` patterns sharing one body.
        TeamCommand::AddMember { team, user, role } | TeamCommand::SetRole { team, user, role } => {
            let change = teams::set_member(pool, &team, &user, role, now).await?;
            describe_member_change(change, &team, &user, role)
        }
        TeamCommand::RemoveMember { team, user } => {
            teams::remove_member(pool, &team, &user).await?;
            format!("removed {user} from team {team}\n")
        }
        TeamCommand::Delete { name } => {
            teams::delete(pool, &name).await?;
            format!("deleted team {name}\n")
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
///
/// # Returns
/// `catalog C: owner OLD -> NEW`, with `(none)` for a catalog that had no
/// owner.
///
/// # Errors
/// An unknown catalog or team, or a database error.
pub(super) async fn set_owner(pool: &PgPool, catalog: &str, team: &str) -> anyhow::Result<String> {
    let previous = catalogs::set_owner(pool, catalog, team).await?;
    Ok(format!(
        "catalog {catalog}: owner {} -> {team}\n",
        previous.as_deref().unwrap_or("(none)")
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

/// One `user list` line, tab-separated: name, `credentials N`, `teams
/// T1,T2`, then the free-text display name last.
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
        &labelled("teams", comma_list(&user.teams).as_deref()),
        &user.display_name,
    ])
}

/// One `team list` line, tab-separated: name, `personal U` (`-` for a
/// shared team), `members U(role),...`, `catalogs C1,C2`, then the
/// free-text display name last.
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

    /// Runs `okf-mcp <args>` against `db` and returns what it prints. The
    /// commands this module handles return their text; any other runs
    /// through `cli::run` (printing to the real stdout) and returns `""`.
    async fn okf(db: &TestDb, args: &[&str]) -> anyhow::Result<String> {
        let parsed = Cli::try_parse_from(std::iter::once(&"okf-mcp").chain(args))?;
        let now = Utc::now();
        let pool = &db.pool;
        match parsed.command.expect("a subcommand") {
            Command::User(command) => user(pool, command, now).await,
            Command::Team(command) => team(pool, command, now).await,
            Command::Unassigned => unassigned(pool).await,
            Command::Catalog(CatalogCommand::SetOwner { name, team }) => {
                set_owner(pool, &name, &team).await
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
            "alice\tcredentials 0\tteams alice\tAlice Example\n\
             test-user\tcredentials 0\tteams test-user\t\n"
        );
        assert_eq!(deleted, "deleted user alice\n");
        assert!(!after.contains("alice"), "{after}");
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
        assert!(
            listed.contains("family\tpersonal -\tmembers bob(owner)\tcatalogs -\t\n"),
            "{listed}"
        );
        assert!(
            listed.contains("alice\tpersonal alice\tmembers alice(owner)\tcatalogs -\t\n"),
            "{listed}"
        );
        assert_eq!(deleted, "deleted team family\n");
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
    async fn deleting_a_user_with_a_key_fails_and_says_why() {
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

        let refused = okf(&db, &["user", "delete", "alice"]).await;

        let message = refused.unwrap_err().to_string();
        assert!(message.contains("name it"), "{message}");
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
