use super::tests::{actor, revision, setup, t0};
use super::{candidates, reports, runs};
use crate::db::test_db;
use crate::error::OkfError;
use crate::maintenance::types::*;
use chrono::Duration;
use sqlx::PgPool;

async fn claim(pool: &PgPool, catalog: i64, key: &str, at: chrono::DateTime<chrono::Utc>) -> Lease {
    runs::claim(pool, catalog, &actor("one"), key, 1, Duration::days(30), at)
        .await
        .unwrap()
        .leases
        .remove(0)
}

fn report(lease: &Lease) -> Submission {
    Submission {
        outcome: WorkOutcome::Completed,
        assessment: Assessment::SupportedCurrent,
        summary: "Investigator assessment".into(),
        evidence: vec![Evidence {
            pin: lease.pin.clone(),
            excerpt: "Body".into(),
        }],
        findings: vec![],
        reported_tokens: Some(42),
    }
}

fn finding(pin: RevisionPin) -> FindingInput {
    FindingInput {
        category: Category::UnverifiableClaim,
        assertion: "Investigate claim".into(),
        follow_up: "Ask owner".into(),
        uncertainty: "Uncertain".into(),
        relationship: Relationship::DirectStatement,
        evidence: vec![Evidence {
            pin,
            excerpt: "Body".into(),
        }],
        external: vec![],
    }
}

async fn submit(
    pool: &PgPool,
    catalog: i64,
    lease: &Lease,
    key: &str,
    input: &Submission,
    at: chrono::DateTime<chrono::Utc>,
) -> Result<Receipt, OkfError> {
    reports::submit(
        pool,
        catalog,
        &actor("one"),
        &lease.work_id,
        &lease.token,
        key,
        input,
        at,
    )
    .await
}

