# M14 task 06c: round-one snapshot-mismatch coverage

## Added

- Deterministic owner-change and grant-set-change regressions for credential
  snapshot refusal. Each test holds the acting user's row and observes the
  pending revoke blocked by that exact backend in `lock_people`, then commits
  the competing change with a different actor before releasing the holder.
- Assertions for the exact retry refusal, unchanged revocation/capabilities,
  preserved competing owner or grants, no revoke audit entry, and exactly one
  entry and generation increment from the competing change. Synchronization,
  competing changes, completion and cancellation waits are bounded.

## Changed

- Test coverage only; production comparison and APIs are unchanged.

## Notes

- Addresses PR #424 inline review comment 4573483999.
- Negative controls independently omitted each half of the comparison. Omitting
  the owner comparison failed only the owner regression; omitting the grant
  comparison failed only the grant regression. Each failed because the revoke
  returned success instead of the required retry refusal (one passed, one failed
  per run, gate exit 101). Production source was restored byte-for-byte before
  the final gates.
- Retained patches, exact source/test hashes, commands, exit statuses and logs:
  `target/i398-logs/r1/negative-{owner,grants}.patch`,
  `negative-{owner,grants}-provenance.log`, `negative-{owner,grants}.log`,
  `snapshot-tests.patch`, and `restoration-provenance.log`.
- These tests exercise both comparison branches, not every possible concurrent
  interleaving. No UI or production behavior changed.
- Implemented by GPT-6.1 Terra.
