# Milestone 12: user login (epic #308)

People sign in to the web console as themselves, with a password, a passkey
or a linked OIDC account. They manage their own API keys and OAuth grants
within a ceiling an admin sets (team member access plus per-user
entitlements), and approve OAuth consent as themselves. Mail (setup and
reset links, notices) goes through a job queue in Postgres. Admins manage
all of it from the CLI, the REST API and the console, at parity. Closes #298
and #299. Design: `docs/superpowers/specs/2026-11-12-okf-user-login-design.md`;
plan: `docs/superpowers/plans/2026-11-12-okf-mcp-m12-user-login.md` (PR #305).

## Added

- **Task 1, schema (#309, PR #328):** migration `0010_user_login.sql`,
  apalis' schema set up by `db::migrate`,
  `OkfError::{Unauthenticated, Gone, LimitExceeded}`, the new dependencies.
- **Task 2, per-catalog levels (#310, PR #329):** `Reach::Catalogs` with a
  level per catalog, `key_catalogs.access`, `api_keys.expires_at` and
  `self_issued`, expiry at lookup.
- **Task 3, ceiling (#311, PR #330):** `store::ceiling` (team member access,
  entitlements, `cap`), capping self-issued credentials in the key cache.
- **Task 4, users (#312, PR #331):** email, admin flag and argon2id
  password; `store::passwords`, `store::email_tokens`,
  `store::sign_in_events`; durable lockout.
- **Task 5, settings and sessions (#313, PR #332):** `store::settings` over
  `[auth]` defaults; `store::user_sessions`.
- **Task 6, mail (#314, PR #333):** the `Mailer` trait (SMTP, Postmark,
  log), templates, the apalis `SendMail` job with durable retry, the queue.
  PR #338 (issue #335) made its two-worker test deterministic.
- **Task 7a, console sessions (#315, PR #334):** `http::pages`, the keyed
  limiter and client addresses, `require_principal` (bearer or cookie plus
  CSRF), `CredentialKind::Session`, `GET` and `DELETE /api/v1/session`.
- **Task 7b, sign-in pages (#316, PR #337):** `/signin` with password,
  setup, forgot and reset pages, sign-in events, the console's minimal
  session support and the first browser journey.
- **Task 8, passkeys (#317, PR #342):** WebAuthn registration and
  discoverable sign-in.
- **Task 9, OIDC (#318, PR #341):** `[[auth.oidc]]`, the provider registry,
  linking by verified email and from the account page.
- **Task 10a, self-service keys and grants (#319, PR #336):**
  `GET /api/v1/me`, `/api/v1/me/keys` and `/api/v1/me/grants`, with every
  limit enforced.
- **Task 10b, self-service account (#320, PR #344):** password, passkeys,
  identities, sessions and sign-ins under `/api/v1/me`.
- **Task 11, consent as the user (#321, PR #340):** `/oauth/authorize`
  sends people to `/signin`; consent within the ceiling; self-issued
  grants.
- **Task 12, admin REST (#322, PR #346):** a route for every admin action
  (users, member access, settings, mail) and their API reference rows.
- **Task 13, CLI (#323, PR #348):**
  `user set-admin|set-email|invite|entitle|unentitle|sessions|revoke-sessions|sign-ins`,
  `catalog set-member-access`, `settings list|set|reset`,
  `mail test|queue|retry|delete`, with their parity rows and test.
- **Task 14a, console (user side) (#324, PR #345):** account sessions in the
  shell, My keys. It also fixed #302 (a stray `null` on four pages).
- **Task 14b, console (user side) (#325, PR #347):** My grants, Account.
- **Task 15, console (admin side) (#326, PR #349):** the user drawer's
  sign-in controls, entitlements, sessions and sign-ins; Team member access;
  the Settings page and mail queue.
- **Task 16, documentation (#327):** "Signing in and self-service", the
  configuration rows, the rest of the API reference (87 endpoints), the
  README and the spec's "What shipped differently".
- **Agent tooling (PRs #343 and #350):** `tools/agentic/` (`gate.sh` with a
  cross-worktree lock and hard timeouts, `watch-review.sh`,
  `merge-reviewed.sh`, `watchdog.sh`, the shared brief and `lessons.md`)
  and `AGENTS.md`, pointed at by every agent's instructions.

## Changed

- `/api/v1` accepts a console session (with `X-OKF-CSRF` on writes) beside
  bearer credentials; `/mcp` and the archive routes stay bearer-only.
- `whoami` answers for a session (`kind: "session"`).
- OAuth consent signs the person in first and caps the grant at their
  ceiling; sign-in codes still work, uncapped.
- The console's pasted-key card is one of two ways in; a live session wins
  over a stored key.
- The request log line names the matched route template, never the literal
  path or query.
- The database needs the trusted extensions `citext`, `pgcrypto` and
  `hstore`, and the mail worker opens up to 4 connections beside
  `database_max_connections`.

## Review findings worth knowing

Each became a rule in `tools/agentic/lessons.md`, so later tasks followed it
from the start:

- **Lock the user row first.** Every transaction that writes a user's
  tokens, sessions, passkeys or identities locks the user's row first; an
  early `redeem_in` locked the token first and could deadlock (PR #337).
- **A credential change and its consequences commit together, and nothing
  runs after the commit.** A reset and its session revocation, a sign-in
  and its event, a new method and its notice mail are one transaction, so
  a blocked table can never leave a committed sign-in without its cookie
  (PRs #337, #341, #342).
- **Every session insert is guarded against a concurrent credential
  change.** A password sign-in carries the hash it matched into the insert;
  passkey, OIDC and setup sign-ins start their session in the transaction
  that checked the credential (PRs #337, #341, #342).
- **The admin level comes from the key cache on every request**, never from
  the sign-in (PR #334).
- **A limiter keyed by client text must find-or-create in one step**, and
  every per-actor map is bounded per actor (PRs #334, #341, #342).
- **Text Postgres refuses never reaches SQL**: a NUL in an email, a link
  token, a `sub` or a path name is the "unknown" answer, never a 500
  (PRs #337, #341, #346; #339 for the older routes).
- **Login CSRF:** every form POST checks `Sec-Fetch-Site`, since `Origin`
  is `null` under `no-referrer` (PR #337).
- **The console sends one credential and treats only 2xx or 401 as signed
  out**; a refusal that is not "signed out" must not be a 401 (the 422 for
  a bad passkey credential, the 403 for a wrong current password; PRs
  #344, #347).
- **Console writes are guarded by a stable key, and only the newest read
  draws** (PR #349).
- **A hung test stalled a task for 12 hours**, which led to `gate.sh`'s hard
  timeouts and to timeouts on every race test (PR #343).

## Notes

- Migration `0010` only adds: existing keys, grants, codes and catalogs are
  unchanged, every existing key is admin-issued (uncapped), and every
  catalog's member access is `write`. `docs/operations.md`, "Rollout", has
  the first steps after deploying.
- The `log` mail provider (the default) logs whole messages, links
  included: development only.
- What differs from the design is in the spec's "What shipped differently".
- Follow-ups:
  - #352 team owners manage member access and entitlements;
  - #353 OIDC providers configured in the database;
  - #354 multi-factor authentication for password sign-in;
  - #355 self-service account deletion (after #297);
  - #356 move the background cleanup loops onto apalis;
  - #357 rate limits shared across replicas;
  - #339 admin routes answer 500 for a NUL byte in a name path segment;
  - #351 the users, teams and catalogs tables draw overlapping loads in
    arrival order.