async fn snapshot(pool: &PgPool, catalog: i64) -> serde_json::Value {
    sqlx::query_scalar("SELECT jsonb_build_array((SELECT jsonb_agg(to_jsonb(w) ORDER BY id) FROM maintenance_work w WHERE catalog_id=$1), (SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM maintenance_runs r WHERE catalog_id=$1), (SELECT jsonb_agg(to_jsonb(f) ORDER BY id) FROM maintenance_findings f WHERE catalog_id=$1), (SELECT jsonb_agg(to_jsonb(e) ORDER BY work_id,finding_id,ordinal) FROM maintenance_evidence e WHERE catalog_id=$1), (SELECT jsonb_agg(to_jsonb(s)) FROM maintenance_state s WHERE catalog_id=$1), (SELECT jsonb_agg(to_jsonb(q) ORDER BY operation,idempotency_key) FROM maintenance_requests q WHERE catalog_id=$1), (SELECT jsonb_agg(to_jsonb(d) ORDER BY path) FROM documents d WHERE catalog_id=$1), (SELECT to_jsonb(c) FROM catalogs c WHERE id=$1))")
        .bind(catalog).fetch_one(pool).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn maintenance_submit_is_atomic_and_idempotent() {
    let db = test_db().await;
    let catalog = setup(&db.pool, &["a.md"]).await;
    let lease = claim(&db.pool, catalog, "claim", t0()).await;
    let mut input = report(&lease);
    input.findings = vec![finding(lease.pin.clone()), finding(lease.pin.clone())];
    input.findings[1].evidence[0].excerpt = "missing".into();
    let before = snapshot(&db.pool, catalog).await;
    assert!(matches!(
        submit(&db.pool, catalog, &lease, "submit", &input, t0()).await,
        Err(OkfError::Refused { .. })
    ));
    assert_eq!(snapshot(&db.pool, catalog).await, before);
    input.findings[1].evidence[0].excerpt = "Body".into();
    let first = submit(&db.pool, catalog, &lease, "submit", &input, t0())
        .await
        .unwrap();
    assert_eq!(first.outcome, WorkOutcome::Completed);
    assert!(!first.outdated);
    assert_eq!(first.finding_ids.len(), 2);
    let stored = snapshot(&db.pool, catalog).await;
    assert_eq!(stored[6], before[6]); // Canonical documents unchanged.
    assert_eq!(stored[7], before[7]); // Catalog generation unchanged.
    input.evidence[0].pin.path = "/./a.md".into();
    input.findings[0].evidence[0].pin.path = "/a.md".into();
    let retry = submit(
        &db.pool,
        catalog,
        &lease,
        "submit",
        &input,
        t0() + Duration::days(2),
    )
    .await
    .unwrap();
    assert_eq!(first, retry);
    assert_eq!(snapshot(&db.pool, catalog).await, stored);
    input.summary.push('!');
    assert!(matches!(
        submit(&db.pool, catalog, &lease, "submit", &input, t0()).await,
        Err(OkfError::Refused { .. })
    ));
    assert!(submit(&db.pool, catalog, &lease, "other", &input, t0())
        .await
        .is_err());
    assert!(matches!(
        reports::submit(
            &db.pool,
            catalog,
            &actor("other"),
            &lease.work_id,
            &lease.token,
            "submit",
            &input,
            t0()
        )
        .await,
        Err(OkfError::NotFound { .. })
    ));
    assert_eq!(snapshot(&db.pool, catalog).await, stored);
    let counts: (i64,i64,String) = sqlx::query_as("SELECT (SELECT count(*) FROM maintenance_findings WHERE catalog_id=$1),(SELECT count(*) FROM maintenance_evidence WHERE catalog_id=$1),(SELECT state FROM maintenance_runs WHERE id=$2)")
        .bind(catalog).bind(&lease.run_id).fetch_one(&db.pool).await.unwrap();
    assert_eq!(counts, (2, 3, "completed".into()));
    let cursors = candidates::cursors(&mut db.pool.acquire().await.unwrap(), catalog)
        .await
        .unwrap();
    assert_eq!(cursors.changed, "a.md");
    assert_eq!(cursors.verification, "a.md");
    assert!(cursors.next_expiry);
}

#[tokio::test(flavor = "multi_thread")]
async fn maintenance_inconclusive_never_refreshes_support() {
    let db = test_db().await;
    let catalog = setup(&db.pool, &["a.md"]).await;
    let lease = claim(&db.pool, catalog, "first", t0()).await;
    submit(&db.pool, catalog, &lease, "success", &report(&lease), t0())
        .await
        .unwrap();
    for (offset, outcome, assessment) in [
        (31, WorkOutcome::Inconclusive, Assessment::Unverifiable),
        (33, WorkOutcome::Failed, Assessment::NotAssessed),
    ] {
        let at = t0() + Duration::days(offset);
        let lease = claim(&db.pool, catalog, &format!("claim-{offset}"), at).await;
        let mut input = report(&lease);
        input.outcome = outcome;
        input.assessment = assessment;
        input.evidence.clear();
        submit(
            &db.pool,
            catalog,
            &lease,
            &format!("submit-{offset}"),
            &input,
            at,
        )
        .await
        .unwrap();
        let rows = candidates::load(&mut db.pool.acquire().await.unwrap(), catalog, at)
            .await
            .unwrap();
        assert_eq!(rows[0].reported_supported, Some((1, t0())));
        assert_eq!(rows[0].last_aging_completed, Some(t0()));
        assert_eq!(rows[0].retry_at, Some(at + Duration::seconds(86_400)));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn maintenance_historical_report_does_not_cover_new_revision() {
    let db = test_db().await;
    let catalog = setup(&db.pool, &["a.md"]).await;
    let lease = claim(&db.pool, catalog, "first", t0()).await;
    revision(&db.pool, catalog, "a.md", 2, t0(), "Body revised").await;
    let cursors_before = candidates::cursors(&mut db.pool.acquire().await.unwrap(), catalog)
        .await
        .unwrap();
    let result = submit(
        &db.pool,
        catalog,
        &lease,
        "historical",
        &report(&lease),
        t0(),
    )
    .await
    .unwrap();
    assert_eq!(result.outcome, WorkOutcome::Superseded);
    assert!(result.outdated);
    let stored = runs::get_run(&db.pool, catalog, &lease.run_id)
        .await
        .unwrap();
    assert_eq!(
        stored.work[0].summary.as_deref(),
        Some("Investigator assessment")
    );
    assert_eq!(
        stored.work[0].assessment,
        Some(Assessment::SupportedCurrent)
    );
    assert_eq!(stored.work[0].evidence, report(&lease).evidence);
    let reason: String = sqlx::query_scalar("SELECT reason FROM maintenance_work WHERE id=$1")
        .bind(&lease.work_id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(reason, "source_changed");
    let payload: serde_json::Value = sqlx::query_scalar("SELECT request FROM maintenance_requests WHERE catalog_id=$1 AND operation='submit' AND idempotency_key='historical'").bind(catalog).fetch_one(&db.pool).await.unwrap();
    assert_eq!(
        payload["submission"],
        serde_json::to_value(report(&lease)).unwrap()
    );
    assert_eq!(
        candidates::cursors(&mut db.pool.acquire().await.unwrap(), catalog)
            .await
            .unwrap(),
        cursors_before
    );
    let rows = candidates::load(&mut db.pool.acquire().await.unwrap(), catalog, t0())
        .await
        .unwrap();
    assert!(!rows[0].changed_done);
    assert_eq!(rows[0].reported_supported, None);
    let newer = claim(&db.pool, catalog, "newer", t0()).await;
    assert_eq!(newer.pin.revision, 2);
    revision(&db.pool, catalog, "a.md", 3, t0(), "Body third").await;
    let replacement = claim(&db.pool, catalog, "replacement", t0()).await;
    assert_eq!(replacement.pin.revision, 3);
    assert!(matches!(
        submit(&db.pool, catalog, &newer, "too-late", &report(&newer), t0()).await,
        Err(OkfError::Busy { .. })
    ));
    assert_eq!(
        submit(
            &db.pool,
            catalog,
            &lease,
            "historical",
            &report(&lease),
            t0() + Duration::days(1)
        )
        .await
        .unwrap(),
        result
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn maintenance_stale_plus_invalid_evidence_rolls_back_everything() {
    let db = test_db().await;
    let catalog = setup(&db.pool, &["a.md", "b.md"]).await;
    let lease = claim(&db.pool, catalog, "first", t0()).await;
    revision(&db.pool, catalog, &lease.pin.path, 2, t0(), "Body revised").await;
    let mut input = report(&lease);
    let mut invalid = finding(RevisionPin {
        path: "b.md".into(),
        revision: 1,
    });
    invalid.evidence[0].excerpt = "absent excerpt".into();
    input.findings.push(invalid);
    let before = snapshot(&db.pool, catalog).await;
    assert!(matches!(
        submit(&db.pool, catalog, &lease, "stale-invalid", &input, t0()).await,
        Err(OkfError::Refused { .. })
    ));
    assert_eq!(snapshot(&db.pool, catalog).await, before);
}

#[tokio::test(flavor = "multi_thread")]
async fn maintenance_submit_all_evidence_controls_effective_support() {
    let db = test_db().await;
    let catalog = setup(&db.pool, &["a.md", "b.md"]).await;
    let lease = claim(&db.pool, catalog, "first", t0()).await;
    assert_eq!(lease.pin.path, "a.md");
    let mut input = report(&lease);
    input.findings.push(finding(RevisionPin {
        path: "b.md".into(),
        revision: 1,
    }));
    submit(&db.pool, catalog, &lease, "support", &input, t0())
        .await
        .unwrap();
    let rows = candidates::load(&mut db.pool.acquire().await.unwrap(), catalog, t0())
        .await
        .unwrap();
    assert_eq!(rows[0].reported_supported, Some((1, t0())));
    revision(&db.pool, catalog, "b.md", 2, t0(), "Body revised").await;
    let rows = candidates::load(&mut db.pool.acquire().await.unwrap(), catalog, t0())
        .await
        .unwrap();
    assert_eq!(rows[0].reported_supported, None);
    let later = t0() + Duration::days(31);
    let batch = runs::claim(
        &db.pool,
        catalog,
        &actor("one"),
        "later",
        2,
        Duration::days(30),
        later,
    )
    .await
    .unwrap();
    let lease = batch.leases.iter().find(|l| l.pin.path == "a.md").unwrap();
    let result = submit(&db.pool, catalog, lease, "stale-neighbor", &input, later)
        .await
        .unwrap();
    assert_eq!(result.outcome, WorkOutcome::Completed);
    assert!(result.outdated);
    let rows = candidates::load(&mut db.pool.acquire().await.unwrap(), catalog, later)
        .await
        .unwrap();
    assert_eq!(rows[0].reported_supported, None);
    assert_eq!(rows[0].last_aging_completed, Some(later));
    // Even if an import restores the old revision as current, a report that was
    // already outdated at submission is not a new current-support assessment.
    sqlx::query("UPDATE documents SET revision=1 WHERE catalog_id=$1 AND path='b.md'")
        .bind(catalog)
        .execute(&db.pool)
        .await
        .unwrap();
    let rows = candidates::load(&mut db.pool.acquire().await.unwrap(), catalog, later)
        .await
        .unwrap();
    assert_eq!(rows[0].reported_supported, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn maintenance_submit_context_and_state_guards() {
    let db = test_db().await;
    let catalog = setup(&db.pool, &["a.md"]).await;
    let lease = claim(&db.pool, catalog, "first", t0()).await;
    let before = snapshot(&db.pool, catalog).await;
    for (outcome, assessment) in [
        (WorkOutcome::Superseded, Assessment::NotAssessed),
        (WorkOutcome::Completed, Assessment::NotAssessed),
        (WorkOutcome::Completed, Assessment::Unverifiable),
        (WorkOutcome::Failed, Assessment::SupportedCurrent),
        (WorkOutcome::Inconclusive, Assessment::Outdated),
    ] {
        let mut input = report(&lease);
        input.outcome = outcome;
        input.assessment = assessment;
        assert!(matches!(
            submit(&db.pool, catalog, &lease, "invalid", &input, t0()).await,
            Err(OkfError::Refused { .. })
        ));
    }
    let mut input = report(&lease);
    input.evidence.clear();
    assert!(submit(&db.pool, catalog, &lease, "empty", &input, t0())
        .await
        .is_err());
    input = report(&lease);
    input.reported_tokens = Some(u64::MAX);
    assert!(matches!(
        submit(&db.pool, catalog, &lease, "overflow", &input, t0()).await,
        Err(OkfError::Refused { .. })
    ));
    input = report(&lease);
    input.evidence[0].pin.revision = 2;
    assert!(submit(&db.pool, catalog, &lease, "wrong-pin", &input, t0())
        .await
        .is_err());
    assert_eq!(snapshot(&db.pool, catalog).await, before);
    assert!(matches!(
        submit(
            &db.pool,
            catalog,
            &lease,
            "expired",
            &report(&lease),
            t0() + Duration::seconds(900)
        )
        .await,
        Err(OkfError::Busy { .. })
    ));
    let newer = claim(&db.pool, catalog, "new", t0() + Duration::seconds(900)).await;
    assert!(submit(
        &db.pool,
        catalog,
        &lease,
        "old",
        &report(&lease),
        t0() + Duration::seconds(900)
    )
    .await
    .is_err());
    assert!(matches!(
        submit(
            &db.pool,
            catalog,
            &newer,
            "deadline",
            &report(&newer),
            t0() + Duration::seconds(4500)
        )
        .await,
        Err(OkfError::Busy { .. })
    ));
    let state: (String, Option<String>) =
        sqlx::query_as("SELECT state,end_reason FROM maintenance_runs WHERE id=$1")
            .bind(&newer.run_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(state, ("failed".into(), Some("budget_exhausted".into())));
    let lease = claim(&db.pool, catalog, "close", t0() + Duration::seconds(4500)).await;
    runs::close(
        &db.pool,
        catalog,
        &actor("one"),
        &lease.run_id,
        RunState::Cancelled,
        "stop",
        false,
        "cancel",
        t0() + Duration::seconds(4500),
    )
    .await
    .unwrap();
    assert!(submit(
        &db.pool,
        catalog,
        &lease,
        "closed",
        &report(&lease),
        t0() + Duration::seconds(4500)
    )
    .await
    .is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn maintenance_submit_concurrent_replay_and_double_completion() {
    let db = test_db().await;
    let catalog = setup(&db.pool, &["a.md"]).await;
    let lease = claim(&db.pool, catalog, "claim", t0()).await;
    let mut input = report(&lease);
    input.findings.push(finding(lease.pin.clone()));
    let (a, b) = tokio::join!(
        submit(&db.pool, catalog, &lease, "same", &input, t0()),
        submit(&db.pool, catalog, &lease, "same", &input, t0())
    );
    assert_eq!(a.unwrap(), b.unwrap());
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM maintenance_findings WHERE catalog_id=$1")
            .bind(catalog)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
    let lease = claim(&db.pool, catalog, "later", t0() + Duration::days(31)).await;
    let input = report(&lease);
    let (a, b) = tokio::join!(
        submit(
            &db.pool,
            catalog,
            &lease,
            "one",
            &input,
            t0() + Duration::days(31)
        ),
        submit(
            &db.pool,
            catalog,
            &lease,
            "two",
            &input,
            t0() + Duration::days(31)
        )
    );
    assert_ne!(a.is_ok(), b.is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn maintenance_submit_rolls_back_late_database_errors() {
    let db = test_db().await;
    let catalog = setup(&db.pool, &["a.md"]).await;
    let lease = claim(&db.pool, catalog, "claim", t0()).await;
    let mut input = report(&lease);
    input.findings = vec![finding(lease.pin.clone()), finding(lease.pin.clone())];
    input.findings[1].assertion = "second".into();
    sqlx::query("CREATE FUNCTION reject_report() RETURNS trigger LANGUAGE plpgsql AS $body$ BEGIN RAISE EXCEPTION 'injected failure'; END $body$").execute(&db.pool).await.unwrap();
    // Both failures occur after earlier writes; no receipt, counter, evidence,
    // finding, work or cursor change may escape the transaction.
    for (create, drop) in [
        ("CREATE TRIGGER reject_report BEFORE INSERT ON maintenance_findings FOR EACH ROW WHEN (NEW.assertion = 'second') EXECUTE FUNCTION reject_report()", "DROP TRIGGER reject_report ON maintenance_findings"),
        ("CREATE TRIGGER reject_report BEFORE INSERT ON maintenance_requests FOR EACH ROW WHEN (NEW.operation = 'submit') EXECUTE FUNCTION reject_report()", "DROP TRIGGER reject_report ON maintenance_requests"),
    ] {
        sqlx::query(create)
            .execute(&db.pool).await.unwrap();
        let before = snapshot(&db.pool, catalog).await;
        assert!(matches!(
            submit(&db.pool, catalog, &lease, "submit", &input, t0()).await,
            Err(OkfError::Database { .. })
        ));
        assert_eq!(snapshot(&db.pool, catalog).await, before);
        sqlx::query(drop)
            .execute(&db.pool)
            .await
            .unwrap();
    }
    assert_eq!(
        submit(&db.pool, catalog, &lease, "submit", &input, t0())
            .await
            .unwrap()
            .finding_ids
            .len(),
        2
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn maintenance_submit_waits_for_source_pins_and_observes_winning_revision() {
    let db = test_db().await;
    let catalog = setup(&db.pool, &["a.md", "b.md"]).await;
    let lease = claim(&db.pool, catalog, "claim", t0()).await;
    let mut input = report(&lease);
    input.findings.push(finding(RevisionPin {
        path: "b.md".into(),
        revision: 1,
    }));
    let mut publisher = db.pool.begin().await.unwrap();
    sqlx::query("INSERT INTO revisions (catalog_id,path,revision,op,content,executable,actor,message,created_at) VALUES ($1,'b.md',2,'import',$2,false,'publisher','edit',$3)")
        .bind(catalog).bind(b"Body updated".as_slice()).bind(t0()).execute(&mut *publisher).await.unwrap();
    sqlx::query("UPDATE documents SET revision=2,content=$2 WHERE catalog_id=$1 AND path='b.md'")
        .bind(catalog)
        .bind(b"Body updated".as_slice())
        .execute(&mut *publisher)
        .await
        .unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *publisher)
        .await
        .unwrap();
    let pool = db.pool.clone();
    let submission =
        tokio::spawn(async move { submit(&pool, catalog, &lease, "submit", &input, t0()).await });
    super::tests::wait_for_source_lock(&db.pool, pid).await;
    publisher.commit().await.unwrap();
    let receipt = submission.await.unwrap().unwrap();
    assert_eq!(receipt.outcome, WorkOutcome::Completed);
    assert!(receipt.outdated);
    let rows = candidates::load(&mut db.pool.acquire().await.unwrap(), catalog, t0())
        .await
        .unwrap();
    assert_eq!(rows[0].reported_supported, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn maintenance_submit_cursors_preserve_other_completed_lanes() {
    let db = test_db().await;
    let catalog = setup(&db.pool, &["a.md", "b.md", "c.md"]).await;
    revision(
        &db.pool,
        catalog,
        "a.md",
        2,
        t0(),
        "---\ntype: Note\nstale_after: 2026-11-01T00:00:00Z\n---\nBody",
    )
    .await;
    revision(
        &db.pool,
        catalog,
        "c.md",
        2,
        t0(),
        "---\ntype: Note\nverified:\n- at: 2026-11-06T12:00:00Z\n---\nBody",
    )
    .await;
    let batch = runs::claim(
        &db.pool,
        catalog,
        &actor("one"),
        "claim",
        3,
        Duration::days(30),
        t0(),
    )
    .await
    .unwrap();
    let a = batch.leases.iter().find(|l| l.pin.path == "a.md").unwrap();
    let b = batch.leases.iter().find(|l| l.pin.path == "b.md").unwrap();
    let c = batch.leases.iter().find(|l| l.pin.path == "c.md").unwrap();
    assert_eq!(c.reasons, vec![Lane::Changed]);
    let ra = report(a);
    let rb = report(b);
    let (a_result, b_result) = tokio::join!(
        submit(&db.pool, catalog, a, "a", &ra, t0()),
        submit(&db.pool, catalog, b, "b", &rb, t0())
    );
    a_result.unwrap();
    b_result.unwrap();
    let state: String = sqlx::query_scalar("SELECT state FROM maintenance_runs WHERE id=$1")
        .bind(&batch.run_id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(state, "running");
    let before = candidates::cursors(&mut db.pool.acquire().await.unwrap(), catalog)
        .await
        .unwrap();
    assert_eq!(before.expiry, "a.md");
    assert_eq!(before.verification, "b.md");
    let mut rc = report(c);
    rc.assessment = Assessment::NotAssessed;
    submit(&db.pool, catalog, c, "c", &rc, t0()).await.unwrap();
    let after = candidates::cursors(&mut db.pool.acquire().await.unwrap(), catalog)
        .await
        .unwrap();
    assert_eq!(after.expiry, before.expiry);
    assert_eq!(after.verification, before.verification);
    assert_eq!(after.next_expiry, before.next_expiry);
    assert_eq!(after.changed, "c.md");
    let result: (String, i32, i64) = sqlx::query_as("SELECT state,primary_count,(SELECT count(*) FROM maintenance_work WHERE run_id=$1 AND outcome='completed') FROM maintenance_runs WHERE id=$1")
        .bind(&batch.run_id).fetch_one(&db.pool).await.unwrap();
    assert_eq!(result, ("completed".into(), 3, 3));
}

#[tokio::test(flavor = "multi_thread")]
async fn maintenance_submit_assessment_pins_and_outdated_findings_never_certify_support() {
    let db = test_db().await;
    let catalog = setup(&db.pool, &["a.md", "b.md"]).await;
    let batch = runs::claim(
        &db.pool,
        catalog,
        &actor("one"),
        "claim",
        2,
        Duration::days(30),
        t0(),
    )
    .await
    .unwrap();
    let a = batch.leases.iter().find(|l| l.pin.path == "a.md").unwrap();
    let b = batch.leases.iter().find(|l| l.pin.path == "b.md").unwrap();
    let mut input = report(a);
    input.evidence.push(Evidence {
        pin: b.pin.clone(),
        excerpt: "Body".into(),
    });
    submit(&db.pool, catalog, a, "a", &input, t0())
        .await
        .unwrap();
    let mut input = report(b);
    let mut f = finding(b.pin.clone());
    f.category = Category::PotentiallyOutdatedInformation;
    input.findings.push(f);
    submit(&db.pool, catalog, b, "b", &input, t0())
        .await
        .unwrap();
    let rows = candidates::load(&mut db.pool.acquire().await.unwrap(), catalog, t0())
        .await
        .unwrap();
    assert_eq!(rows[0].reported_supported, Some((1, t0())));
    assert_eq!(rows[1].reported_supported, None);
    sqlx::query("UPDATE documents SET deleted_at=$2 WHERE catalog_id=$1 AND path='b.md'")
        .bind(catalog)
        .bind(t0())
        .execute(&db.pool)
        .await
        .unwrap();
    let rows = candidates::load(&mut db.pool.acquire().await.unwrap(), catalog, t0())
        .await
        .unwrap();
    assert_eq!(rows[0].reported_supported, None);
}
