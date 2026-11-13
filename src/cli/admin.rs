//! `okf-mcp settings ...` and `okf-mcp mail ...`: the policy settings and
//! the mail queue (user login design, "Admin surface"). Like `people`, each
//! handler returns the text to print, and calls the same store function as
//! its REST route (`src/http/api/settings.rs`, `mail.rs`).
//!
//! Settings writes bump the administrative generation (`store::settings`),
//! so a running server applies them within one poll interval. `mail test`
//! only queues the job on the CLI's pool: no mailer is built, so no mail
//! secret is needed, and a running server's worker sends it.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use super::{labelled, tab_row, MailCommand, SettingsCommand};
use crate::clock::format_timestamp;
use crate::config::AuthConfig;
use crate::mail::queue::{self, QueuedMail};
use crate::mail::Template;
use crate::store::settings::{self, SettingRow};

/// Who the CLI records as having changed a setting: it has no principal.
const CLI_ACTOR: &str = "cli";

/// Runs `okf-mcp settings ...`.
///
/// # Arguments
/// * `pool` - the migrated database.
/// * `defaults` - the config file's `[auth]` table.
/// * `command` - which settings subcommand.
/// * `now` - the time a `set` is recorded at.
///
/// # Returns
/// The text to print.
///
/// # Errors
/// A value the setting refuses, or a database error.
pub(super) async fn settings(
    pool: &PgPool,
    defaults: &AuthConfig,
    command: SettingsCommand,
    now: DateTime<Utc>,
) -> anyhow::Result<String> {
    Ok(match command {
        SettingsCommand::List => settings::list(pool, defaults)
            .await?
            .iter()
            .map(|row| format_setting(row) + "\n")
            .collect(),
        SettingsCommand::Set { key, value } => {
            settings::set(pool, key, &value, CLI_ACTOR, now).await?;
            format!("set {} to {value}\n", key.as_str())
        }
        SettingsCommand::Reset { key } => {
            settings::reset(pool, key).await?;
            format!("reset {} to its default\n", key.as_str())
        }
    })
}

/// Runs `okf-mcp mail ...`.
///
/// # Arguments
/// * `pool` - the migrated database (apalis's schema included).
/// * `command` - which mail subcommand.
/// * `now` - the server clock: a queued mail's and a retry's start of the
///   24-hour retry window (plan Decision 11).
///
/// # Returns
/// The text to print.
///
/// # Errors
/// An address that is not one, an unknown id, mail that has not failed
/// (retry) or is being sent (delete), or a database error.
pub(super) async fn mail(
    pool: &PgPool,
    command: MailCommand,
    now: DateTime<Utc>,
) -> anyhow::Result<String> {
    Ok(match command {
        MailCommand::Test { address } => {
            queue::enqueue(pool, &address, &Template::Test, now).await?;
            format!("queued a test email to {address}\n")
        }
        MailCommand::Queue => {
            let queued = queue::list(pool).await?;
            if queued.is_empty() {
                "the mail queue is empty\n".to_string()
            } else {
                queued.iter().map(|mail| format_mail(mail) + "\n").collect()
            }
        }
        MailCommand::Retry { id } => {
            queue::retry(pool, &id, now).await?;
            format!("requeued mail {id}\n")
        }
        MailCommand::Delete { id } => {
            queue::delete(pool, &id).await?;
            format!("deleted mail {id}\n")
        }
    })
}

/// One `settings list` line, tab-separated: key, value (`-` when empty),
/// then `default` or `set by WHO at T`.
///
/// # Arguments
/// * `row` - the setting.
///
/// # Returns
/// The line, without a newline.
fn format_setting(row: &SettingRow) -> String {
    let source = match (row.overridden, row.updated_by.as_deref(), row.updated_at) {
        (true, Some(by), Some(at)) => format!("set by {by} at {}", format_timestamp(at)),
        (true, _, _) => "set".to_string(),
        (false, _, _) => "default".to_string(),
    };
    let value = if row.value.is_empty() {
        "-"
    } else {
        row.value.as_str()
    };
    tab_row(&[row.key.as_str(), value, &source])
}

