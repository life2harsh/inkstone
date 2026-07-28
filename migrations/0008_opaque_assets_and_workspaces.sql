-- Opaque workspace metadata and chunked encrypted assets. No content-bearing
-- names, paths, media types, or plaintext sizes are retained by the server.
ALTER TABLE workspaces ADD COLUMN encrypted_metadata BYTEA;
ALTER TABLE workspaces ADD COLUMN metadata_nonce BYTEA;
ALTER TABLE workspaces ADD COLUMN metadata_version BIGINT NOT NULL DEFAULT 1;
UPDATE workspaces
SET encrypted_metadata = decode('00000000000000000000000000000000', 'hex'),
    metadata_nonce = decode('000000000000000000000000', 'hex');
ALTER TABLE workspaces ALTER COLUMN encrypted_metadata SET NOT NULL;
ALTER TABLE workspaces ALTER COLUMN metadata_nonce SET NOT NULL;
ALTER TABLE workspaces ADD CONSTRAINT workspaces_metadata_envelope_size
    CHECK (octet_length(encrypted_metadata) BETWEEN 16 AND 65536);
ALTER TABLE workspaces ADD CONSTRAINT workspaces_metadata_nonce_size
    CHECK (octet_length(metadata_nonce) = 12);
ALTER TABLE workspaces DROP COLUMN name;
ALTER TABLE workspaces DROP COLUMN description;

DROP TABLE assets;
CREATE TABLE assets (
    id UUID PRIMARY KEY,
    workspace_id UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    version BIGINT NOT NULL CHECK (version > 0),
    chunk_count INTEGER NOT NULL CHECK (chunk_count BETWEEN 1 AND 256),
    encrypted_size BIGINT NOT NULL CHECK (encrypted_size BETWEEN 16 AND 268439552),
    encrypted_metadata BYTEA NOT NULL CHECK (octet_length(encrypted_metadata) BETWEEN 16 AND 65536),
    metadata_nonce BYTEA NOT NULL CHECK (octet_length(metadata_nonce) = 12),
    aad_version INTEGER NOT NULL CHECK (aad_version > 0),
    uploaded_by UUID NOT NULL REFERENCES users(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX assets_workspace_id_idx ON assets(workspace_id);

CREATE TABLE asset_chunks (
    asset_id UUID NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    asset_version BIGINT NOT NULL,
    chunk_index INTEGER NOT NULL CHECK (chunk_index BETWEEN 0 AND 255),
    ciphertext BYTEA NOT NULL CHECK (octet_length(ciphertext) BETWEEN 16 AND 1048592),
    nonce BYTEA NOT NULL CHECK (octet_length(nonce) = 12),
    aad_version INTEGER NOT NULL CHECK (aad_version > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (asset_id, asset_version, chunk_index)
);

CREATE TABLE workspace_path_projections (
    id UUID PRIMARY KEY,
    workspace_id UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    entry_kind TEXT NOT NULL CHECK (entry_kind IN ('folder', 'asset')),
    encrypted_path BYTEA NOT NULL CHECK (octet_length(encrypted_path) BETWEEN 16 AND 16384),
    path_nonce BYTEA NOT NULL CHECK (octet_length(path_nonce) = 12),
    projection_version BIGINT NOT NULL CHECK (projection_version > 0),
    created_by UUID NOT NULL REFERENCES users(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX workspace_path_projections_workspace_idx
    ON workspace_path_projections(workspace_id, updated_at DESC);
