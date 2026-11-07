# Catalog maintenance slice 2: controlled external investigator

Date: 2026-11-06

## Status, intent, and prerequisites

This is a design for an external investigator, not an implementation or deployment.
The user approved the slice-1 plan and asked to continue to slice 2. Automatic mode
approved the outline below; the runtime and numerical defaults are agent proposals,
not individually supplied user preferences.

Read with:

- `2026-11-06-okf-maintenance-design.md`: preservation and report-only principles.
- `../plans/2026-11-06-okf-maintenance-slice-1.md`: proposed MCP contracts.

Slice 1 is planned, not delivered in the current checkout (`b070e6a`). Slice 2
cannot run live until its required tools and permission rules are implemented and
validated. Its plan must depend on slice 1, not silently implement a second server
API or open direct database access.

The goal is a bounded, schedulable investigation that identifies useful links,
possible duplication, conflicting claims, and aging information while keeping
original submissions intact. A report that explicitly cannot verify an old fact
is preferable to a persuasive but unsupported refresh of that fact.

## Goals, non-goals, and anti-goals

Deliver a separate `okf-maintenance` Rust binary in this repository with preflight,
run-once, and offline evaluation commands. It communicates exclusively through
MCP, uses a tool-free Claude Code subprocess for semantic suggestions, and leaves
reports for a human. An external scheduler invokes run-once; the runner contains
no resident scheduling loop.

No canonical edits or publication; no database credentials; no new provider SDK;
no autonomous shell/browser/MCP access for the model; no external URL fetching;
no web UI; no claim that rereading the catalog verifies the real world. No enabled
scheduled task, paid model invocation, provider account setup, or production
credential creation is authorized by the design or planning session.

Failure conditions include leaking a catalog credential to the model, attributing
copied statements as independent confirmation, letting injected prose request new
tools or URLs, losing an original source, and presenting a spending threshold as
a guaranteed hard billing cap.

## Approaches considered

1. **Rust controller plus tool-free Claude Code (selected through automatic mode).**
   Controller owns MCP access, source selection, leases, validation, and reporting.
   The model only receives bounded evidence and proposes structured findings.
   Reuses the Rust repository and an installed CLI, with explicit orchestration.
2. **Autonomous MCP agent.** Less controller code, but the model participates in
   read planning and run lifecycle, complicating permissions, budgets and recovery.
3. **Direct provider API integration.** Better per-request controls, but adds
   provider-specific implementation and maintenance contrary to the parent design.

Local read-only CLI inspection found Claude Code supports `--print`, structured
output/schema, `--tools`, `--strict-mcp-config`, `--bare`, `--restricted`,
`--disable-slash-commands`, `--no-session-persistence`, and `--max-budget-usd`.
Zidane supports headless structured output but its help did not expose an equivalent
spending flag. This is a reason for the proposed adapter, not a claim that Zidane
cannot support budgets through other interfaces. CLI capabilities are checked
again at execution time; discovery must make no inference request. The inspected
CLI is `2.1.319 (Claude Code)` and the local Rust toolchain is `1.99.1`; checking
version/help did not establish the shape of a paid invocation's output.

## Components and trust boundaries

- **CLI/configuration:** validates one catalog, one MCP origin, an exact model ID,
  limits, local state location, and explicit upload/spending consent.
- **MCP client:** only calls a compiled allowlist of read and maintenance tools.
  It authenticates with an ordinary Read plus `maintenance_report` key limited to
  the chosen catalog. Its token stays in the controller, never the child process.
- **Evidence collector:** discovers a few neighbors, pins all text to revisions,
  and creates a bounded packet. It never treats search snippets as evidence.
- **Claude adapter:** starts an isolated, tool-free process with the packet on
  stdin and consumes a bounded structured result. There is no model-controlled
  command construction or tool dispatch.
- **Report gate:** accepts only citations to supplied evidence and enforces
  slice-1 categories, size/secret checks, and conservative aging outcomes.
