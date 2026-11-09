# Maintenance runner: record why a proposal was rejected

Issue #209. When a model proposal failed a check, the runner kept only
`failure: invalid_proposal`, and the server's work row got the generic
summary "Investigation stopped without a usable assessment". Diagnosing run
`7542030392c6223f90eb708f05f752ba` meant copying `journal.json` off the
runner's state volume and re-running every check by hand.

## Changed

- `to_submission` (`src/investigator/proposal.rs`) now returns
  `Result<Submission, ProposalRejection>`. A `ProposalRejection` holds a
  `RejectionSite` (`packet`, `proposal`, `summary`, `finding N`, or
  `finding N evidence M`, all 0-based) and the failed check, and renders as
  `<site>: <check>`, e.g. `finding 1 evidence 1: excerpt is not a substring
  of s1`. It converts into `RunnerError::InvalidProposal` with `?`, so
  existing callers are unchanged.
- When the final S1 validation (`validate::submission_for_work`) fails, the
  runner re-validates each finding on its own to name the first one that
  fails, e.g. `finding 4: comparison findings require two distinct concept
  paths`. A report-wide failure (size, primary evidence) is attributed to
  `proposal`. The check text is `validate`'s fixed `Refused` reason, or
  `secret detected`.
- No model text is ever echoed: a secret is reported as `secret detected in
  <field>`, and an unknown `source_id` is quoted only when it has the packet
  label form `s<1-3 digits>`.
- `WorkState` (`src/investigator/journal.rs`) gains an optional
  `rejection` field, `#[serde(default)]` and skipped when empty, so journals
  written before it still load and the schema version stays 1. Validation
  requires `failure: invalid_proposal` beside it, and text that is 1..=256
  bytes, single-line and secret-free.
- `Controller::record_model` stores the rendered reason and emits
  `tracing::warn!` on the fixed target `okf_maintenance::rejection`.
  `generic()` uses it as the failed submission's summary:
  `Proposal rejected: <reason>`.
- `okf-maintenance` used to install `NoSubscriber`, so that warn line would
  have gone nowhere. `quiet_logging` now installs a stderr `fmt` layer with a
  fixed `Targets` filter that passes only `okf_maintenance::rejection` at
  warn. It still ignores `RUST_LOG`, and rmcp and reqwest stay silent.
- `docs/operations.md` describes the rejection reason and the new logging
  policy. It also corrects the stale claim that invalid output stops further
  inference, which has been untrue since #205.

## Tests

- `investigator_proposal_rejection_names_site_and_check`
  (`tests/investigator_collection.rs`): 20 cases, one per check, assert the
  site, the check and the rendered text. They also check that neither the
  secret nor any source text is echoed.
- `investigator_journal_rejection_requires_invalid_proposal_and_clean_text`:
  round-trip, omitted when empty, and refusal of mismatched, empty,
  multi-line, oversize and secret-bearing reasons.
- `investigator_supervisor_invalid_proposal_fails_only_its_item` now asserts
  that the journal keeps the reason and that the Submit summary carries it.
  The recovered-journal test asserts that an old journal without a reason
  keeps the generic summary.
- `investigator_cli_logs_only_the_rejection_reason` runs the real CLI entry
  with `RUST_LOG=trace`. Stderr holds exactly one WARN line with the reason,
  and no canary leaks.

## Compatibility

A journal holding a `rejection` field cannot be read by an older runner
(`deny_unknown_fields`). The field is written only for rejected items, and
the journal holds only the latest run.
