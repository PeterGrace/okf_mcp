//! End-to-end tests of the write tools (design "Testing"), over real HTTP
//! with rmcp's client:
//! - a full create, link, index, log, publish and export cycle;
//! - concurrent publishes of `log.md`;
//! - a conflict as an agent sees it;
//! - a changeset surviving a restart;
//! - `X-OKF-Changeset` keeping two changesets of one key apart;
//! - concurrent writes from two connections sharing one changeset.

mod support;

use okf_mcp::archive::{read_tarball, Files};
use okf_mcp::config::Config;
use okf_mcp::store::keys::Access;
use serde_json::{json, Value};
use support::server::{call, files, structured, Client, TestServer};
use tokio::sync::Barrier;

const TIME: &str =
    "---\ntype: System\ntitle: Time server\ndescription: The GPS clock.\n---\n# Time server\n";

/// One catalog, `pg`: a root index, one folder with one concept, a log.
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

/// The `path` field of each object in `list` (a result's `revisions` or
/// `files`). Panics on an item without a string `path`, so a change in the
/// result's shape fails here instead of shortening the list.
fn paths(list: &Value) -> Vec<&str> {
    list.as_array()
        .expect("a list")
        .iter()
        .map(|item| item["path"].as_str().expect("a string path"))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn create_link_index_log_publish_and_export() {
    let server = TestServer::start(catalog(), Config::default()).await;
    let key = writer(&server, "writer").await;
    let reader_key = server.key("reader", Access::Read, &[], Some("pg")).await;
    let client = server.client(&key, None).await.unwrap();
    let reader = server.client(&reader_key, None).await.unwrap();

    let concept = json!({
        "path": "/infra/ntp.md",
        "frontmatter": {"type": "System", "title": "NTP", "description": "How hosts sync."},
        "body": "# NTP\n\nHosts sync from the [time server](/infra/time.md).\n",
    });
    ok(&client, "create_concept", concept).await;
    let index = json!({"path": "/infra/ntp.md", "summary": "how hosts sync"});
    ok(&client, "index", index).await;
    ok(
        &client,
        "log",
        json!({"text": "added NTP", "kind": "Creation"}),
    )
    .await;
    // Before publish: the writer sees the concept through its overlay, the reader does not.
    let pending = ok(&client, "pending_changes", json!({})).await;
    let seen = ok(&client, "get_concept", json!({"path": "/infra/ntp.md"})).await;
    let unseen = call(&reader, "get_concept", json!({"path": "/infra/ntp.md"})).await;
    let published = ok(&client, "publish", json!({"message": "add NTP"})).await;
    let history = ok(&reader, "history", json!({"path": "/infra/ntp.md"})).await;
    let response = reqwest::Client::new()
        .get(format!("{}/catalogs/pg/export", server.base))
        .bearer_auth(&reader_key)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let tarball = response.bytes().await.unwrap();
    client.cancel().await.unwrap();
    reader.cancel().await.unwrap();

    let mut pending_paths = paths(&pending["files"]);
    pending_paths.sort_unstable();
    assert_eq!(
        pending_paths,
        ["/infra/index.md", "/infra/ntp.md", "/log.md"]
    );
    assert_eq!(seen["title"], "NTP");
    assert_eq!(unseen.is_error, Some(true));
    assert_eq!(published["published"], true);
    let mut revisions: Vec<(&str, &str)> = published["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|revision| {
            let path = revision["path"].as_str().unwrap();
            (path, revision["op"].as_str().unwrap())
        })
        .collect();
    revisions.sort_unstable();
    let expected = [
        ("/infra/index.md", "update"),
        ("/infra/ntp.md", "create"),
        ("/log.md", "update"),
    ];
    assert_eq!(revisions, expected);
    assert_eq!(history["revisions"][0]["message"], "add NTP");
    assert_eq!(history["revisions"][0]["actor"], "writer");
    assert_eq!(status, reqwest::StatusCode::OK);
    let exported = read_tarball(&tarball[..], u64::MAX).unwrap();
    let text = |path: &str| String::from_utf8(exported[path].content.clone()).unwrap();
    assert!(text("infra/ntp.md").contains("[time server](/infra/time.md)"));
    assert!(text("infra/index.md").contains("[NTP](ntp.md) - how hosts sync"));
    assert!(text("log.md").contains("* **Creation:** added NTP"));
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_log_publishes_from_two_keys_keep_both_bullets() {
    let server = TestServer::start(catalog(), Config::default()).await;
    let first_key = writer(&server, "writer-a").await;
    let second_key = writer(&server, "writer-b").await;
    let first = server.client(&first_key, None).await.unwrap();
    let second = server.client(&second_key, None).await.unwrap();
    ok(&first, "log", json!({"text": "from a"})).await;
    ok(&second, "log", json!({"text": "from b"})).await;

    // `tokio::join!` (like `asyncio.gather`) sends both publishes at once;
    // the server's publish lock runs them one after the other.
    let (one, two) = tokio::join!(
        call(&first, "publish", json!({"message": "a"})),
        call(&second, "publish", json!({"message": "b"})),
    );
    let log = ok(&first, "read_document", json!({"path": "/log.md"})).await;
    first.cancel().await.unwrap();
    second.cancel().await.unwrap();

    let (one, two) = (structured(&one).clone(), structured(&two).clone());
    assert_eq!(one["published"], true);
    assert_eq!(two["published"], true);
    let replays: usize = [&one, &two]
        .iter()
        .map(|result| result["replayed"].as_array().unwrap().len())
        .sum();
    assert_eq!(replays, 1, "exactly the second publish replays log.md");
    let text = log["text"].as_str().unwrap();
    assert!(text.contains("* **Update:** from a"), "{text}");
    assert!(text.contains("* **Update:** from b"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_sees_a_conflict_and_can_redo_its_edit() {
    let server = TestServer::start(catalog(), Config::default()).await;
    let first_key = writer(&server, "writer-a").await;
    let second_key = writer(&server, "writer-b").await;
    let first = server.client(&first_key, None).await.unwrap();
    let second = server.client(&second_key, None).await.unwrap();
    let edit = |text: &str| {
        let body = format!("# Time server\n\n{text}\n");
        json!({"path": "/infra/time.md", "body": body})
    };
    ok(&first, "replace_body", edit("From a.")).await;
    ok(&second, "replace_body", edit("From b.")).await;
    ok(&first, "publish", json!({"message": "a"})).await;

    let refused = call(&second, "publish", json!({"message": "b"})).await;
    let still_pending = ok(&second, "pending_changes", json!({})).await;
    ok(&second, "discard", json!({})).await;
    ok(&second, "replace_body", edit("From a, then b.")).await;
    let redone = ok(&second, "publish", json!({"message": "b again"})).await;
    first.cancel().await.unwrap();
    second.cancel().await.unwrap();

    assert_eq!(refused.is_error, Some(true));
    let body = refused.structured_content.expect("the error's JSON");
    assert_eq!(body["error"], "Conflict");
    let conflict = json!([{"path": "/infra/time.md", "base_revision": 1, "current_revision": 2}]);
    assert_eq!(body["files"], conflict);
    // The refused changeset stays open, and says its entry is stale.
    let file = &still_pending["files"][0];
    assert_eq!(file["path"], "/infra/time.md");
    assert_eq!(file["current_revision"], 2);
    assert_eq!(file["stale"], true);
    assert_eq!(redone["revisions"][0]["revision"], 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_changeset_survives_a_server_restart() {
    let mut server = TestServer::start(catalog(), Config::default()).await;
    let key = writer(&server, "writer").await;
    // No label: the `default` changeset is the same across a restart, even
    // though the new client gets a new MCP session.
    let before = server.client(&key, None).await.unwrap();
    let written = ok(&before, "log", json!({"text": "before the restart"})).await;
    before.cancel().await.unwrap();

    server.restart(Config::default()).await;
    let after = server.client(&key, None).await.unwrap();
    let pending = ok(&after, "pending_changes", json!({})).await;
    let published = ok(&after, "publish", json!({"message": "after the restart"})).await;
    let log = ok(&after, "read_document", json!({"path": "/log.md"})).await;
    after.cancel().await.unwrap();

    assert_eq!(written["label"], "default");
    assert_eq!(pending["changeset"], written["changeset"]);
    assert_eq!(paths(&pending["files"]), ["/log.md"]);
    assert_eq!(published["published"], true);
    assert_eq!(published["changeset"], written["changeset"]);
    let text = log["text"].as_str().unwrap();
    assert!(text.contains("* **Update:** before the restart"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn x_okf_changeset_keeps_two_changesets_of_one_key_apart() {
    let server = TestServer::start(catalog(), Config::default()).await;
    let key = writer(&server, "writer").await;
    let drafts = server
        .client_with(&key, &[("x-okf-changeset", "draft-a")])
        .await
        .unwrap();
    let others = server
        .client_with(&key, &[("x-okf-changeset", "draft-b")])
        .await
        .unwrap();
    let draft = json!({"path": "/infra/time.md", "body": "# Time server\n\nDraft a.\n"});
    let edited = ok(&drafts, "replace_body", draft).await;
    let logged = ok(&others, "log", json!({"text": "draft b"})).await;
    // Before publish, draft b's connection does not see draft a's edit.
    let before = ok(&others, "get_concept", json!({"path": "/infra/time.md"})).await;

    let published = ok(&drafts, "publish", json!({"message": "draft a"})).await;
    let left = ok(&others, "pending_changes", json!({})).await;
    let after = ok(&others, "get_concept", json!({"path": "/infra/time.md"})).await;
    drafts.cancel().await.unwrap();
    others.cancel().await.unwrap();

    assert_eq!(edited["label"], "draft-a");
    assert_eq!(logged["label"], "draft-b");
    assert_ne!(edited["changeset"], logged["changeset"]);
    assert!(!before["body"].as_str().unwrap().contains("Draft a."));
    assert_eq!(paths(&published["revisions"]), ["/infra/time.md"]);
    assert_eq!(left["changeset"], logged["changeset"]);
    assert_eq!(paths(&left["files"]), ["/log.md"]);
    // Published now, so draft b's connection sees draft a's edit too.
    assert!(after["body"].as_str().unwrap().contains("Draft a."));
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_writes_from_two_connections_of_one_key_share_one_changeset() {
    let server = TestServer::start(catalog(), Config::default()).await;
    let key = writer(&server, "writer").await;
    // Two unlabelled connections of one key share its `default` changeset
    // (Decision 1); every write takes the changeset's row lock, so writes
    // racing from both lose nothing.
    let first = server.client(&key, None).await.unwrap();
    let second = server.client(&key, None).await.unwrap();
    let entry = |client: &str, number: usize| json!({"text": format!("{client} {number}")});
    // Both connections wait here before each write, so every round sends the
    // two writes together (like `asyncio.Barrier`), not one loop after the other.
    let round = Barrier::new(2);

    // `tokio::join!` runs the two connections' loops at once.
    tokio::join!(
        async {
            for number in 0..3 {
                round.wait().await;
                ok(&first, "log", entry("first", number)).await;
            }
        },
        async {
            for number in 0..3 {
                round.wait().await;
                ok(&second, "log", entry("second", number)).await;
            }
        },
    );
    let pending = ok(&first, "pending_changes", json!({})).await;
    let published = ok(&second, "publish", json!({"message": "both"})).await;
    let log = ok(&first, "read_document", json!({"path": "/log.md"})).await;
    first.cancel().await.unwrap();
    second.cancel().await.unwrap();

    assert_eq!(paths(&pending["files"]), ["/log.md"]);
    assert_eq!(published["changeset"], pending["changeset"]);
    let text = log["text"].as_str().unwrap();
    for client in ["first", "second"] {
        for number in 0..3 {
            let bullet = format!("* **Update:** {client} {number}");
            assert!(text.contains(&bullet), "{bullet} missing from {text}");
        }
    }
}