- **Run supervisor/journal:** holds local exclusion, records durable request keys,
  maintains leases, records usage, and recovers interrupted submissions.

The model's configured provider necessarily receives selected catalog text.
`allow_model_upload` defaults false; live invocation requires explicit true.
Documentation must state this privacy boundary. The operator supplies a provider
key via `ANTHROPIC_API_KEY`; `--bare` intentionally does not reuse interactive OAuth
or keychain sign-in. No key is placed in command arguments, TOML examples, or logs.

## MCP and transport contract

At startup, list the server's tools and validate required parameter/result schema
shapes against the implemented slice-1 client contract. Missing or incompatible
tools fail before claiming or invoking a model. Do not hardcode a schema hash from
the still-unimplemented slice-1 service. Test the same client against the real
slice-1 test server before shipping.

Allowed calls: `search`, `get_concept` (discovery links only), `history`,
`maintenance_claim`, `maintenance_renew`, `maintenance_read_revision`,
`maintenance_submit`, `maintenance_close`, and `maintenance_get_run`.
The controller never invokes `maintenance_dispose` or any canonical write tool.
All calls supply the configured catalog (`catalogs: [catalog]` for search).
Catalog-valued fields in responses must match. Wrong catalog or malformed replies
halt the run rather than searching a different catalog.

Require HTTPS, valid certificate verification, an explicit endpoint origin, and no
HTTP redirects; tests may explicitly permit HTTP loopback. Do not log authorization
headers. A 401/403 halts immediately. Each call has a 10-second timeout. A lost
mutation response is an ambiguous commit: preserve and retry the exact request
and idempotency key, never make up a replacement mutation.

## Evidence collection and freshness

Claim one batch, default 10, maximum 50, but configure no more than 10 model calls
in a local run. Use slice 1's lane selection rather than reimplementing scheduling.
Fetch primary content only through `maintenance_read_revision` at the claim pin.
If it is already outdated or deleted, do not investigate an alternative revision
under that lease; stop that item and let the server supersede it.

Discover neighbors from resolved outbound/backlinks on `get_concept`, then lexical
search using the primary title, aliases, and filename tokens. Discovery content is
untrusted and may be newer than the pin. Use it only to choose candidate paths.
Deduplicate and deterministically rank explicit linked paths before lexical hits,
then by path; consider at most 20 candidate paths and include at most four neighbors.
For each, read `history` to obtain a concrete latest revision and then read the pin;
drop missing, nonconcept, wrong-catalog, or already-outdated responses. New edits
after collection remain possible: submission/report validation handles that race.

Hard byte limits: primary document 65,536; each complete neighbor document 16,384;
serialized stdin prompt 131,072. Reject decoded discovery payloads larger than
1,048,576 serialized bytes before application traversal, and set the MCP
transport's raw SSE-event cap to the same value. The existing rmcp JSON-response
path buffers a response before parsing; this is not a hard raw-HTTP allocation
limit. Keep the 10-second RPC timeout and recommend an OS memory limit for the
controller. Replacing the transport solely for a raw JSON cap is out of scope. Exclude oversized neighbors and reduce neighbor count to fit the
prompt. If the whole primary plus fixed instructions does not fit, record an
inconclusive result without model invocation; never hide an omitted primary section
behind a full-document conclusion. All text remains exact UTF-8, retaining CRLF
for citation matching. Scan source text for detectable secrets before upload;
a hit skips upload and records only a generic failure, not the secret itself.

Packets assign immutable local evidence IDs to each pin/document. The model emits
IDs and exact excerpts, not arbitrary catalog paths/revisions. The controller maps
IDs back to pins and verifies excerpt membership and limits. Unknown IDs, invented
excerpts, missing comparison sides, extraneous fields or external observations
invalidate the result; do not repair a citation by guessing its intended source.

