//! Policy settings (spec "Settings"): the `[auth]` config supplies the
//! defaults, and `server_settings` rows override them. Every change bumps
//! `admin_generation`, so running servers reload the effective settings
//! with the key cache (`App::reload_keys_if_changed`); request handlers
//! read them from `App::settings()`, never from here.
//!
//! | Key | Stored as (`server_settings.value`) | Accepts ([`set`]) |
//! |---|---|---|
//! | `password_sign_in` | JSON boolean | `true`, `false`, `on`, `off` |
//! | `max_keys_per_user` | JSON number | `0` to `1000` |
//! | `max_key_lifetime` | JSON string (humantime) or `null` | a duration such as `90d`, or `none` |
//! | the four other lifetimes | JSON string (humantime) | a duration above zero |
//!
//! One function, `apply`, checks a value both when it is set (a bad one
//! is refused with 422) and when it is loaded (a row this server cannot
//! read, from a newer version or a hand edit, is logged at `warn` and the
//! config default used), so a stored value can never stop the server.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
// humantime is the crate humantime_serde parses config durations with;
// `re` re-exports it, so the formats match without another dependency.
use humantime_serde::re::humantime;
use serde_json::Value;
use sqlx::PgPool;

use crate::config::{check_auth_duration, AuthConfig, MAX_KEYS_PER_USER_LIMIT};
use crate::error::OkfError;
use crate::store::catalogs::bump_admin_generation;

/// The effective sign-in policy: config defaults with overrides applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Whether `/signin` offers email and password.
    pub password_sign_in: bool,
    /// Live self-issued keys a user may hold.
    pub max_keys_per_user: u32,
    /// The longest self-issued key expiry; `None` is no limit.
    pub max_key_lifetime: Option<Duration>,
    /// Console session idle timeout.
    pub console_session_idle: Duration,
    /// Console session absolute lifetime.
    pub console_session_absolute: Duration,
    /// Setup link lifetime.
    pub setup_link_ttl: Duration,
    /// Password-reset link lifetime.
    pub reset_link_ttl: Duration,
}

impl Settings {
    /// The settings with no overrides.
    ///
    /// # Arguments
    /// * `defaults` - the `[auth]` section.
    ///
    /// # Returns
    /// Its policy values.
    pub fn from_config(defaults: &AuthConfig) -> Self {
        Self {
            password_sign_in: defaults.password_sign_in,
            max_keys_per_user: defaults.max_keys_per_user,
            max_key_lifetime: defaults.max_key_lifetime,
            console_session_idle: defaults.console_session_idle,
            console_session_absolute: defaults.console_session_absolute,
            setup_link_ttl: defaults.setup_link_ttl,
            reset_link_ttl: defaults.reset_link_ttl,
        }
    }
}

/// One setting's name. clap spells each exactly as stored
/// (`#[value(name = ...)]`; its default would be kebab-case).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, clap::ValueEnum)]
pub enum SettingKey {
    /// `password_sign_in`.
    #[value(name = "password_sign_in")]
    PasswordSignIn,
    /// `max_keys_per_user`.
    #[value(name = "max_keys_per_user")]
    MaxKeysPerUser,
    /// `max_key_lifetime`.
    #[value(name = "max_key_lifetime")]
    MaxKeyLifetime,
    /// `console_session_idle`.
    #[value(name = "console_session_idle")]
    ConsoleSessionIdle,
    /// `console_session_absolute`.
    #[value(name = "console_session_absolute")]
    ConsoleSessionAbsolute,
    /// `setup_link_ttl`.
    #[value(name = "setup_link_ttl")]
    SetupLinkTtl,
    /// `reset_link_ttl`.
    #[value(name = "reset_link_ttl")]
    ResetLinkTtl,
}

impl SettingKey {
    /// Every key, in the order `settings list` shows them.
    pub const ALL: [SettingKey; 7] = [
        SettingKey::PasswordSignIn,
        SettingKey::MaxKeysPerUser,
        SettingKey::MaxKeyLifetime,
        SettingKey::ConsoleSessionIdle,
        SettingKey::ConsoleSessionAbsolute,
        SettingKey::SetupLinkTtl,
        SettingKey::ResetLinkTtl,
    ];

