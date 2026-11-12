# M12 Task 11: OAuth consent as the signed-in user

## Added
- `GET /oauth/authorize` without a session sends the person to `/signin?return_to=<the authorize URL>`, after the client, redirect URI, response type, PKCE and resource are validated and before anything is stored. Signed in, it shows the person's ceiling as a checkbox and a level select per catalog (only the levels the ceiling allows), with a default catalog.
- Approving creates a self-issued `oauth` grant (`store::oauth::grants::create_self_issued`) named `<user>.<client host>-<8 hex>`, with the client's name as actor. The key cache caps it by the ceiling on every reload.
- "Use a sign-in code instead" (`signin=code`) on the consent page, and on `/signin` when it returns to an authorize URL.
- `store::ceiling::ceiling_in`, one user's ceiling read inside a transaction.
- A browser journey: sign in on the way, approve from the ceiling, land back at the client.

## Changed
- The sign-in code path is reached with `signin=code`. It is otherwise unchanged, and its grants stay admin-issued and uncapped.
- Every consent POST is refused with 403 when the browser says it came from another site (`Sec-Fetch-Site`), as the sign-in forms are.
- `console_session::signed_in_user` lost its `expect(dead_code)`: the consent page uses it.

## Notes
- The POST checks the CSRF token, spends the page's consent nonce under the pending map's lock (one Approve per page), checks the cookie still belongs to the user the page was shown to, and refuses anything the page did not offer.
- The grant insert locks the user's row, then the approving session's, and re-reads the ceiling in that transaction: a sign-out, reset, admin-flag change or delete either waits for it or leaves no grant, and a ceiling lowered meanwhile refuses it.
- No `Debug` of a consent type prints a secret: `UserPage`, `PageView`, `UserConsent`, `ConsentForm` and `NewSelfIssuedGrant` redact their nonce, pending id, CSRF token, sign-in code or session id, each with a test.
