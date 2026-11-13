# M12 Task 14b: My grants and Account

## Added
- `pages/my-grants.js`: the person's live OAuth grants, with levels per catalog (capped by the ceiling for self-issued ones) and where each came from (their consent or an admin's sign-in code); each grant's drawer revokes it (`DELETE /api/v1/me/grants/{id}`).
- `pages/account.js`: profile and the read-only ceiling; password set or change (`PUT /api/v1/me/password`), with "Keep me signed in elsewhere"; passkeys: add through Task 8's `register/start`, `createCredential` and `register/finish`, rename, remove; linked identities: link through `POST /api/v1/me/identities/{provider}/link`, unlink; console sessions, each with its own Sign out, this browser's included; recent sign-ins.
- `core/shell.js`: `freshAccount()` (GET `/me`, and its ceiling becomes `store.session.ceiling`), and `setSignedOutHandler` / `signedOut()`, the one way the console leaves a session it ended itself.
- `components/activity.js`: `sessionTable` takes `revokeCurrent`, which gives the current session a Sign out too.
- Journeys: `tests/ui_e2e/account.rs` (10: password change, wrong current password, mismatched passwords, a passkey added with Chromium's virtual authenticator, rename, remove and a cancelled add, the last sign-in method kept, signing other sessions out, signing this browser out, unlinking an identity with the ceiling and sign-ins, and the key's view) and `self_service::a_member_revokes_an_oauth_grant`.

## Changed
- `pages/my-keys.js` reads `/me` through `freshAccount()`; its local `ceilingMap` is gone.
- `main.js`: the Sign out button's clean-up is `signedOut()`, registered with the shell, so the Account page's "sign this browser out" ends the same way.
- `self_service::a_member_signs_in_from_the_card_and_sees_only_their_pages` now expects the My grants and Account links.

## Notes
- Every value Account and My grants show comes from the server's answer on that load, never from the sign-in snapshot: both journeys change the ceiling mid-session and check the page follows.
- A wrong current password is a 403 (Task 10b). The page shows the server's reason and stays signed in. A refused finish (403 for another session's ceremony, 409, 410) shows its reason too.
- A cancelled or timed-out passkey prompt is an `info` toast ("No passkey was added"), not an error.
- There is no "sign out everywhere else" for one's own sessions: each session has its own Sign out, and the spec asks for no more.
- Each write journey was checked to fail with its feature reverted: signing this browser out, the quiet password refusal, the calm cancel toast, the finish call, and My grants' fresh ceiling.
- The plan's Task 14b has a `Corrected (issue #325)` note on these differences.
