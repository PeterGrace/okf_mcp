# M12 Task 10a: self-service keys and grants

## Added
- `GET /api/v1/me`: the caller's account (user, email, admin flag, password set, teams, ceiling, `password_sign_in`, the key limits). It answers a console session or a key that belongs to a user; a key with no user gets 404.
- `/api/v1/me/keys` (`GET` lists, `POST` creates; `PATCH /me/keys/{id}` renames, `DELETE` revokes) and `/api/v1/me/grants` (`GET` lists; `DELETE /me/grants/{id}` revokes), for console sessions only. A bearer key or OAuth token gets 403; another user's key or grant is a 404, worded like an unknown id.
- `store::keys::create_self_issued` (the live-key count runs under a `FOR UPDATE` lock on the user row) and `store::keys::rename_by_id`.

## Changed
- `store::keys::create_with_id` now wraps a shared `insert_key`, which holds all of its checks and inserts.

## Notes
- Self-issued keys are never admin or maintenance reporters and always name their catalogs, each within the user's ceiling. At most `max_keys_per_user` are live, and each expires at most `max_key_lifetime` from now (and at that limit by default when it is set). Each refusal is a 422 `LimitExceeded` naming the limit (`ceiling`, `max_keys_per_user`, `max_key_lifetime`). The key belongs to the signed-in user, and a body naming `user`, `all` or `maintenance_report` is a 422.
- Admin users are capped like everyone else for their own keys: the ceiling comes from teams and entitlements, never from the admin flag.
- The limits come from `app.settings()`, so a changed setting applies at the next poll without a restart.
- The new key's secret is in the create answer only. No list, rename or revoke answer holds it, and the log line names only the key id.
