# M12 Task 4: Users gain email, admin flag and password; emailed links and sign-in events

## Added
- `store::users::{set_email, set_admin, find_by_email, find_auth}` and `UserAuth`. `UserRecord`, and the API's user object, gain `email`, `is_admin` and `has_password`.
- `store::passwords`:
  - the policy: 12 to 1024 characters, and not in the bundled `common_passwords.txt`;
  - argon2id hashing at OWASP's parameters;
  - `verify` (by email) and `verify_user` (by id), with a durable lockout (10 consecutive failures lock the account for 15 minutes) and a constant dummy hash for unknown emails.
- `store::email_tokens`: single-use setup and reset tokens, stored as SHA-256 only. A new token replaces the user's older one of the same purpose.
- `store::sign_in_events`: the sign-in audit (`record`, which also logs at `info`; `recent`; `prune`).
- `clock::after`: now plus a std duration, checked.

## Changed
- `App::poll`'s hourly prune also deletes sign-in events older than 90 days, and emailed links a week past their expiry.
- Changing a user's email drops its outstanding emailed links. A change of letter case alone is not a change.
- Changing a user's admin flag bumps `admin_generation` and ends the user's console sessions. `set_admin` is the only code that writes the flag.
- `argon2` and `blake2` build optimized in the dev profile, so the password tests run in seconds.

## Notes
- Clearing the last admin user is refused unless the caller passes `allow_last` (the CLI does; Decision 16). Two concurrent clears of the last two admins leave one.
- Every password outcome costs one argon2 verification, including a locked account and an unknown email.
- Concurrency rules, each with a test:
  - concurrent failures all count;
  - a lock set while a correct password is being checked still holds;
  - a password changed during the check fails the old one;
  - one emailed link redeemed at once from several places works exactly once;
  - concurrent issues leave one live link.
- No password, hash or raw token reaches a log line, an error message or a `Debug` output. The row type that holds the hash has no `Debug`.
- Plan corrections (issue #312) are in Task 4's Steps 4, 8 and 9.
