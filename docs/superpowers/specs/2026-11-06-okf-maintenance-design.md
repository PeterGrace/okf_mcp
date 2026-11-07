# Report-only catalog maintenance

Date: 2026-11-06

## Status and goal

Proposed implementation design, produced through brainstorming. Automatic mode
selected report-only operation and approved the architectural outline. The user
has not individually reviewed every default below. No feature is implemented by
this document, and implementation requires a separate execution decision.

Help a growing OKF catalog remain useful by discovering connections, proposing
consolidation, and investigating aging information without discarding original
submissions or silently rewriting canonical concepts.

## Existing foundations

The repository at `b070e6a` has:

- Exact document bytes and append-only revision history in Postgres
  (`migrations/0001_schema.sql`). History survives ordinary document deletion,
  but catalog deletion cascades to history; this design does not change that
  administrative operation or promise preservation beyond it.
- Staged edits and conflict-checked publication through changesets.
- Snapshots, lexical search, link analysis, sprawl, expiry metadata, and review
  tools. No model-provider integration appears in the dependency manifest.
- An `oldest` report that sorts by the newest `generated`, `verified`, or
  `reviewed` timestamp (`src/okf/analysis/oldest.rs`). A recent edit or unsuccessful
  review can therefore affect that report's ordering. It is a touched-age report,
  not proof of current factual verification.
- Ordered access levels `Read < Write < Admin` and per-key catalog grants
  (`src/auth.rs`). A reporter needs a separate capability, not document write
  access or a new intermediate level that accidentally inherits write access.

Keep existing tool contracts unchanged. Maintenance has its own selection and
reporting interfaces.

## Scope and delivery boundaries

### Slice 1: server-side maintenance foundation

Deliver deterministic candidate selection, revision-pinned evidence, bounded work
claims, durable report storage, report retrieval, and dispositions. A human or
existing agent can exercise the workflow through MCP without a new scheduler.
This slice is independently usable and testable.

### Slice 2: external scheduled investigation

After slice 1 is delivered, design the runner against its interfaces. The runner
uses an existing agent harness rather than embedding inference inside the server.
Its scheduler, model choice, credentials, token accounting, network policy, and
operational deployment require their own implementation specification. They are
not prerequisites for slice 1 and are not deployed by this design.

### Exclusions

No automatic canonical edits, merges, splits, deletions, verification stamps, or
changeset publication. No automatically generated material in normal search.
No embeddings, graph database, web UI, cross-catalog synthesis, or new retention
policy. No claim of improved retrieval quality without evaluation. Accepting a
finding only records that it merits follow-up; it never applies a correction.

## Architecture and boundaries

The server owns catalog access, candidate selection, work ownership, evidence
validation, and persistence. Add maintenance-specific modules alongside existing
store, analysis, and MCP modules; do not place inference in the snapshot poll loop.

An external investigator claims work, reads published content and relevant
neighbors, and submits findings. Existing read tools remain available. Maintenance
inputs exclude unpublished changesets and maintenance reports themselves.

A reporting key has ordinary read access plus an explicitly granted
`maintenance_report` capability, default false for existing keys. Its existing
catalog grants still limit every call. Canonical write tools continue to require
`Write`; the reporting capability cannot satisfy that requirement. Administrators
manage the capability through the existing key administration interface. Human
finding dispositions require ordinary write access to that catalog; reporter-only
keys cannot dismiss their own findings as a substitute for review.

Maintenance records are not OKF concepts: they do not appear in concept search,
link reports, indexes, or ordinary bundle export. Database backup includes them;
report retrieval provides a JSON export. Canonical content remains portable as
before. Reports are advisory records, not an additional source of catalog truth.

## Candidate selection

Each claim targets exactly one explicitly selected catalog and a bounded number
of live, non-deprecated concepts. Machinery, indexes, and logs are not candidates.

Two lanes share the batch:

1. **Changed:** a concept's current revision has not had a completed connection
   analysis. On first use, existing concepts are eligible too.
2. **Aging:** concepts due for relevance investigation, independent of edits.

Default batch size is 10, maximum 50. Split slots evenly, giving an odd slot to
aging; use spare slots from an empty lane in the other lane. Return each concept
only once per batch, carrying both reasons if applicable. Persist lane cursors
rather than restarting at the same path on every run.

For aging work, alternate slots between overdue explicit `stale_after` values and
oldest verification evidence. Rotate within each group by last attempt and path
so a permanently expired concept cannot monopolize service. Empty groups lend
slots to the other group. Never-verified concepts precede verified concepts in
the verification group. Initial review interval is 30 days, catalog-configurable
with a minimum of one day. Volatility scoring and backlink weighting are deferred
until measured workloads justify them.

Track three distinct signals:

- Last content edit, from the document revision.
- Last investigation attempt, including failure or inability to verify.
- Last evidenced successful verification and the revision it actually covered.

Existing `verified` timestamps are legacy scheduling hints, not revision-bound
proof. Imported `generated` and `reviewed` stamps never count as successful
verification. Report-only completion does not update canonical `verified` data;
a supported finding is an investigator's attributed assessment, not a server
certification. A later content revision is uncovered even if its predecessor was
verified.

Failed or inconclusive investigations become eligible after a 24-hour retry
cooldown. This updates attempt bookkeeping only, not factual freshness. A
successful aging investigation schedules the next investigation after the review
interval; changed content still independently enters the changed lane. A finding
of outdated information does not certify the content as current.

## Work and run lifecycle

Persist maintenance runs, work items, and findings in separate Postgres tables.
A work item records catalog, path, revision, lane reasons, attempt outcome, key ID,
actor, and server-generated timestamps. A run records its idempotency key, bounded
budgets, state, and counters. Findings reference their run and evidence revisions.

