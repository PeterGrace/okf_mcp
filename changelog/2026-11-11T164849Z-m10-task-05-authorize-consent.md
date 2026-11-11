# M10 Task 5: authorize and the consent page

## Added
- `GET /oauth/authorize`: validates the client, the exact redirect URI, then `response_type=code`, PKCE S256 and `resource`, in that order. Errors before the redirect URI is trusted render an error page and never redirect; later ones go back to the client (303) with `error`, `state` and `iss`.
- `POST /oauth/authorize`: the consent form. Step 1 takes the sign-in code (checked with `find_live`, not consumed), step 2 states what will be granted, and Approve redeems the code into an OAuth grant (`codes::redeem`) and redirects with a one-time `okfac_` code (303, 60 s). Deny redirects with `access_denied`. A CSRF token bound to the pending request, the consent rate limit, and five wrong codes end the request. Each step-2 summary carries its own nonce, stored with the code it shows (`PendingAuthorization.accepted`); Approve redeems only the code of the summary it came from, so an older summary cannot approve a code entered later (PR #259 review).
- The consent page: server-rendered, no script, no inline style, the console's look, following the system theme (`src/http/ui/consent.css`). Every page carries a per-request CSP (`default-src 'none'`, `form-action 'self' <redirect origin>`, `frame-ancestors 'none'`), `no-store`, `no-referrer` and `nosniff`. Client-supplied text goes through `oauth::consent::escape`.
- `oauth::pkce`: S256 challenge checks and constant-time verification, with the RFC 7636 vector and property tests.
- `src/oauth/testing.rs`: shared test helpers for Tasks 5 and 6.
- `TestServer::start_with` in `tests/support/server.rs` (Task 6, Step 7's version), and the browser journey `tests/ui_e2e/consent.rs`.

## Changed
- `PendingAuthorization.sign_in_code_id` becomes `accepted: Option<AcceptedCode { code_id, summary_nonce }>`.
- `tests/support/server.rs`: `serve_app` binds before loading the app, so settings can name the server's own URL.

## Notes
- The consent CSP's `form-action` names the redirect URI's origin, since browsers apply it to the redirect after the POST. Verified: with the origin removed, Chromium blocks the hand-off and the journey times out waiting for the client page. The CSP also allows the console's Google Fonts hosts (a deviation from the spec's `style-src 'self'`).
- `consent.css` re-maps the console tokens onto Pico's variables under `:root:not([data-theme])`, which matches Pico's own theme selectors in specificity; `styles.css`'s plain `:root` mapping loses to Pico when no `data-theme` is set.