The first version cannot establish fresh real-world truth from catalog prose.
It never submits `Assessment::SupportedCurrent`. For changed-only work, valid
connection analysis may complete with `NotAssessed`. For any work including aging,
submit `Inconclusive` with `Unverifiable`, including useful findings and a specific
question where possible; even an internally contradictory old statement is not
proof of present real-world state. A run that found useful aging questions may
therefore be operationally successful while aging work remains inconclusive.
No canonical `verified` or successful-aging-completion timestamp is renewed.
This uses slice 1's existing `Inconclusive`/`Unverifiable` outcome, which permits
findings; its supported-current/outdated restriction applies only to completed
aging work. No slice-1 schema change is needed.
This intentionally trades repeated 24-hour reviews for honest uncertainty; bounded
run capacity prevents an unchecked polling loop. External authoritative checking
is a later, separately designed extension.

The model must distinguish an explicit relation from an inferred one, historical
change from contradiction, and multiple copies from independent evidence.
Model assertions remain proposals even after structural checks pass.

## Process isolation and output

Launch a directly resolved executable, never a shell command. Use a fresh private
working directory and HOME, and an allowlisted environment. Preserve only the
provider key and nonsecret runtime variables required by the executable; remove
MCP keys, database credentials, unrelated provider keys, proxy overrides and user
configuration paths. A controller MCP credential must never be inherited.

Use `--print --output-format json --json-schema <fixed schema> --tools ""`,
`--strict-mcp-config` with an empty MCP configuration, `--bare --restricted`,
`--disable-slash-commands --no-session-persistence --no-chrome`,
`--setting-sources ""`, explicit model and `--max-budget-usd`. The schema, model ID,
and budget are trusted configuration, never source-derived. No bypass-permissions
flag, resume, fallback model, plugins, or helper commands. Send the prompt via
stdin so it does not appear in the process command line.

Tool disabling and a clean environment restrict agent behavior; they are not an
OS sandbox or a network firewall. The CLI still needs provider network access.
Do not promise OS-level isolation; process/container/network hardening can be
applied by the operator, and the disabled service example uses available systemd
filesystem restrictions without claiming an egress allowlist.

Child timeout is 90 seconds, raw stdout limit 1,048,576 bytes, stderr limit 65,536
bytes. Kill and reap the owned child on timeout, oversize output, lost lease or
cancellation; do not kill unrelated processes. Discard raw output after parsing
and validation; no prompt/transcript logs. `tokio::process` and bounded async pipe
readers avoid blocking the lease heartbeat. Child startup/version or flags that do
not satisfy the preflight are fatal before the first claim.

The adapter targets one concrete JSON result envelope: `type: "result"`,
`subtype: "success"`, `is_error: false`, object-valued `structured_output`, finite
nonnegative `total_cost_usd`, and a `usage` object with nonnegative integer
`input_tokens`, `output_tokens`, `cache_creation_input_tokens`, and
`cache_read_input_tokens`. Account for all four token classes; zero cache counts
must be explicit, not assumed. Ignore unrelated envelope fields but reject unknown
fields inside the proposal. Accept no text/Markdown fallback, alternate provider
envelope, or implicit zero accounting. This is a target contract, not an observed
paid response: synthetic tests must be labeled synthetic. Before enabling a live
schedule, an explicitly authorized one-item smoke test must confirm it against the
pinned CLI/model, with sanitized fixture capture and human review. Config requires
an exact `expected_claude_version`; preflight compares version output and rejects
mismatch, but does not claim a free preflight proves inference behavior.
Missing/invalid cost or usage is unknown spending, not zero.
A nonzero exit, invalid result, or unknown accounting stops further model calls;
do not automatically invoke a second model to repair output. Preserve valid work
already submitted. No paid calls occur in unit/integration tests.

## Limits and accounting: guarantees versus thresholds

Defaults proposed for this runner:

| Setting | Default / constraint |
|---|---|
| Batch size | 10, accepted 1..50 |
| Concurrent model processes | exactly 1 |
| Model invocations per run | 10, maximum 50 and no more than batch size |
| Local wall time | 1,800 seconds, maximum 3,000 |
| Per-child wall time | 90 seconds |
| Lease heartbeat | every 60 seconds for every unsubmitted lease |
| Per-invocation spend threshold | USD 0.10 |
| Per-run reserved spend threshold | USD 1.00 |
| Observed token stopping threshold | 100,000, includes reported input/cache/output |
| Automatic model retries | 0 |
| Automatic transport retries | at most 2 after the first attempt, 1 then 2 seconds |

Read the server run deadline after claiming and use the earlier of that deadline
minus 30 seconds and the local wall deadline. Heartbeats include queued leases;
claiming ten items must not let the ninth lease silently expire while waiting.
Maintain independent RPC timeouts and stop before a lease cannot be safely renewed.

Reserve USD 0.10 locally before each invocation; do not launch if another reservation
would exceed USD 1.00. Charge every started invocation against the invocation count,
including failed/unknown-cost calls. Observed usage is validated and aggregated;
when a stop threshold is reached, launch no further calls. Never refund an ambiguous
or failed call's reservation simply because output or accounting was lost.

Byte, process-count, and elapsed-time limits are enforceable locally. Token and
money totals reported after requests are not pre-request hard guarantees: a CLI
may finish an in-flight request above a stopping threshold. The CLI's own budget
flag is another stop control, not evidence that no overshoot can occur. Strict
financial/token ceilings require provider-enforced quotas or an approved enforcing
gateway; this version implements neither.

Live operation requires both `allow_model_upload = true` and
`acknowledge_inflight_budget_overshoot = true`, default false. A configuration asking
for a strict billing/token guarantee is rejected as unsupported before claiming.
This refines the parent design's generic 'budget enforcement' language rather than
pretending existing CLI controls provide stronger guarantees. No unlimited mode.

## Journal, failure recovery, and exclusion

Use an operator-owned state directory (0700, files 0600) with an exclusive local
lock using `std::fs::File::try_lock`, held on a stable lock file for the process
lifetime. Do not replace or unlink the locked inode on completion. A crash releases
the OS lock, not the journal; recovery must still examine it. No locking crate is
needed with the repository's inspected Rust toolchain. A second process for the
same state path exits without claiming. Claim/lease
state is still authoritative on the server, including across machines. Local locks
are a convenience, not distributed safety.

Before each mutation, atomically persist its request ID, exact normalized payload
and pending status. After a successful response, atomically save the receipt and
next state. Bind the journal to endpoint origin, catalog and SHA-256 credential
fingerprint, never the raw bearer token. A changed binding refuses recovery rather
than transferring a run to another credential. The journal holds lease tokens,
selected evidence and pending reports, so treat it as sensitive even without a raw
API key. Detect corruption and nonprivate permissions; fail closed rather than
starting a replacement paid run.

Recovery replays ambiguous pending MCP mutations with their original keys before
new work. If a claim response was lost, replay the original `maintenance_claim`
request to recover its run ID and leases; do not invent a lookup or a second claim.
Read the recovered run before deciding which leases remain actionable. A journal records `invocation_started` durably before spawning a child.
After a crash during inference, its cost is unknown: do not spawn another model for
that item or launch fresh inference in that run. Recover existing submissions,
mark remaining work failed/inconclusive if authorized, then close; a later scheduler
invocation may claim new work under server cooldown rules. A completed local run is
not resumed as a new run until its durable terminal state is recorded.