    /// The stored name.
    ///
    /// # Returns
    /// The `server_settings.key` value.
    pub fn as_str(self) -> &'static str {
        match self {
            SettingKey::PasswordSignIn => "password_sign_in",
            SettingKey::MaxKeysPerUser => "max_keys_per_user",
            SettingKey::MaxKeyLifetime => "max_key_lifetime",
            SettingKey::ConsoleSessionIdle => "console_session_idle",
            SettingKey::ConsoleSessionAbsolute => "console_session_absolute",
            SettingKey::SetupLinkTtl => "setup_link_ttl",
            SettingKey::ResetLinkTtl => "reset_link_ttl",
        }
    }

    /// Reads a stored name.
    ///
    /// # Arguments
    /// * `name` - a `server_settings.key` value.
    ///
    /// # Returns
    /// The key, or `None` for a name this server does not know.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|key| key.as_str() == name)
    }
}

/// One row of `settings list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingRow {
    /// Which setting.
    pub key: SettingKey,
    /// Its effective value, as [`set`] accepts it (`"true"`, `"20"`,
    /// `"12h"`, `"none"`).
    pub value: String,
    /// Whether a `server_settings` row overrides the config default.
    pub overridden: bool,
    /// When the override was written.
    pub updated_at: Option<DateTime<Utc>>,
    /// Who wrote it (an actor or user name).
    pub updated_by: Option<String>,
}

/// The refusal for a value a setting cannot take.
fn invalid(reason: String) -> OkfError {
    OkfError::LimitExceeded {
        limit: "setting",
        reason,
    }
}

/// Parses a duration in the config file's format and checks it as the
/// config file does.
fn duration(key: SettingKey, text: &str) -> Result<Duration, String> {
    let parsed = humantime::parse_duration(text)
        .map_err(|error| format!("{}: {text:?} is not a duration ({error})", key.as_str()))?;
    check_auth_duration(key.as_str(), parsed)?;
    Ok(parsed)
}

/// Turns the text [`set`] was given into the JSON to store.
fn parse_value(key: SettingKey, text: &str) -> Result<Value, String> {
    let text = text.trim();
    match key {
        SettingKey::PasswordSignIn => match text {
            "true" | "on" => Ok(Value::Bool(true)),
            "false" | "off" => Ok(Value::Bool(false)),
            _ => Err(format!(
                "password_sign_in takes true or false, not {text:?}"
            )),
        },
        // `u32` parsing refuses a sign, so "-1" fails here; the upper
        // bound is `apply`'s.
        SettingKey::MaxKeysPerUser => text.parse::<u32>().map(Value::from).map_err(|_| {
            format!("max_keys_per_user takes a whole number from 0 to {MAX_KEYS_PER_USER_LIMIT}")
        }),
        SettingKey::MaxKeyLifetime if text == "none" => Ok(Value::Null),
        // Stored in humantime's own spelling, so `list` shows what `load` reads.
        SettingKey::MaxKeyLifetime
        | SettingKey::ConsoleSessionIdle
        | SettingKey::ConsoleSessionAbsolute
        | SettingKey::SetupLinkTtl
        | SettingKey::ResetLinkTtl => duration(key, text)
            .map(|parsed| Value::String(humantime::format_duration(parsed).to_string())),
    }
}

