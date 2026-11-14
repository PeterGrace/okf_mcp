use chrono::Duration;
use sqlx::{PgPool, Row};

use super::tests::{actor, revision, setup, t0};
use super::{candidates, reports, runs};
use crate::db::test_db;
use crate::maintenance::select::select;
use crate::maintenance::types::*;
use crate::okf::workspace::StoredFile;
use crate::store::import::{import, ImportOptions, ImportScope};

#[tokio::test(flavor = "multi_thread")]
async fn maintenance_final_candidate_queries_are_constant_with_growth() {
    let db = test_db().await;
    let catalog = setup(&db.pool, &[]).await;
    let mut previous = 0;
    for size in [8, 64] {
        for n in previous..size {
            let path = format!("c{n:03}.md");
            for rev in 1..=4 {
                revision(
                    &db.pool,
                    catalog,
                    &path,
                    rev,
                    t0(),
                    "---\ntype: Note\nverified:\n  - at: '2026-11-06T12:00:00Z'\n---\nBody\n",
                )
                .await;
            }
        }
        sqlx::query("INSERT INTO maintenance_runs (id,catalog_id,credential_id,actor,state,deadline,created_at,ended_at,primary_count) SELECT path,$1,'fixture','fixture','completed',$2,$2,$2,4 FROM documents WHERE catalog_id=$1 ON CONFLICT DO NOTHING").bind(catalog).bind(t0()).execute(&db.pool).await.unwrap();
        sqlx::query("INSERT INTO maintenance_work (id,run_id,catalog_id,path,revision,lanes,outcome,assessment,lease_token,lease_expires_at,attempted_at,ended_at) SELECT path || revision,path,$1,path,revision,ARRAY['changed','aging'],'completed','not_assessed','fixture',$2,$2,$2 FROM revisions WHERE catalog_id=$1 ON CONFLICT DO NOTHING").bind(catalog).bind(t0()).execute(&db.pool).await.unwrap();
        let mut tx = db.pool.begin().await.unwrap();
        // Force sequential scans so the measurement counts relation passes, not
        // planner-dependent per-row index probes. No wall-clock threshold.
        sqlx::raw_sql("SET LOCAL enable_indexscan=off; SET LOCAL enable_bitmapscan=off; SET LOCAL enable_nestloop=off").execute(&mut *tx).await.unwrap();
        let before: i64 = sqlx::query_scalar(
            "SELECT seq_scan FROM pg_stat_xact_user_tables WHERE relname='maintenance_work'",
        )
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        let revisions_before: i64 = sqlx::query_scalar(
            "SELECT seq_scan FROM pg_stat_xact_user_tables WHERE relname='revisions'",
        )
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        let rows = candidates::load(&mut tx, catalog, t0()).await.unwrap();
        assert_eq!(rows.len(), size);
        assert!(rows.iter().all(|r| r.legacy_verified.is_none()
            && r.last_attempt == Some(t0())
            && r.changed_done
            && r.last_aging_completed == Some(t0())
            && r.reported_supported.is_none()));
        let after: i64 = sqlx::query_scalar(
            "SELECT seq_scan FROM pg_stat_xact_user_tables WHERE relname='maintenance_work'",
        )
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        let revisions_after: i64 = sqlx::query_scalar(
            "SELECT seq_scan FROM pg_stat_xact_user_tables WHERE relname='revisions'",
        )
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        let revision_scans = revisions_after - revisions_before;
        assert_eq!(
            revision_scans, 1,
            "legacy provenance must use one batched scan"
        );
        let scans = after - before;
        eprintln!(
            "candidate growth: {size} concepts, {} revisions/attempts, {scans} work-history scans, {revision_scans} provenance scan",
            size * 4
        );
        assert!(
            scans > 0,
            "database instrumentation must observe history scans"
        );
        assert!(
            scans <= 2,
            "history must be batched: {scans} scans for {size} concepts"
        );
        tx.rollback().await.unwrap();
        previous = size;
    }
}

fn supported(lease: &Lease) -> Submission {
    Submission {
        outcome: WorkOutcome::Completed,
        assessment: Assessment::SupportedCurrent,
        summary: "Original assessment".into(),
        evidence: vec![Evidence {
            pin: lease.pin.clone(),
            excerpt: "Body".into(),
        }],
        findings: vec![],
        reported_tokens: None,
    }
}

async fn wait_blocked(pool: &PgPool, pid: i32, needle: &str) -> String {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let query: Option<String> = sqlx::query_scalar("SELECT query FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE $2 AND $1=ANY(pg_blocking_pids(pid)) LIMIT 1")
                .bind(pid).bind(format!("%{needle}%")).fetch_optional(pool).await.unwrap();
            if let Some(query) = query { return query; }
            tokio::task::yield_now().await;
        }
    }).await.expect("transaction did not reach expected lock")
}

