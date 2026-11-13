//! Catalog administration and import/export in the console (M9 task 5),
//! and a catalog's team member access (M12 Task 15).

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use chrono::Utc;
use okf_mcp::archive::{write_tarball, ExportFile};
use okf_mcp::config::Config;
use okf_mcp::store::ceiling::{self, MemberAccess};
use okf_mcp::store::keys::Access;
use okf_mcp::store::teams::{self, NewTeam, Role};
use serde_json::json;

use crate::harness::{drive, Ui};
use crate::self_service::{console_base, login_server, EMAIL, PASSWORD};
use crate::support::server::{files, TestServer, OWNER};

const ALPHA: &str = "---\ntype: system\ntitle: Alpha\n---\n# Alpha\n\nAlpha.\n";

/// Puts a file into the import page's `#upload-file`, as a user choosing it
/// would: rustwright has no set_input_files, so the page builds the File from
/// base64 and assigns it through a DataTransfer, then fires `change`.
const CHOOSE_FILE: &str = "(arg) => {
    const bytes = Uint8Array.from(atob(arg.data), char => char.charCodeAt(0));
    const transfer = new DataTransfer();
    transfer.items.add(new File([bytes], arg.name, { type: 'application/gzip' }));
    const input = document.querySelector('#upload-file');
    input.files = transfer.files;
    input.dispatchEvent(new Event('change', { bubbles: true }));
    return input.files.length;
}";

async fn server() -> TestServer {
    TestServer::start(
        vec![(
            "main",
            files(&[("index.md", "# Main\n"), ("systems/alpha.md", ALPHA)]),
        )],
        Config::default(),
    )
    .await
}

/// A `.tar.gz` of `(path, text)` pairs.
fn tarball(entries: &[(&str, &str)]) -> Vec<u8> {
    let modified = chrono::Utc::now();
    let entries: Vec<ExportFile<'_>> = entries
        .iter()
        .map(|&(path, text)| ExportFile {
            path,
            content: text.as_bytes(),
            executable: false,
            modified,
        })
        .collect();
    write_tarball(Vec::new(), &entries).expect("tarball")
}

