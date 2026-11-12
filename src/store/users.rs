//! Users: the people credentials belong to (spec "Data model"). Creating a
//! user also creates its personal team, so every user can own catalogs.
//! Users are addressed by name; ids stay inside SQL.

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};

use crate::error::OkfError;
use crate::store::catalogs::{bump_admin_generation, is_unique_violation, is_valid_name};

/// A user to create.
// `<'a>` is a lifetime parameter: the struct borrows its strings from the
// caller (`&'a str`) instead of owning copies, and cannot outlive them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewUser<'a> {
    /// `[a-z0-9-]+`; also its personal team's name.
    pub name: &'a str,
    /// Free text shown beside the name.
    pub display_name: &'a str,
}

/// A stored user.
// `sqlx::FromRow` maps a result row's columns onto the fields by name.
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

/// Refuses a user or team name that is not `[a-z0-9-]+`.
///
/// # Arguments
/// * `noun` - `"user"` or `"team"`, for the message.
/// * `name` - the proposed name.
///
/// # Errors
/// [`OkfError::Refused`] for an invalid name.
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
/// # Arguments
/// * `conn` - the caller's transaction (pass `&mut tx`).
/// * `name` - the proposed name.
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

/// Maps a UNIQUE violation from inserting a user or team row to the
/// "name taken" refusal. `check_free` only narrows the race: two creates of
/// one name (a user and a team, or two of either) can both pass it, and then
/// the `users.name` or `teams.name` constraint decides inside the INSERT.
/// Every user and team insert goes through this, so the loser always gets
/// the 409, never a database error.
///
/// # Arguments
/// * `inserted` - the INSERT's result.
/// * `name` - the name being created.
///
/// # Returns
/// The INSERT's value, unchanged.
///
/// # Errors
/// [`OkfError::Refused`] for a unique violation; [`OkfError::Database`]
/// otherwise.
// Generic over `T`: an insert may return an id (`String`) or anything else.
pub(crate) fn or_name_taken<T>(
    inserted: Result<T, sqlx::Error>,
    name: &str,
) -> Result<T, OkfError> {
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
/// has (including one taken by a concurrent create); [`OkfError::Database`]
/// otherwise.
pub async fn create(
    pool: &PgPool,
    new: &NewUser<'_>,
    now: DateTime<Utc>,
) -> Result<UserRecord, OkfError> {
    check_name("user", new.name)?;
    // A transaction rolls back when `tx` is dropped without `commit`, so
    // every early `?` return below undoes the rows inserted before it.
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
        "INSERT INTO team_members (team_id, user_id, role, created_at) \
         VALUES ($1, $2, 'owner', $3)",
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
/// # Arguments
/// * `pool` - the database.
///
/// # Returns
/// The users, sorted by name.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn list(pool: &PgPool) -> Result<Vec<UserRecord>, OkfError> {
    Ok(
        sqlx::query_as::<_, UserRecord>(concat!(select_users!(), " ORDER BY u.name"))
            .fetch_all(pool)
            .await?,
    )
}

/// The user called `name`.
///
/// # Arguments
/// * `pool` - the database.
/// * `name` - the user's name.
///
/// # Returns
/// The user.
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
/// # Arguments
/// * `conn` - the caller's transaction (pass `&mut tx`).
/// * `name` - the user's name.
///
/// # Returns
/// Its `users.id`.
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

/// The error for a user name that names no user.
fn unknown(name: &str) -> OkfError {
    OkfError::UnknownName {
        noun: "user",
        name: name.to_string(),
    }
}

/// Deletes a user that nothing references, with its personal team, and
/// bumps the administrative generation (spec "Deletion").
///
/// # Arguments
/// * `pool` - the database.
/// * `name` - the user's name.
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
    // Lock the personal team too: a catalog assigned to it concurrently takes
    // `FOR KEY SHARE` on this row, so either it waits for this delete (and
    // then fails its foreign-key check), or this waits for it to commit and
    // the count below sees the catalog. Without the lock the count could miss
    // it, and the team's DELETE would fail as a database error, not a 409.
    // (`Option`: every user has one, but a missing row should not panic.)
    let team_id: Option<String> =
        sqlx::query_scalar("SELECT id FROM teams WHERE personal_of = $1 FOR UPDATE")
            .bind(&user_id)
            .fetch_optional(&mut *tx)
            .await?;
    // A tuple destructures the three columns of the one row, like Python's
    // `a, b, c = row`.
    let (credentials, other_teams, owned): (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM api_keys WHERE user_id = $1) \
              + (SELECT count(*) FROM sign_in_codes WHERE user_id = $1), \
                (SELECT count(*) FROM team_members m JOIN teams t ON t.id = m.team_id \
                 WHERE m.user_id = $1 AND t.personal_of IS DISTINCT FROM $1), \
                (SELECT count(*) FROM catalogs WHERE owner_team_id = $2)",
    )
    .bind(&user_id)
    .bind(&team_id)
    .fetch_one(&mut *tx)
    .await?;
    let blocker = if credentials > 0 {
        Some(format!(
            "{credentials} key(s), grant(s) or sign-in code(s) name it"
        ))
    } else if other_teams > 0 {
        Some(format!("it belongs to {other_teams} other team(s)"))
    } else if owned > 0 {
        Some(format!("its personal team owns {owned} catalog(s)"))
    } else {
        None
    };
    if let Some(what) = blocker {
        return Err(OkfError::Refused {
            reason: format!("user {name:?} cannot be deleted: {what}"),
        });
    }
    // Memberships cascade with the team; the team row goes before the user
    // row because `teams.personal_of` restricts the user's delete.
    if let Some(team_id) = &team_id {
        sqlx::query("DELETE FROM teams WHERE id = $1")
            .bind(team_id)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(&user_id)
        .execute(&mut *tx)
        .await?;
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
// `pub(crate)` so `teams::tests` shares the seeding helpers below.
pub(crate) mod tests {
    use super::*;
    use crate::db::test_db;
    use crate::store::catalogs::{self, tests::until_a_lock_is_awaited};
    use crate::store::teams;

    /// Gives `team` a catalog, `shared`.
    pub(crate) async fn seed_catalog_owned_by(pool: &PgPool, team: &str) {
        catalogs::create(pool, "shared", "", team, now())
            .await
            .unwrap();
    }

    /// Makes catalog `shared` belong to the team named `$1`, by writing
    /// `catalogs.owner_team_id` inside a transaction the race test holds open.
    const OWN_SHARED: &str = "UPDATE catalogs SET owner_team_id = \
                              (SELECT id FROM teams WHERE name = $1) WHERE name = 'shared'";

    /// Asserts a refused user delete changed nothing: the user is still
    /// there, still names `credentials` credentials, and the generation is
    /// still `generation`.
    async fn assert_unchanged(pool: &PgPool, credentials: i64, generation: i64) {
        let user = find(pool, "alice").await.unwrap();
        assert_eq!(user.credentials, credentials);
        assert_eq!(user.teams, ["alice"]);
        assert_eq!(catalogs::admin_generation(pool).await.unwrap(), generation);
    }

    fn now() -> DateTime<Utc> {
        // A fixed instant: tests never read the wall clock.
        DateTime::parse_from_rfc3339("2026-11-11T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn alice() -> NewUser<'static> {
        NewUser {
            name: "alice",
            display_name: "Alice Example",
        }
    }

    fn family_owned_by_alice() -> teams::NewTeam<'static> {
        teams::NewTeam {
            name: "family",
            display_name: "",
            owner: "alice",
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn creating_a_user_creates_its_personal_team_with_it_as_owner() {
        let db = test_db().await;

        let user = create(&db.pool, &alice(), now()).await.unwrap();

        assert_eq!(user.name, "alice");
        assert_eq!(user.teams, ["alice"]);
        assert_eq!(find(&db.pool, "alice").await.unwrap(), user);
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
        teams::create(&db.pool, &family_owned_by_alice(), now())
            .await
            .unwrap();

        let clash = create(
            &db.pool,
            &NewUser {
                name: "family",
                display_name: "",
            },
            now(),
        )
        .await;

        assert!(matches!(clash, Err(OkfError::Refused { .. })), "{clash:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_invalid_or_duplicate_name_is_refused() {
        let db = test_db().await;
        create(&db.pool, &alice(), now()).await.unwrap();

        let duplicate = create(&db.pool, &alice(), now()).await;
        let invalid = create(
            &db.pool,
            &NewUser {
                name: "Alice",
                display_name: "",
            },
            now(),
        )
        .await;

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
        teams::create(&db.pool, &family_owned_by_alice(), now())
            .await
            .unwrap();

        let inserted = sqlx::query_scalar::<_, String>(
            "INSERT INTO teams (name, created_at) VALUES ('family', now()) RETURNING id",
        )
        .fetch_one(&mut *tx)
        .await;

        let mapped = or_name_taken(inserted, "family");
        assert!(
            matches!(mapped, Err(OkfError::Refused { .. })),
            "{mapped:?}"
        );
    }

    #[tokio::test]
    async fn concurrent_user_and_team_creates_of_one_name_give_one_winner_and_a_409() {
        let db = test_db().await;
        create(&db.pool, &alice(), now()).await.unwrap();
        let user = NewUser {
            name: "family",
            display_name: "",
        };
        let team = family_owned_by_alice();

        // `tokio::join!` runs both futures concurrently, like `asyncio.gather`.
        let (made_user, made_team) = tokio::join!(
            create(&db.pool, &user, now()),
            teams::create(&db.pool, &team, now()),
        );

        let refused = [made_user.is_err(), made_team.is_err()];
        assert_eq!(
            refused.iter().filter(|r| **r).count(),
            1,
            "{made_user:?} {made_team:?}"
        );
        for error in [made_user.err(), made_team.err()].into_iter().flatten() {
            assert!(matches!(error, OkfError::Refused { .. }), "{error:?}");
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
        let before = crate::store::catalogs::admin_generation(&db.pool)
            .await
            .unwrap();

        delete(&db.pool, "alice").await.unwrap();

        assert!(matches!(
            find(&db.pool, "alice").await,
            Err(OkfError::UnknownName { .. })
        ));
        assert!(matches!(
            teams::find(&db.pool, "alice").await,
            Err(OkfError::UnknownName { .. })
        ));
        let after = crate::store::catalogs::admin_generation(&db.pool)
            .await
            .unwrap();
        assert!(after > before, "the delete bumps the admin generation");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_user_in_another_team_is_not_deleted() {
        let db = test_db().await;
        create(&db.pool, &alice(), now()).await.unwrap();
        teams::set_member(
            &db.pool,
            crate::db::TEST_TEAM,
            "alice",
            teams::Role::Member,
            now(),
        )
        .await
        .unwrap();

        let refused = delete(&db.pool, "alice").await;

        assert!(
            matches!(refused, Err(OkfError::Refused { .. })),
            "{refused:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deleting_an_unknown_user_is_unknown_name() {
        let db = test_db().await;
        let missing = delete(&db.pool, "nobody").await;
        assert!(matches!(
            missing,
            Err(OkfError::UnknownName { noun: "user", .. })
        ));
    }
    #[tokio::test(flavor = "multi_thread")]
    async fn a_user_named_by_a_key_is_not_deleted() {
        let db = test_db().await;
        create(&db.pool, &alice(), now()).await.unwrap();
        sqlx::query(
            "INSERT INTO api_keys (name, sha256, access, all_catalogs, actor, created_at, user_id) \
             VALUES ('k', decode(repeat('ab', 32), 'hex'), 'read', true, 'k', now(), \
                     (SELECT id FROM users WHERE name = 'alice'))",
        )
        .execute(&db.pool)
        .await
        .unwrap();
        let generation = catalogs::admin_generation(&db.pool).await.unwrap();

        let refused = delete(&db.pool, "alice").await;

        assert!(
            matches!(&refused, Err(OkfError::Refused { reason }) if reason.contains("key(s)")),
            "{refused:?}"
        );
        assert_unchanged(&db.pool, 1, generation).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_user_named_by_a_sign_in_code_is_not_deleted() {
        let db = test_db().await;
        create(&db.pool, &alice(), now()).await.unwrap();
        sqlx::query(
            "INSERT INTO sign_in_codes \
                 (name, sha256, access, all_catalogs, actor, created_at, expires_at, user_id) \
             VALUES ('laptop', decode(repeat('cd', 32), 'hex'), 'read', true, 'laptop', \
                     now(), now() + interval '1 hour', \
                     (SELECT id FROM users WHERE name = 'alice'))",
        )
        .execute(&db.pool)
        .await
        .unwrap();
        let generation = catalogs::admin_generation(&db.pool).await.unwrap();

        let refused = delete(&db.pool, "alice").await;

        assert!(
            matches!(&refused, Err(OkfError::Refused { reason }) if reason.contains("code(s)")),
            "{refused:?}"
        );
        assert_unchanged(&db.pool, 1, generation).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_user_whose_personal_team_owns_a_catalog_is_not_deleted() {
        let db = test_db().await;
        create(&db.pool, &alice(), now()).await.unwrap();
        seed_catalog_owned_by(&db.pool, "alice").await;
        let generation = catalogs::admin_generation(&db.pool).await.unwrap();

        let refused = delete(&db.pool, "alice").await;

        assert!(
            matches!(&refused, Err(OkfError::Refused { reason }) if reason.contains("catalog")),
            "{refused:?}"
        );
        assert_unchanged(&db.pool, 0, generation).await;
        let team = teams::find(&db.pool, "alice").await.unwrap();
        assert_eq!(team.catalogs, ["shared"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_catalog_assigned_during_the_delete_is_refused_not_a_database_error() {
        // Arrange: a transaction assigns a catalog to alice's personal team
        // and holds its commit while the delete runs.
        let db = test_db().await;
        create(&db.pool, &alice(), now()).await.unwrap();
        catalogs::create(&db.pool, "shared", "", crate::db::TEST_TEAM, now())
            .await
            .unwrap();
        let mut assigning = db.pool.begin().await.unwrap();
        sqlx::query(OWN_SHARED)
            .bind("alice")
            .execute(&mut *assigning)
            .await
            .unwrap();
        // `tokio::spawn` runs the delete concurrently, like a thread; it
        // blocks on the personal team's row, which the assignment's
        // foreign-key check holds `FOR KEY SHARE`.
        let pool = db.pool.clone();
        let deleting = tokio::spawn(async move { delete(&pool, "alice").await });
        until_a_lock_is_awaited(&db.pool).await;

        assigning.commit().await.unwrap();
        let outcome = deleting.await.unwrap();

        assert!(
            matches!(&outcome, Err(OkfError::Refused { reason }) if reason.contains("catalog")),
            "{outcome:?}"
        );
        assert_eq!(
            teams::find(&db.pool, "alice").await.unwrap().catalogs,
            ["shared"]
        );
    }
}
