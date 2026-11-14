# okf-mcp operator API (milestone 14): design

Status: draft for approval, 2026-11-13. Milestone 15 ("teams run
themselves") follows it and is out of scope here.

## Intent

okf-mcp is increasingly run by an operator for other people: a company
for its staff, or a hosting provider for its customers. An operator's own
tooling (a signup flow, an offboarding script, a compliance review) needs to
drive okf-mcp safely and leave a trail. This milestone adds the
general-purpose operator features that makes possible. Every feature here is
useful to a self-hoster with no outside tooling at all, and none of it knows
about payment, plans or any particular operator.

## Goals

1. **Branding:** the product name people see in email and on the server's
   pages is the operator's, not "okf".
2. **Find a user by email** without listing every user.
3. **Delete a user in one call.** Their account and every credential they
   ever had go; the catalogs their teams own, and those catalogs' history,
   stay. This replaces #297's "disable, never delete".
4. **A narrow key for automation** (a "provisioner") that can create and
   remove accounts, teams and catalogs, and suspend teams, but can never
   read a catalog, change settings or make anyone an admin.
5. **A stored audit log** of administrative changes, catalog writes,
   exports and admin reads of other people's data, readable by the
   operator in full and by each team's owners for their team.
6. **Unlock before access (break-glass):** on a server that turns it on,
   operator staff can't open a customer's catalog until they unlock it,
   with a reason and a time limit; the unlock is recorded in full and the
   team's owners are told. People's memories are their own data, and the
   operator wants to show it treats their privacy very seriously.

## Non-goals

- Team owners managing members themselves (#352), seat counts and
  membership-change notifications for outside systems: milestone 15.
- Self-service account deletion (#355): milestone 15 or later, though
  Goal 3 makes it a thin layer.
- Recording ordinary members' reads. A future opt-in "read accounting"
  setting may add it; the event model leaves room (see "Audit log").
- Shipping audit entries to an outside log service (a SIEM, a cloud log
  store). Entries are queryable and exportable; streaming can come later.
- Multiple replicas (unchanged: single replica).

## Anti-goals (a failure even if it works)

- **A change with no audit entry.** Entries are written in the same
  database transaction as the change they record, or the change fails.
- **Deleting a person deletes a memory.** User deletion never deletes a
  catalog; it is refused while the user's personal team still owns one.
- **A provisioner key that can read data.** It reaches no catalog
  contents by construction, not by convention.
- **Secrets in the audit log.** No key, token, password, link or session id,
  ever; only ids and prefixes already shown elsewhere.
- **Operator-specific code.** No product names, URLs or payment concepts in
  okf-mcp.

## Approaches considered

**Where audit entries live**
- **A. A table in okf-mcp's Postgres, written in the change's own
  transaction (chosen by the founder).** Nothing changes without its entry,
  per-team queries are cheap, and self-hosters get it with nothing else to
  run. Streaming to an outside service can be added on top.
- **B. Structured log lines only, collected by an outside log service.**
  Least work, but lines can be lost, team owners can't query them through
  okf-mcp, and self-hosters get nothing. Rejected.

**The narrow automation key**
- **A. A `provisioner` flag on a key, like `maintenance_report`, allowing a
  fixed list of administrative routes (chosen).** Matches how the platform
  works: it creates catalogs for teams that don't exist yet, so per-catalog
  scoping can't express it.
- **B. Admin access scoped to some teams' catalogs.** Fits today's access
  model, but creating a catalog for a new team needs every-catalog admin
  anyway. Rejected.

**Removing a person**
- **A. Real deletion of the account and its credentials; catalogs and
  their history stay (chosen).** The catalog, owned by a team, owns what was
  written in it; history keeps the "who wrote it" label it recorded.
- **B. #297's "disable": revoke everything, keep the user row and its email
  forever.** Rejected: it keeps personal data the operator has promised to
  delete, and the founder doesn't need the record kept.

## Decided by the founder (2026-11-13)

- Two milestones: this one (operator side), then 15 (teams run
  themselves).
- A person isn't what owns data: a team owns its catalogs, and catalogs own
  their history. Deleting a user removes the account, not contributions.
- Audit log: approach A. Record administrative changes, **catalog writes**,
  **every export**, and **admin reads of other people's data**. Don't
  record members' ordinary reads; read accounting may come later as an
  opt-in, configured feature.
- **Team owners can read their own team's audit entries**, for customers'
  own compliance needs.
- Provisioner: approach A. It is a **server-wide** flag on a key (an
  "admin lite" for automation), not a per-team role and not a user role.
  It may manage **every user except admin users**, and every team and
  catalog (Part 4), but never read catalog contents (2026-11-13, after the
  builder check). Accepted consequence: whoever holds a provisioner key can
  take over any non-admin account (set its email, send a setup link), so
  the key is protected like an admin key and every use is audited.
- **Audit entries don't identify deleted people** (2026-11-13): entries
  refer to people through a separate people table; deleting a user
  replaces their row with "deleted user #N". Email addresses never go into
  entries. Client IPs are kept for the retention period as a security
  record.
- **Unlock first** (2026-11-13): operator staff must unlock a customer's
  catalog, giving a reason, before reading it (Part 6).
- **Operator access is recorded in full, shown to customers as "an
  operator"** (2026-11-13): entries store the staff member's identity and
  IP; a team owner reading the log sees "an operator" with no identity or
  IP, while admins see everything.

## Part 1: branding

- Two new **settings** (the existing `server_settings` mechanism, so they
  can be changed from the CLI, API and console without a restart):
  - `product_name`, default `okf`: used in every email subject and body, the
    console's page title and header, and the server-rendered sign-in,
    setup, reset and consent pages.
  - `support_contact`, default unset: an email address or URL. When set,
    every email ends with a line "Questions? Contact <it>", and the sign-in
    pages show it.
- Every hard-coded "okf" a person can see (today: `src/mail/templates.rs`,
  `src/http/ui/index.html`, `src/http/pages.rs`) reads the setting.
- The mail sender (`mail.from`) stays configuration, as today.
- Validation: `product_name` 1 to 60 characters, no control characters;
  `support_contact` a valid email address or `https://` URL.

## Part 2: find a user by email

- `GET /api/v1/users?email=<address>`: the users whose email matches,
  ignoring letter case (0 or 1, since emails are unique in any case).
  Same view as the list. Admin or provisioner.
- CLI: `okf-mcp user list --email ADDRESS`.
- Console: the Users page's filter box matches emails too (no new control).

## Part 3: deleting a user

`okf-mcp user delete NAME` and `DELETE /api/v1/users/{name}` keep their
names; what they may delete changes.

**Deleted, in one transaction:**
- the user row and its email;
- every key, OAuth grant and sign-in code naming the user, **revoked ones
  included**, with their tokens and catalog grants (cascades that already
  exist);
- sign-in methods, passkeys, linked OIDC identities, sessions, email
  tokens, entitlements and sign-in events (already cascade);
- memberships in shared teams;
- the personal team.

**Changed:** open changesets of the deleted keys are **discarded** in the
same transaction (their `key_id` would otherwise go null and leave them
open with no owner); each is recorded in the audit log.

**Kept:** every catalog and its whole revision history, including the
`actor` text each revision recorded, and every audit entry (which store
names, not foreign keys).

**Refused (409), with the reason:**
- the user's personal team owns a catalog (the message names them: delete
  them or move them to another team first);
- the user is the last owner of a shared team (add another owner first);
- the user is the last admin user (only the CLI may, as today for
  clearing the admin flag).

**Schema:** the `ON DELETE RESTRICT` references to `users` from
`api_keys.user_id`, `sign_in_codes.user_id` and `team_members.user_id`
become `ON DELETE CASCADE`, and the code's guards replace the database's
refusal with the three rules above. `teams.personal_of` stays `RESTRICT`;
deletion removes the personal team explicitly after checking it owns
nothing.

This closes #297 (its goal, a way to retire a person, is met by deletion;
the disable variant isn't built). #296 (make credential user and catalog
owner `NOT NULL`) is unaffected.

## Part 4: the provisioner key

- A key flag, `provisioner` (`api_keys.provisioner boolean NOT NULL DEFAULT
  false`), set only at creation: `okf-mcp key create NAME --provisioner`,
  or `POST /api/v1/keys` with `"provisioner": true` by an every-catalog
  admin. Never self-issued, never an OAuth grant.
- A provisioner key **reaches no catalog** (its grant list is empty and the
  schema refuses grants on it), so every catalog read, write and export is
  refused by the existing checks, and it is not an MCP credential.
- It may call exactly these routes, which today need an every-catalog admin
  (or any admin, for the reads):
  - `GET /api/v1/users?email=`, `GET /api/v1/users/{name}`,
    `POST /api/v1/users`, `PUT /api/v1/users/{name}/email`,
    `POST /api/v1/users/{name}/invite`, `DELETE /api/v1/users/{name}`;
  - `GET /api/v1/teams/{name}`, `POST /api/v1/teams`,
    `PUT /api/v1/teams/{name}/members/{user}`,
    `DELETE /api/v1/teams/{name}/members/{user}`,
    `DELETE /api/v1/teams/{name}`, `PUT /api/v1/teams/{name}/suspension`
    (milestone 13);
  - `POST /api/v1/catalogs`, `DELETE /api/v1/catalogs/{catalog}`,
    `PUT /api/v1/catalogs/{catalog}/member-access`;
  - `GET /api/v1/audit` (to confirm its own actions).
- It may **not**: act on an admin user in any way (each allowed handler
  refuses a target that is an admin, 403), list every user or team, read
  or export catalogs, manage keys or grants, change settings, set the admin
  flag, entitle users, import, or end other people's sessions.
- It is server-wide, not scoped to teams: it may act on any non-admin user,
  any team and any catalog through the routes above.
- One check function decides, from an explicit allow-list of route
  templates, so adding a route never widens a provisioner by accident.
- Everything it does is audited with its key as the actor.

## Part 5: the audit log

### What is recorded

| Group | Actions |
|---|---|
| Users | `user.created`, `user.deleted`, `user.email_changed`, `user.admin_changed`, `user.invited`, `user.entitlement_set`, `user.entitlement_removed`, `user.sessions_revoked` |
| Teams | `team.created`, `team.deleted`, `team.member_added`, `team.role_changed`, `team.member_removed`, `team.suspended`, `team.unsuspended` |
| Catalogs | `catalog.created`, `catalog.deleted`, `catalog.owner_changed`, `catalog.member_access_changed`, `catalog.description_changed`, `catalog.imported` |
| Credentials | `key.created`, `key.revoked`, `key.catalog_granted`, `key.catalog_ungranted`, `key.user_assigned`, `key.report_flag_changed`, `oauth.code_created`, `oauth.code_revoked`, `oauth.grant_created` (consent), `oauth.grant_revoked` |
| Settings | `setting.changed`, `setting.cleared` |
| Catalog writes | `catalog.published`, `catalog.reverted`, `catalog.changeset_discarded` (by an admin, or by user deletion). Moves, splits and deletes of concepts are publishes and are recorded as such. |
| Data access | `catalog.exported` (every export, by anyone); `catalog.admin_read` (below) |

- A **catalog write** entry names the catalog, the changeset handle, the
  number of files and the revision range; the diff stays in the catalog's
  history, linked, not copied.
- **Admin reads of other people's data:** a request by an `admin`
  credential (key, grant or admin console session) that reads a catalog
  whose owner team the credential's user is not a member of (a legacy key
  with no user always counts). Recorded as at most **one entry per
  credential, per catalog identity and owning team, per clock hour**,
  carrying a request count, so the count is exact and the log stays
  readable. A catalog moved to another team, or deleted and created again
  under the same name, starts a new entry within the hour, scoped to the
  team that owns it then. *(Corrected, PR #375 review, 2026-11-13: the
  bucket was per catalog name.)*
  *(Corrected, PR #375 review, 2026-11-13.)* Who read, which catalog and
  its owning team are fixed when the read is admitted: a read just before
  a transfer stays with the old owner, a read of a catalog deleted or
  replaced before the record is written keeps the original catalog, and
  the record is written only with references fixed at that moment, so it
  can never re-create anything a user's deletion removed.
- **Not recorded:** reads by anyone else; sign-ins (already in
  `sign_in_events`, which the audit view links to); maintenance runner
  reports (already in their own tables).
- Read accounting, if ever added, would be a new action
  (`catalog.read`) behind a setting; nothing in the schema assumes it
  won't exist.

### People in entries

Entries refer to a person (actor or target) by an id in a separate
`audit_people` table holding the name and display name as last known.
Deleting a user replaces its row's name with `deleted user #<id>` in the
deletion's transaction, so entries stay intact and append-only but stop
identifying the person. Email addresses are never written into entries or
`details` (an email change records only that it changed).

> **Corrected (PR #375 review, 2026-11-13).** No column of an entry ever
> holds a person's name or a reusable name: the actor label is a kind
> ("console session", "api key", "oauth grant", the CLI's operating-system
> user, or the system), never a session's user name or a user's key label;
> credentials are referred to through an `audit_credentials` table whose id
> and label are cleared when their user is deleted (a legacy key's id was
> its name); and teams through an `audit_teams` table keyed by the team's
> immutable id (below). See the M14 plan's Decisions 16 to 18.
>
> **Corrected (PR #375 review, 2026-11-13).** A credential's person is
> taken from the credential tables (its key's or code's user) while it
> exists, never only from the change that names it, so a credential from
> before the audit log that is only ever a target is still cleared when
> its user is deleted (plan Decision 19).

### An entry

- `at` (server clock), `action`;
- **actor:** the key or grant (through `audit_credentials`), the person
  (through `audit_people`), and a non-identifying actor label; `source` (`cli`, `api`, `console`, `system`); the client
  IP for web requests;
- **target:** kind and name (user, team, catalog, key, setting);
- **teams:** the teams the entry belongs to, for scoping, by their
  immutable identity (an `audit_teams` id per team id, never reused), with
  the name kept only as a display snapshot. *(Corrected, PR #375 review,
  2026-11-13: scoping by name let a team created again under an old name
  read the old team's entries.)*
  *My call, after the builder check:* only where the event happened, never
  a user's other teams. For a catalog event (writes, exports, admin reads,
  access changes, an entitlement on it), its owner team at the time; for a
  team event (membership, roles, suspension), that team; for a key or
  grant event, the owner teams of the catalogs it is granted. User events
  that aren't about one team (`user.created`, `user.deleted`,
  `user.email_changed`, `user.admin_changed`, `user.invited`,
  `user.sessions_revoked`) belong to no team: operator only. So on a
  server shared by several customers, one customer's owners never see
  another customer's events about a shared person;
- `details`: a small JSON object specific to the action (old and new
  values for a change, counts for a write or an admin read). Never a
  secret.

Entries keep no foreign key to users, teams, keys or catalogs (only to the
audit tables), so deleting one never changes or removes its entries.

### Integrity and retention

- Written in the **same transaction** as the change. The CLI writes
  entries too (`source = cli`, actor the operating-system user name).
- **Append-only:** no route or command edits or deletes entries. A
  database trigger refuses `UPDATE`, and refuses `DELETE` except of
  entries older than the retention period.
- **Retention:** a new setting `audit_retention_days`, default **400**
  (auditors usually look back a year), minimum 30. A daily job deletes
  older entries.

### Who reads it, and how

- **Admins** (every-catalog) read everything. **A team's owners** read the
  entries whose `teams` include that team, **through a signed-in console
  session only** (*my call:* not through their keys, which are often handed
  to AI clients with narrower purposes). Members who aren't owners don't.
  A provisioner reads only its own entries.
- **Operator staff, as owners see them:** an entry whose actor is an admin
  credential, or a provisioner, is shown to owners as actor "an operator",
  with no key id, user name or IP. Admins see the full entry. The stored
  entry is never reduced; the redaction happens when owners read it.
  Owners never see client IPs of anyone but their own team's members.
- *(Corrected, PR #375 review, 2026-11-13.)* The same holds for operator
  staff as a **target**: an entry whose target person is an admin user
  (such as the admin an unlock is for) shows owners no person and no
  credential naming them, and owners' actor and target filters never match
  an operator, so they cannot be used to find out who staff are.
- `GET /api/v1/audit?team=&action=&actor=&target=&since=&until=&after=&limit=`,
  newest first, cursor-paged (`after`).
- `GET /api/v1/audit/export?...` with the same filters: JSON Lines, for a
  customer's compliance request.
- CLI: `okf-mcp audit list` and `okf-mcp audit export` (same filters).
- Console: an **Audit** page (admins: everything; owners: a team picker
  limited to the teams they own), with the filters and an Export button.

## Part 6: unlock before access

### Who needs an unlock

A new setting, `require_unlock`, default **off** (a self-hoster running a
server for themselves is unaffected); a hosted operator turns it on. When
it is on, a credential needs an active unlock to read or write the
**contents** of a catalog unless its user reaches the catalog as a
customer:
- the user is a member of the catalog's owner team, or
- the user is **not an admin** and holds an entitlement to the catalog.

Everything else is operator access and needs an unlock: admin keys and
admin console sessions, keys with no user, and any key an admin issued
reaching a catalog outside its user's teams. An admin entitling themselves,
or minting an all-catalogs read key, gains nothing without an unlock.

"Contents" means everything that reads or writes a catalog's documents:
MCP tools, REST browse, search, history, health and reports, changesets,
publish, **export** and import. It does not cover catalog metadata (the
catalog list, names, counts, owner team, member access) or administrative
actions that don't read contents (deleting a catalog, suspending its team,
managing users and keys); those are audited as usual. A provisioner never
reads contents, so it never needs one.

**The maintenance runner is exempt, per catalog (founder, 2026-11-13):**
it works on the customer's behalf, so it needs no unlock, but each catalog
can opt out.
- A new per-catalog flag, `maintenance` (`on` or `off`), **default `on`**
  (*my call:* the product promises catalogs that check themselves for
  stale, bloated and broken notes).
- A **maintenance credential** is a non-admin key with the existing
  `maintenance_report` flag. On a catalog with `maintenance` on, it reaches
  the catalog without an unlock (reads and maintenance reports only, as
  its access level already limits). On a catalog with `maintenance` off,
  it is refused (`Forbidden`) whatever its grants, and the runner skips
  that catalog.
- An admin key with the flag gets no exemption: the exemption is for the
  runner, not for staff.
- Who sets it: an admin, **or an owner of the catalog's owner team**
  (signed-in console session): `okf-mcp catalog set-maintenance C on|off`,
  `PUT /api/v1/catalogs/{catalog}/maintenance` with `{"maintenance":
  "on"|"off"}`, and a switch on the catalog in the console. This is the
  one owner-managed control in this milestone (the rest is milestone 15).
- Recorded: `catalog.maintenance_changed` (who, old and new value), and one
  `catalog.maintenance_run` entry per runner pass over a catalog (when,
  findings reported), visible to the team's owners as "maintenance ran".
  The runner's individual reads aren't recorded.
- The flag applies whether or not `require_unlock` is on: an owner who
  opts out keeps the runner out on any server.

### An unlock

- Made by an **admin user**, for **one catalog**, with a **reason
  category** (`support`, `deletion_request`, `export_request`,
  `security_investigation`, `legal`, `other`), a free-text **reason** (for
  example a ticket number; required) and a **duration**, at most the
  setting `unlock_max_hours` (default 4).
- It applies to that admin user's credentials (console session and their
  own keys). A key with no user can't be unlocked: give it a user.
- It ends at its expiry, or earlier when ended by its admin or any admin.
  It can't be extended; a new unlock is a new record.
- Surfaces: `okf-mcp unlock create --catalog C --user U --category K
  --reason TEXT [--hours N]`, `okf-mcp unlock list`, `okf-mcp unlock end
  ID`; `POST /api/v1/unlocks`, `GET /api/v1/unlocks`,
  `DELETE /api/v1/unlocks/{id}`; in the console, an **Unlock** button with a
  short form wherever a locked catalog is refused, and an Unlocks page.
- Creating and ending an unlock bump `admin_generation`; the key cache
  holds the active unlocks, and expiry is checked at each request (like
  key expiry), so an expired unlock stops working at once.
- The CLI itself works on the database directly, as today; its export and
  import are audited (`source = cli`) but don't need an unlock. Access to
  the server's command line is governed by the operator's infrastructure
  controls, not by okf-mcp.

### The refusal

A new error kind, `UnlockRequired` (403), for operator access without an
active unlock: `{"error": "UnlockRequired", "message", "catalog"}`, with a
message that says how to unlock. It is checked after reach, level and
suspension (milestone 13), and only on a catalog the credential reaches.
*(Qualified, milestone 13 merged, 2026-11-14; confirmed by the founder,
2026-11-14: as milestone 13 shipped
suspension, a tool that picks its catalog through the selection before
checking its own level (`open_changeset`, `maintenance_dispose`, the
maintenance work tools) refuses a locked catalog at the selection, so a
credential below that tool's level gets `UnlockRequired` rather than
`Forbidden`. Reach is still checked first everywhere.)*

### Recording and telling the customer

- `catalog.unlocked`: the admin's identity, IP, category, reason, start
  and expiry, in full. `catalog.unlock_ended`: who ended it and when.
- Reads and writes under an unlock are tallied on the unlock (counts per
  kind of request), shown with it; writes are also recorded as
  `catalog.published` and so on, as for anyone. On a server with
  `require_unlock` off, the hourly `catalog.admin_read` entries (Part 5)
  record operator reads instead.
- **Team owners see** each unlock of their team's catalogs as "an operator
  opened <catalog>: <category>, <start> to <end>, N reads, M writes". They
  see the category, never the free-text reason (which can hold internal
  ticket details), the staff member or the IP.
- *My call:* when an unlock is created, the owner team's owners get an
  email ("An operator opened <catalog> for <category> until <time>"), on by
  default with `require_unlock` (setting `unlock_notify_owners`). It goes
  through the existing mail queue, with the branding of Part 1.

## Surfaces and parity

Every new CLI subcommand has a REST route and a console control, and goes
in `docs/operations.md`'s parity table (`tests/admin_parity.rs` enforces it):
`settings set product_name|support_contact|audit_retention_days|require_unlock|unlock_max_hours|unlock_notify_owners`,
`user list --email`, `key create --provisioner`, `audit list`,
`audit export`, `unlock create`, `unlock list`, `unlock end`,
`catalog set-maintenance`.

## Testing

- Branding: every template and page uses the setting; defaults unchanged.
- Lookup: case-insensitive match, none, provisioner allowed, a plain key
  refused.
- Deletion: a user with revoked and live keys, grants and codes is deleted;
  a catalog's history keeps its actor text; open changesets are discarded
  and audited; the three refusals; concurrent deletion and key creation
  (the existing lock order).
- Provisioner: every allowed route works; every other admin route and every
  catalog read, write and export is refused; `key create --provisioner`
  can't be combined with grants.
- Maintenance: with the flag on, the runner's key reads a customer catalog
  without an unlock; with it off, the runner is refused and skips it; an
  owner can switch it, a plain member can't; an admin key with the report
  flag still needs an unlock; each pass writes one `maintenance_run` entry.
- Unlock: with `require_unlock` off nothing changes; with it on, an admin,
  an all-catalogs read key, a self-entitled admin and a user-less key are
  each refused with `UnlockRequired`, and a member and an entitled
  non-admin are not; an unlock lets exactly its user into exactly its
  catalog until it expires or ends; export needs it; owners see the
  redacted record and get the email; the tallies are exact.
- Audit: each action writes exactly one entry, in the same transaction (a
  forced failure leaves neither); scoping for owners, members and
  provisioners; admin-read bucketing and counts; the trigger refuses
  `UPDATE` and early `DELETE`; retention pruning; export format.

## Order relative to milestone 13

Milestone 13 (team suspension) has merged (2026-11-14), as planned. This
milestone's migrations start at `0012`, the provisioner's allow-list
includes `PUT /api/v1/teams/{name}/suspension`, and `team.suspended` and
`team.unsuspended` are instrumented here. The plan was checked against what
milestone 13 shipped ("Verified facts" and its `Corrected (M13 merged)`
notes).

## Delivery order (for the plan)

1. Audit log storage and the write helper (everything else records into
   it).
2. Branding settings.
3. Email lookup.
4. User deletion.
5. Provisioner key.
6. Recording: administrative changes, then catalog writes, exports and
   admin reads.
7. Audit reading: REST, CLI, console, export.
8. Unlock before access: the setting, unlocks, the check, the console
   flow, owner notification.
9. Docs and close-out.

## Left to the builder

- Table and index layout, the trigger's exact form, the admin-read bucket
  mechanism (within "one entry per credential, catalog and hour, exact
  count").
- How store functions receive the audit actor (an argument, a context
  struct), as long as every write path records in its own transaction.
- Exact `details` fields per action.
- Console layout of the Audit page.

## My calls (to check)

- `product_name` and `support_contact` as runtime settings, not
  configuration.
- The provisioner's exact allow-list (Part 4), including team deletion and
  member access.
- Discarding a deleted user's open changesets (rather than leaving them
  ownerless).
- Refusing deletion while the personal team owns catalogs.
- Admin reads bucketed per credential, catalog and hour.
- 400-day default retention, 30-day minimum.
- Team owners, but not plain members, read their team's audit entries.
- The CLI's audit actor is the operating-system user name.
- `require_unlock` off by default (self-hosters unaffected); 4-hour
  maximum unlock; the six reason categories.
- Who counts as a customer (member of the owner team, or entitled
  non-admin); everything else needs an unlock.
- The maintenance runner's per-catalog flag defaults to on; owners may
  switch it; admin keys with the report flag are not exempt.
- Owners see the category but not the free-text reason; owners are
  emailed when an unlock starts.
