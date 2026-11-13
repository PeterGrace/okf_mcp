# M12 Task 15: console admin controls for sign-in, member access and Settings

## Added
- A user's drawer gains Sign-in (email with Save and Clear, the password state, the Admin switch, Send setup email with the reason it is unavailable), Entitlements (add, change level, remove), Console sessions (sign one out, or everywhere) and Recent sign-ins (`components/user-account.js`, reusing Task 14a's `sessionTable` and `signInTable`).
- The Users list shows each user's email and an `admin` pill.
- A catalog's drawer gains Team member access (`none`, `read`, `write`), read from and saved to `/api/v1/catalogs/{catalog}/member-access`.
- `pages/settings.js`, linked under Administer: the policy settings with their effective value and source (default, or override by whom and when), Save and Reset; the configured OIDC providers and the mail provider and From address, read-only; Send test email; the mail queue (recipient, template, status, attempts, next try, last error) with Retry and Delete.
- Journeys: five in `users.rs` (one for the last-admin refusal), two in `catalogs.rs`, four in the new `tests/ui_e2e/settings.rs`; the smoke journey visits Settings.

## Changed
- After every account change the user drawer reads the user again and re-renders through its own ticket, so each control shows the server's state (a refused admin switch springs back) and a late answer never replaces a newer drawer.
- `.settings-input`, and inputs inside `.inline-form` lose their margin.
- `core/pending.js` (new): writes in flight by a stable key (`guard`, `once`, `recheck`). A control drawn again while its write runs (a re-sorted table, a Refresh, a reopened drawer) is drawn disabled, and a second start of a running write is ignored. `sessionTable` (so the Account page too), the user drawer, member access and Settings use it (review of PR #349, rounds 1 and 2).
- When the read that follows an action fails, the user drawer, the catalog drawer's member access and the Settings tables are drawn again from the last answer and marked out of date (`#user-stale`, `#settings-stale`, `#mail-queue-stale`), so a refusal's reason still shows and every control works; (review of PR #349).
- `tests/ui_e2e/harness.rs` gains `Ui::hold_next`, `held_matches`, `release_held` (moved from `keys.rs`), `fail_next`, `click_twice`, `mark`, `is_marked` and `wait_redrawn`.

## Notes
- Any admin reads these; changing them needs an admin that reaches every catalog, as in M11. For a scoped admin every control renders disabled with the reason.
- A refusal (the last-admin 409, a setting value refused with 422 `limit: "setting"`, a 403) is toasted and its reason also stays beside the control until that control's next success.
- The console never receives a secret: the API sends no client secret, SMTP password, Postmark token or mail body, and the queue never shows a link.
- The plan's Task 15 carries Corrected and Superseded notes for each divergence (the mail job's recipient path, the scoped-admin entitlement check, the shipped module shapes).
