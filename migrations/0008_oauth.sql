-- OAuth 2.1 (M10): registered and metadata-document clients, admin-issued
-- sign-in codes, grants (api_keys rows of kind 'oauth') and their tokens.
-- Every timestamp is supplied by the server clock; every secret is stored
-- only as its SHA-256.

-- A client is a dynamically registered one (a UUID id) or a Client ID
-- Metadata Document (the document's URL is the id), cached here.
CREATE TABLE oauth_clients (
    client_id     text PRIMARY KEY CHECK (client_id <> ''),
    kind          text NOT NULL CHECK (kind IN ('dcr', 'cimd')),
    client_name   text,
    redirect_uris text[] NOT NULL CHECK (cardinality(redirect_uris) > 0),
    metadata      jsonb NOT NULL,
    -- When a metadata document was last fetched, and until when the cached
    -- copy may be used (its Cache-Control, clamped by Task 3); never set for DCR.
    fetched_at       timestamptz,
    cache_expires_at timestamptz,
    created_at    timestamptz NOT NULL,
    CONSTRAINT oauth_clients_fetched_cimd CHECK (
        (kind = 'cimd') = (fetched_at IS NOT NULL)
        AND (kind = 'cimd') = (cache_expires_at IS NOT NULL)
    )
);

-- A grant is a key row with no hash of its own: its tokens carry the
-- credential. `length(NULL) = 32` is NULL, which a CHECK accepts, and UNIQUE
-- treats NULLs as distinct, so the existing sha256 constraints stay.
ALTER TABLE api_keys
    ADD COLUMN kind text NOT NULL DEFAULT 'key' CHECK (kind IN ('key', 'oauth')),
    ADD COLUMN client_id text REFERENCES oauth_clients (client_id) ON DELETE SET NULL,
    ALTER COLUMN sha256 DROP NOT NULL,
    ADD CONSTRAINT api_keys_kind_hash CHECK ((kind = 'key') = (sha256 IS NOT NULL)),
    -- No code path can make an OAuth grant that administers or reports.
    ADD CONSTRAINT api_keys_oauth_caps
        CHECK (kind = 'key' OR (access <> 'admin' AND NOT maintenance_report));
-- Lets pruning find the clients no grant names.
CREATE INDEX api_keys_client_id ON api_keys (client_id) WHERE client_id IS NOT NULL;

-- Grants are named after their sign-in code; two codes of one name and actor
-- may both be redeemed, so the live name/actor rule covers keys only. The
-- index keeps its name, which `store::keys::create_with_id` matches.
DROP INDEX api_keys_live_name_actor;
CREATE UNIQUE INDEX api_keys_live_name_actor
    ON api_keys (name, actor) WHERE revoked_at IS NULL AND kind = 'key';

-- A one-time credential bound to a grant template. Admin is never offered.
CREATE TABLE sign_in_codes (
    id                 text PRIMARY KEY DEFAULT gen_random_uuid()::text,
    name               text NOT NULL CHECK (name ~ '^[A-Za-z0-9._-]+$'),
    sha256             bytea NOT NULL UNIQUE CHECK (length(sha256) = 32),
    access             text NOT NULL CHECK (access IN ('read', 'write')),
    all_catalogs       boolean NOT NULL,
    default_catalog_id bigint REFERENCES catalogs (id) ON DELETE SET NULL,
    actor              text NOT NULL,
    created_at         timestamptz NOT NULL,
    expires_at         timestamptz NOT NULL CHECK (expires_at > created_at),
    redeemed_at        timestamptz,
    grant_id           text REFERENCES api_keys (id) ON DELETE SET NULL,
    revoked_at         timestamptz,
    -- A grant id only on a redeemed code. A redeemed code may lose it: the
    -- `ON DELETE SET NULL` above clears it if the grant row is ever deleted.
    CONSTRAINT sign_in_codes_redeemed_grant CHECK (grant_id IS NULL OR redeemed_at IS NOT NULL)
);
-- Lets deleting a catalog find the codes whose default it clears.
CREATE INDEX sign_in_codes_default_catalog_id ON sign_in_codes (default_catalog_id)
    WHERE default_catalog_id IS NOT NULL;
-- Lets pruning find unredeemed codes past their expiry.
CREATE INDEX sign_in_codes_unredeemed_expires_at ON sign_in_codes (expires_at)
    WHERE redeemed_at IS NULL;

CREATE TABLE sign_in_code_catalogs (
    code_id    text NOT NULL REFERENCES sign_in_codes (id) ON DELETE CASCADE,
    catalog_id bigint NOT NULL REFERENCES catalogs (id) ON DELETE CASCADE,
    PRIMARY KEY (code_id, catalog_id)
);
CREATE INDEX sign_in_code_catalogs_catalog_id ON sign_in_code_catalogs (catalog_id);

-- Access and refresh tokens. A family is every token minted from one
-- authorization code; rotating a refresh token keeps the family.
CREATE TABLE oauth_tokens (
    sha256     bytea PRIMARY KEY CHECK (length(sha256) = 32),
    grant_id   text NOT NULL REFERENCES api_keys (id) ON DELETE CASCADE,
    kind       text NOT NULL CHECK (kind IN ('access', 'refresh')),
    family_id  text NOT NULL CHECK (family_id <> ''),
    resource   text NOT NULL,
    created_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL CHECK (expires_at > created_at),
    -- Refresh tokens: when the token was rotated.
    used_at    timestamptz,
    revoked_at timestamptz
);
CREATE INDEX oauth_tokens_family_id ON oauth_tokens (family_id);
CREATE INDEX oauth_tokens_grant_id ON oauth_tokens (grant_id);
CREATE INDEX oauth_tokens_expires_at ON oauth_tokens (expires_at);
