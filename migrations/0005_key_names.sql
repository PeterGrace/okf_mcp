-- A credential's immutable id is distinct from its human grouping name.
-- Existing ids remain valid for their holders and become the corresponding
-- names. New credentials receive UUID ids, allowing a revoked name/actor pair
-- to be reissued while historical foreign keys continue to identify the old
-- credential.
ALTER TABLE api_keys ADD COLUMN name text;
UPDATE api_keys SET name = id;
ALTER TABLE api_keys ALTER COLUMN name SET NOT NULL;
ALTER TABLE api_keys ALTER COLUMN id SET DEFAULT gen_random_uuid()::text;
ALTER TABLE api_keys ADD CONSTRAINT api_keys_name_check
    CHECK (name ~ '^[A-Za-z0-9._-]+$');
CREATE UNIQUE INDEX api_keys_live_name_actor
    ON api_keys (name, actor) WHERE revoked_at IS NULL;
