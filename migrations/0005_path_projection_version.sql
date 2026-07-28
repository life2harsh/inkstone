ALTER TABLE docs
    ADD COLUMN path_projection_version BIGINT NOT NULL DEFAULT 0,
    ADD CONSTRAINT docs_path_projection_version_nonnegative
        CHECK (path_projection_version >= 0);
