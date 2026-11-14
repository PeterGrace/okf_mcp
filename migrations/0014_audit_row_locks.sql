-- Audit rows that never make a change wait (issue #396). The resolvers in
-- 0012_audit.sql upsert their row on every call, and an upsert's
-- ON CONFLICT DO UPDATE locks the existing row even when nothing changes.
-- Two changes that name the same two people in opposite order (admin A
-- invites B while B invites A; A suspends B's personal team while B
-- suspends A's) each held one audit_people row and waited for the
-- other's: a deadlock.
--
-- Now a resolver reads an existing row without a lock. A stale row (a
-- renamed user or team, a credential's new label or owner) is refreshed
-- only if no other transaction holds it (FOR NO KEY UPDATE SKIP LOCKED):
-- one that does is refreshing it to current values itself, and if it
-- rolls back, the next entry refreshes it. The rows are display snapshots
-- (forget_person finds a user's credentials through the credential tables
-- too), so a refresh that waits for nobody loses nothing. Only a row
-- that does not exist yet is inserted, and every live user and team has
-- one: they are backfilled here, and users::create (and the team writes
-- that record team.created) make them in the transaction that creates
-- the user or team, which no other transaction can see before it
-- commits. Same arguments and results as before.

-- The audit_people row of a user, created or refreshed from users. NULL
-- for an unknown or NULL id. `FOR KEY SHARE` on the user, as before: a
-- concurrent users::delete (FOR UPDATE) is waited for, and a user it
-- deleted is then skipped, so no call that races a deletion can re-create
-- a forgotten person's name.
CREATE OR REPLACE FUNCTION audit_person(p_user_id text) RETURNS bigint LANGUAGE plpgsql AS $$
DECLARE
    live_id text;
    live_name text;
    live_display text;
    person bigint;
    is_current boolean;
BEGIN
    SELECT id, name, display_name INTO live_id, live_name, live_display
      FROM users WHERE id = p_user_id FOR KEY SHARE;
    IF NOT FOUND THEN
        RETURN NULL;
    END IF;
    SELECT id, name = live_name AND display_name = live_display INTO person, is_current
      FROM audit_people WHERE user_id = live_id;
    IF FOUND THEN
        IF NOT is_current THEN
            UPDATE audit_people SET name = live_name, display_name = live_display
             WHERE id = (SELECT id FROM audit_people WHERE id = person
                         FOR NO KEY UPDATE SKIP LOCKED);
        END IF;
        RETURN person;
    END IF;
    INSERT INTO audit_people (user_id, name, display_name)
    VALUES (live_id, live_name, live_display)
    ON CONFLICT (user_id) DO UPDATE
        SET name = EXCLUDED.name, display_name = EXCLUDED.display_name
    RETURNING id INTO person;
    RETURN person;
END
$$;

-- The audit_teams row of a team: created on first use, its shared-team
-- name snapshot refreshed; a personal team's person is resolved (and so
-- refreshed) on every call, as before. NULL for an unknown team.
CREATE OR REPLACE FUNCTION audit_team_ref(p_team_id text) RETURNS bigint LANGUAGE plpgsql AS $$
DECLARE
    team_name text;
    personal_of text;
    person bigint;
    ref bigint;
    is_current boolean;
BEGIN
    SELECT t.name, t.personal_of INTO team_name, personal_of FROM teams t WHERE t.id = p_team_id;
    IF NOT FOUND THEN
        RETURN NULL;
    END IF;
    IF personal_of IS NOT NULL THEN
        person := audit_person(personal_of);
    END IF;
    SELECT id, name IS NOT DISTINCT FROM CASE WHEN personal_of IS NULL THEN team_name END
      INTO ref, is_current FROM audit_teams WHERE team_id = p_team_id;
    IF FOUND THEN
        IF NOT is_current THEN
            UPDATE audit_teams SET name = team_name
             WHERE id = (SELECT id FROM audit_teams WHERE id = ref FOR NO KEY UPDATE SKIP LOCKED);
        END IF;
        RETURN ref;
    END IF;
    INSERT INTO audit_teams (team_id, name, person_id)
    VALUES (p_team_id,
            CASE WHEN personal_of IS NULL THEN team_name END,
            CASE WHEN personal_of IS NOT NULL THEN person END)
    ON CONFLICT (team_id) DO UPDATE SET name = EXCLUDED.name
    RETURNING id INTO ref;
    RETURN ref;
END
$$;

-- The audit_credentials row of a credential id, created or refreshed; a
-- NULL label keeps the stored one. NULL for a NULL id. The owning person
-- comes from the credential tables while the credential exists, whatever
-- the caller passes; the owner is locked first (through audit_person); a
-- credential whose owner is already deleted gets no row, and a credential
-- with no user gets one only while it exists. All as in 0012_audit.sql.
CREATE OR REPLACE FUNCTION audit_credential(p_credential_id text, p_person_id bigint, p_label text)
RETURNS bigint LANGUAGE plpgsql AS $$
DECLARE
    owner_id text;
    owner_person bigint;
    person bigint;
    ref bigint;
    is_current boolean;
BEGIN
    IF p_credential_id IS NULL THEN
        RETURN NULL;
    END IF;
    owner_id := COALESCE((SELECT user_id FROM api_keys WHERE id = p_credential_id),
                         (SELECT user_id FROM sign_in_codes WHERE id = p_credential_id));
    owner_person := audit_person(owner_id);
    IF owner_id IS NULL THEN
        IF NOT (EXISTS (SELECT 1 FROM api_keys WHERE id = p_credential_id)
                OR EXISTS (SELECT 1 FROM sign_in_codes WHERE id = p_credential_id)) THEN
            RETURN NULL;
        END IF;
    ELSIF owner_person IS NULL THEN
        RETURN NULL;
    END IF;
    person := COALESCE(owner_person, p_person_id);
    -- Current: what the refresh would leave unchanged.
    SELECT id, (p_label IS NULL OR label IS NOT DISTINCT FROM p_label)
               AND (person IS NULL OR person_id IS NOT DISTINCT FROM person)
      INTO ref, is_current FROM audit_credentials WHERE credential_id = p_credential_id;
    IF FOUND THEN
        IF NOT is_current THEN
            UPDATE audit_credentials
               SET label = COALESCE(p_label, label), person_id = COALESCE(person, person_id)
             WHERE id = (SELECT id FROM audit_credentials WHERE id = ref
                         FOR NO KEY UPDATE SKIP LOCKED);
        END IF;
        RETURN ref;
    END IF;
    INSERT INTO audit_credentials (credential_id, person_id, label)
    VALUES (p_credential_id, person, p_label)
    ON CONFLICT (credential_id) DO UPDATE
        SET label = COALESCE(EXCLUDED.label, audit_credentials.label),
            person_id = COALESCE(EXCLUDED.person_id, audit_credentials.person_id)
    RETURNING id INTO ref;
    RETURN ref;
END
$$;

-- Every live user and team has its row from now on (see above).
INSERT INTO audit_people (user_id, name, display_name)
SELECT id, name, display_name FROM users
ON CONFLICT (user_id) DO NOTHING;

INSERT INTO audit_teams (team_id, name, person_id)
SELECT t.id,
       CASE WHEN t.personal_of IS NULL THEN t.name END,
       (SELECT p.id FROM audit_people p WHERE p.user_id = t.personal_of)
FROM teams t
ON CONFLICT (team_id) DO NOTHING;
