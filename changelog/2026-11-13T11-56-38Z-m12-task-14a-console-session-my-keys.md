# M12 Task 14a: account sessions in the console shell, and My keys

## Added
- The login card's main button "Sign in with your account" (`#login-signin`), whose `return_to` brings the person back to the console page they asked for.
- A "Your account" nav group (`data-need="session"`) for people signed in with an account: My keys now; My grants and Account arrive in Task 14b (the manifest hides their links until then).
- `pages/my-keys.js`: the person's keys, with levels per catalog (capped by the ceiling for self-issued keys), expiry and state; New key within the ceiling, with a level per catalog and an expiry within `max_key_lifetime`, and its secret shown once; rename and revoke. New key is disabled, with the reason, at the key limit or with an empty ceiling. Signed in with a key, the page says it needs an account.
- `components/secret.js` (`showOnce`, moved from `keys.js`), `components/levels.js` (`ACCESS_KIND`, `accessPill`, `levelsCell`), `components/activity.js` (`sessionTable`, `signInTable`, for Tasks 14b and 15).
- `core/dom.js`: `fill(el, ...children)` and `fact(label, value)`. `h()` and `fill()` share one child filter that drops `null`, `undefined` and `false`.
- `core/shell.js`: `requireSession` for the account pages.
- `levelsCell` shows a key granted every catalog as "All catalogs" with its level (review, PR #345).
- My keys reads the ceiling from each `GET /me` and stores it in `store.session.ceiling`, so a ceiling an admin lowered mid-session shows in its cap notes, drawer and New key form without a reload (review, PR #345).
- Journeys: `tests/ui_e2e/self_service.rs` (11, including the lifetime-limit expiry choices, an admin-issued all-catalog key and a ceiling lowered mid-session), `users::no_page_prints_a_stray_null_under_its_content`, and `Ui::wait_text_gone`.

## Changed
- `core/store.js`: `store.session` (the whole `GET /api/v1/session` answer, memory only) beside `store.csrf`. `store.can(level)` uses the ceiling's level on the selected catalog for a member's session.
- `core/shell.js`: `refreshIdentity` reads the session again beside `whoami`, and applies `data-need` once the catalog is settled.
- `main.js`: boot, sign-out, the 401 handler and key sign-in also keep or forget `store.session`. `showLogin` sets the card's `return_to`.
- `core/api.js`: exports `CSRF_HEADER`, which `authHeaders` uses.
- `index.html`: the Administer group starts hidden, and stays hidden for a member's session. An admin user's session sees the full console.
- Issue #302: the Users, Teams, Catalogs and Import & export pages no longer print a stray `null` (they passed a conditional `null` to the DOM's native `append`).

## Notes
- A stored key still wins when it is the only credential. A live session found at boot replaces it (Task 7b).
- Hiding Administer is a convenience only: the API refuses a member's session on admin routes with 403, which the member journey checks.
- A new key's secret lives only in its dialog. Once the dialog closes, it is in neither the DOM nor storage, which the create journey checks.
- Each new journey was checked to fail with its feature reverted: the `null` filter, the `data-need` check, secret removal, the CSRF header, the `return_to` line and the catalog-aware `can`.
