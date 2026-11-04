-- Changesets are named by handle only (M8): the connection label and its
-- one-open-per-label index go. Dropping the column would drop the index
-- with it; dropping the index first says so.
DROP INDEX changesets_one_open_per_label;
ALTER TABLE changesets DROP COLUMN label;
