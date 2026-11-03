//! End-to-end tests of the restructuring tools (design "Testing"), over real
//! HTTP with rmcp's client:
//! - a move, published and exported;
//! - a move against a concurrent edit of a linking file (a conflict);
//! - a move against a concurrent `index` of the same folder (a replay);
//! - a delete refused while linked, then allowed.

mod support;

use okf_mcp::archive::{read_tarball, Files};
use okf_mcp::config::Config;
use okf_mcp::store::keys::Access;
use serde_json::{json, Value};
use support::server::{call, files, structured, Client, TestServer};

const TIME: &str = "---\ntype: System\ntitle: Time server\ndescription: The GPS clock.\n---\n\
                    # Time server\n\n## Ports\n\nUDP 123.\n";
const HUB: &str = "---\ntype: Note\ntitle: Hub\ndescription: Links.\n---\n\
                   See the [time server](/infra/time.md#ports).\n";

/// One catalog, `pg`: a root index, a folder with one concept, a hub linking
/// it, a log.
fn catalog() -> Vec<(&'static str, Files)> {
    vec![(
        "pg",
        files(&[
            (
                "index.md",
                "# PG\n\n# Sections\n\n* [Infra](/infra/index.md) - systems\n",
            ),
            (
                "infra/index.md",
                "# Infra\n\n* [Time server](time.md) - the GPS clock\n",
            ),
            ("infra/time.md", TIME),
            ("hub.md", HUB),
            ("log.md", "# Log\n"),
        ]),
    )]
}

/// A write key with `pg` as its default.
async fn writer(server: &TestServer, id: &str) -> String {
    server.key(id, Access::Write, &[], Some("pg")).await
}

/// Calls a tool that must succeed and returns its JSON.
async fn ok(client: &Client, tool: &'static str, arguments: Value) -> Value {
    structured(&call(client, tool, arguments).await).clone()
}

/// The `(path, op)` pairs of a publish's revisions, sorted.
fn revisions(published: &Value) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = published["revisions"]
        .as_array()
        .expect("a list of revisions")
        .iter()
        .map(|row| {
            let path = row["path"].as_str().expect("a path").to_string();
            (path, row["op"].as_str().expect("an op").to_string())
        })
        .collect();
    // Sorting makes the comparison independent of the order publish lists
    // them in. `sort_unstable` is `list.sort()` without keeping equal items'
    // order, which distinct paths do not need.
    pairs.sort_unstable();
    pairs
}

/// The move every test but the last makes.
fn move_time() -> Value {
    json!({"old": "/infra/time", "new": "/infra/clock.md"})
}

