//! Every admin subcommand for users, teams, assignment, sign-in, settings
//! and mail has a documented REST route and console control (users and
//! teams design, "Web console"; user login design, "Admin surface"): the
//! parity table in `docs/operations.md` lists each one, so the CLI, the API
//! and the console cannot drift apart unnoticed.
//!
//! The checks, all without a database or a server:
//! - every covered subcommand exists in the clap tree;
//! - every `user`, `team`, `settings` and `mail` subcommand is covered, so
//!   a new one fails until it is added to the table;
//! - each covered subcommand has a parity row whose REST cell names a route
//!   that `docs/operations.md`'s API reference documents, and whose console
//!   cell is not empty.
//!
//! (`tests/parity.rs` is a different test: the differential tests against
//! `okf.py`.)

use clap::CommandFactory;
use okf_mcp::cli::Cli;

/// `docs/operations.md`, read at compile time. `include_str!` embeds the
/// file's text in the test binary, like reading it once at import time.
const OPERATIONS: &str = include_str!("../docs/operations.md");

/// The heading the parity table sits under.
const PARITY_HEADING: &str = "### CLI, API and console parity";

/// The subcommands under parity, as `okf-mcp <path>`.
const COVERED: &[&str] = &[
    "user create",
    "user list",
    "user delete",
    "user set-admin",
    "user set-email",
    "user invite",
    "user entitle",
    "user unentitle",
    "user sessions",
    "user revoke-sessions",
    "user sign-ins",
    "team create",
    "team list",
    "team add-member",
    "team set-role",
    "team remove-member",
    "team delete",
    "team suspend",
    "team unsuspend",
    "catalog set-owner",
    "catalog set-member-access",
    "key assign",
    "oauth code assign",
    "unassigned",
    "settings list",
    "settings set",
    "settings reset",
    "mail test",
    "mail queue",
    "mail retry",
    "mail delete",
];

/// The command groups whose every subcommand must be covered.
const SCANNED_GROUPS: &[&str] = &["user", "team", "settings", "mail"];

/// The parity section: from its heading to the next heading of any level.
///
/// # Returns
/// The section's text, heading excluded.
///
/// # Panics
/// When `docs/operations.md` has no parity heading.
fn parity_section() -> &'static str {
    let (_, after) = OPERATIONS
        .split_once(PARITY_HEADING)
        .expect("docs/operations.md has the parity section");
    // The section ends at the next line that starts a heading.
    let end = after.find("\n#").unwrap_or(after.len());
    &after[..end]
}

/// The cells of the parity row for `okf-mcp <path>`, trimmed.
///
/// # Arguments
/// * `path` - the subcommand, such as `user create`.
///
/// # Returns
/// The row's cells (CLI, REST, console), or `None` when no row starts with
/// that command.
fn parity_row(path: &str) -> Option<Vec<&'static str>> {
    let command = format!("`okf-mcp {path}`");
    parity_section().lines().find_map(|line| {
        // `| a | b | c |` splits into "", " a ", " b ", " c ", "": drop the
        // empty ends and trim the rest.
        let cells: Vec<&str> = line
            .trim()
            .strip_prefix('|')?
            .strip_suffix('|')?
            .split('|')
            .map(str::trim)
            .collect();
        (cells.first() == Some(&command.as_str())).then_some(cells)
    })
}

/// Every `` `METHOD /api/v1/...` `` route that starts a row of an API
/// reference table, outside the parity section.
///
/// # Returns
/// The routes, as `METHOD /api/v1/path`.
fn documented_routes() -> Vec<&'static str> {
    OPERATIONS
        .lines()
        .filter_map(|line| line.strip_prefix("| `"))
        .filter_map(|rest| rest.split_once('`').map(|(route, _)| route))
        .filter(|route| route.contains(" /api/v1/"))
        .collect()
}

#[test]
fn every_covered_subcommand_exists_and_is_in_the_parity_table() {
    let cli = Cli::command();
    for path in COVERED {
        // Walk the clap tree to prove the subcommand exists.
        let mut command = &cli;
        for part in path.split(' ') {
            command = command
                .find_subcommand(part)
                .unwrap_or_else(|| panic!("`okf-mcp {path}` is not a subcommand"));
        }
        assert!(
            parity_row(path).is_some(),
            "parity table lacks `okf-mcp {path}`"
        );
    }
}

#[test]
fn every_subcommand_of_the_scanned_groups_is_covered() {
    let cli = Cli::command();
    for group in SCANNED_GROUPS {
        let subcommands = cli
            .find_subcommand(group)
            .unwrap_or_else(|| panic!("`okf-mcp {group}` is not a subcommand"))
            .get_subcommands();
        for sub in subcommands {
            let path = format!("{group} {}", sub.get_name());
            assert!(
                COVERED.contains(&path.as_str()),
                "`okf-mcp {path}` is missing from COVERED (and the parity table)"
            );
        }
    }
}

#[test]
fn every_parity_row_names_a_documented_route_and_a_console_control() {
    let routes = documented_routes();
    for path in COVERED {
        let cells = parity_row(path).unwrap_or_else(|| panic!("no row for `okf-mcp {path}`"));
        let [_, rest, console] = cells[..] else {
            panic!(
                "the row for `okf-mcp {path}` has {} cells, not 3",
                cells.len()
            );
        };
        let route = rest
            .strip_prefix('`')
            .and_then(|cell| cell.strip_suffix('`'))
            .unwrap_or_else(|| panic!("`okf-mcp {path}`: the REST cell is not one route"));
        assert!(
            routes.contains(&route),
            "`okf-mcp {path}`: `{route}` is not in the API reference tables"
        );
        assert!(
            !console.is_empty(),
            "`okf-mcp {path}` has no console control"
        );
    }
}
