//! Pure selection of maintenance candidates.

use chrono::{DateTime, Duration, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::types::{AgingGroup, Lane, RevisionPin};
use crate::error::OkfError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Candidate {
    pub pin: RevisionPin,
    pub edited_at: DateTime<Utc>,
    pub stale_after: Option<DateTime<Utc>>,
    pub legacy_verified: Option<DateTime<Utc>>,
    pub changed_done: bool,
    pub last_attempt: Option<DateTime<Utc>>,
    pub last_aging_completed: Option<DateTime<Utc>>,
    pub reported_supported: Option<(i32, DateTime<Utc>)>,
    pub retry_at: Option<DateTime<Utc>>,
    pub claimed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Cursors {
    pub changed: String,
    pub expiry: String,
    pub verification: String,
    pub next_expiry: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Selected {
    pub pin: RevisionPin,
    pub reasons: Vec<Lane>,
    pub aging_group: Option<AgingGroup>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Selection {
    pub items: Vec<Selected>,
    pub cursors: Cursors,
}

/// Selects a bounded fair batch without persisting proposed cursors.
///
/// # Errors
/// Refuses batch sizes outside 1..=50.
pub fn select(
    candidates: &[Candidate],
    cursors: &Cursors,
    now: DateTime<Utc>,
    review_interval: Duration,
    limit: usize,
) -> Result<Selection, OkfError> {
    if !(1..=50).contains(&limit) {
        return Err(OkfError::Refused {
            reason: "maintenance batch size must be 1..=50".into(),
        });
    }
    let available: Vec<_> = candidates
        .iter()
        .filter(|c| !c.claimed && c.retry_at.is_none_or(|retry| now >= retry))
        .collect();
    let mut changed: Vec<_> = available
        .iter()
        .copied()
        .filter(|c| !c.changed_done)
        .collect();
    changed.sort_by(|a, b| a.pin.path.cmp(&b.pin.path));
    let start = changed
        .iter()
        .position(|c| c.pin.path > cursors.changed)
        .unwrap_or(0);
    changed.rotate_left(start);

    let mut expiry = Vec::new();
    let mut verification = Vec::new();
    for candidate in available {
        // A completed aging attempt for this revision controls the cadence,
        // even if stale_after remains in the past.
        let completed = candidate.last_aging_completed.filter(|at| *at <= now);
        if completed.is_some_and(|at| now < at + review_interval) {
            continue;
        }
        let expired = candidate.stale_after.is_some_and(|at| now >= at);
        let support = candidate
            .reported_supported
            .filter(|(rev, at)| *rev == candidate.pin.revision && *at <= now)
            .map(|(_, at)| at);
        let legacy = candidate.legacy_verified.filter(|at| *at <= now);
        let hint = support.into_iter().chain(legacy).max();
        if expired {
            expiry.push(candidate);
        } else if completed.is_some() || hint.is_none_or(|at| now >= at + review_interval) {
            // A verification hint delays scheduling; it never proves support.
            verification.push(candidate);
        }
    }
    let epoch = DateTime::<Utc>::UNIX_EPOCH;
    expiry.sort_by_key(|c| (c.last_attempt.unwrap_or(epoch), c.pin.path.clone()));
    verification.sort_by_key(|c| {
        (
            c.last_attempt.unwrap_or(epoch),
            c.reported_supported
                .filter(|(rev, at)| *rev == c.pin.revision && *at <= now)
                .map(|(_, at)| at)
                .into_iter()
                .chain(c.legacy_verified.filter(|at| *at <= now))
                .max()
                .unwrap_or(epoch),
            c.pin.path.clone(),
        )
    });
    rotate_equal_rank(&mut expiry, &cursors.expiry, |c| {
        (c.last_attempt.unwrap_or(epoch), epoch)
    });
    rotate_equal_rank(&mut verification, &cursors.verification, |c| {
        (
            c.last_attempt.unwrap_or(epoch),
            c.reported_supported
                .filter(|(rev, at)| *rev == c.pin.revision && *at <= now)
                .map(|(_, at)| at)
                .into_iter()
                .chain(c.legacy_verified.filter(|at| *at <= now))
                .max()
                .unwrap_or(epoch),
        )
    });

    let mut state = cursors.clone();
    let mut items = Vec::new();
    let mut changed_pos = 0;
    let mut expiry_pos = 0;
    let mut verification_pos = 0;
    let changed_budget = limit / 2;
    let aging_budget = limit - changed_budget;
    for lane in [Lane::Changed, Lane::Aging] {
        let budget = if lane == Lane::Changed {
            changed_budget
        } else {
            aging_budget
        };
        for _ in 0..budget {
            let next = match lane {
                Lane::Changed => {
                    let candidate = changed.get(changed_pos).copied();
                    changed_pos += usize::from(candidate.is_some());
                    candidate.map(|c| (c, None))
                }
                Lane::Aging => next_aging(
                    &expiry,
                    &verification,
                    &mut expiry_pos,
                    &mut verification_pos,
                    &mut state.next_expiry,
                ),
            };
            if let Some((candidate, group)) = next {
                insert(&mut items, candidate, lane, group, &mut state);
            }
        }
    }
    // Overlap can leave aging's reserved slots unfilled. Fill them before
    // borrowing the remaining capacity for changed work.
    while items.len() < limit
        && items.iter().filter(|i| i.reasons == [Lane::Aging]).count() < aging_budget
    {
        let Some((c, group)) = next_aging(
            &expiry,
            &verification,
            &mut expiry_pos,
            &mut verification_pos,
            &mut state.next_expiry,
        ) else {
            break;
        };
        insert(&mut items, c, Lane::Aging, group, &mut state);
    }
    while items.len() < limit {
        if let Some(c) = changed.get(changed_pos).copied() {
            changed_pos += 1;
            insert(&mut items, c, Lane::Changed, None, &mut state);
        } else if let Some((c, group)) = next_aging(
            &expiry,
            &verification,
            &mut expiry_pos,
            &mut verification_pos,
            &mut state.next_expiry,
        ) {
            insert(&mut items, c, Lane::Aging, group, &mut state);
        } else {
            break;
        }
    }
    // Add all eligible reasons to pins selected in only one lane.
    for item in &mut items {
        if changed.iter().any(|c| c.pin == item.pin) && !item.reasons.contains(&Lane::Changed) {
            item.reasons.insert(0, Lane::Changed);
        }
        if item.aging_group.is_none() {
            if expiry.iter().any(|c| c.pin == item.pin) {
                item.reasons.push(Lane::Aging);
                item.aging_group = Some(AgingGroup::Expiry);
            } else if verification.iter().any(|c| c.pin == item.pin) {
                item.reasons.push(Lane::Aging);
                item.aging_group = Some(AgingGroup::Verification);
            }
        }
    }
    Ok(Selection {
        items,
        cursors: state,
    })
}

fn rotate_equal_rank(
    rows: &mut [&Candidate],
    cursor: &str,
    rank: impl Fn(&Candidate) -> (DateTime<Utc>, DateTime<Utc>),
) {
    let mut start = 0;
    while start < rows.len() {
        let mut end = start + 1;
        while end < rows.len() && rank(rows[start]) == rank(rows[end]) {
            end += 1;
        }
        if let Some(offset) = rows[start..end]
            .iter()
            .position(|c| c.pin.path.as_str() > cursor)
        {
            rows[start..end].rotate_left(offset);
        }
        start = end;
    }
}

fn next_aging<'a>(
    expiry: &[&'a Candidate],
    verification: &[&'a Candidate],
    expiry_pos: &mut usize,
    verification_pos: &mut usize,
    next_expiry: &mut bool,
) -> Option<(&'a Candidate, Option<AgingGroup>)> {
    let use_expiry = if *expiry_pos >= expiry.len() {
        false
    } else if *verification_pos >= verification.len() {
        true
    } else {
        *next_expiry
    };
    if use_expiry {
        let candidate = *expiry.get(*expiry_pos)?;
        *expiry_pos += 1;
        *next_expiry = false;
        Some((candidate, Some(AgingGroup::Expiry)))
    } else {
        let candidate = *verification.get(*verification_pos)?;
        *verification_pos += 1;
        *next_expiry = true;
        Some((candidate, Some(AgingGroup::Verification)))
    }
}

fn insert(
    items: &mut Vec<Selected>,
    candidate: &Candidate,
    lane: Lane,
    group: Option<AgingGroup>,
    state: &mut Cursors,
) {
    if let Some(item) = items.iter_mut().find(|item| item.pin == candidate.pin) {
        if !item.reasons.contains(&lane) {
            item.reasons.push(lane);
        }
        if group.is_some() {
            item.aging_group = group;
        }
    } else {
        items.push(Selected {
            pin: candidate.pin.clone(),
            reasons: vec![lane],
            aging_group: group,
        });
    }
    match group {
        Some(AgingGroup::Expiry) => state.expiry = candidate.pin.path.clone(),
        Some(AgingGroup::Verification) => state.verification = candidate.pin.path.clone(),
        None => state.changed = candidate.pin.path.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::collections::HashSet;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 6, 12, 0, 0).unwrap()
    }

    fn candidate(path: &str) -> Candidate {
        Candidate {
            pin: RevisionPin {
                path: path.into(),
                revision: 1,
            },
            edited_at: t0(),
            stale_after: None,
            legacy_verified: None,
            changed_done: false,
            last_attempt: None,
            last_aging_completed: None,
            reported_supported: None,
            retry_at: None,
            claimed: false,
        }
    }

    fn cursors() -> Cursors {
        Cursors {
            changed: String::new(),
            expiry: String::new(),
            verification: String::new(),
            next_expiry: true,
        }
    }

    fn pick(
        candidates: &[Candidate],
        cursors: &Cursors,
        now: DateTime<Utc>,
        limit: usize,
    ) -> Selection {
        select(candidates, cursors, now, Duration::days(30), limit).unwrap()
    }

    #[test]
    fn maintenance_selection_public_types_are_serializable_and_schematized() {
        fn assert_contract<
            T: serde::Serialize + serde::de::DeserializeOwned + schemars::JsonSchema,
        >() {
        }
        assert_contract::<Candidate>();
        assert_contract::<Cursors>();
        assert_contract::<Selected>();
        assert_contract::<Selection>();
        let encoded = serde_json::to_value(candidate("test.md")).unwrap();
        assert_eq!(encoded["pin"]["path"], "test.md");
    }

    #[test]
    fn maintenance_selection_shares_lanes_and_wraps() {
        let mut candidates: Vec<_> = (0..12)
            .map(|n| {
                let mut c = candidate(&format!("changed-{n:02}.md"));
                c.last_aging_completed = Some(t0());
                c
            })
            .chain((0..12).map(|n| {
                let mut c = candidate(&format!("aging-{n:02}.md"));
                c.changed_done = true;
                c
            }))
            .collect();
        let first = pick(&candidates, &cursors(), t0(), 10);
        let changed_only_count = first
            .items
            .iter()
            .filter(|i| i.reasons == [Lane::Changed])
            .count();
        let aging_only_count = first
            .items
            .iter()
            .filter(|i| i.reasons == [Lane::Aging])
            .count();
        assert_eq!((changed_only_count, aging_only_count), (5, 5));
        let selected_unique_pin_count = first
            .items
            .iter()
            .map(|i| (i.pin.path.clone(), i.pin.revision))
            .collect::<HashSet<_>>()
            .len();
        assert_eq!(selected_unique_pin_count, first.items.len());
        assert_eq!(
            pick(&candidates, &cursors(), t0(), 3)
                .items
                .iter()
                .filter(|i| i.reasons == [Lane::Aging])
                .count(),
            2
        );
        let mut state = cursors();
        let mut seen = HashSet::new();
        for round in 0..12 {
            let selected = pick(&candidates, &state, t0(), 3);
            state = selected.cursors;
            for item in selected.items {
                if item.reasons.contains(&Lane::Aging) {
                    seen.insert(item.pin.path.clone());
                    let c = candidates.iter_mut().find(|c| c.pin == item.pin).unwrap();
                    c.last_aging_completed = Some(t0());
                }
                if item.reasons.contains(&Lane::Changed) {
                    let c = candidates.iter_mut().find(|c| c.pin == item.pin).unwrap();
                    c.changed_done = true;
                }
            }
            candidates.push(candidate(&format!("changed-new-{round:02}.md")));
            candidates.last_mut().unwrap().last_aging_completed = Some(t0());
        }
        assert!(
            (0..12).all(|n| seen.contains(&format!("aging-{n:02}.md"))),
            "aging paths missed: {seen:?}"
        );
    }

    #[test]
    fn maintenance_selection_overlap_backfill_preserves_aging_capacity() {
        let mut candidates: Vec<_> = (0..5)
            .map(|n| candidate(&format!("a-overlap-{n}.md")))
            .collect();
        candidates.extend((0..12).map(|n| {
            let mut c = candidate(&format!("b-changed-{n:02}.md"));
            c.last_aging_completed = Some(t0());
            c
        }));
        candidates.extend((0..5).map(|n| {
            let mut c = candidate(&format!("z-aging-{n}.md"));
            c.changed_done = true;
            c
        }));
        let selected = pick(&candidates, &cursors(), t0(), 10);
        assert_eq!(selected.items.len(), 10);
        assert_eq!(
            selected
                .items
                .iter()
                .filter(|i| i.pin.path.starts_with("z-aging-"))
                .count(),
            5,
            "selected: {:?}",
            selected.items
        );
        assert_eq!(
            selected
                .items
                .iter()
                .filter(|i| i.pin.path.starts_with("b-changed-"))
                .count(),
            0
        );
        assert!(selected
            .items
            .iter()
            .filter(|i| i.pin.path.starts_with("a-overlap-"))
            .all(|i| i.reasons == [Lane::Changed, Lane::Aging]));
    }

    #[test]
    fn maintenance_selection_retry_and_expiry_boundaries() {
        let mut c = candidate("expired.md");
        c.changed_done = true;
        c.stale_after = Some(t0());
        c.retry_at = Some(t0() + Duration::days(1));
        assert!(pick(
            &[c.clone()],
            &cursors(),
            t0() + Duration::seconds(86_399),
            1
        )
        .items
        .is_empty());
        assert_eq!(
            pick(
                &[c.clone()],
                &cursors(),
                t0() + Duration::seconds(86_400),
                1
            )
            .items[0]
                .aging_group,
            Some(AgingGroup::Expiry)
        );
        c.retry_at = None;
        assert_eq!(pick(&[c.clone()], &cursors(), t0(), 1).items.len(), 1);
        c.last_aging_completed = Some(t0());
        assert!(pick(&[c.clone()], &cursors(), t0() + Duration::days(29), 1)
            .items
            .is_empty());
        assert_eq!(
            pick(&[c.clone()], &cursors(), t0() + Duration::days(30), 1)
                .items
                .len(),
            1
        );
        c.claimed = true;
        assert!(pick(&[c], &cursors(), t0() + Duration::days(30), 1)
            .items
            .is_empty());
    }

    #[test]
    fn maintenance_selection_future_completion_does_not_delay_expiry() {
        let mut c = candidate("future-completion.md");
        c.changed_done = true;
        c.stale_after = Some(t0());
        c.last_aging_completed = Some(t0() + Duration::days(90));
        assert_eq!(
            pick(&[c.clone()], &cursors(), t0(), 1).items[0].aging_group,
            Some(AgingGroup::Expiry)
        );
        c.last_aging_completed = Some(t0());
        assert!(pick(&[c], &cursors(), t0(), 1).items.is_empty());
    }

    #[test]
    fn maintenance_selection_ignores_edit_as_verification() {
        let mut c = candidate("unverified.md");
        c.changed_done = true;
        c.edited_at = t0(); // Even a freshly generated revision is not verified.
        assert_eq!(
            pick(&[c.clone()], &cursors(), t0(), 1).items[0].aging_group,
            Some(AgingGroup::Verification)
        );
        c.legacy_verified = Some(t0() + Duration::days(1));
        assert_eq!(pick(&[c.clone()], &cursors(), t0(), 1).items.len(), 1);
        c.reported_supported = Some((1, t0()));
        assert!(pick(&[c.clone()], &cursors(), t0(), 1).items.is_empty());
        c.pin.revision = 2;
        c.changed_done = false;
        let selected = pick(&[c], &cursors(), t0(), 1);
        assert_eq!(selected.items[0].reasons, [Lane::Changed, Lane::Aging]);
    }

    #[test]
    fn maintenance_selection_uses_latest_usable_verification_hint() {
        let mut c = candidate("both-hints.md");
        c.changed_done = true;
        c.legacy_verified = Some(t0() - Duration::days(5));
        c.reported_supported = Some((1, t0() - Duration::days(40)));
        assert!(pick(&[c.clone()], &cursors(), t0(), 1).items.is_empty());
        c.legacy_verified = Some(t0() - Duration::days(40));
        c.reported_supported = Some((1, t0() - Duration::days(5)));
        assert!(pick(&[c.clone()], &cursors(), t0(), 1).items.is_empty());
        c.legacy_verified = Some(t0() + Duration::days(1));
        c.reported_supported = Some((1, t0() - Duration::days(30)));
        assert_eq!(pick(&[c], &cursors(), t0(), 1).items.len(), 1);
    }

    #[test]
    fn maintenance_selection_empty_overlap_and_rotation() {
        assert!(pick(&[], &cursors(), t0(), 1).items.is_empty());
        assert!(select(&[], &cursors(), t0(), Duration::days(30), 0).is_err());
        assert!(select(&[], &cursors(), t0(), Duration::days(30), 51).is_err());
        let changed: Vec<_> = (0..3)
            .map(|n| {
                let mut c = candidate(&format!("c{n}.md"));
                c.last_aging_completed = Some(t0());
                c
            })
            .collect();
        assert_eq!(pick(&changed, &cursors(), t0(), 3).items.len(), 3);
        let aging: Vec<_> = (0..3)
            .map(|n| {
                let mut c = candidate(&format!("a{n}.md"));
                c.changed_done = true;
                c
            })
            .collect();
        assert_eq!(pick(&aging, &cursors(), t0(), 3).items.len(), 3);
        let overlap = vec![candidate("both.md")];
        assert_eq!(
            pick(&overlap, &cursors(), t0(), 3).items[0].reasons,
            [Lane::Changed, Lane::Aging]
        );

        let mut expired = aging;
        for c in &mut expired {
            c.stale_after = Some(t0());
        }
        let first = pick(&expired, &cursors(), t0(), 1);
        let next = pick(&expired, &first.cursors, t0(), 1);
        assert_ne!(first.items[0].pin, next.items[0].pin);
        let third = pick(&expired, &next.cursors, t0(), 1);
        assert_eq!(third.items[0].pin.path, "a2.md");
        let wrapped = pick(&expired, &third.cursors, t0(), 1);
        assert_eq!(wrapped.items[0].pin, first.items[0].pin);
        expired
            .iter_mut()
            .find(|c| c.pin == first.items[0].pin)
            .unwrap()
            .last_attempt = Some(t0());
        let next = pick(&expired, &first.cursors, t0(), 1);
        assert_ne!(first.items[0].pin, next.items[0].pin);
    }

    #[test]
    fn maintenance_selection_prioritizes_older_attempts_and_alternates_groups() {
        let mut expiry = candidate("expiry.md");
        expiry.changed_done = true;
        expiry.stale_after = Some(t0());
        let mut verification = candidate("verification.md");
        verification.changed_done = true;
        let first = pick(&[expiry.clone(), verification.clone()], &cursors(), t0(), 1);
        assert_eq!(first.items[0].pin.path, "expiry.md");
        let second = pick(
            &[expiry.clone(), verification.clone()],
            &first.cursors,
            t0(),
            1,
        );
        assert_eq!(second.items[0].pin.path, "verification.md");
        expiry.last_attempt = Some(t0());
        let mut older = expiry.clone();
        older.pin.path = "older.md".into();
        older.last_attempt = None;
        assert_eq!(
            pick(&[expiry, older], &cursors(), t0(), 1).items[0]
                .pin
                .path,
            "older.md"
        );
        let mut legacy = candidate("legacy.md");
        legacy.changed_done = true;
        legacy.legacy_verified = Some(t0() - Duration::days(10));
        assert!(pick(&[legacy.clone()], &cursors(), t0(), 1)
            .items
            .is_empty());
        assert_eq!(
            pick(&[legacy], &cursors(), t0() + Duration::days(20), 1)
                .items
                .len(),
            1
        );
    }
}