/// The catalog switcher's options.
pub(crate) fn switcher(ui: &Ui) -> Vec<String> {
    let values = ui.eval(
        "() => [...document.querySelectorAll('#catalog-select option')] \
         .map(option => option.value)",
        json!(null),
    );
    serde_json::from_value(values).expect("a list of names")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admin_creates_and_deletes_a_catalog_and_the_switcher_follows() {
    let server = server().await;
    // No grants: every catalog, the only kind of key that may create one.
    let key = server
        .key("console-root", Access::Admin, &[], Some("main"))
        .await;
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        ui.open("/catalogs");
        ui.wait_visible("[data-catalog=main]");
        ui.click("#catalog-create");
        ui.wait_visible("#catalog-create-form");
        ui.fill("#catalog-create-form input[name=name]", "scratch");
        ui.fill(
            "#catalog-create-form textarea[name=description]",
            "Scratch space",
        );
        ui.select("#catalog-create-form select[name=owner]", OWNER);
        ui.click("#catalog-create-form button[type=submit]");
        // The page rebuilds the switcher before it redraws the table.
        ui.wait_visible("[data-catalog=scratch]");
        ui.wait_text("[data-test=catalog-owner-scratch]", OWNER);
        assert_eq!(switcher(ui), ["main", "scratch"]);

        ui.click("[data-catalog=scratch]");
        ui.wait_visible("#catalog-delete");
        ui.click("#catalog-delete");
        ui.wait_visible("#confirm-typed");
        ui.fill("#confirm-typed", "scratch");
        ui.click("#confirm-ok");
        ui.wait_hidden("[data-catalog=scratch]");
        assert_eq!(switcher(ui), ["main"]);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admin_changes_a_catalogs_owner_and_a_scoped_admin_cannot() {
    let server = server().await;
    let root = server.key("console-root", Access::Admin, &[], None).await;
    let scoped = server
        .key("console-scoped", Access::Admin, &["main"], Some("main"))
        .await;
    let family = NewTeam {
        name: "family",
        display_name: "",
        owner: OWNER,
    };
    teams::create(&server.db.pool, &family, Utc::now())
        .await
        .expect("create a team");
    let ran = drive(server.base.clone(), move |ui| {
        // A scoped admin sees the owner, but the control is disabled.
        ui.login(&scoped);
        ui.open("/catalogs");
        ui.wait_text("[data-test=catalog-owner-main]", OWNER);
        ui.click("[data-catalog=main]");
        ui.wait_visible("#catalog-owner-select[disabled]");
        ui.wait_visible("#catalog-owner-save[disabled]");
        ui.click("#drawer-close");
        ui.click("#signout");
        // One that reaches every catalog changes it.
        ui.login(&root);
        ui.open("/catalogs");
        ui.click("[data-catalog=main]");
        // The scoped admin's closed drawer still holds its disabled control
        // until this one replaces it.
        ui.wait_visible("#catalog-owner-select:not([disabled])");
        ui.wait_text("#catalog-owner-save", "Change owner");
        ui.select("#catalog-owner-select", "family");
        ui.click("#catalog-owner-save");
        ui.wait_text("[data-test=catalog-owner-main]", "family");
    })
    .await;
    if !ran {
        return;
    }
    let owned = teams::find(&server.db.pool, "family")
        .await
        .expect("read the team");
    assert_eq!(owned.catalogs, ["main"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn export_hands_the_browser_the_catalog_tarball() {
    let server = server().await;
    let key = server
        .key("console-exporter", Access::Read, &["main"], Some("main"))
        .await;
    let browser_key = key.clone();
    drive(server.base.clone(), move |ui| {
        ui.login(&browser_key);
        ui.open("/transfer");
        ui.wait_visible("#export-btn");
        ui.click("#export-btn");
        ui.wait_visible("#export-link");
        let link = ui.eval(
            "() => { const a = document.querySelector('#export-link'); \
             return { href: a.href, download: a.download, size: Number(a.dataset.size) }; }",
            json!(null),
        );
        assert!(
            link["href"].as_str().expect("an href").starts_with("blob:"),
            "{link}"
        );
        assert_eq!(link["download"], "main.tar.gz");
        assert!(link["size"].as_u64().expect("a size") > 0);
        // A read key sees no import form.
        assert!(!ui.page.is_visible("#import-btn").expect("visible"));
    })
    .await;
    // rustwright cannot save downloads (Deviation 5): fetch what the button
    // fetched, with the same bearer key, and check it is gzip.
    let response = reqwest::Client::new()
        .get(format!("{}/api/v1/catalogs/main/export", server.base))
        .bearer_auth(&key)
        .send()
        .await
        .expect("export");
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "application/gzip");
    let bytes = response.bytes().await.expect("body");
    assert_eq!(bytes[..2], [0x1f, 0x8b]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_import_reports_its_counts_and_a_stale_generation_is_refused() {
    let server = server().await;
    let key = server
        .key("console-importer", Access::Admin, &["main"], Some("main"))
        .await;
    let generation = server
        .app
        .catalog("main")
        .expect("main")
        .current()
        .generation();
    let archive = tarball(&[
        ("index.md", "# Main\n\nImported.\n"),
        ("systems/alpha.md", ALPHA),
        (
            "systems/gamma.md",
            "---\ntype: system\ntitle: Gamma\n---\n# Gamma\n",
        ),
    ]);
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        ui.open("/transfer");
        ui.wait_text("#transfer-generation", &format!("generation {generation}"));
        let prefilled = ui.eval(
            "() => document.querySelector('#import-generation').value",
            json!(null),
        );
        assert_eq!(prefilled, json!(generation.to_string()));
        ui.wait_visible("#upload-file");
        let chosen = ui.eval(
            CHOOSE_FILE,
            json!({ "name": "main.tar.gz", "data": STANDARD.encode(&archive) }),
        );
        assert_eq!(chosen, json!(1));
        ui.wait_text("#upload-name", "main.tar.gz");
        ui.click("#import-replace");
        ui.click("#import-btn");
        ui.wait_visible("#import-report");
        assert_eq!(ui.text("#import-added"), "1");
        assert_eq!(ui.text("#import-updated"), "1");
        assert_eq!(ui.text("#import-deleted"), "0");
        assert_eq!(ui.text("#import-unchanged"), "1");
        ui.wait_text("#import-lint", "Lint:");
        // The page re-reads the catalog: the field now holds the new generation.
        ui.wait_text(
            "#transfer-generation",
            &format!("generation {}", generation + 1),
        );

        // The same file against an old generation: refused, and the panel says why.
        ui.fill("#import-generation", &generation.to_string());
        ui.click("#import-btn");
        ui.wait_text("#import-error", "export it again");
        assert!(!ui.page.is_visible("#import-report").expect("visible"));
    })
    .await;
    assert_eq!(
        server
            .app
            .catalog("main")
            .expect("main")
            .current()
            .generation(),
        generation + 1
    );
}

/// Delays every export `fetch` by 1.5 s, so a journey can act while one is
/// in flight.
const DELAY_EXPORTS: &str = "() => {
    const real = window.fetch;
    window.fetch = (url, options) => String(url).includes('/export')
        ? new Promise(done => setTimeout(done, 1500)).then(() => real.call(window, url, options))
        : real.call(window, url, options);
    return true;
}";

/// Delays every `XMLHttpRequest` send (the import upload) by 1.5 s and
/// counts them in `window.importPosts`.
const DELAY_UPLOADS: &str = "() => {
    const send = XMLHttpRequest.prototype.send;
    window.importPosts = 0;
    XMLHttpRequest.prototype.send = function (body) {
        window.importPosts += 1;
        setTimeout(() => send.call(this, body), 1500);
    };
    return true;
}";

/// Whether each of `selectors` is disabled, in order.
fn disabled(ui: &Ui, selectors: &[&str]) -> Vec<bool> {
    let states = ui.eval(
        "(selectors) => selectors.map(selector => document.querySelector(selector).disabled)",
        json!(selectors),
    );
    serde_json::from_value(states).expect("a list of booleans")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_export_or_import_in_flight_locks_the_catalog_and_admits_no_second_import() {
    let server = TestServer::start(
        vec![
            (
                "main",
                files(&[("index.md", "# Main\n"), ("systems/alpha.md", ALPHA)]),
            ),
            ("other", files(&[("index.md", "# Other\n")])),
        ],
        Config::default(),
    )
    .await;
    let key = server
        .key(
            "console-busy",
            Access::Admin,
            &["main", "other"],
            Some("main"),
        )
        .await;
    let generation = server
        .app
        .catalog("main")
        .expect("main")
        .current()
        .generation();
    let first = STANDARD.encode(tarball(&[("index.md", "# Main\n\nFirst.\n")]));
    let second = STANDARD.encode(tarball(&[("index.md", "# Main\n\nSecond.\n")]));
    let controls = ["#transfer-catalog", "#export-btn", "#import-btn"];
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        ui.open("/transfer/main");
        ui.wait_text("#transfer-generation", &format!("generation {generation}"));

        // An export in flight: the picker cannot move to `other` under it,
        // and the file it hands over is named for the catalog it fetched.
        ui.eval(DELAY_EXPORTS, json!(null));
        ui.click("#export-btn");
        assert_eq!(disabled(ui, &controls), [true, true, true]);
        ui.wait_visible("#export-link");
        let download = ui.eval(
            "() => document.querySelector('#export-link').download",
            json!(null),
        );
        assert_eq!(download, json!("main.tar.gz"));
        assert_eq!(disabled(ui, &controls[..2]), [false, false]);

        // An import in flight: choosing another file does not unlock Import,
        // and a click on it sends nothing.
        ui.eval(DELAY_UPLOADS, json!(null));
        ui.eval(
            CHOOSE_FILE,
            json!({ "name": "first.tar.gz", "data": first }),
        );
        ui.click("#import-replace");
        ui.click("#import-btn");
        ui.eval(
            CHOOSE_FILE,
            json!({ "name": "second.tar.gz", "data": second }),
        );
        assert_eq!(disabled(ui, &controls), [true, true, true]);
        let posts = ui.eval(
            "() => { document.querySelector('#import-btn').click(); return window.importPosts; }",
            json!(null),
        );
        assert_eq!(posts, json!(1));
        ui.wait_visible("#import-report");
        ui.wait_text(
            "#transfer-generation",
            &format!("generation {}", generation + 1),
        );
        let posts = ui.eval("() => window.importPosts", json!(null));
        assert_eq!(posts, json!(1));
        // Done: the second file may be imported now.
        assert_eq!(disabled(ui, &controls), [false, false, false]);
    })
    .await;
    assert_eq!(
        server
            .app
            .catalog("main")
            .expect("main")
            .current()
            .generation(),
        generation + 1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admin_signed_in_with_a_password_imports_an_archive() {
    // `public_url` is the server's own address, so `/signin` exists.
    let server = TestServer::start_with(
        vec![(
            "main",
            files(&[("index.md", "# Main\n"), ("systems/alpha.md", ALPHA)]),
        )],
        |base| Config {
            public_url: Some(base.to_string()),
            ..Config::default()
        },
    )
    .await;
    let password = "violet-lantern-orbit-42";
    server
        .user_with_password("erin", "erin@example.com", password)
        .await;
    okf_mcp::store::users::set_admin(&server.db.pool, "erin", true, false)
        .await
        .expect("make erin an admin");
    server.app.poll().await.expect("poll");
    let generation = server
        .app
        .catalog("main")
        .expect("main")
        .current()
        .generation();
    let archive = tarball(&[
        ("index.md", "# Main\n\nImported by a session.\n"),
        ("systems/alpha.md", ALPHA),
    ]);

    let ran = drive(server.base.clone(), move |ui| {
        ui.sign_in_password("erin@example.com", password);
        ui.wait_text("#whoami", "erin - admin");
        // The cookie session alone: no key in this browser.
        assert_eq!(
            ui.eval("() => localStorage.getItem('okf.key')", json!(null)),
            serde_json::Value::Null
        );
        ui.open("/transfer");
        ui.wait_text("#transfer-generation", &format!("generation {generation}"));
        ui.wait_visible("#upload-file");
        let chosen = ui.eval(
            CHOOSE_FILE,
            json!({ "name": "main.tar.gz", "data": STANDARD.encode(&archive) }),
        );
        assert_eq!(chosen, json!(1));
        ui.wait_text("#upload-name", "main.tar.gz");
        ui.click("#import-replace");
        // The upload is a write, so it carries the session's CSRF token.
        ui.click("#import-btn");
        ui.wait_visible("#import-report");
        assert_eq!(ui.text("#import-updated"), "1");
        ui.wait_text(
            "#transfer-generation",
            &format!("generation {}", generation + 1),
        );
    })
    .await;
    if !ran {
        return;
    }
    let current = server.app.catalog("main").expect("main").current();
    assert_eq!(current.generation(), generation + 1);
}

/// Makes the page's next `PUT .../member-access` answer 409 with a reason,
/// without reaching the server, as a refusal would (the console has no
/// control a full admin can get refused through).
const REFUSE_NEXT_MEMBER_ACCESS_PUT: &str = "() => { \
    const real = window.fetch; \
    window.fetch = (url, init) => { \
        if (init?.method === 'PUT' && String(url).endsWith('/member-access')) { \
            window.fetch = real; \
            return Promise.resolve(new Response( \
                JSON.stringify({ error: 'refused', message: 'refused by the test' }), \
                { status: 409, headers: { 'Content-Type': 'application/json' } })); \
        } \
        return real(url, init); \
    }; \
    return true; }";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn member_access_read_makes_a_team_catalog_read_only_for_its_members() {
    let server = login_server().await;
    server.user_with_password("alice", EMAIL, PASSWORD).await;
    let pool = &server.db.pool;
    let family = NewTeam {
        name: "family",
        display_name: "",
        owner: OWNER,
    };
    teams::create(pool, &family, Utc::now())
        .await
        .expect("create a team");
    teams::set_member(pool, "family", "alice", Role::Member, Utc::now())
        .await
        .expect("add alice");
    okf_mcp::store::catalogs::create(pool, "shared", "", "family", Utc::now())
        .await
        .expect("create the team's catalog");
    server.app.poll().await.expect("poll");
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    let ran = drive(console_base(&server), move |ui| {
        ui.login(&admin);
        ui.open("/catalogs");
        ui.click("[data-catalog=shared]");
        ui.wait_text("#catalog-member-access-current", "write");
        // Nothing to save until the choice differs from the server's value.
        ui.wait_visible("#catalog-member-access-save[disabled]");
        // A refused save: its reason stays in the drawer, the value is read
        // again (still write), and the drawer stays usable.
        ui.eval(REFUSE_NEXT_MEMBER_ACCESS_PUT, json!(null));
        ui.select("#catalog-member-access-select", "read");
        ui.click("#catalog-member-access-save");
        ui.wait_visible("#catalog-member-access-error");
        ui.wait_text("#catalog-member-access-error", "refused by the test");
        ui.wait_text("#catalog-member-access-current", "write");
        ui.wait_visible("#catalog-member-access-select:not([disabled])");
        // Then a real one.
        ui.select("#catalog-member-access-select", "read");
        ui.click("#catalog-member-access-save");
        ui.wait_text("#toasts", "now reach shared at read");
        ui.wait_hidden("#catalog-member-access-error");
        // The drawer stays open and shows the value read again.
        ui.wait_text("#catalog-member-access-current", "read");
        ui.wait_visible("#catalog-member-access-save[disabled]");
        ui.click("#drawer-close");
        ui.click("#signout");
        ui.wait_visible("#login");
        // alice reaches the team's catalog read-only.
        ui.sign_in_password(EMAIL, PASSWORD);
        ui.open("/my-keys");
        ui.click("#my-key-new");
        ui.wait_visible("#my-key-form");
        assert_eq!(
            ui.eval(
                "() => [...document.querySelectorAll('#my-key-form select[name=level-shared] option')] \
                 .map(option => option.value)",
                json!(null)
            ),
            json!(["read"])
        );
    })
    .await;
    if !ran {
        return;
    }
    let access = ceiling::member_access(&server.db.pool, "shared")
        .await
        .expect("read member access");
    assert_eq!(access, MemberAccess::Read);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_scoped_admin_cannot_change_member_access() {
    let server = server().await;
    let scoped = server
        .key("console-scoped", Access::Admin, &["main"], Some("main"))
        .await;
    drive(server.base.clone(), move |ui| {
        ui.login(&scoped);
        ui.open("/catalogs");
        ui.click("[data-catalog=main]");
        // Read, but not changeable.
        ui.wait_text("#catalog-member-access-current", "write");
        ui.wait_visible("#catalog-member-access-select[disabled]");
        ui.wait_visible("#catalog-member-access-save[disabled]");
        ui.wait_text("#catalog-member-access", "every catalog");
        // The page itself carries no stray "null" under the table.
        assert!(!ui.text("#main").contains("null"));
    })
    .await;
}
