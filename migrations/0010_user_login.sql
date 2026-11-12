-- User login (spec 2026-11-12-okf-user-login-design.md, corrected by the M12
-- plan's Decisions 1-4 and 8). It only adds: every existing user, key,
-- grant, code and catalog keeps its meaning. Every timestamp is supplied by
-- the server clock. apalis keeps its tables in its own `apalis` schema,
-- which `db::migrate` sets up after this migrator runs.

-- Case-insensitive email addresses. citext is a trusted extension, so the
-- database owner may create it; no superuser is needed.
CREATE EXTENSION IF NOT EXISTS citext;

ALTER TABLE users
    ADD COLUMN email               citext UNIQUE,
    ADD COLUMN is_admin            boolean NOT NULL DEFAULT false,
    -- An argon2id PHC string; NULL until the user sets a password.
    ADD COLUMN password_hash       text,
    ADD COLUMN password_changed_at timestamptz,
    -- Account lockout (Decision 8): consecutive failed password sign-ins,
    -- and until when sign-in is refused after the tenth.
    ADD COLUMN failed_sign_ins     integer NOT NULL DEFAULT 0 CHECK (failed_sign_ins >= 0),
    ADD COLUMN locked_until        timestamptz;

-- External identities (OIDC). A user may have several.
CREATE TABLE user_identities (
    provider      text NOT NULL,                  -- a configured provider id
    subject       text NOT NULL,                  -- the provider's `sub`
    user_id       text NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    email_at_link citext NOT NULL,
    created_at    timestamptz NOT NULL,
    last_used_at  timestamptz,
    PRIMARY KEY (provider, subject)
);
CREATE INDEX user_identities_user_id ON user_identities (user_id);

-- WebAuthn credentials: public keys only. The sign counter lives inside
-- the serialized Passkey.
CREATE TABLE user_passkeys (
    credential_id bytea PRIMARY KEY,
    user_id       text NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    passkey       jsonb NOT NULL,                 -- webauthn-rs serialized Passkey
    name          text NOT NULL,
    created_at    timestamptz NOT NULL,
    last_used_at  timestamptz
);
CREATE INDEX user_passkeys_user_id ON user_passkeys (user_id);

-- Console sessions. The cookie carries the id; only its SHA-256 is stored.
CREATE TABLE user_sessions (
    id_hash      bytea PRIMARY KEY,
    user_id      text NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    csrf_token   text NOT NULL,
    created_at   timestamptz NOT NULL,
    last_seen_at timestamptz NOT NULL,
    expires_at   timestamptz NOT NULL,            -- the absolute limit
    ip           inet,
    user_agent   text NOT NULL DEFAULT ''
);
CREATE INDEX user_sessions_user_id ON user_sessions (user_id);
-- The hourly prune deletes past the absolute limit.
CREATE INDEX user_sessions_expires_at ON user_sessions (expires_at);

-- Single-use emailed links. Only the SHA-256 of the token is stored.
CREATE TABLE email_tokens (
    hash       bytea PRIMARY KEY,
    user_id    text NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    purpose    text NOT NULL CHECK (purpose IN ('setup', 'reset')),
    created_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL,
    used_at    timestamptz
);
-- Issuing a link invalidates the user's older ones of the same purpose.
CREATE INDEX email_tokens_user_id_purpose ON email_tokens (user_id, purpose);

-- Sign-in audit, shown to the user and to admins. `user_id` is NULL for an
-- attempt on an unknown email.
CREATE TABLE sign_in_events (
    id         bigserial PRIMARY KEY,
    user_id    text REFERENCES users (id) ON DELETE CASCADE,
    method     text NOT NULL CHECK (method IN ('password', 'passkey', 'oidc', 'setup')),
    outcome    text NOT NULL CHECK (outcome IN ('ok', 'failed', 'locked')),
    ip         inet,
    at         timestamptz NOT NULL
);
CREATE INDEX sign_in_events_user_at ON sign_in_events (user_id, at DESC);
-- The 90-day prune.
CREATE INDEX sign_in_events_at ON sign_in_events (at);

-- Per-user entitlements: catalogs beyond the user's teams' catalogs.
-- `catalogs.id` is bigint (Decision 2).
CREATE TABLE user_catalogs (
    user_id    text NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    catalog_id bigint NOT NULL REFERENCES catalogs (id) ON DELETE CASCADE,
    access     text NOT NULL CHECK (access IN ('read', 'write')),
    created_at timestamptz NOT NULL,
    PRIMARY KEY (user_id, catalog_id)
);
-- The primary key leads with user_id; the catalog's cascade needs its own index.
CREATE INDEX user_catalogs_catalog_id ON user_catalogs (catalog_id);

-- What members of the owning team may reach through self-issued
-- credentials. Ownership alone still grants nothing to admin-issued ones.
ALTER TABLE catalogs
    ADD COLUMN member_access text NOT NULL DEFAULT 'write'
        CHECK (member_access IN ('none', 'read', 'write'));

-- Self-issued credentials (Decision 3) and key expiry (Decision 4). A
-- self-issued row is capped by its user's ceiling, so it always names its
-- user and its catalogs, and is never admin or a reporter (mirroring
-- `api_keys_oauth_caps`).
ALTER TABLE api_keys
    ADD COLUMN self_issued boolean NOT NULL DEFAULT false,
    ADD COLUMN expires_at  timestamptz,
    ADD CONSTRAINT api_keys_self_issued_caps CHECK (
        NOT self_issued
        OR (access <> 'admin' AND NOT maintenance_report AND NOT all_catalogs)
    ),
    ADD CONSTRAINT api_keys_self_issued_user CHECK (NOT self_issued OR user_id IS NOT NULL);

-- Per-catalog levels (Decision 1). NULL is the key's own `access`, so every
-- existing grant keeps its meaning.
ALTER TABLE key_catalogs
    ADD COLUMN access text
        CONSTRAINT key_catalogs_access_level CHECK (access IN ('read', 'write'));

-- Policy settings edited in the console; an absent key falls back to the
-- config file's `[auth]` default.
CREATE TABLE server_settings (
    key        text PRIMARY KEY,
    value      jsonb NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by text NOT NULL
);
