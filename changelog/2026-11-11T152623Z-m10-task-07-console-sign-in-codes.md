# M10 Task 7: Console sign-in codes panel and OAuth grant display

## Added
- A "Sign-in codes" section on the API keys page (`#codes-panel`): "New
  sign-in code" mints one (name, actor, read or write access, catalogs,
  default, expiry of 1 hour to 7 days) and shows the `okfsc_` code once in a
  modal; the list shows live codes, or every state with
  `#codes-show-all`; a live or expired code's drawer revokes it.
- OAuth grants are marked in the key list (an `OAuth` pill and the client's
  name) and in their drawer (kind "OAuth grant", the client, its own revoke
  warning).
- Two browser journeys in `tests/ui_e2e/keys.rs`:
  `an_admin_mints_a_sign_in_code_sees_it_once_and_revokes_it` and
  `an_oauth_grant_is_marked_and_cannot_be_given_reporting`.
  The first also covers a refused mint (no catalog) and its retry, and
  that Escape leaves the code dialog open.
- A paragraph on the console's codes section in `docs/operations.md`
  ("OAuth sign-in codes").

## Changed
- The one-time secret dialog (`showOnce({ prefix, ... })`) and the catalog
  fields (`catalogFields`, `readGrants`) are shared by the key and code forms;
  the key form's selectors are unchanged.
- An OAuth grant's drawer has no reporting toggle (the server refuses it).
- The keys page's helpers check `main.isConnected` after each await before
  touching the drawer or reloading.

## Notes
- The code lives only in the dialog: it is never logged or stored in the
  browser, and the dialog drops it (text and the copy handler's reference)
  when "I have stored it" closes it. Escape does not close it.
- Plan corrections are in a dated "Corrected (issue #251)" note under Task 7.
