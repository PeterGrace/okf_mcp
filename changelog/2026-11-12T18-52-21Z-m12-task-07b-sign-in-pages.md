# M12 Task 7b: sign-in pages and the console session

## Added
- `/signin` (email and password), `/signin/forgot`, `/account/setup` and `/account/reset` (`src/http/signin.rs`), server-rendered with the console CSP and no script; without `public_url` only `GET /signin` exists, offering "Use an API key". Every refused password sign-in answers the same 401 text after one argon2 check; a malformed post takes that path too. Attempts are recorded as sign-in events. `/signin` is limited per address (30 per 15 minutes), and `/signin/forgot` per address (20 an hour) and per email (3 an hour), with one answer for known and unknown emails. Setup and reset links are single-use and expire; a reset ends every session of the user and mails the "a new password" notice.
- The form posts refuse a browser that says it came from another site (`Sec-Fetch-Site`), so another site cannot sign a browser in as someone else.
- `sign_in`, `audit` and `notify_method_added` for Tasks 8 to 10b.
- The sign-in journeys (`tests/ui_e2e/signin.rs`), `TestServer::user_with_password` and `Ui::sign_in_password`.

## Changed
- The console resumes a cookie session at boot (`GET /api/v1/session`), sends `X-OKF-CSRF` on writes, and signs out over `DELETE /api/v1/session`; the login card links to `/signin`; `#whoami` shows a session as `<user> - <level>`.
- The request trace span records the path without the query, so an emailed link's token never reaches the log.
- `pages::safe_return_to`, `is_safe_return_to` and `DEFAULT_RETURN_TO` lose their `expect(dead_code)` attributes.

## Notes
- Emailed links are built by Task 6's `mail::link` with `SETUP_PATH` and `RESET_PATH`.
- The plan's Task 7b carries a dated Corrected note for each place the code differs.
