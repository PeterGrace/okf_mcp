-- The provisioner key (spec Part 4): a server-wide flag for automation that
-- creates and removes accounts, teams and catalogs but reaches no catalog.
-- Set only at creation; it holds no grants, which a trigger enforces.
ALTER TABLE api_keys
    ADD COLUMN provisioner boolean NOT NULL DEFAULT false,
    ADD CONSTRAINT api_keys_provisioner_shape CHECK (
        NOT provisioner
        OR (kind = 'key' AND access = 'read' AND NOT all_catalogs AND NOT self_issued
            AND NOT maintenance_report AND default_catalog_id IS NULL AND user_id IS NOT NULL)
    );

CREATE FUNCTION key_catalogs_refuse_provisioner() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM api_keys WHERE id = NEW.key_id AND provisioner) THEN
        RAISE EXCEPTION 'a provisioner key reaches no catalog' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER key_catalogs_refuse_provisioner BEFORE INSERT OR UPDATE ON key_catalogs
    FOR EACH ROW EXECUTE FUNCTION key_catalogs_refuse_provisioner();

CREATE FUNCTION api_keys_provisioner_fixed() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.provisioner IS DISTINCT FROM OLD.provisioner THEN
        RAISE EXCEPTION 'the provisioner flag is set only at creation'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER api_keys_provisioner_fixed BEFORE UPDATE OF provisioner ON api_keys
    FOR EACH ROW EXECUTE FUNCTION api_keys_provisioner_fixed();
