//! The skills under `skills/` cite the server's tools and their arguments in
//! backticks. This test starts a server, lists its tools, and checks that
//! every backticked snake_case name in a skill is a tool name, an input-schema
//! property, an enum or const value, or one of the known output fields below,
//! so a renamed or misspelled tool fails here rather than in a session.

mod support;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use okf_mcp::config::Config;
use regex::Regex;
use serde_json::Value;
use support::server::{files, TestServer};

/// Names the skills cite that are not input-schema names: fields of tool
/// results and catalog frontmatter keys.
const KNOWN: &[&str] = &[
    "next_after",
    "created_at",
    "ended_at",
    "primary_count",
    "completed_count",
    "failed_count",
    "inconclusive_count",
    "follow_up",
    "finding_ids",
    "stale_after",
];

/// Adds every property name, enum value and const value in `schema`.
fn schema_names(schema: &Value, out: &mut BTreeSet<String>) {
    match schema {
        Value::Object(map) => {
            if let Some(Value::Object(properties)) = map.get("properties") {
                out.extend(properties.keys().cloned());
            }
            if let Some(Value::Array(values)) = map.get("enum") {
                out.extend(values.iter().filter_map(Value::as_str).map(str::to_owned));
            }
            if let Some(Value::String(value)) = map.get("const") {
                out.insert(value.clone());
            }
            map.values().for_each(|value| schema_names(value, out));
        }
        Value::Array(items) => items.iter().for_each(|item| schema_names(item, out)),
        _ => {}
    }
}

/// `skills/<name>/SKILL.md` for every skill directory, sorted.
fn skill_files() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("skills");
    let mut found: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap_or_else(|error| panic!("read {}: {error}", root.display()))
        .map(|entry| entry.expect("directory entry").path().join("SKILL.md"))
        .filter(|path| path.is_file())
        .collect();
    found.sort();
    found
}

#[tokio::test(flavor = "multi_thread")]
async fn skills_cite_only_real_tools_and_arguments() {
    let skills = skill_files();
    assert_eq!(skills.len(), 4, "expected four skills, found {skills:?}");

    let server = TestServer::start(vec![("main", files(&[]))], Config::default()).await;
    let key = server.reporter_key("skills", &["main"]).await;
    let client = server.client(&key, None).await.unwrap();
    let tools = client.list_all_tools().await.unwrap();

    let mut valid: BTreeSet<String> = KNOWN.iter().map(|name| name.to_string()).collect();
    for tool in &tools {
        valid.insert(tool.name.to_string());
        schema_names(&Value::Object((*tool.input_schema).clone()), &mut valid);
    }

    let cited = Regex::new(r"`(?:mcp__okf-mcp__)?([a-z][a-z0-9]*(?:_[a-z0-9]+)+)`").unwrap();
    let mut unknown = Vec::new();
    for path in &skills {
        let text = std::fs::read_to_string(path).unwrap();
        for capture in cited.captures_iter(&text) {
            if !valid.contains(&capture[1]) {
                unknown.push(format!("{}: {}", path.display(), &capture[1]));
            }
        }
    }
    assert!(
        unknown.is_empty(),
        "names that are not tools, arguments or known fields:\n{}",
        unknown.join("\n")
    );
}