#[tokio::test(flavor = "multi_thread")]
async fn a_move_is_published_and_exported() {
    let server = TestServer::start(catalog(), Config::default()).await;
    let key = writer(&server, "writer").await;
    let client = server.client(&key, None).await.unwrap();

    let moved = ok(&client, "move_concept", move_time()).await;
    let seen = ok(&client, "get_concept", json!({"path": "/infra/clock.md"})).await;
    let published = ok(
        &client,
        "publish",
        json!({"message": "rename the time server"}),
    )
    .await;
    let new_history = ok(&client, "history", json!({"path": "/infra/clock.md"})).await;
    let old_history = ok(&client, "history", json!({"path": "/infra/time.md"})).await;
    let response = reqwest::Client::new()
        .get(format!("{}/catalogs/pg/export", server.base))
        .bearer_auth(&key)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let tarball = response.bytes().await.unwrap();
    client.cancel().await.unwrap();

    assert_eq!(
        moved["touched"],
        json!([
            "/infra/time.md",
            "/infra/clock.md",
            "/hub.md",
            "/infra/index.md"
        ])
    );
    assert_eq!(seen["title"], "Time server");
    let expected = [
        ("/hub.md", "update"),
        ("/infra/clock.md", "create"),
        ("/infra/index.md", "update"),
        ("/infra/time.md", "delete"),
    ]
    // `[T; N]::map` builds a new fixed-size array (no `Vec` needed), owning
    // `String`s to compare with what `revisions` returns.
    .map(|(path, op)| (path.to_string(), op.to_string()));
    assert_eq!(revisions(&published), expected);
    assert_eq!(new_history["revisions"][0]["op"], "create");
    assert_eq!(old_history["revisions"][0]["op"], "delete");
    assert_eq!(status, reqwest::StatusCode::OK);
    let exported = read_tarball(&tarball[..], u64::MAX).unwrap();
    let text = |path: &str| {
        // `get` returns an `Option` (like `dict.get`), so a missing entry
        // fails with its path rather than a bare map-index panic.
        let file = exported
            .get(path)
            .unwrap_or_else(|| panic!("{path} missing from the export"));
        String::from_utf8(file.content.clone()).unwrap()
    };
    assert!(!exported.contains_key("infra/time.md"));
    assert_eq!(text("infra/clock.md"), TIME);
    assert!(text("hub.md").contains("[time server](/infra/clock.md#ports)"));
    assert!(text("infra/index.md").contains("[Time server](clock.md)"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_concurrent_edit_of_a_linking_file_conflicts() {
    let server = TestServer::start(catalog(), Config::default()).await;
    let mover_key = writer(&server, "writer-a").await;
    let editor_key = writer(&server, "writer-b").await;
    let mover = server.client(&mover_key, None).await.unwrap();
    let editor = server.client(&editor_key, None).await.unwrap();
    ok(&mover, "move_concept", move_time()).await;
    let body = json!({"path": "/hub.md", "body": "See the [time server](/infra/time.md).\n"});
    ok(&editor, "replace_body", body).await;
    ok(&editor, "publish", json!({"message": "reword the hub"})).await;

    let refused = call(&mover, "publish", json!({"message": "rename"})).await;
    let pending = ok(&mover, "pending_changes", json!({})).await;
    // The editor has nothing pending, so it reads the published catalog.
    let old = ok(&editor, "get_concept", json!({"path": "/infra/time.md"})).await;
    let new = call(&editor, "get_concept", json!({"path": "/infra/clock.md"})).await;
    mover.cancel().await.unwrap();
    editor.cancel().await.unwrap();

    assert_eq!(refused.is_error, Some(true));
    let body = refused.structured_content.expect("the error's JSON");
    assert_eq!(body["error"], "Conflict");
    assert_eq!(
        body["files"],
        json!([{"path": "/hub.md", "base_revision": 1, "current_revision": 2}])
    );
    // The whole move is still pending, its hub entry marked stale.
    let pending_files = pending["files"].as_array().unwrap();
    let mut pending_paths: Vec<&str> = pending_files
        .iter()
        .map(|file| file["path"].as_str().expect("a string path"))
        .collect();
    pending_paths.sort_unstable();
    assert_eq!(
        pending_paths,
        [
            "/hub.md",
            "/infra/clock.md",
            "/infra/index.md",
            "/infra/time.md"
        ]
    );
    let hub = pending_files
        .iter()
        .find(|file| file["path"] == "/hub.md")
        .expect("the hub is still pending");
    assert_eq!(hub["stale"], true);
    // Nothing of the refused publish reached the published catalog.
    assert_eq!(old["title"], "Time server");
    assert_eq!(new.is_error, Some(true));
    let new = new.structured_content.expect("the error's JSON");
    assert_eq!(new["error"], "NotFound");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_concurrent_index_of_the_same_folder_replays() {
    let server = TestServer::start(catalog(), Config::default()).await;
    let mover_key = writer(&server, "writer-a").await;
    let lister_key = writer(&server, "writer-b").await;
    let mover = server.client(&mover_key, None).await.unwrap();
    let lister = server.client(&lister_key, None).await.unwrap();
    ok(&mover, "move_concept", move_time()).await;
    let concept = json!({
        "path": "/infra/ntp.md",
        "frontmatter": {"type": "System", "title": "NTP", "description": "How hosts sync."},
        "body": "# NTP\n",
        "index_summary": "how hosts sync",
    });
    ok(&lister, "create_concept", concept).await;
    ok(&lister, "publish", json!({"message": "add NTP"})).await;

    let published = ok(&mover, "publish", json!({"message": "rename"})).await;
    let index = ok(&mover, "read_document", json!({"path": "/infra/index.md"})).await;
    mover.cancel().await.unwrap();
    lister.cancel().await.unwrap();

    assert_eq!(published["published"], true);
    assert_eq!(published["replayed"], json!(["/infra/index.md"]));
    let expected = [
        ("/hub.md", "update"),
        ("/infra/clock.md", "create"),
        ("/infra/index.md", "update"),
        ("/infra/time.md", "delete"),
    ]
    .map(|(path, op)| (path.to_string(), op.to_string()));
    assert_eq!(revisions(&published), expected);
    let text = index["text"].as_str().unwrap();
    assert!(
        text.contains("* [Time server](clock.md) - the GPS clock"),
        "{text}"
    );
    assert!(text.contains("* [NTP](ntp.md) - how hosts sync"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_delete_is_refused_while_linked_then_published() {
    let server = TestServer::start(catalog(), Config::default()).await;
    let key = writer(&server, "writer").await;
    let client = server.client(&key, None).await.unwrap();
    // A closure taking no arguments (a `lambda: ...`): `json!` builds a new
    // value per call, since `call` takes its arguments by value.
    let delete = || json!({"path": "/infra/time.md"});

    let refused = call(&client, "delete_concept", delete()).await;
    let unlink = json!({
        "path": "/hub.md",
        "old_str": "See the [time server](/infra/time.md#ports).",
        "new_str": "See the time server.",
    });
    ok(&client, "edit_document", unlink).await;
    let deleted = ok(&client, "delete_concept", delete()).await;
    let published = ok(
        &client,
        "publish",
        json!({"message": "retire the time server"}),
    )
    .await;
    let index = ok(&client, "read_document", json!({"path": "/infra/index.md"})).await;
    client.cancel().await.unwrap();

    assert_eq!(refused.is_error, Some(true));
    let body = refused.structured_content.expect("the error's JSON");
    assert_eq!(body["error"], "StillLinked");
    assert_eq!(body["path"], "infra/time.md");
    assert_eq!(body["links"], json!([{"source": "hub.md", "line": 6}]));
    assert_eq!(
        deleted["touched"],
        json!(["/infra/time.md", "/infra/index.md"])
    );
    let expected = [
        ("/hub.md", "update"),
        ("/infra/index.md", "update"),
        ("/infra/time.md", "delete"),
    ]
    .map(|(path, op)| (path.to_string(), op.to_string()));
    assert_eq!(revisions(&published), expected);
    assert_eq!(index["text"], "# Infra\n\n");
}