Work is claimed transactionally with a 15-minute lease and an opaque lease token.
Only one active claim per catalog/path/revision is permitted. Renewal requires
the owning credential and current lease token. Lease ownership is not an API-key
secret. A worker losing its lease cannot complete that claim; a replacement can
reclaim it. Recheck grants and revocation on every operation using the server's
existing authentication-refresh behavior.

A claim returns exact source revisions and content through a revision-pinned read
interface. The current `get_concept` alone is insufficient because content may
change between selection and reading. Evidence reads can retrieve additional
published revisions in the same authorized catalog, never another catalog.

Run states are running, completed, failed, or cancelled. Work outcomes are
completed, inconclusive, failed, or superseded; an expired lease is reclaimable,
not successful completion. A newer document revision supersedes unfinished work
on its predecessor. A submitted historical report may still be stored, but is
marked outdated and cannot mark the current revision processed. Completion and
cursor movement occur atomically. Cancellation stops new claims and invalidates
outstanding leases. Cap run duration at one hour and primary concepts at 50.

Retries use the caller's idempotency key, scoped to credential and catalog. An
identical submission returns the existing result; different content with the same
key is rejected. Do not keep transactions open during investigation or inference.

## Findings and provenance

Finding categories are suggested link, possible duplicate, conflicting assertions,
potentially outdated information, and unverifiable claim. Each includes:

- A concise assertion and proposed follow-up, explicitly distinguished from fact.
- Investigator actor and run identity.
- At least one catalog/path/revision evidence reference with an exact UTF-8
  excerpt; comparisons require evidence from each compared concept.
- An evidence relationship: direct statement, inference, or external observation.
- An explanation of uncertainty and dependencies on unavailable information.

The server checks catalog permission, revision existence, excerpt membership,
category shape, and payload limits. It cannot establish semantic truth merely by
checking that an excerpt exists. Reject secrets using the existing scanner before
persisting report text. Default limits: 20 findings per primary work item, 20
evidence references per finding, 2 KiB per excerpt, and 256 KiB per submission.

Findings identify a possible duplicate, not a proven equivalent entity. Preserve
historical versus current assertions rather than treating all disagreement as
contradiction. Several copies of one assertion are not independent corroboration.
Synthesis must cite original concepts, never earlier generated maintenance reports.

For slice 1, external observations may include attributed source URIs and excerpts,
but the server does not fetch URLs or claim to have verified them. The future
runner must enforce an explicit network allowlist, prohibit private-network and
credential leakage, and treat fetched material and catalog prose as untrusted
data rather than executable instructions.

## Retrieval and human dispositions

Proposed MCP operations cover claiming and renewing work, revision-pinned reads,
submitting results, completing/cancelling runs, listing runs and findings, and
recording dispositions. Final tool names and request types belong in the slice-1
implementation plan. Every operation names a catalog; an opaque run or finding ID
never bypasses catalog authorization.

Finding dispositions are open, accepted-for-follow-up, dismissed, and needs-input.
Record actor, server time, and rationale; preserve disposition history. Outdated
is a separate computed flag, true when any cited concept is deleted or differs
from its cited revision. An old acceptance does not authorize applying a proposal
to newer source text. Missing primary concepts supersede queued work. Catalog
re-creation receives a new database identity and must not inherit old claims.

Reports retain evidence pins and remain retrievable even after sources change.
Deletion of the whole catalog follows existing administrative semantics; it is
not a maintenance operation.

## Error handling and operations

Database errors leave uncommitted claims/results invisible. Worker crashes recover
through lease expiry. Inference failures produce attributed failures without
renewing verification. Unauthorized IDs are indistinguishable from unknown IDs.
Budget exhaustion records partial progress and ends the run; uncompleted concepts
remain eligible. The server enforces document, duration, and payload limits; model
token/cost ceilings must be enforced by the future runner, not trusted as a server
guarantee. Record reported token usage as attributed telemetry only.

Use the existing server clock for timestamps and existing authentication polling
semantics. Log run IDs, counts, failures, and durations, not document bodies,
excerpts, prompts, credentials, or database connection strings.

## Validation and acceptance

Slice 1 must demonstrate:

1. Expired and never-verified concepts receive work despite continuous edits
   elsewhere; retries and overlapping lanes do not starve other concepts.
2. An inconclusive review changes attempt bookkeeping but never verification.
3. Existing `oldest`, search, changeset, history, and export contracts remain intact.
4. Claims are exclusive; expired leases recover; obsolete tokens cannot complete
   replacement claims; identical retries do not duplicate results.
5. Editing or deleting a source during investigation flags historical results as
   outdated and never marks a newer revision processed.
6. Reporter credentials cannot edit or publish canonical content, dispose of
   findings, or access another catalog through evidence or guessed identifiers.
7. Malformed evidence, oversized payloads, and detected secrets are rejected.
8. Successful, failed, cancelled, and retried maintenance leave all canonical
   document and revision bytes unchanged.
9. Reports stay outside ordinary search and exported OKF bundles.

Use pure selection tests with a fixed clock, Postgres integration tests following
the existing store tests, and MCP authorization/lifecycle tests following sibling
modules. Run the repository's own formatting, clippy, rustdoc, and test gates.

Before automating semantic investigation, use a human-reviewed fixture set with
true and false link suggestions, distinct entities with similar names, copied
assertions, genuine conflicts, historical changes, and unavailable sources.
Measure evidence accuracy, useful findings, false positives, coverage, and cost.
No semantic quality threshold or retrieval speedup is claimed by this design.

## Next approval boundary

Review this specification, then prepare an implementation plan for slice 1 only.
Do not implement slice 2 or deploy a scheduled task as an incidental extension.
No git commit, push, issue creation, or deployment is authorized by this document.
