//! The archive routes (design "Process and routes"): a catalog's export as a
//! `.tar.gz`, and an import from one.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, FromRequest, Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, MethodRouter};
use axum::{Extension, Json, Router};
use serde::Deserialize;

use crate::app::App;
use crate::archive::read_tarball;
use crate::auth::Principal;
use crate::error::OkfError;
use crate::http::{timeout_layer, AppError, REQUEST_TIMEOUT};
use crate::mcp::blocking;
use crate::store::audit::AuditActor;
use crate::store::export::export;
use crate::store::import::{import, ImportOptions, ImportScope};
use crate::store::keys::Access;

/// The two routes, for the keyed part of the router.
///
/// # Arguments
/// * `app` - the shared state (for `max_import_bytes`).
///
/// # Returns
/// A router over `Arc<App>` state: the export behind the request timeout,
/// the import with its body cap (and its own deadline, see
/// [`import_catalog`]).
pub(crate) fn routes(app: &App) -> Router<Arc<App>> {
    Router::new()
        .route("/catalogs/{name}/export", export_route())
        .route("/catalogs/{name}/import", import_route(app))
}

/// The export route's method router: [`export_catalog`] behind the request
/// timeout (503 after [`REQUEST_TIMEOUT`], as for `/healthz`). Shared with
/// `/api/v1`, so both paths behave the same; mount it on a path with one
/// parameter, the catalog.
///
/// # Returns
/// The `GET` method router.
pub(crate) fn export_route() -> MethodRouter<Arc<App>> {
    // A layer on one method router applies to that route only.
    get(export_catalog).layer(timeout_layer())
}

/// The import route's method router: [`import_catalog`] with the body cap
/// [`body_limit`] computes from `max_import_bytes`, and no timeout layer.
/// One that fired after the import committed would answer 503 for an import
/// that happened (and a retry would get 409), so the handler bounds the
/// upload itself. Shared with `/api/v1`; mount it on a path with one
/// parameter, the catalog.
///
/// # Arguments
/// * `app` - the shared state (for `max_import_bytes`).
///
/// # Returns
/// The `POST` method router.
pub(crate) fn import_route(app: &App) -> MethodRouter<Arc<App>> {
    let limit = body_limit(app.config().max_import_bytes.as_u64());
    post(import_catalog).layer(DefaultBodyLimit::max(limit))
}

/// The request-body cap for an import whose decompressed tar stream may be
/// `max_import_bytes`. gzip makes incompressible input slightly larger (5
/// bytes per stored 64 KiB block, plus an 18-byte header and trailer), so the
/// cap adds 0.1% and 64 KiB: a tarball the CLI would accept is never refused
/// here with 413. `read_tarball` still enforces the exact limit on the
/// decompressed stream.
///
/// # Arguments
/// * `max_import_bytes` - the configured limit.
///
/// # Returns
/// The body cap, saturating at `usize::MAX`.
fn body_limit(max_import_bytes: u64) -> usize {
    let slack = max_import_bytes / 1000 + 64 * 1024;
    // `try_from` fails only for a `u64` that does not fit a `usize` (a
    // 32-bit target), which is larger than any body anyway.
    usize::try_from(max_import_bytes.saturating_add(slack)).unwrap_or(usize::MAX)
}

/// `GET /catalogs/{name}/export`: the published documents as a `.tar.gz`,
/// for a key with `read` on the catalog.
///
/// # Arguments
/// * `app` - the shared state.
/// * `principal` - the caller, from the auth layer.
/// * `name` - the catalog, from the path.
///
/// # Returns
/// The tarball, as `application/gzip` with a download file name.
///
/// # Errors
/// 404 ([`OkfError::CatalogNotSelected`]) for a catalog that does not exist
/// or that the key does not reach; 500 for a database or archive failure.
// Each argument is an extractor: axum fills it from the request (the state,
// the auth layer's principal, the `{name}` path segment), much as a Python
// web framework injects arguments by their annotations.
async fn export_catalog(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(name): Path<String>,
) -> Result<Response, AppError> {
    // `?` returns early with the error, converted to `AppError` by `From`.
    // Export stays open while the owner team is suspended (M13 spec), so
    // members can take their data; reach and read access still apply.
    principal.require_export(&name)?;
    // M3's export builds the whole tarball in memory (a `Vec<u8>`).
    // The generation is for the command line's `import --expect-generation`.
    let (bytes, _generation) = export(app.pool(), &name, Vec::new()).await?;
    // `name` is a catalog that exists, so it is `[a-z0-9-]+` and safe in a header.
    let disposition = format!("attachment; filename=\"{name}.tar.gz\"");
    let headers = [
        (header::CONTENT_TYPE, "application/gzip".to_string()),
        (header::CONTENT_DISPOSITION, disposition),
    ];
    Ok((headers, bytes).into_response())
}