/// Applies one stored value to `settings`, after checking it. Used when
/// setting a value (to refuse a bad one) and when loading (to skip one).
/// On an error `settings` is unchanged.
fn apply(settings: &mut Settings, key: SettingKey, value: &Value) -> Result<(), String> {
    // A closure (Python's lambda, but it may span lines) shared by the
    // five duration keys.
    let as_duration = |value: &Value| -> Result<Duration, String> {
        let text = value
            .as_str()
            .ok_or_else(|| format!("{} must be a duration string", key.as_str()))?;
        duration(key, text)
    };
    match key {
        SettingKey::PasswordSignIn => {
            settings.password_sign_in = value
                .as_bool()
                .ok_or_else(|| "password_sign_in must be true or false".to_string())?;
        }
        SettingKey::MaxKeysPerUser => {
            // `as_u64` is `None` for negatives, fractions and non-numbers;
            // `try_from` refuses what does not fit a `u32`.
            settings.max_keys_per_user = value
                .as_u64()
                .and_then(|count| u32::try_from(count).ok())
                .filter(|count| *count <= MAX_KEYS_PER_USER_LIMIT)
                .ok_or_else(|| {
                    format!("max_keys_per_user must be from 0 to {MAX_KEYS_PER_USER_LIMIT}")
                })?;
        }
        SettingKey::MaxKeyLifetime => {
            settings.max_key_lifetime = match value {
                Value::Null => None,
                other => Some(as_duration(other)?),
            };
        }
        SettingKey::ConsoleSessionIdle => settings.console_session_idle = as_duration(value)?,
        SettingKey::ConsoleSessionAbsolute => {
            settings.console_session_absolute = as_duration(value)?;
        }
        SettingKey::SetupLinkTtl => settings.setup_link_ttl = as_duration(value)?,
        SettingKey::ResetLinkTtl => settings.reset_link_ttl = as_duration(value)?,
    }
    Ok(())
}

/// A setting's effective value as [`set`] would accept it.
fn display(settings: &Settings, key: SettingKey) -> String {
    let format = |value: Duration| humantime::format_duration(value).to_string();
    match key {
        SettingKey::PasswordSignIn => settings.password_sign_in.to_string(),
        SettingKey::MaxKeysPerUser => settings.max_keys_per_user.to_string(),
        SettingKey::MaxKeyLifetime => settings
            .max_key_lifetime
            .map_or_else(|| "none".to_string(), format),
        SettingKey::ConsoleSessionIdle => format(settings.console_session_idle),
        SettingKey::ConsoleSessionAbsolute => format(settings.console_session_absolute),
        SettingKey::SetupLinkTtl => format(settings.setup_link_ttl),
        SettingKey::ResetLinkTtl => format(settings.reset_link_ttl),
    }
}

/// One stored override.
#[derive(sqlx::FromRow)]
struct StoredSetting {
    key: String,
    value: Value,
    updated_at: DateTime<Utc>,
    updated_by: String,
}

/// Overlays stored rows on the defaults, skipping (with a log line) rows
/// this server cannot read.
fn overlay(defaults: &AuthConfig, rows: &[StoredSetting]) -> Settings {
    let mut settings = Settings::from_config(defaults);
    for row in rows {
        let Some(key) = SettingKey::parse(&row.key) else {
            tracing::debug!(setting = %row.key, "ignoring a setting this server does not know");
            continue;
        };
        if let Err(problem) = apply(&mut settings, key, &row.value) {
            tracing::warn!(
                setting = %row.key,
                %problem,
                "stored setting is invalid; using the default"
            );
        }
    }
    settings
}

/// Every stored override.
async fn stored(pool: &PgPool) -> Result<Vec<StoredSetting>, OkfError> {
    Ok(sqlx::query_as::<_, StoredSetting>(
        "SELECT key, value, updated_at, updated_by FROM server_settings",
    )
    .fetch_all(pool)
    .await?)
}

/// The effective settings. Only code without an `App` (the CLI, and
/// `App` itself when it loads or reloads) calls this.
///
/// # Arguments
/// * `pool` - the database.
/// * `defaults` - the `[auth]` section.
///
/// # Returns
/// The defaults with every readable override applied.
///
/// # Errors
/// [`OkfError::Database`]. An unreadable stored value is not an error.
pub async fn load(pool: &PgPool, defaults: &AuthConfig) -> Result<Settings, OkfError> {
    Ok(overlay(defaults, &stored(pool).await?))
}

