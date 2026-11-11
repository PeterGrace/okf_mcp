//! The connector directories' tool metadata (M10 Task 1): every tool has a
//! title and all four behaviour hints, and is classified exactly once here,
//! so a new tool cannot ship without deciding what it does to the catalog.
//!
//! The hints themselves live on each `#[tool(...)]` attribute; this module
//! is the single list they are checked against.

use std::collections::BTreeSet;

use crate::mcp::OkfServer;

/// Tools that only read: never change a catalog, a changeset or a run.
const READS: &[&str] = &[
    "get_concept",
    "history",
    "links_report",
    "lint",
    "list_catalogs",
    "list_changesets",
    "maintenance_get_finding",
    "maintenance_get_run",
    "maintenance_list_findings",
    "maintenance_list_runs",
    "maintenance_read_revision",
    "oldest",
    "open_questions",
    "pending_changes",
    "read_document",
    "search",
    "sprawl",
];

/// Tools whose effect stays unpublished (a changeset entry, a maintenance
/// lease or report) and can be undone by `discard` or by letting it lapse.
const STAGED: &[&str] = &[
    "add_source",
    "create_concept",
    "edit_document",
    "index",
    "log",
    "maintenance_claim",
    "maintenance_dispose",
    "maintenance_renew",
    "maintenance_submit",
    "move_concept",
    "open_changeset",
    "question",
    "remove_source",
    "replace_body",
    "review",
    "revert",
    "set_frontmatter",
    "split_concept",
    "stamp",
    "unquestion",
];

/// Tools that cannot be undone from inside the server, or that delete.
const DESTRUCTIVE: &[&str] = &["delete_concept", "discard", "maintenance_close", "publish"];

/// The hints a tool's class implies (`open_world` is always false: every
/// tool acts only on this server's catalogs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Hints {
    read_only: bool,
    destructive: bool,
    idempotent: bool,
}

/// The hints for `name`'s class, or `None` for an unclassified tool.
fn expected(name: &str) -> Option<Hints> {
    // `contains(&name)`: the slices hold `&str`, so the needle is a `&&str`.
    if READS.contains(&name) {
        Some(Hints {
            read_only: true,
            destructive: false,
            idempotent: true,
        })
    } else if STAGED.contains(&name) {
        Some(Hints {
            read_only: false,
            destructive: false,
            idempotent: false,
        })
    } else if DESTRUCTIVE.contains(&name) {
        Some(Hints {
            read_only: false,
            destructive: true,
            idempotent: false,
        })
    } else {
        None
    }
}

/// Is every word of `title` capitalized ("List Catalogs")?
fn is_title_case(title: &str) -> bool {
    !title.is_empty()
        && title
            .split(' ')
            .all(|word| word.chars().next().is_some_and(|c| c.is_ascii_uppercase()))
}

#[test]
fn every_tool_is_classified_exactly_once() {
    let registered: BTreeSet<String> = OkfServer::tools()
        .list_all()
        .into_iter()
        // `name` is a `Cow<'static, str>`; `into_owned` gives the `String`.
        .map(|tool| tool.name.into_owned())
        .collect();
    let mut classified = BTreeSet::new();

    for name in READS.iter().chain(STAGED).chain(DESTRUCTIVE) {
        // `insert` returns false for a value already in the set.
        assert!(
            classified.insert((*name).to_string()),
            "{name} is in more than one class"
        );
    }

    assert_eq!(
        registered, classified,
        "the registered tools and the classified tools differ"
    );
    assert_eq!(registered.len(), 41);
}

#[test]
fn every_tool_has_a_title_and_all_four_hints() {
    for tool in OkfServer::tools().list_all() {
        let name = tool.name.as_ref();
        let title = tool
            .title
            .as_deref()
            .unwrap_or_else(|| panic!("{name} has no title"));
        let annotations = tool
            .annotations
            .as_ref()
            .unwrap_or_else(|| panic!("{name} has no annotations"));
        let hints = expected(name).unwrap_or_else(|| panic!("{name} is not classified"));

        assert!(is_title_case(title), "{name}: {title:?} is not Title Case");
        assert_eq!(
            annotations.title.as_deref(),
            Some(title),
            "{name}: the annotation title differs from the tool title"
        );
        assert_eq!(annotations.read_only_hint, Some(hints.read_only), "{name}");
        assert_eq!(
            annotations.destructive_hint,
            Some(hints.destructive),
            "{name}"
        );
        assert_eq!(
            annotations.idempotent_hint,
            Some(hints.idempotent),
            "{name}"
        );
        assert_eq!(annotations.open_world_hint, Some(false), "{name}");
    }
}

#[test]
fn a_read_only_tool_is_never_destructive() {
    for tool in OkfServer::tools().list_all() {
        let annotations = tool.annotations.expect("annotated");

        if annotations.read_only_hint == Some(true) {
            assert_eq!(annotations.destructive_hint, Some(false), "{}", tool.name);
        }
    }
}

#[test]
fn the_hints_go_on_the_wire_in_camel_case() {
    let search = OkfServer::tools()
        .list_all()
        .into_iter()
        .find(|tool| tool.name == "search")
        .expect("search is registered");

    let json = serde_json::to_value(&search).expect("a tool serializes");

    assert_eq!(json["title"], "Search Concepts");
    assert_eq!(json["annotations"]["title"], "Search Concepts");
    assert_eq!(json["annotations"]["readOnlyHint"], true);
    assert_eq!(json["annotations"]["destructiveHint"], false);
    assert_eq!(json["annotations"]["idempotentHint"], true);
    assert_eq!(json["annotations"]["openWorldHint"], false);
}
