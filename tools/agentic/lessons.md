# Lessons from earlier reviews (read before you write code)

Each rule below records a real bug the review bot found in an earlier PR, mostly while building user login (milestone 12). Your code must follow every rule that applies to it, and a test must prove it does. When a review finds a new class of bug, add a rule here in the same PR.

## Locks and races
- **Lock order is always: the user row first** (`FOR NO KEY UPDATE`, or `FOR SHARE` for a guarded insert), then the token, session, passkey or identity rows. Read a child row's `user_id` without a lock, lock the user, then re-check the child under the lock. The rule is documented in `src/store/email_tokens.rs`'s module docs.
- **A credential change and its consequences go in one transaction.** Redeeming a link, setting a password or passkey, and revoking sessions all commit together. Hash before you open the transaction, never inside it.
- **Every session insert is guarded against a concurrent credential change.** A password sign-in uses the stamped path (`passwords::Verified { stamp }`, then `user_sessions::start` with `password: Some(stamp)`). Other methods must not create a session that survives a reset or admin change. Ask: if a reset or `set_admin` commits between my check and my insert, does the session still appear? If so, fix it. Test that interleaving deterministically: hold the user row, queue both operations, and watch `pg_stat_activity`.
- **After a commit, await nothing that can block or fail before returning the response:** record success events in the transaction; side effects are in-transaction or detached best-effort.
- **Maps keyed by a client-chosen value** (IP, email, ceremony id) use `ExpiringMap::update_or_insert_sliding` (find-or-create plus change under one lock), never `get` followed by `insert`.
- **Admin level and ceiling come from the key cache on every request**, never from a value read at sign-in.

- **Every race test that holds a lock or waits on another task has a timeout** (`tokio::time::timeout`, about 30 s) and fails with a message naming what it waited for. A hung test once stalled a task for 12 hours. Run gates through `tools/agentic/gate.sh`, which enforces a hard timeout. Release held locks on every path, including assertion failures (use a guard or drop the connection).

## Inputs and errors
- **Normalize user text before it reaches SQL.** Postgres rejects NUL, so a NUL becomes a 500. Use `users::normalize_email` and `users::is_lookup_email`. A malformed value must take the same path as "unknown", with the same status, the same body and the same cost.
- **Every refused sign-in looks identical:** the same 401 text, one argon2 check, and no hint whether the account exists.
- **Genuine database errors stay 500.** Don't turn them into 4xx.
- **Bounded state per actor.** When any client, signed in or not, can add entries to a shared in-memory map (flows, ceremonies, pending authorizations), limit each actor: by address for anonymous starts, by user or session for signed-in ones. A new entry from the same browser or session must replace its previous one instead of stranding it. Test that one actor's repeated starts never grow the map and that others can still start.

## Browser-facing pages and routes
- **Every state-changing form POST on a server-rendered page** must call the cross-site check in `src/http/signin.rs` (`Sec-Fetch-Site` must be `same-origin` or `none`). This blocks login CSRF. Don't use `Origin`: with `Referrer-Policy: no-referrer`, browsers send `Origin: null`.
- **Cookie-authenticated `/api/v1` writes need `X-OKF-CSRF`.** In the console JS, every request, including raw fetch and XHR uploads, builds its headers through `authHeaders` in `core/api.js`.
- **`return_to` goes through `pages::safe_return_to`.** Test hostile values: `//evil.example`, `https://evil.example`, `/\evil.example`, `javascript:`, and their encoded forms.
- **Request traces log the path only.** Never put a secret in a URL's query if you can avoid it. When it can't be avoided (emailed links), it must still never be logged.
- **No secret reaches a log line or `Debug`:** passwords, tokens, session ids, CSRF tokens, WebAuthn or OIDC state, verifiers, nonces. Give such types a hand-written `Debug`.

## Console behaviour
- **The console uses one credential at a time:** a live cookie session wins over a stored key.
- **Sign-out counts as done only on a 2xx, or a 401** meaning the session was already over. Any other failure keeps the session and shows a retryable error.
- **A browser journey for a write path** must actually perform the write and check the persisted result. It must fail if the header or feature it covers is removed; check that by temporarily reverting the code.

## Process
- **Find the whole class.** When you fix one path, grep for every sibling path with the same pattern and fix those too.
- **Tests must fail without the fix.** Verify that by temporarily reverting the code, and say so in your report. After restoring a reverted file, `touch` it (or restore with `git checkout`, which sets a new modification time). Otherwise cargo can reuse the test binary built from the reverted copy and report stale results.
- **The PR description and claims must match the final code** exactly. Update them after every review round.
