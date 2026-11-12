# M12 Task 7a: console sessions over HTTP

## Added
- `okf_session` cookie sessions on `/api/v1` (`http::console_session::require_principal`): a bearer credential wins and the cookie is then never read; a cookie write (anything but GET, HEAD, OPTIONS) needs `X-OKF-CSRF` equal to the session's token, compared in constant time, or it is 403; an ended, revoked or unknown session is 401 at once and its cookie is cleared.
- `GET /api/v1/session` (user, email, admin, CSRF token, ceiling, expiry) and `DELETE /api/v1/session` (sign out: the session is deleted, the cookie cleared). A key or grant gets 403 from both.
- `CredentialKind::Session` principals (`Principal::for_session`), built on every request from the key cache, which now holds every user (`users::auth_by_id`, `KeyCache::with_users`, `Keys::user_admin`). An admin user's session acts as an admin key on every catalog; anyone else's reaches exactly their ceiling, at its levels.
- Per-address and per-email limiters (`http::limits::KeyedLimiter`, `SignInLimits` on `App`, `ip_key`), bounded at 100,000 keys each; client address from the peer, or the right-most hop of `[auth] trusted_proxy_header` when (and only when) it is set.
- `http::pages` (the shared document, `page`, `scripted_page`, `escape`, `safe_return_to`); `console_session::signed_in_user` for routes outside `/api/v1`; a placeholder `src/http/ui/signin.css`.
- The cookie-aware `Browser` test helper (`session_for`, `signed_in`, `Payload`, `Reply::parts`), `login_app` and `user_with_password`.

## Changed
- `consent::document`, `page` and `escape` moved to `http::pages`; `oauth::consent` re-exports `escape` for `mail::templates`.
- `http::serve` records each request's peer address (`ConnectInfo`).
- `users::set_email` bumps `admin_generation` when the address changes, so the cached email stays current.
- `open_changeset` refuses a console session (403): `changesets.key_id` references `api_keys`.
- `ExpiringMap` tracks a lower bound on its expiries, so a full map refuses a new key without scanning every entry.
- `ip_key` groups IPv6 addresses by /64.

## Notes
- No sign-in page yet (Task 7b); tests start sessions directly.
- `/mcp` and `/catalogs/{name}/export|import` stay bearer-only (`require_key`). The `/api/v1` export and import routes accept a session at its levels, as Task 14a's console needs.
- A session's admin flag and ceiling are as current as the key cache: a change is seen at the next reload (the API polls right after its own changes), and an admin-flag change also ends the user's sessions at once (Decision 20).
- No session id, CSRF token or password reaches a log line or a `Debug` output.
