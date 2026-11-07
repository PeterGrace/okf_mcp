-- Assessment evidence has no finding; a primary key would make finding_id NOT NULL.
ALTER TABLE maintenance_evidence DROP CONSTRAINT maintenance_evidence_pkey;
ALTER TABLE maintenance_evidence ALTER COLUMN finding_id DROP NOT NULL;
CREATE UNIQUE INDEX maintenance_evidence_finding_ordinal
    ON maintenance_evidence (catalog_id, work_id, finding_id, ordinal)
    WHERE finding_id IS NOT NULL;
-- The existing partial assessment index protects the NULL-finding ordinals.