/// One `mail queue` line, tab-separated: id, `status S`, `attempts N`,
/// `to A`, `template T`, `run_at T`, then `error E` last (free text, `-`
/// for none). Never the body, which holds a setup or reset link.
///
/// # Arguments
/// * `mail` - the queued job.
///
/// # Returns
/// The line, without a newline.
fn format_mail(mail: &QueuedMail) -> String {
    tab_row(&[
        &mail.id,
        &format!("status {}", mail.status),
        &format!("attempts {}", mail.attempts),
        &format!("to {}", mail.to),
        &format!("template {}", mail.template),
        &format!("run_at {}", format_timestamp(mail.run_at)),
        &labelled("error", mail.last_error.as_deref()),
    ])
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use clap::Parser;

    use super::{mail, settings};
    use crate::cli::{Cli, Command, MailCommand, SettingsCommand};
    use crate::config::AuthConfig;
    use crate::db::test_db;
    use crate::store::settings::SettingKey;

    /// Parses `okf-mcp <args>` into its command.
    fn parse(args: &[&str]) -> Result<Command, clap::Error> {
        Cli::try_parse_from(std::iter::once(&"okf-mcp").chain(args))
            .map(|cli| cli.command.expect("a subcommand"))
    }

    /// Runs `okf-mcp settings ...` with the default `[auth]` table.
    async fn run_settings(pool: &sqlx::PgPool, command: SettingsCommand) -> anyhow::Result<String> {
        settings(pool, &AuthConfig::default(), command, Utc::now()).await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn settings_set_list_and_reset() {
        let db = test_db().await;
        let set_to = |value: &str| SettingsCommand::Set {
            key: SettingKey::MaxKeysPerUser,
            value: value.to_string(),
        };

        let set = run_settings(&db.pool, set_to("5")).await.unwrap();
        let listed = run_settings(&db.pool, SettingsCommand::List).await.unwrap();
        let invalid = run_settings(&db.pool, set_to("many")).await;
        let reset = run_settings(
            &db.pool,
            SettingsCommand::Reset {
                key: SettingKey::MaxKeysPerUser,
            },
        )
        .await
        .unwrap();
        let after = run_settings(&db.pool, SettingsCommand::List).await.unwrap();

        assert_eq!(set, "set max_keys_per_user to 5\n");
        assert!(
            listed.contains("max_keys_per_user\t5\tset by cli at "),
            "{listed}"
        );
        assert!(
            listed.contains("password_sign_in\ttrue\tdefault\n"),
            "{listed}"
        );
        assert_eq!(listed.lines().count(), SettingKey::ALL.len(), "{listed}");
        assert!(invalid.is_err());
        assert_eq!(reset, "reset max_keys_per_user to its default\n");
        assert!(
            after.contains("max_keys_per_user\t20\tdefault\n"),
            "{after}"
        );
    }

    #[test]
    fn a_setting_key_is_parsed_by_its_snake_case_name() {
        let parsed = parse(&["settings", "set", "max_key_lifetime", "30d"]);
        let reset = parse(&["settings", "reset", "setup_link_ttl"]);
        let kebab = parse(&["settings", "set", "max-key-lifetime", "30d"]);
        let unknown = parse(&["settings", "set", "max_keys", "5"]);

        assert!(matches!(
            parsed,
            Ok(Command::Settings(SettingsCommand::Set {
                key: SettingKey::MaxKeyLifetime,
                ..
            }))
        ));
        assert!(matches!(
            reset,
            Ok(Command::Settings(SettingsCommand::Reset {
                key: SettingKey::SetupLinkTtl
            }))
        ));
        assert!(kebab.is_err(), "only the stored snake_case spelling");
        let message = unknown.expect_err("an unknown key").to_string();
        assert!(
            message.contains("max_keys_per_user"),
            "lists the keys: {message}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn mail_test_queue_retry_and_delete() {
        let db = test_db().await;
        let run = |command| mail(&db.pool, command, Utc::now());

        let queued = run(MailCommand::Test {
            address: "ops@example.com".to_string(),
        })
        .await
        .unwrap();
        let not_an_address = run(MailCommand::Test {
            address: "not an address".to_string(),
        })
        .await;
        let listed = run(MailCommand::Queue).await.unwrap();
        let id = crate::mail::queue::list(&db.pool).await.unwrap()[0]
            .id
            .clone();
        let unfailed = run(MailCommand::Retry { id: id.clone() }).await;
        // As a failed job looks once its retries are spent.
        sqlx::query("UPDATE apalis.jobs SET status = 'Killed' WHERE id = $1")
            .bind(&id)
            .execute(&db.pool)
            .await
            .unwrap();
        let retried = run(MailCommand::Retry { id: id.clone() }).await.unwrap();
        let deleted = run(MailCommand::Delete { id: id.clone() }).await.unwrap();
        let empty = run(MailCommand::Queue).await.unwrap();
        let unknown = run(MailCommand::Delete { id: id.clone() }).await;

        assert_eq!(queued, "queued a test email to ops@example.com\n");
        assert!(not_an_address.is_err(), "the address is checked");
        assert_eq!(listed.lines().count(), 1, "{listed}");
        assert!(
            listed.starts_with(&format!("{id}\tstatus pending\tattempts 0\t")),
            "{listed}"
        );
        assert!(
            listed.contains("\tto ops@example.com\ttemplate test\trun_at "),
            "{listed}"
        );
        assert!(listed.ends_with("\terror -\n"), "{listed}");
        assert!(unfailed.is_err(), "only failed mail is retried");
        assert_eq!(retried, format!("requeued mail {id}\n"));
        assert_eq!(deleted, format!("deleted mail {id}\n"));
        assert_eq!(empty, "the mail queue is empty\n");
        assert!(unknown.is_err(), "a deleted id is unknown");
    }
}
