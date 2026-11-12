# M12 Task 5: `[auth]` config, policy settings and console sessions

## Added
- `[auth]` in the config file: `password_sign_in`, `max_keys_per_user`, `max_key_lifetime`, `console_session_idle`, `console_session_absolute`, `setup_link_ttl`, `reset_link_ttl` (the policy defaults) and `trusted_proxy_header`, validated by `Config::validate`.
- `store::settings`: `load`, `set` (validated per key; bumps `admin_generation`), `reset` and `list` over `server_settings`.
- `store::user_sessions`: console sessions stored by SHA-256 of a 256-bit id, with idle and absolute expiry, `last_seen_at` written at most once per `min(1 minute, idle / 4)` (once even under concurrent requests), revoke, revoke-all with an exception, listing by 12-hex-character id prefix, revoke by prefix, and prune.
- `App::settings()`: the effective settings.

## Changed
- `App` loads the effective settings at start and reloads them with the key cache whenever `admin_generation` moves, so every replica sees a settings change within one poll interval. A failed reload keeps the old settings and the counter, so the next pass retries.
- The hourly prune also deletes console sessions past their absolute limit.
- `users::set_admin` ends the user's console sessions through `user_sessions::revoke_all` (in its transaction), so the session store owns all of its SQL.
- `Config::validate` checks the `[oauth]` lifetimes with the same `check_auth_duration` as `[auth]`.

## Notes
- A stored setting this server cannot read (from a newer version or a hand edit) is logged at `warn` and the config default used; it never stops the server.
- The CSRF token is derived from the session id (SHA-256 under its own label), so `user_sessions` stores no secret: `csrf_token` holds only the token's hash. Neither raw value appears in a `Debug` output.
- `touch`'s one write re-checks the absolute and idle limits and the write interval under the row lock, so a concurrent request can never extend a session that has been revoked or has ended, and a burst of requests writes once. A touch that writes nothing re-reads the row, so it never signs out a live session.
- The write resolution follows the idle limit, so a short `console_session_idle` (such as `30s`) still keeps an active session alive.
- The idle limit has one write resolution of grace, because a use too soon after the last write is not recorded: a session is never ended sooner than `console_session_idle` after its last use, and always by `console_session_idle` plus the resolution (12 h 1 min with the defaults).
- Console session settings are `console_session_*`, apart from MCP's `session_idle_timeout` (Decision 9).
- The plan's Task 5 carries a Corrected (issue #313) note for these differences.
