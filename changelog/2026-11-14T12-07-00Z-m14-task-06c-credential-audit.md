# M14 task 06c: credential-change audit

## Added

- Transactional audit entries for key creation/revocation, catalog grants,
  reporting flags, user assignment, sign-in code lifecycle and OAuth grants.
- Regression coverage for no-ops, refusals, audit-write rollback, legacy-target
  anonymization, eager new-credential references and bounded locking behavior.

## Changed

- Credential writers take the caller and server clock; CLI, REST, self-service,
  consent and all test fixtures use those signatures. Code redemption keeps its
  API and records the system OAuth actor; consent records the approving session.
- Credential changes lock all resolved people before credential rows and the
  generation row, rechecking owner/grant snapshots rather than acquiring a newly
  discovered user out of order. Consent retains its stronger guarded-insert lock.
- Updated operations documentation and the authoritative Task 6c plan.

## Notes

- No migration renames or backfill: migrations 0014 and 0015 are unchanged.
  New creation transactions resolve their references eagerly; an absent legacy
  reference can still wait on concurrent first resolution. Existing-reference
  refreshes are skip-locked, not every possible resolver operation.
- Assignment to the existing user is a no-op, including the cache generation.
- Removing the revocation record made the required legacy-target privacy test
  fail; the record was restored before validation.
- The provisioner every-use design gap (#423), publish/delete (#418), and
  catalog-owner snapshot work (#421) remain outside this change.
- Implemented by GPT-6.1 Terra; no claims ledger is used.