/// An import's query: `?replace=true&only_machinery=true&expect_generation=N`,
/// the HTTP spelling of the CLI's `--replace`, `--only-machinery` and
/// `--expect-generation N`. Booleans are `true` or `false`; anything else is
/// axum's 400.
#[derive(Debug, Default, Deserialize)]
struct ImportQuery {
    /// Replace a non-empty catalog's documents.
    #[serde(default)]
    replace: bool,
    /// Replace only repository machinery ([`ImportScope::Machinery`]); the
    /// archive's other files are ignored and the catalog's concepts are
    /// kept. A non-empty catalog also needs `replace`, as with the CLI.
    #[serde(default)]
    only_machinery: bool,
    /// Refuse (409) unless the catalog is still at this generation, the one
    /// its export had. A missing `Option` field deserializes as `None`.
    expect_generation: Option<i64>,
}

/// `POST /catalogs/{name}/import[?replace=true&only_machinery=true&expect_generation=N]`:
/// imports the uploaded `.tar.gz` as the key's actor, for a key with `admin`
/// on the catalog, and swaps the new snapshot in before answering, so the
/// next read sees it.
///
/// The key is checked before the body is read, so a key that may not import
/// cannot make the server buffer an upload. Reading and checking the upload
/// must finish within [`REQUEST_TIMEOUT`] (else 503, nothing imported); the
/// import and the refresh after it have no deadline, so a committed import
/// is never answered with a timeout.
///
/// # Arguments
/// * `app` - the shared state.
/// * `principal` - the caller, from the auth layer.
/// * `actor` - the caller, as the audit log's `catalog.imported` records it.
/// * `name` - the catalog, from the path.
/// * `query` - the import options.
/// * `request` - the rest of the request: its body is the tarball.
///
/// # Returns
/// The import's report as JSON; 413 past [`body_limit`]; 503 if the upload
/// is not read and checked within [`REQUEST_TIMEOUT`].
///
/// # Errors
/// 404 for a catalog that does not exist or that the key does not reach;
/// 403 ([`OkfError::Forbidden`]) for a key below `admin`; 400
/// ([`OkfError::ImportRefused`]) for an unreadable or rule-breaking archive;
/// 409 ([`OkfError::Refused`]) for a non-empty catalog without
/// `?replace=true`, or one no longer at `expect_generation`; 500 for a
/// database failure. A refresh that fails after the import committed is
/// logged, not returned.
async fn import_catalog(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    actor: AuditActor,
    Path(name): Path<String>,
    Query(query): Query<ImportQuery>,
    // The whole request, unread, must come last: reading its body consumes
    // it. Taking `Request` rather than `Bytes` defers the read until after
    // the key check.
    request: Request,
) -> Result<Response, AppError> {
    principal.require(Access::Admin, &name)?;
    let max_bytes = app.config().max_import_bytes.as_u64();
    // An `async` block is a future that runs only when awaited (here, under
    // the deadline). Its error is the response to send instead.
    let upload = async {
        // `Bytes::from_request` is what a `Bytes` handler argument runs: it
        // honours the route's `DefaultBodyLimit`, and its rejection is a
        // ready-made response (413 past the cap).
        let body = Bytes::from_request(request, &())
            .await
            .map_err(IntoResponse::into_response)?;
        // Decompressing and checking every entry is CPU work, so it runs on
        // a blocking thread; `move` hands `body` (a cheap reference-counted
        // buffer) to the closure.
        blocking(move || read_tarball(body.as_ref(), max_bytes))
            .await
            .map_err(|error| AppError(error).into_response())
    };
    // `timeout` gives `Err(Elapsed)` if the deadline passes first; dropping
    // the unfinished upload then cancels it, and nothing is imported.
    let files = match tokio::time::timeout(REQUEST_TIMEOUT, upload).await {
        Ok(Ok(files)) => files,
        Ok(Err(refusal)) => return Ok(refusal),
        Err(_elapsed) => return Ok(upload_timed_out()),
    };
    let options = ImportOptions {
        replace: query.replace,
        scope: if query.only_machinery {
            ImportScope::Machinery
        } else {
            ImportScope::Everything
        },
        expect_generation: query.expect_generation,
        actor: &principal.actor,
        audit: &actor,
        now: app.clock().now(),
    };
    let report = import(app.pool(), &name, files, &options)
        .await
        // `store::import`'s "not empty" refusal names the CLI flag: say what
        // HTTP needs instead. Its generation refusal ("... export it again")
        // already fits and passes through unchanged.
        .map_err(|error| match error {
            OkfError::Refused { reason } => OkfError::Refused {
                reason: reason.replace("pass --replace", "add ?replace=true"),
            },
            other => other,
        })?;
    // Swap the new snapshot in now, not at the next poll. The import has
    // committed, so a failed refresh must not turn it into an error (a
    // retry would then get 409): log it and leave it to the poll loop. A
    // catalog the app has not loaded yet arrives with the next poll. So does
    // a committed import whose client disconnects (dropping this future)
    // during the refresh; a disconnect during `import` itself drops its
    // transaction, which rolls back, and nothing is imported.
    if let Some(state) = app.catalog(&name) {
        if let Err(error) = state.refresh(app.pool()).await {
            tracing::error!(
                catalog = %name,
                %error,
                "refresh after import failed; the poll loop retries"
            );
        }
    }
    // The key's id, never the key itself.
    tracing::info!(catalog = %name, key = %principal.key_id, "imported");
    Ok(Json(report).into_response())
}

