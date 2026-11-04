-- Changeset handles (M8, Decision 1): the server names each changeset with a
-- random handle that `open_changeset` returns and every changeset call passes
-- back, instead of a label the connection sends. A volatile default is
-- evaluated per row, so existing changesets get distinct handles too.
-- That rewrites the table under ACCESS EXCLUSIVE, on purpose: changesets is
-- a small bookkeeping table and migrations run at start-up.
ALTER TABLE changesets
    ADD COLUMN handle text NOT NULL DEFAULT gen_random_uuid()::text;
ALTER TABLE changesets ADD CONSTRAINT changesets_handle_key UNIQUE (handle);
