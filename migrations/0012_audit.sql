-- The audit log (spec 2026-11-13-okf-operator-api-design.md, Part 5).
-- Entries are written in the same transaction as the change they record,
-- never updated, and deleted only past the retention period. People are
-- referred to through audit_people, so deleting a user renames one row
-- ("deleted user #N") and leaves every entry intact. Every timestamp is
-- written by the server from its clock.

CREATE TABLE audit_people (
    id           bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- The user while it exists; NULL once deleted. No foreign key: deleting
    -- a user must never touch the log.
    user_id      text UNIQUE,
    name         text NOT NULL,
    display_name text NOT NULL DEFAULT ''
);

-- Teams as entries refer to them: by this id, never by name. teams.id is a
-- UUID that is never reused, so a team deleted and created again under the
-- same name gets a new row here, and its new owners never see the old
-- team's entries. `name` is a display snapshot of a shared team; a
-- personal team (whose name is its user's) is shown through its person.
CREATE TABLE audit_teams (
    id        bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    team_id   text NOT NULL UNIQUE,
    name      text,
    person_id bigint REFERENCES audit_people (id),
    CHECK ((name IS NULL) <> (person_id IS NULL))
);

-- Credentials (keys, grants, sign-in codes) as entries refer to them. The
-- id and the label are mutable, so deleting a user clears them: a legacy
-- key's id was its name, and a self-issued key's label defaults to a name
-- its user chose.
CREATE TABLE audit_credentials (
    id            bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    credential_id text UNIQUE,
    person_id     bigint REFERENCES audit_people (id),
    label         text
);
CREATE INDEX audit_credentials_person_id ON audit_credentials (person_id);

CREATE TABLE audit_entries (
    id                    bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    at                    timestamptz NOT NULL,
    action                text NOT NULL CHECK (action ~ '^[a-z]+\.[a-z_]+$'),
    source                text NOT NULL CHECK (source IN ('cli', 'api', 'console', 'system')),
    actor_credential_ref  bigint REFERENCES audit_credentials (id),
    actor_person_id       bigint REFERENCES audit_people (id),
    -- Never a person's name: 'console session', 'api key', 'oauth grant',
    -- 'cli:<os user>', 'system:<what>', or the label of a key with no user.
    -- A credential's own label is in audit_credentials.
    actor_label           text NOT NULL,
    -- An admin credential or a provisioner: owners see "an operator".
    actor_operator        boolean NOT NULL,
    client_ip             inet,
    target_kind           text NOT NULL CHECK (target_kind IN
                              ('user', 'team', 'catalog', 'key', 'grant', 'code', 'setting', 'unlock')),
    -- Only for a catalog, setting or unlock target; a team target is in
    -- target_team_ref, a credential in target_credential_ref, a user in
    -- target_person_id.
    target_name           text,
    target_team_ref       bigint REFERENCES audit_teams (id),
    target_credential_ref bigint REFERENCES audit_credentials (id),
    target_person_id      bigint REFERENCES audit_people (id),
    -- The target person was an admin user when the entry was written:
    -- operator staff, hidden from team owners.
    target_operator       boolean NOT NULL DEFAULT false,
    -- The audit_teams ids the entry belongs to (the scope of an owner's view).
    teams                 bigint[] NOT NULL DEFAULT '{}',
    details               jsonb NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(details) = 'object'),
    CONSTRAINT audit_entries_target_shape CHECK (
        CASE target_kind
            WHEN 'user' THEN target_person_id IS NOT NULL AND target_name IS NULL
            WHEN 'team' THEN target_team_ref IS NOT NULL AND target_name IS NULL
            WHEN 'key' THEN target_credential_ref IS NOT NULL AND target_name IS NULL
            WHEN 'grant' THEN target_credential_ref IS NOT NULL AND target_name IS NULL
            WHEN 'code' THEN target_credential_ref IS NOT NULL AND target_name IS NULL
            ELSE target_name IS NOT NULL
        END
    )
);
-- Newest first is `ORDER BY id DESC` (the primary key); these serve the
-- time window, the retention prune, a team's view and a credential's view.
CREATE INDEX audit_entries_at ON audit_entries (at);
CREATE INDEX audit_entries_teams ON audit_entries USING gin (teams);
CREATE INDEX audit_entries_credential ON audit_entries (actor_credential_ref)
    WHERE actor_credential_ref IS NOT NULL;

-- Request counts of the hourly catalog.admin_read entries (Task 7b). The
-- entry itself never changes; its count lives here. The bucket names the
-- actor by its audit_credentials or audit_people id, never by a name.
CREATE TABLE audit_access_counts (
    bucket   text PRIMARY KEY,
    entry_id bigint REFERENCES audit_entries (id) ON DELETE CASCADE,
    requests bigint NOT NULL CHECK (requests > 0)
);
CREATE INDEX audit_access_counts_entry_id ON audit_access_counts (entry_id);

-- The audit_people row of a user, created or refreshed from users. NULL
-- for an unknown or NULL id (the INSERT then inserts nothing).
-- `FOR KEY SHARE` on the user: a concurrent users::delete (FOR UPDATE)
-- is waited for, and a user it deleted is then skipped, so no call that
-- races a deletion can re-create a forgotten person's name.
CREATE FUNCTION audit_person(p_user_id text) RETURNS bigint LANGUAGE sql AS $$
    WITH live AS (SELECT id, name, display_name FROM users WHERE id = p_user_id FOR KEY SHARE)
    INSERT INTO audit_people (user_id, name, display_name)
    SELECT id, name, display_name FROM live
    ON CONFLICT (user_id) DO UPDATE
        SET name = EXCLUDED.name, display_name = EXCLUDED.display_name
    RETURNING id
$$;

-- The audit_teams row of a team (its id never changes): created on first
-- use, its shared-team name snapshot refreshed. NULL for an unknown team.
CREATE FUNCTION audit_team_ref(p_team_id text) RETURNS bigint LANGUAGE sql AS $$
    INSERT INTO audit_teams (team_id, name, person_id)
    SELECT t.id,
           CASE WHEN t.personal_of IS NULL THEN t.name END,
           CASE WHEN t.personal_of IS NOT NULL THEN audit_person(t.personal_of) END
    FROM teams t WHERE t.id = p_team_id
    ON CONFLICT (team_id) DO UPDATE SET name = EXCLUDED.name
    RETURNING id
$$;

-- The audit_credentials row of a credential id, created or refreshed; a
-- NULL label keeps the stored one. NULL for a NULL id. The owning person
-- comes from the credential tables while the credential exists (a key or
-- grant's api_keys.user_id, a code's sign_in_codes.user_id), whatever the
-- caller passes, so a credential from before this migration that is only
-- ever a target (revoked, granted) still has its person, and
-- forget_person clears it with its user.
--
-- The owner is locked first (through audit_person), so a user deletion
-- cannot remove the credential meanwhile; a credential whose owner is
-- already deleted gets no row (the owner resolves, its person does not),
-- so a write racing a deletion cannot re-create a cleared id or label.
CREATE FUNCTION audit_credential(p_credential_id text, p_person_id bigint, p_label text)
RETURNS bigint LANGUAGE sql AS $$
    WITH owner AS (
        SELECT COALESCE((SELECT user_id FROM api_keys WHERE id = p_credential_id),
                        (SELECT user_id FROM sign_in_codes WHERE id = p_credential_id)) AS user_id
    ),
    person AS (SELECT audit_person(user_id) AS id FROM owner)
    INSERT INTO audit_credentials (credential_id, person_id, label)
    SELECT p_credential_id, COALESCE((SELECT id FROM person), p_person_id), p_label
    WHERE p_credential_id IS NOT NULL
      -- A user's credential only while the user lives; a credential with no
      -- user (a legacy key, the runner's) whenever it exists.
      AND CASE WHEN (SELECT user_id FROM owner) IS NULL
               THEN EXISTS (SELECT 1 FROM api_keys WHERE id = p_credential_id)
                    OR EXISTS (SELECT 1 FROM sign_in_codes WHERE id = p_credential_id)
               ELSE (SELECT id FROM person) IS NOT NULL END
    ON CONFLICT (credential_id) DO UPDATE
        SET label = COALESCE(EXCLUDED.label, audit_credentials.label),
            person_id = COALESCE(EXCLUDED.person_id, audit_credentials.person_id)
    RETURNING id
$$;

-- Append-only: no UPDATE ever; a DELETE only of an entry older than the
-- retention period (the audit_retention_days setting, default 400, at
-- least 30). The CLI and the API never delete; the daily prune does.
CREATE FUNCTION audit_entries_append_only() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    retention integer;
BEGIN
    IF TG_OP <> 'DELETE' THEN
        RAISE EXCEPTION 'audit entries are append-only' USING ERRCODE = 'insufficient_privilege';
    END IF;
    SELECT GREATEST(30, COALESCE(
               (SELECT (value #>> '{}')::integer FROM server_settings
                 WHERE key = 'audit_retention_days'), 400))
      INTO retention;
    IF OLD.at >= now() - make_interval(days => retention) THEN
        RAISE EXCEPTION 'audit entry % is inside the retention period', OLD.id
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    RETURN OLD;
END
$$;
CREATE TRIGGER audit_entries_append_only BEFORE UPDATE OR DELETE ON audit_entries
    FOR EACH ROW EXECUTE FUNCTION audit_entries_append_only();
CREATE TRIGGER audit_entries_no_truncate BEFORE TRUNCATE ON audit_entries
    FOR EACH STATEMENT EXECUTE FUNCTION audit_entries_append_only();
