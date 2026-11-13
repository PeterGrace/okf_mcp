# M12 Task 10b: self-service account routes

## Added
- `PUT /api/v1/me/password`; `GET /me/passkeys`, `PATCH` and `DELETE /me/passkeys/{id}`; `GET /me/identities` (with the configured providers), `DELETE /me/identities/{provider}`; `GET /me/sessions`, `DELETE /me/sessions/{id_prefix}`; `GET /me/sign-ins` (the 50 newest). Console sessions only: a bearer key or OAuth token is a 403, and another user's passkey, identity or session is a 404.
- `store::passwords::change_from_session_in` and `SessionChange` (redacted `Debug`): a signed-in password change, in the caller's transaction.
- `config::is_provider_id`, shared by config validation and `identities::unlink`.

## Changed
- `user_sessions::revoke_by_prefix` refuses (409) a prefix that matches more than one of the user's sessions, and ends none.
- `identities::unlink` answers 404 for a malformed provider (a NUL included) before any query, and the identity's 404 for an unknown user.
- `http::passkeys::passkey_name` is `pub(crate)`, so a rename checks the same name rule as registration.

## Notes
- A password change locks the user's row, checks that the asking session is still in force and that the stored password is still the one the current password proved, sets the new hash, ends the other sessions unless `keep_sessions`, and queues the notice, all in one transaction. Hashing happens before it opens.
- A wrong current password, a locked account or a password changed meanwhile is a 403, never a 401, so the console does not sign out.
- The last sign-in method cannot be removed or unlinked (409); the count runs under the user's row lock, so two concurrent removals cannot both pass.
