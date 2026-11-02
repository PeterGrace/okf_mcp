-- The okf_mcp schema: the design's "Storage" section. Every timestamp is
-- written by the server from its clock, so no column defaults to now().

-- One row: bumped by every `catalog` and `key` subcommand, so a running
-- server notices administrative changes.
CREATE TABLE server_state (
    singleton        boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    admin_generation bigint NOT NULL DEFAULT 0
);
INSERT INTO server_state DEFAULT VALUES;

CREATE TABLE catalogs (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name        text NOT NULL UNIQUE CHECK (name ~ '^[a-z0-9-]+$'),
    description text NOT NULL DEFAULT '',
    -- Bumped by every publish and import of this catalog.
    generation  bigint NOT NULL DEFAULT 0,
    created_at  timestamptz NOT NULL
);

-- One row per file. A deleted document keeps its row as a tombstone
-- (deleted_at set, content empty), so a path created again continues its
-- revision numbering.
CREATE TABLE documents (
    catalog_id   bigint NOT NULL REFERENCES catalogs (id) ON DELETE CASCADE,
    path         text NOT NULL,
    content      bytea NOT NULL,
    executable   boolean NOT NULL,
    revision     integer NOT NULL CHECK (revision > 0),
    deleted_at   timestamptz,
    updated_at   timestamptz NOT NULL,
    updated_by   text NOT NULL,
    -- Derived from content on every write (NULL for non-markdown files).
    kind         text CHECK (kind IN ('concept', 'index', 'log', 'machinery')),
    concept_type text,
    title        text,
    status       text,
    tags         text[] NOT NULL DEFAULT '{}',
    PRIMARY KEY (catalog_id, path)
);

CREATE TABLE api_keys (
    id              text PRIMARY KEY CHECK (id ~ '^[A-Za-z0-9._-]+$'),
    sha256          bytea NOT NULL UNIQUE CHECK (length(sha256) = 32),
    access          text NOT NULL CHECK (access IN ('read', 'write', 'admin')),
    all_catalogs    boolean NOT NULL,
    -- Cleared when that catalog is deleted.
    default_catalog_id bigint REFERENCES catalogs (id) ON DELETE SET NULL,
    actor           text NOT NULL,
    created_at      timestamptz NOT NULL,
    revoked_at      timestamptz
);
-- Lets deleting a catalog find the keys whose default it clears.
CREATE INDEX api_keys_default_catalog_id ON api_keys (default_catalog_id)
    WHERE default_catalog_id IS NOT NULL;

CREATE TABLE key_catalogs (
    key_id     text NOT NULL REFERENCES api_keys (id) ON DELETE CASCADE,
    catalog_id bigint NOT NULL REFERENCES catalogs (id) ON DELETE CASCADE,
    PRIMARY KEY (key_id, catalog_id)
);
-- The primary key leads with key_id, so deleting a catalog needs its own
-- index to find the grants that cascade.
CREATE INDEX key_catalogs_catalog_id ON key_catalogs (catalog_id);

CREATE TABLE changesets (
    id         bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    catalog_id bigint NOT NULL REFERENCES catalogs (id) ON DELETE CASCADE,
    session_id text NOT NULL,
    key_id     text REFERENCES api_keys (id) ON DELETE SET NULL,
    actor      text NOT NULL,
    state      text NOT NULL CHECK (state IN ('open', 'published', 'discarded')),
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL
);
-- A racing "get or create" cannot open two changesets for one session.
CREATE UNIQUE INDEX changesets_one_open_per_session
    ON changesets (catalog_id, session_id) WHERE state = 'open';
-- The foreign-key lookups of a catalog or key delete: the partial index above
-- cannot serve them, since they are not limited to open changesets.
CREATE INDEX changesets_catalog_id ON changesets (catalog_id);
CREATE INDEX changesets_key_id ON changesets (key_id) WHERE key_id IS NOT NULL;

CREATE TABLE changeset_entries (
    changeset_id  bigint NOT NULL REFERENCES changesets (id) ON DELETE CASCADE,
    path          text NOT NULL,
    -- NULL for a path with no live document when the entry was made.
    base_revision integer,
    -- NULL for a delete.
    content       bytea,
    executable    boolean NOT NULL DEFAULT false,
    -- The entry's log and listing operations while every change to it came
    -- from one; NULL once any other edit touches it.
    replay        jsonb,
    PRIMARY KEY (changeset_id, path)
);

-- Append-only history.
CREATE TABLE revisions (
    catalog_id   bigint NOT NULL REFERENCES catalogs (id) ON DELETE CASCADE,
    path         text NOT NULL,
    revision     integer NOT NULL,
    op           text NOT NULL CHECK (op IN ('create', 'update', 'delete', 'import')),
    -- NULL for a delete.
    content      bytea,
    executable   boolean NOT NULL,
    actor        text NOT NULL,
    changeset_id bigint REFERENCES changesets (id) ON DELETE SET NULL,
    message      text NOT NULL,
    created_at   timestamptz NOT NULL,
    -- A primary key (for logical replication and tooling), named explicitly
    -- with the name a plain UNIQUE constraint would get, which tests match.
    CONSTRAINT revisions_catalog_id_path_revision_key
        PRIMARY KEY (catalog_id, path, revision)
);
-- Deleting a changeset (when its catalog is deleted) clears changeset_id on
-- its revisions; without this index each one would scan all history.
CREATE INDEX revisions_changeset_id ON revisions (changeset_id)
    WHERE changeset_id IS NOT NULL;
