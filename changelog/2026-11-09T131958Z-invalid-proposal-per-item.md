# Maintenance runner: a rejected proposal fails only its own item

Issue #205. Maintenance run `c80785b314c83014507d190b22e72f8a` (2026-11-09)
ended with `reason: invalid_proposal`, two items failed, and one model call.
The model's proposal for `/as18470-network/fios-uplink.md` had five
findings, and every excerpt was an exact substring of its source. One was a
`conflicting_assertions` finding whose two excerpts both came from `s0`.
`validate::submission` requires comparison findings to cite two different
concept paths, so the whole proposal was rejected. Then the run stopped, and
the second item was never sent to the model.

## Changed

- `Controller::record_model` (`src/investigator/supervisor.rs`) still tags a
  rejected proposal `invalid_proposal` and keeps its usage. It no longer
  returns `RunnerError::InvalidProposal`. `submit` already sends the generic
  failed submission for that tag, so the item is reported as failed and the
  loop goes on to the next item. The observed-budget check still runs after
  every result, so spend limits are unchanged.
- The recovery pre-check in `Controller::execute` used to turn any stored
  per-item failure into a run stop. It now skips `invalid_proposal`, so a
  journal resumed after a crash finishes the remaining items too. Every other
  stored failure (`unknown_accounting`, `budget`, `child` and the rest) still
  stops the run.
- `INSTRUCTIONS` (`src/investigator/collect.rs`) now names the three
  comparison categories. It says each one must cite excerpts from at least two
  sources with different paths, and that a tension inside one concept should
  be reported as `unverifiable_claim` or `potentially_outdated_information`.

A run where a proposal was rejected now ends with reason `completed` and
counts that item in `failed`. `RunSummary::exit_code` is still 4 whenever
`failed > 0`, so the CronJob still reports the failure.

## Tests

- New: `investigator_supervisor_invalid_proposal_fails_only_its_item`. Item
  1's proposal is rejected and item 2's is valid. It checks that both items
  are invoked, item 1 is submitted as failed and item 2 as completed, and the
  run reason is `completed`.
- New: `investigator_supervisor_recovered_invalid_proposal_is_not_a_run_stop`.
  A journal is resumed with a rejected proposal already stored. It checks that
  the item is submitted as failed and the run closes `completed`. With the
  recovery filter removed, this test fails.
- New: `investigator_prompt_states_the_comparison_rule`
  (`tests/investigator_collection.rs`).
- Updated to match the new behaviour:
  `investigator_supervisor_known_invalid_proposal_preserves_usage` (both
  items invoked), `investigator_supervisor_invalid_second_result_never_erases_prior_usage`
  (three invocations), and the `invalid` mode of
  `investigator_aging_cancel_and_failed_validation_preserve_canonical`
  (reason `completed`).

## Not changed

- When the model's structured output cannot be decoded at all,
  `claude.rs` still returns `InvalidProposal` before any usage is recorded.
  The supervisor treats that as `unknown_accounting`, which still stops the
  run.
- Keeping a proposal's valid findings while dropping the invalid ones is
  left to a separate decision (out of scope in #205).