/// Overrides one setting, and bumps the administrative generation in the
/// same transaction.
///
/// # Arguments
/// * `pool` - the database.
/// * `key` - which setting.
/// * `value` - the new value, as text (see the module's table).
/// * `by` - who is setting it, for `settings list`.
/// * `now` - when (server clock).
///
/// # Returns
/// `Ok(())` once the override is stored.
///
/// # Errors
/// [`OkfError::LimitExceeded`] with `limit: "setting"` for a value the
/// setting cannot take (nothing is written); [`OkfError::Database`].
pub async fn set(
    pool: &PgPool,
    key: SettingKey,
    value: &str,
    by: &str,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    let json = parse_value(key, value).map_err(invalid)?;
    // Apply to a scratch copy only to run the same checks `load` will.
    apply(
        &mut Settings::from_config(&AuthConfig::default()),
        key,
        &json,
    )
    .map_err(invalid)?;
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO server_settings (key, value, updated_at, updated_by) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, \
             updated_at = EXCLUDED.updated_at, updated_by = EXCLUDED.updated_by",
    )
    .bind(key.as_str())
    .bind(&json)
    .bind(now)
    .bind(by)
    .execute(&mut *tx)
    .await?;
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Removes a setting's override, so the config default applies again.
/// Bumps the administrative generation when there was one.
///
/// # Arguments
/// * `pool` - the database.
/// * `key` - which setting.
///
/// # Returns
/// `Ok(())`, whether or not there was an override.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn reset(pool: &PgPool, key: SettingKey) -> Result<(), OkfError> {
    let mut tx = pool.begin().await?;
    let deleted = sqlx::query("DELETE FROM server_settings WHERE key = $1")
        .bind(key.as_str())
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if deleted > 0 {
        bump_admin_generation(&mut tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Every setting with its effective value and override, in
/// [`SettingKey::ALL`] order.
///
/// # Arguments
/// * `pool` - the database.
/// * `defaults` - the `[auth]` section.
///
/// # Returns
/// One row per key. Rows for unknown keys are left out; an unreadable
/// override shows the default as the value, with `overridden: true`.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn list(pool: &PgPool, defaults: &AuthConfig) -> Result<Vec<SettingRow>, OkfError> {
    let rows = stored(pool).await?;
    let settings = overlay(defaults, &rows);
    // Index the rows by name once instead of scanning them per key.
    let by_name: HashMap<&str, &StoredSetting> =
        rows.iter().map(|row| (row.key.as_str(), row)).collect();
    Ok(SettingKey::ALL
        .into_iter()
        .map(|key| {
            let row = by_name.get(key.as_str());
            SettingRow {
                key,
                value: display(&settings, key),
                overridden: row.is_some(),
                updated_at: row.map(|row| row.updated_at),
                updated_by: row.map(|row| row.updated_by.clone()),
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_db;
    use crate::store::catalogs::admin_generation;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-12T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn defaults() -> AuthConfig {
        AuthConfig {
            max_keys_per_user: 7,
            ..AuthConfig::default()
        }
    }

    #[test]
    fn every_key_round_trips_and_spells_with_underscores() {
        for key in SettingKey::ALL {
            assert_eq!(SettingKey::parse(key.as_str()), Some(key));
            // clap's spelling (`settings set KEY`) is the stored one.
            let clap = clap::ValueEnum::to_possible_value(&key).unwrap();
            assert_eq!(clap.get_name(), key.as_str());
        }
        assert_eq!(SettingKey::parse("session_idle"), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn without_overrides_the_settings_are_the_config_defaults() {
        let db = test_db().await;

        let settings = load(&db.pool, &defaults()).await.unwrap();

        assert_eq!(settings, Settings::from_config(&defaults()));
        assert_eq!(settings.max_keys_per_user, 7);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_overrides_a_default_records_who_and_bumps_the_generation() {
        let db = test_db().await;
        let start = admin_generation(&db.pool).await.unwrap();

        set(&db.pool, SettingKey::MaxKeysPerUser, "3", "alice", now())
            .await
            .unwrap();
        set(&db.pool, SettingKey::PasswordSignIn, "off", "alice", now())
            .await
            .unwrap();
        set(
            &db.pool,
            SettingKey::ConsoleSessionIdle,
            "30m",
            "bob",
            now(),
        )
        .await
        .unwrap();
        set(&db.pool, SettingKey::MaxKeyLifetime, "90d", "bob", now())
            .await
            .unwrap();

        let settings = load(&db.pool, &defaults()).await.unwrap();
        assert_eq!(settings.max_keys_per_user, 3);
        assert!(!settings.password_sign_in);
        assert_eq!(settings.console_session_idle, Duration::from_secs(30 * 60));
        assert_eq!(
            settings.max_key_lifetime,
            Some(Duration::from_secs(90 * 86_400))
        );
        assert_eq!(admin_generation(&db.pool).await.unwrap(), start + 4);
        let rows = list(&db.pool, &defaults()).await.unwrap();
        let idle = rows
            .iter()
            .find(|r| r.key == SettingKey::ConsoleSessionIdle)
            .unwrap();
        assert_eq!(
            (
                idle.value.as_str(),
                idle.overridden,
                idle.updated_by.as_deref()
            ),
            ("30m", true, Some("bob"))
        );
        assert_eq!(idle.updated_at, Some(now()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_lifetime_limit_can_be_lifted_with_none() {
        let db = test_db().await;
        let limited = AuthConfig {
            max_key_lifetime: Some(Duration::from_secs(86_400)),
            ..AuthConfig::default()
        };

        set(&db.pool, SettingKey::MaxKeyLifetime, "none", "alice", now())
            .await
            .unwrap();

        assert_eq!(
            load(&db.pool, &limited).await.unwrap().max_key_lifetime,
            None
        );
        let rows = list(&db.pool, &limited).await.unwrap();
        let lifetime = rows
            .iter()
            .find(|r| r.key == SettingKey::MaxKeyLifetime)
            .unwrap();
        assert_eq!(lifetime.value, "none");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_bad_value_is_refused_and_changes_nothing() {
        let db = test_db().await;
        let start = admin_generation(&db.pool).await.unwrap();

        let refused = [
            set(&db.pool, SettingKey::MaxKeysPerUser, "1001", "alice", now()).await,
            set(&db.pool, SettingKey::MaxKeysPerUser, "-1", "alice", now()).await,
            set(
                &db.pool,
                SettingKey::PasswordSignIn,
                "maybe",
                "alice",
                now(),
            )
            .await,
            set(&db.pool, SettingKey::ResetLinkTtl, "0s", "alice", now()).await,
            set(&db.pool, SettingKey::SetupLinkTtl, "soon", "alice", now()).await,
            set(
                &db.pool,
                SettingKey::ConsoleSessionAbsolute,
                "none",
                "alice",
                now(),
            )
            .await,
        ];

        for result in refused {
            assert!(
                matches!(
                    result,
                    Err(OkfError::LimitExceeded {
                        limit: "setting",
                        ..
                    })
                ),
                "{result:?}"
            );
        }
        assert_eq!(admin_generation(&db.pool).await.unwrap(), start);
        assert!(list(&db.pool, &defaults())
            .await
            .unwrap()
            .iter()
            .all(|r| !r.overridden));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reset_returns_to_the_default_and_bumps_only_when_there_was_an_override() {
        let db = test_db().await;
        set(&db.pool, SettingKey::MaxKeysPerUser, "3", "alice", now())
            .await
            .unwrap();
        let before = admin_generation(&db.pool).await.unwrap();

        reset(&db.pool, SettingKey::MaxKeysPerUser).await.unwrap();
        reset(&db.pool, SettingKey::MaxKeysPerUser).await.unwrap();

        assert_eq!(
            load(&db.pool, &defaults()).await.unwrap().max_keys_per_user,
            7
        );
        assert_eq!(admin_generation(&db.pool).await.unwrap(), before + 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unreadable_and_unknown_rows_fall_back_to_the_defaults() {
        let db = test_db().await;
        sqlx::query(
            "INSERT INTO server_settings (key, value, updated_at, updated_by) VALUES \
             ('max_keys_per_user', '\"lots\"', now(), 'hand'), \
             ('console_session_idle', '\"0s\"', now(), 'hand'), \
             ('password_sign_in', '1', now(), 'hand'), \
             ('a_later_setting', 'true', now(), 'newer-server')",
        )
        .execute(&db.pool)
        .await
        .unwrap();

        let settings = load(&db.pool, &defaults()).await.unwrap();
        let rows = list(&db.pool, &defaults()).await.unwrap();

        assert_eq!(settings, Settings::from_config(&defaults()));
        assert_eq!(rows.len(), SettingKey::ALL.len());
        assert_eq!(
            rows.iter().map(|r| r.key).collect::<Vec<_>>(),
            SettingKey::ALL
        );
    }
}