Renewals are real journaled mutations, not untracked best-effort requests. Serialize
local mutation dispatch while allowing the inference process to run concurrently;
prioritize renewals. Before submission, drain any pending renewal for that item
and use the last acknowledged lease; do not submit concurrently with renewal.
If a renewal response is ambiguous, replay it with its existing key. No submission
or new inference may pass that unresolved mutation. If its acknowledged expiry
cannot be established at least 30 seconds ahead, kill the child and halt for
recovery rather than guessing that authority remains valid. On repeated transport
failure or loss of authority, kill the
owned child, retain recoverable state, and exit nonzero. Do not guess whether an
ambiguous mutation committed or submit under a new lease token.

On clean stop/budget exhaustion, submit bounded failure or inconclusive outcomes
for remaining owned leases, then close the run with a reason if reachable. When
permission or lease ownership is lost, leave recovery/expiry to the server. No
infinite retry loop, no fallback to a catalog checkout or direct database writes.

## Scheduling and operator experience

Provide a disabled systemd oneshot service/timer example for Linux, initially daily
with randomized delay, persistent catch-up, no overlapping local runs, and a
30-minute runtime cap. The service uses a dedicated account, protected credential
files, private state, no repository working tree, and a pinned executable. Do not
copy workstation credentials or install/enable units during development.

`preflight --config PATH` performs configuration, executable capability, credential
presence and MCP contract checks without claims, inference, or report mutations.
`run-once --config PATH` performs at most one batch and exits. `evaluate --cases PATH`
checks recorded proposals against human-authored synthetic annotations offline;
it never calls a provider. A future live evaluation requires explicit authorization.

Normal output is a JSON run summary with run ID, counts, skip/failure reasons and
accounting-known flag; no source body, excerpt, token, prompt, or child stderr.
Differentiate completed-with-inconclusive findings from broken/unsafe execution.
Exit 0 means every selected item reached an acknowledged report outcome and no
runtime/protocol failure occurred; 2 means rejected configuration/preflight;
3 means another local run owns the lock; 4 means partial/failed/recoverable execution.
An empty acknowledged batch is successful. Never equate exit 0 with factual currency.

## Acceptance and validation

1. Run against real slice-1 MCP tools with a fake inference process; no paid test.
   Missing/changed contracts fail before a claim or model process starts.
2. Verify every allowed MCP request is single-catalog, and no canonical mutation,
   disposition, network-fetch tool, or direct SQL path exists in the runner.
3. Malicious instructions and source URLs remain data; model proposals cannot add
   evidence IDs, alter pins, invoke tools, or obtain the MCP credential.
4. Preserve exact UTF-8/CRLF excerpts and reject wrong IDs, invented citations,
   oversized inputs/outputs, secrets, malformed envelopes, and unknown accounting.
5. Aged concepts never receive SupportedCurrent from this version; changed-only
   reports cannot manufacture successful aging verification.
6. Fake-clock/process tests cover queued-lease heartbeat, timeout, cancellation,
   stdout/stderr caps, expired/replaced lease, revocation and ambiguous submissions.
7. Crash/restart tests cover each journal boundary; lost inference accounting never
   causes an automatic paid retry or resets a spending reservation.
8. Gate on a human-reviewed synthetic corpus: valid/invalid links, similar-name
   distinct entities, copied assertions, historical changes, actual contradictions,
   unverifiable aging information and injected instructions. Offline evaluation
   reports precision/recall and false verification separately; no invented quality
   claim or live-provider benchmark in this implementation.
9. All existing canonical rows, revisions, search and bundle export remain unchanged
   across successful, failed, inconclusive and recovered maintenance runs.
10. Run repository formatting, clippy, rustdoc and test gates. Document actual tested
    CLI envelope/version fixtures and the limits of budget/process restrictions.

## Builder discretion and approval boundary

Builders choose local helper names and test fixture mechanics, not permission,
accounting, evidence or freshness policies. The implementation plan will pin module
paths and function interfaces. Adding external verification, another provider,
strong billing enforcement, a container deployment, or an enabled schedule requires
a new design/approval boundary. No commit, push, PR, deployment, or paid inference
is authorized by writing these documents.
