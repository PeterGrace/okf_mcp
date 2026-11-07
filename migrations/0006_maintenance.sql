-- Maintenance records are catalog-scoped; every timestamp is supplied by the server clock.
ALTER TABLE api_keys ADD COLUMN maintenance_report boolean NOT NULL DEFAULT false;

CREATE TABLE maintenance_state (
    catalog_id bigint PRIMARY KEY REFERENCES catalogs (id) ON DELETE CASCADE,
    changed_cursor text NOT NULL DEFAULT '',
    expiry_cursor text NOT NULL DEFAULT '',
    verification_cursor text NOT NULL DEFAULT '',
    next_aging_group text NOT NULL DEFAULT 'expiry'
        CHECK (next_aging_group IN ('expiry', 'verification')),
    updated_at timestamptz NOT NULL
);

CREATE TABLE maintenance_runs (
    id text PRIMARY KEY,
    catalog_id bigint NOT NULL REFERENCES catalogs (id) ON DELETE CASCADE,
    key_id text REFERENCES api_keys (id) ON DELETE SET NULL,
    credential_id text NOT NULL CHECK (credential_id <> ''),
    actor text NOT NULL,
    state text NOT NULL CHECK (state IN ('running', 'completed', 'failed', 'cancelled')),
    deadline timestamptz NOT NULL,
    created_at timestamptz NOT NULL,
    ended_at timestamptz,
    end_reason text,
    primary_count integer NOT NULL DEFAULT 0 CHECK (primary_count BETWEEN 0 AND 50),
    UNIQUE (catalog_id, id)
);
CREATE INDEX maintenance_runs_catalog_created ON maintenance_runs (catalog_id, created_at, id);
CREATE INDEX maintenance_runs_key_id ON maintenance_runs (key_id) WHERE key_id IS NOT NULL;

CREATE TABLE maintenance_work (
    id text PRIMARY KEY,
    run_id text NOT NULL,
    catalog_id bigint NOT NULL REFERENCES catalogs (id) ON DELETE CASCADE,
    path text NOT NULL,
    revision integer NOT NULL,
    lanes text[] NOT NULL CHECK (
        cardinality(lanes) > 0 AND lanes <@ ARRAY['changed', 'aging']::text[]
    ),
    aging_group text CHECK (aging_group IN ('expiry', 'verification')),
    outcome text CHECK (outcome IN ('completed', 'inconclusive', 'failed', 'superseded')),
    assessment text CHECK (assessment IN ('supported_current', 'outdated', 'unverifiable', 'not_assessed')),
    reason text,
    summary text,
    lease_token text NOT NULL,
    lease_expires_at timestamptz NOT NULL,
    attempted_at timestamptz NOT NULL,
    ended_at timestamptz,
    reported_tokens bigint CHECK (reported_tokens >= 0),
    UNIQUE (catalog_id, id),
    UNIQUE (catalog_id, run_id, id),
    FOREIGN KEY (catalog_id, run_id) REFERENCES maintenance_runs (catalog_id, id) ON DELETE CASCADE,
    FOREIGN KEY (catalog_id, path, revision) REFERENCES revisions (catalog_id, path, revision) ON DELETE NO ACTION DEFERRABLE INITIALLY DEFERRED
);
CREATE UNIQUE INDEX maintenance_work_live_claim ON maintenance_work (catalog_id, path, revision)
    WHERE outcome IS NULL;
CREATE INDEX maintenance_work_path_revision ON maintenance_work (catalog_id, path, revision);
CREATE INDEX maintenance_work_lease_expiry ON maintenance_work (catalog_id, lease_expires_at)
    WHERE outcome IS NULL;
CREATE INDEX maintenance_work_run ON maintenance_work (catalog_id, run_id);

CREATE TABLE maintenance_findings (
    id text PRIMARY KEY,
    run_id text NOT NULL,
    work_id text NOT NULL,
    catalog_id bigint NOT NULL REFERENCES catalogs (id) ON DELETE CASCADE,
    category text NOT NULL CHECK (category IN (
        'suggested_link', 'possible_duplicate', 'conflicting_assertions',
        'potentially_outdated_information', 'unverifiable_claim'
    )),
    assertion text NOT NULL,
    follow_up text NOT NULL,
    uncertainty text NOT NULL,
    relationship text NOT NULL CHECK (relationship IN ('direct_statement', 'inference', 'external_observation')),
    external jsonb NOT NULL DEFAULT '[]'::jsonb,
    created_at timestamptz NOT NULL,
    UNIQUE (catalog_id, id),
    UNIQUE (catalog_id, work_id, id),
    FOREIGN KEY (catalog_id, run_id, work_id)
        REFERENCES maintenance_work (catalog_id, run_id, id) ON DELETE CASCADE
);
CREATE INDEX maintenance_findings_catalog_created ON maintenance_findings (catalog_id, created_at, id);
CREATE INDEX maintenance_findings_work ON maintenance_findings (catalog_id, work_id);

CREATE TABLE maintenance_evidence (
    catalog_id bigint NOT NULL REFERENCES catalogs (id) ON DELETE CASCADE,
    work_id text NOT NULL,
    finding_id text,
    ordinal integer NOT NULL CHECK (ordinal >= 0),
    path text NOT NULL,
    revision integer NOT NULL,
    excerpt text NOT NULL,
    PRIMARY KEY (catalog_id, work_id, finding_id, ordinal),
    FOREIGN KEY (catalog_id, work_id) REFERENCES maintenance_work (catalog_id, id) ON DELETE CASCADE,
    FOREIGN KEY (catalog_id, work_id, finding_id)
        REFERENCES maintenance_findings (catalog_id, work_id, id) ON DELETE CASCADE,
    FOREIGN KEY (catalog_id, path, revision) REFERENCES revisions (catalog_id, path, revision) ON DELETE NO ACTION DEFERRABLE INITIALLY DEFERRED
);
CREATE UNIQUE INDEX maintenance_evidence_assessment_ordinal
    ON maintenance_evidence (catalog_id, work_id, ordinal) WHERE finding_id IS NULL;
CREATE INDEX maintenance_evidence_finding ON maintenance_evidence (catalog_id, work_id, finding_id);
CREATE INDEX maintenance_evidence_revision ON maintenance_evidence (catalog_id, path, revision);

CREATE TABLE maintenance_dispositions (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    catalog_id bigint NOT NULL REFERENCES catalogs (id) ON DELETE CASCADE,
    finding_id text NOT NULL,
    disposition text NOT NULL CHECK (disposition IN ('open', 'accepted_for_follow_up', 'dismissed', 'needs_input')),
    rationale text NOT NULL,
    credential_id text NOT NULL,
    actor text NOT NULL,
    created_at timestamptz NOT NULL,
    FOREIGN KEY (catalog_id, finding_id) REFERENCES maintenance_findings (catalog_id, id) ON DELETE CASCADE
);
CREATE INDEX maintenance_dispositions_finding ON maintenance_dispositions (catalog_id, finding_id, id);

CREATE TABLE maintenance_requests (
    catalog_id bigint NOT NULL REFERENCES catalogs (id) ON DELETE CASCADE,
    credential_id text NOT NULL,
    operation text NOT NULL,
    idempotency_key text NOT NULL,
    request jsonb NOT NULL,
    response jsonb NOT NULL,
    PRIMARY KEY (catalog_id, credential_id, operation, idempotency_key)
);
