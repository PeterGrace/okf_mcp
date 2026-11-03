-- Changeset identity (M5, Decision 1): one open changeset per (catalog,
-- key, label). The label comes from the connection (`X-OKF-Changeset`,
-- `?changeset=`, else 'default'; never the MCP session id), so a client
-- keeps its changeset across reconnects and restarts, and the key is part of
-- the identity so one key never sees another's.
ALTER TABLE changesets RENAME COLUMN session_id TO label;

DROP INDEX changesets_one_open_per_session;
-- A racing "get or create" cannot open two changesets for one identity.
-- A NULL key_id (its key deleted) never conflicts, as NULLs are distinct.
CREATE UNIQUE INDEX changesets_one_open_per_label
    ON changesets (catalog_id, key_id, label) WHERE state = 'open';

-- Bumped by every change to the changeset or its entries; overlay caches
-- are valid while it (and the catalog's generation) stays the same.
ALTER TABLE changesets ADD COLUMN version bigint NOT NULL DEFAULT 0;
