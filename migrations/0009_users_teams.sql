-- Users and teams (spec 2026-11-11-okf-users-teams-design.md). A credential
-- belongs to a user; a catalog belongs to a team; every user has a personal
-- team. Ownership grants no access. Rows from before this migration stay
-- NULL ("unassigned") until an admin assigns them; the server refuses to
-- create new ones without a user or owner. Every timestamp is supplied by
-- the server clock.

CREATE TABLE users (
    id           text PRIMARY KEY DEFAULT gen_random_uuid()::text,
    name         text NOT NULL UNIQUE CHECK (name ~ '^[a-z0-9-]+$'),
    display_name text NOT NULL DEFAULT '',
    created_at   timestamptz NOT NULL
);

CREATE TABLE teams (
    id           text PRIMARY KEY DEFAULT gen_random_uuid()::text,
    name         text NOT NULL UNIQUE CHECK (name ~ '^[a-z0-9-]+$'),
    display_name text NOT NULL DEFAULT '',
    -- Set on exactly one team per user: the personal team made with it.
    personal_of  text UNIQUE REFERENCES users (id) ON DELETE RESTRICT,
    created_at   timestamptz NOT NULL
);

CREATE TABLE team_members (
    team_id    text NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    user_id    text NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    role       text NOT NULL CHECK (role IN ('owner', 'member')),
    created_at timestamptz NOT NULL,
    PRIMARY KEY (team_id, user_id)
);
-- The primary key leads with team_id; a user's teams need their own index.
CREATE INDEX team_members_user_id ON team_members (user_id);

ALTER TABLE api_keys
    ADD COLUMN user_id text REFERENCES users (id) ON DELETE RESTRICT;
ALTER TABLE sign_in_codes
    ADD COLUMN user_id text REFERENCES users (id) ON DELETE RESTRICT;
ALTER TABLE catalogs
    ADD COLUMN owner_team_id text REFERENCES teams (id) ON DELETE RESTRICT;

-- Serve the RESTRICT checks, the per-user and per-team listings, and
-- `unassigned` (which scans the NULL side; tables are small).
CREATE INDEX api_keys_user_id ON api_keys (user_id) WHERE user_id IS NOT NULL;
CREATE INDEX sign_in_codes_user_id ON sign_in_codes (user_id) WHERE user_id IS NOT NULL;
CREATE INDEX catalogs_owner_team_id ON catalogs (owner_team_id)
    WHERE owner_team_id IS NOT NULL;