async fn replacement_import_race(submit: bool) {
    let db = test_db().await;
    let catalog = setup(&db.pool, &["a.md", "b.md"]).await;
    let claimed = runs::claim(
        &db.pool,
        catalog,
        &actor("one"),
        "initial",
        2,
        Duration::days(30),
        t0(),
    )
    .await
    .unwrap();
    if !submit {
        runs::close(
            &db.pool,
            catalog,
            &actor("one"),
            &claimed.run_id,
            RunState::Cancelled,
            "reset",
            false,
            "cancel",
            t0(),
        )
        .await
        .unwrap();
    }
    let mut blocker = db.pool.begin().await.unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
    sqlx::query("SELECT path FROM documents WHERE catalog_id=$1 AND path='b.md' FOR UPDATE")
        .bind(catalog)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let pool = db.pool.clone();
    let importer = tokio::spawn(async move {
        import(
            &pool,
            "pg",
            [(
                "b.md".into(),
                StoredFile {
                    content: b"---\ntype: Note\n---\nBody updated\n".to_vec(),
                    executable: false,
                },
            )]
            .into(),
            &ImportOptions {
                replace: true,
                scope: ImportScope::Everything,
                expect_generation: None,
                actor: "importer",
                audit: &crate::store::audit::AuditActor::cli(),
                now: t0() + Duration::seconds(1),
            },
        )
        .await
    });
    wait_blocked(&db.pool, pid, "INSERT INTO documents").await;
    let import_pid: i32 = sqlx::query_scalar("SELECT pid FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND $1=ANY(pg_blocking_pids(pid))").bind(pid).fetch_one(&db.pool).await.unwrap();
    let pool = db.pool.clone();
    let worker = tokio::spawn(async move {
        if submit {
            let lease = claimed
                .leases
                .iter()
                .find(|l| l.pin.path == "a.md")
                .unwrap();
            let mut input = supported(lease);
            input.evidence.push(Evidence {
                pin: RevisionPin {
                    path: "b.md".into(),
                    revision: 1,
                },
                excerpt: "Body".into(),
            });
            let receipt = reports::submit(
                &pool,
                catalog,
                &actor("one"),
                &lease.work_id,
                &lease.token,
                "result",
                &input,
                t0() + Duration::seconds(2),
            )
            .await?;
            assert_eq!(receipt.outcome, WorkOutcome::Superseded);
            assert!(receipt.outdated);
        } else {
            let result = runs::claim(
                &pool,
                catalog,
                &actor("one"),
                "after-import",
                2,
                Duration::days(30),
                t0() + Duration::seconds(2),
            )
            .await?;
            assert_eq!(result.leases.len(), 1);
            assert_eq!(
                result.leases[0].pin,
                RevisionPin {
                    path: "b.md".into(),
                    revision: 2
                }
            );
        }
        Ok::<_, crate::error::OkfError>(())
    });
    // With the fix maintenance waits on import's advisory lock, not its document locks.
    let waiting = wait_blocked(&db.pool, import_pid, "").await;
    blocker.commit().await.unwrap();
    let imported = tokio::time::timeout(std::time::Duration::from_secs(10), importer)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let worked = tokio::time::timeout(std::time::Duration::from_secs(10), worker)
        .await
        .unwrap()
        .unwrap();
    assert!(
        waiting.contains("pg_advisory_xact_lock"),
        "wrong lock order: {waiting}"
    );
    worked.unwrap();
    assert_eq!(imported.updated, ["b.md"]);
    assert_eq!(imported.deleted.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn maintenance_final_claim_coordinates_replacement_import() {
    replacement_import_race(false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn maintenance_final_submit_coordinates_replacement_import() {
    replacement_import_race(true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn maintenance_final_recreated_hint_skips_null_history() {
    let db = test_db().await;
    let catalog = setup(&db.pool, &["a.md", "unrelated.md"]).await;
    sqlx::query("INSERT INTO revisions (catalog_id,path,revision,op,content,executable,actor,message,created_at) VALUES ($1,'a.md',2,'delete',NULL,false,'test','delete',$2)").bind(catalog).bind(t0()).execute(&db.pool).await.unwrap();
    let text = "---\ntype: Note\nverified:\n  - at: '2026-11-06T12:00:00Z'\n---\nBody\n";
    revision(&db.pool, catalog, "a.md", 3, t0(), text).await;
    let result = runs::claim(
        &db.pool,
        catalog,
        &actor("one"),
        "recreated",
        2,
        Duration::days(30),
        t0(),
    )
    .await
    .unwrap();
    assert_eq!(result.leases.len(), 2);
    let rows = candidates::load(&mut db.pool.acquire().await.unwrap(), catalog, t0())
        .await
        .unwrap();
    assert_eq!(
        rows.iter()
            .find(|r| r.pin.path == "a.md")
            .unwrap()
            .legacy_verified,
        Some(t0())
    );
    revision(&db.pool, catalog, "a.md", 4, t0(), text).await;
    let rows = candidates::load(&mut db.pool.acquire().await.unwrap(), catalog, t0())
        .await
        .unwrap();
    assert_eq!(
        rows.iter()
            .find(|r| r.pin.path == "a.md")
            .unwrap()
            .legacy_verified,
        None
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn maintenance_final_edits_preserve_attempt_fairness() {
    let db = test_db().await;
    let catalog = setup(&db.pool, &["a.md", "b.md"]).await;
    let text = "---\ntype: Note\nstale_after: '2020-01-01T00:00:00Z'\n---\nBody\n";
    for path in ["a.md", "b.md"] {
        revision(&db.pool, catalog, path, 2, t0(), text).await;
    }
    let batch = runs::claim(
        &db.pool,
        catalog,
        &actor("one"),
        "first",
        2,
        Duration::days(30),
        t0(),
    )
    .await
    .unwrap();
    runs::close(
        &db.pool,
        catalog,
        &actor("one"),
        &batch.run_id,
        RunState::Cancelled,
        "reset",
        false,
        "cancel",
        t0(),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE maintenance_work SET attempted_at=$2 WHERE catalog_id=$1 AND path='a.md'")
        .bind(catalog)
        .bind(t0() + Duration::seconds(1))
        .execute(&db.pool)
        .await
        .unwrap();
    for rev in 3..=5 {
        revision(
            &db.pool,
            catalog,
            "a.md",
            rev,
            t0() + Duration::seconds(i64::from(rev)),
            text,
        )
        .await;
        let mut conn = db.pool.acquire().await.unwrap();
        let rows = candidates::load(&mut conn, catalog, t0() + Duration::days(2))
            .await
            .unwrap();
        let edited = rows.iter().find(|c| c.pin.path == "a.md").unwrap();
        assert_eq!(edited.last_attempt, Some(t0() + Duration::seconds(1)));
        assert!(!edited.changed_done);
        assert_eq!(edited.last_aging_completed, None);
        assert_eq!(edited.reported_supported, None);
        let selection = select(
            &rows,
            &candidates::cursors(&mut conn, catalog).await.unwrap(),
            t0() + Duration::days(2),
            Duration::days(30),
            1,
        )
        .unwrap();
        assert_eq!(selection.items[0].pin.path, "b.md");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn maintenance_final_completed_close_acknowledges_without_rewriting_history() {
    let db = test_db().await;
    let catalog = setup(&db.pool, &["a.md"]).await;
    let batch = runs::claim(
        &db.pool,
        catalog,
        &actor("one"),
        "first",
        1,
        Duration::days(30),
        t0(),
    )
    .await
    .unwrap();
    let lease = &batch.leases[0];
    reports::submit(
        &db.pool,
        catalog,
        &actor("one"),
        &lease.work_id,
        &lease.token,
        "result",
        &supported(lease),
        t0(),
    )
    .await
    .unwrap();
    let empty = runs::claim(
        &db.pool,
        catalog,
        &actor("one"),
        "empty",
        1,
        Duration::days(30),
        t0(),
    )
    .await
    .unwrap();
    assert!(empty.leases.is_empty());
    for run in [&batch.run_id, &empty.run_id] {
        let before = runs::get_run(&db.pool, catalog, run).await.unwrap();
        let result = runs::close(
            &db.pool,
            catalog,
            &actor("one"),
            run,
            RunState::Completed,
            "acknowledged",
            false,
            run,
            t0() + Duration::seconds(1),
        )
        .await
        .unwrap();
        assert_eq!(result, before.run);
        assert_eq!(runs::get_run(&db.pool, catalog, run).await.unwrap(), before);
        assert_eq!(
            runs::close(
                &db.pool,
                catalog,
                &actor("one"),
                run,
                RunState::Completed,
                "acknowledged",
                false,
                run,
                t0() + Duration::days(2)
            )
            .await
            .unwrap(),
            result
        );
        assert!(runs::close(
            &db.pool,
            catalog,
            &actor("other"),
            run,
            RunState::Completed,
            "acknowledged",
            false,
            run,
            t0()
        )
        .await
        .is_err());
        let failure_key = format!("failure-{run}");
        for at in [t0() + Duration::seconds(1), t0() + Duration::days(2)] {
            let acknowledged = runs::close(
                &db.pool,
                catalog,
                &actor("one"),
                run,
                RunState::Failed,
                "terminal reconciliation",
                false,
                &failure_key,
                at,
            )
            .await
            .unwrap();
            assert_eq!(acknowledged, before.run);
            assert_eq!(runs::get_run(&db.pool, catalog, run).await.unwrap(), before);
        }
        assert!(runs::close(
            &db.pool,
            catalog,
            &actor("other"),
            run,
            RunState::Failed,
            "terminal reconciliation",
            false,
            &failure_key,
            t0(),
        )
        .await
        .is_err());
        for state in [RunState::Running, RunState::Cancelled] {
            assert!(runs::close(
                &db.pool,
                catalog,
                &actor("one"),
                run,
                state,
                "invalid",
                false,
                "invalid",
                t0()
            )
            .await
            .is_err());
        }
        let receipt = sqlx::query("SELECT request,response FROM maintenance_requests WHERE operation='close' AND idempotency_key=$1").bind(run).fetch_one(&db.pool).await.unwrap();
        assert_eq!(
            receipt.get::<serde_json::Value, _>("request")["rationale"],
            "acknowledged"
        );
        assert_eq!(
            receipt.get::<serde_json::Value, _>("response"),
            serde_json::to_value(result).unwrap()
        );
    }
}
