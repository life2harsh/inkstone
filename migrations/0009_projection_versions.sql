ALTER TABLE assets ADD COLUMN metadata_version BIGINT NOT NULL DEFAULT 1
    CHECK (metadata_version > 0);

ALTER TABLE workspace_path_projections ADD COLUMN aad_version INTEGER NOT NULL DEFAULT 1
    CHECK (aad_version > 0);
CREATE UNIQUE INDEX workspace_path_projection_identity
    ON workspace_path_projections(workspace_id, id);