/// The answer to an upload not read and checked within [`REQUEST_TIMEOUT`]:
/// 503 with the `{error, message}` JSON every other refusal of the route
/// has. Built here rather than as an [`OkfError`] variant, which only this
/// route would ever return.
///
/// # Returns
/// The response.
fn upload_timed_out() -> Response {
    let body = serde_json::json!({
        "error": "Timeout",
        "message": format!(
            "the upload was not received and checked within {} s; nothing was imported",
            REQUEST_TIMEOUT.as_secs()
        ),
    });
    (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response()
}

/// A valid `.tar.gz` of `entries` (`(path, text)` pairs, none executable),
/// for the tests here and in `http::api`.
///
/// # Panics
/// If the archive cannot be written (it is built in memory, so it can't).
#[cfg(test)]
pub(crate) fn tarball_of(entries: &[(&str, &str)]) -> Vec<u8> {
    use crate::archive::{write_tarball, ExportFile};
    let modified = chrono::Utc::now();
    let files: Vec<ExportFile<'_>> = entries
        .iter()
        // `&(path, text)` destructures each borrowed pair, copying out the
        // two `&str`s (which are `Copy`).
        .map(|&(path, text)| ExportFile {
            path,
            content: text.as_bytes(),
            executable: false,
            modified,
        })
        .collect();
    write_tarball(Vec::new(), &files).expect("an in-memory tarball")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use axum::body::Body;
    use tokio::io::AsyncWriteExt;
    use tokio_util::io::ReaderStream;
    // `oneshot` sends one request through a router without a socket.
    use tower::ServiceExt;

    use crate::config::Config;
    use crate::mcp::testing::{app_with, principal, TestApp};
    use crate::store::catalogs;

    /// A key with `admin` on every catalog.
    fn admin() -> Arc<Principal> {
        Arc::new(Principal {
            access: Access::Admin,
            ..(*principal(&[], None)).clone()
        })
    }

    /// The import routes over `test`'s app, with `caller` as the auth
    /// layer's principal (added by hand).
    fn router(test: &TestApp, caller: Arc<Principal>) -> Router {
        routes(&test.app)
            .layer(Extension(caller))
            .with_state(Arc::clone(&test.app))
    }

    /// A valid archive of one document, `index.md`.
    fn tarball() -> Vec<u8> {
        tarball_of(&[("index.md", "# Index\n")])
    }

    /// A response's status and its body as JSON.
    async fn status_and_json(response: Response) -> (StatusCode, serde_json::Value) {
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    /// `pg`'s generation and live document count in the database.
    async fn stored(test: &TestApp) -> (i64, i64) {
        let catalog = catalogs::find(test.app.pool(), "pg").await.unwrap();
        let live: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM documents WHERE catalog_id = $1 AND deleted_at IS NULL",
        )
        .bind(catalog.id)
        .fetch_one(test.app.pool())
        .await
        .unwrap();
        (catalog.generation, live)
    }

    /// An import of `size` zero bytes into `pg` by `caller`, through the
    /// routes alone (the auth layer's principal added by hand), with a 1 KiB
    /// `max_import_bytes`.
    async fn import_status(caller: Arc<Principal>, size: usize) -> StatusCode {
        let config = Config {
            max_import_bytes: bytesize::ByteSize::b(1024),
            ..Config::default()
        };
        let test = app_with(&[("pg", &[])], config).await;
        let request = Request::post("/catalogs/pg/import")
            .body(Body::from(vec![0u8; size]))
            .expect("request");
        router(&test, caller)
            .oneshot(request)
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn an_upload_timeout_is_503_with_the_error_json() {
        let response = upload_timed_out();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"], "Timeout");
        assert_eq!(
            body["message"],
            "the upload was not received and checked within 120 s; nothing was imported"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_key_is_checked_before_the_body_is_read() {
        let reader = principal(&[], None);
        let oversized = body_limit(1024) + 1;

        let as_reader = import_status(reader, oversized).await;
        let as_admin = import_status(admin(), oversized).await;

        // A body read first would have made both 413.
        assert_eq!(as_reader, StatusCode::FORBIDDEN);
        assert_eq!(as_admin, StatusCode::PAYLOAD_TOO_LARGE);
    }

    // Paused time (after the database is up, which needs real time): with
    // nothing else to do, the runtime jumps the clock to the next timer, so
    // the 120-second deadline passes at once. A paused clock needs the
    // single-threaded runtime, `#[tokio::test]`'s default.
    #[tokio::test]
    async fn a_slow_upload_is_503_and_imports_nothing() {
        let test = app_with(&[("pg", &[])], Config::default()).await;
        let archive = tarball();
        let (head, tail) = archive.split_at(archive.len() / 2);
        // An in-memory pipe: the request body reads `reader` as `writer`
        // sends, so the upload arrives as slowly as the writer makes it.
        let (mut writer, reader) = tokio::io::duplex(archive.len());
        writer.write_all(head).await.unwrap();
        // `tail` is copied into the task (`to_vec`): a spawned task must own
        // what it uses (`'static`), as it may outlive this function.
        let tail = tail.to_vec();
        tokio::time::pause();
        let sender = tokio::spawn(async move {
            tokio::time::sleep(REQUEST_TIMEOUT + Duration::from_secs(1)).await;
            writer.write_all(&tail).await
        });
        let body = Body::from_stream(ReaderStream::new(reader));
        let request = Request::post("/catalogs/pg/import")
            .body(body)
            .expect("request");

        let response = router(&test, admin()).oneshot(request).await.unwrap();
        let (status, json) = status_and_json(response).await;
        // The rest of the upload, a second after the deadline, finds the
        // body dropped: the route stopped reading it.
        let late = sender.await.unwrap();
        tokio::time::resume();

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(json["error"], "Timeout");
        assert!(late.is_err(), "the late bytes were read: {late:?}");
        assert_eq!(stored(&test).await, (0, 0));
        assert_eq!(test.app.catalog("pg").unwrap().current().files(), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_refresh_failing_after_a_committed_import_still_reports_the_import() {
        let test = app_with(&[("pg", &[])], Config::default()).await;
        let state = test.app.catalog("pg").unwrap();
        let loaded_at = state.current().generation();
        // The fault: the import's generation bump (its last write, in its
        // transaction) also stores a row no workspace accepts, so the import
        // commits and the refresh after it fails to build the snapshot.
        sqlx::raw_sql(
            "CREATE FUNCTION break_the_refresh() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN \
                 INSERT INTO documents (catalog_id, path, content, executable, revision, \
                                        updated_at, updated_by) \
                 VALUES (NEW.id, '../escape.md', '', false, 1, now(), 'test'); \
                 RETURN NULL; \
             END $$; \
             CREATE TRIGGER break_the_refresh AFTER UPDATE OF generation ON catalogs \
                 FOR EACH ROW EXECUTE FUNCTION break_the_refresh();",
        )
        .execute(test.app.pool())
        .await
        .unwrap();
        let import = || {
            let request = Request::post("/catalogs/pg/import")
                .body(Body::from(tarball()))
                .expect("request");
            router(&test, admin()).oneshot(request)
        };

        let (status, report) = status_and_json(import().await.unwrap()).await;
        let after_import = stored(&test).await;
        let snapshot_after_import = state.current().generation();
        // A retry of the same upload: the catalog is no longer empty.
        let (retry_status, retry) = status_and_json(import().await.unwrap()).await;
        // Repair the data by hand, then let the poll loop's next pass retry
        // the refresh.
        sqlx::raw_sql(
            "DROP TRIGGER break_the_refresh ON catalogs; \
             DELETE FROM documents WHERE path = '../escape.md';",
        )
        .execute(test.app.pool())
        .await
        .unwrap();
        test.app.poll().await.unwrap();

        // Reported as the import it was, not as a failure.
        assert_eq!(status, StatusCode::OK);
        assert_eq!(report["added"], serde_json::json!(["index.md"]));
        // Committed: one generation on, with the imported document and the
        // trigger's row live; the snapshot was not swapped in.
        assert_eq!(after_import, (loaded_at + 1, 2));
        assert_eq!(snapshot_after_import, loaded_at);
        assert_eq!(retry_status, StatusCode::CONFLICT);
        assert_eq!(
            retry["message"],
            "catalog \"pg\" is not empty; add ?replace=true to replace its contents"
        );
        // The poll pass swapped the import in.
        let current = state.current();
        assert_eq!(current.generation(), loaded_at + 1);
        assert_eq!(current.files(), 1);
    }

    /// An admin's import into `test`'s `pg`, with `query` ("" or "?...").
    async fn import_with(
        test: &TestApp,
        query: &str,
        archive: Vec<u8>,
    ) -> (StatusCode, serde_json::Value) {
        let request = Request::post(format!("/catalogs/pg/import{query}"))
            .body(Body::from(archive))
            .expect("request");
        status_and_json(router(test, admin()).oneshot(request).await.unwrap()).await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn only_machinery_replaces_machinery_and_keeps_the_concepts() {
        let test = app_with(
            &[(
                "pg",
                &[
                    ("index.md", "# Index\n"),
                    ("AGENTS.md", "# Old\n"),
                    ("a.md", "# A\n"),
                ],
            )],
            Config::default(),
        )
        .await;
        let archive = tarball_of(&[("AGENTS.md", "# New\n"), ("a.md", "# Overwritten\n")]);

        let (status, report) =
            import_with(&test, "?replace=true&only_machinery=true", archive).await;
        let kept: Vec<u8> = sqlx::query_scalar(
            "SELECT content FROM documents WHERE path = 'a.md' \
             AND catalog_id = (SELECT id FROM catalogs WHERE name = 'pg')",
        )
        .fetch_one(test.app.pool())
        .await
        .unwrap();

        assert_eq!(status, StatusCode::OK, "{report}");
        assert_eq!(report["updated"], serde_json::json!(["AGENTS.md"]));
        assert_eq!(report["added"], serde_json::json!([]));
        assert_eq!(report["deleted"], serde_json::json!([]));
        assert_eq!(kept, b"# A\n");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_import_expecting_another_generation_is_409_and_imports_nothing() {
        let test = app_with(&[("pg", &[("index.md", "# Index\n")])], Config::default()).await;
        let before = stored(&test).await;
        let stale = before.0 + 1;
        let archive = || tarball_of(&[("index.md", "# Changed\n")]);

        let (status, refused) = import_with(
            &test,
            &format!("?replace=true&expect_generation={stale}"),
            archive(),
        )
        .await;
        let after_refusal = stored(&test).await;
        let (current_status, report) = import_with(
            &test,
            &format!("?replace=true&expect_generation={}", before.0),
            archive(),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(refused["error"], "Refused");
        let message = refused["message"].as_str().unwrap();
        // The generation refusal keeps its own words; only the "not empty"
        // refusal is reworded for HTTP.
        assert!(
            message.contains(&format!("generation {}, not {stale}", before.0)),
            "{message}"
        );
        assert!(message.contains("export it again"), "{message}");
        assert_eq!(after_refusal, before);
        assert_eq!(current_status, StatusCode::OK);
        assert_eq!(report["updated"], serde_json::json!(["index.md"]));
    }

    /// M13 Task 3: export stays open to every credential that reaches a
    /// suspended catalog, and import into it stays behind `admin` (which is
    /// exempt from suspension); a key that does not reach it gets 404.
    #[tokio::test(flavor = "multi_thread")]
    async fn export_stays_open_and_only_an_admin_imports_while_suspended() {
        use crate::mcp::testing::{cached_principal, stored_secret, suspend_catalogs};
        let test = app_with(
            &[("pg", &[("index.md", "# Index\n")]), ("other", &[])],
            Config::default(),
        )
        .await;
        let reader = stored_secret(&test, "reader", Access::Read, &["pg"], false).await;
        let writer = stored_secret(&test, "writer", Access::Write, &["pg"], false).await;
        let stranger = stored_secret(&test, "stranger", Access::Read, &["other"], false).await;
        let root = stored_secret(&test, "root", Access::Admin, &[], false).await;
        suspend_catalogs(&test, &["pg"]).await;
        // Each request goes through the routes with the principal the key
        // cache holds now, as the auth layer would add it.
        let call = |secret: &str, request: Request<Body>| {
            let caller = cached_principal(&test, secret);
            router(&test, caller).oneshot(request)
        };
        let export = || {
            Request::get("/catalogs/pg/export")
                .body(Body::empty())
                .unwrap()
        };
        let import = || {
            Request::post("/catalogs/pg/import?replace=true")
                .body(Body::from(tarball_of(&[("index.md", "# Imported\n")])))
                .unwrap()
        };

        let exported = call(&reader, export()).await.unwrap();
        let hidden = status_and_json(call(&stranger, export()).await.unwrap()).await;
        let refused = status_and_json(call(&writer, import()).await.unwrap()).await;
        let imported = status_and_json(call(&root, import()).await.unwrap()).await;

        assert_eq!(
            exported.status(),
            StatusCode::OK,
            "a suspended catalog still exports"
        );
        assert_eq!(hidden.0, StatusCode::NOT_FOUND, "{}", hidden.1);
        assert_eq!(hidden.1["error"], "CatalogNotSelected");
        assert_eq!(refused.0, StatusCode::FORBIDDEN, "{}", refused.1);
        assert_eq!(
            refused.1["error"], "Forbidden",
            "the level check comes first"
        );
        assert_eq!(
            imported.0,
            StatusCode::OK,
            "an admin is exempt: {}",
            imported.1
        );
    }

    #[test]
    fn the_body_limit_leaves_room_for_gzip_overhead() {
        let zero = body_limit(0);
        let default = body_limit(64 << 20);
        let huge = body_limit(u64::MAX);

        assert_eq!(zero, 64 * 1024);
        assert_eq!(default, (64 << 20) + (64 << 20) / 1000 + 64 * 1024);
        assert_eq!(huge, usize::MAX);
    }
}
