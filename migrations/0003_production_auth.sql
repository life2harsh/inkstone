-- Production authentication, invitations, role constraints, and envelope integrity.
CREATE EXTENSION IF NOT EXISTS pgcrypto;

ALTER TABLE users ADD COLUMN email TEXT;
ALTER TABLE users ADD COLUMN password_hash TEXT;
CREATE UNIQUE INDEX users_email_unique ON users (LOWER(email)) WHERE email IS NOT NULL;
ALTER TABLE users ADD CONSTRAINT users_email_pair_check
    CHECK ((email IS NULL) = (password_hash IS NULL));
ALTER TABLE users ADD CONSTRAINT users_argon2id_hash_check
    CHECK (password_hash IS NULL OR password_hash LIKE '$argon2id$%') NOT VALID;

ALTER TABLE devices ADD CONSTRAINT devices_id_user_unique UNIQUE (id, user_id);

CREATE TABLE auth_sessions (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    token_hash BYTEA NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device_id UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL CHECK (expires_at > created_at),
    FOREIGN KEY (device_id, user_id) REFERENCES devices(id, user_id) ON DELETE CASCADE
);
CREATE INDEX auth_sessions_user_id_idx ON auth_sessions(user_id);
CREATE INDEX auth_sessions_expires_at_idx ON auth_sessions(expires_at);

-- Pre-token invitations cannot be redeemed safely. Preserve them as expired records.
UPDATE invitations SET expires_at = LEAST(expires_at, NOW()), accepted = TRUE WHERE accepted = FALSE;
ALTER TABLE invitations ADD COLUMN token_hash BYTEA;
ALTER TABLE invitations ADD COLUMN accepted_at TIMESTAMPTZ;
ALTER TABLE invitations ADD COLUMN accepted_by UUID REFERENCES users(id);
UPDATE invitations SET token_hash = digest(id::text, 'sha256') WHERE token_hash IS NULL;
ALTER TABLE invitations ALTER COLUMN token_hash SET NOT NULL;
ALTER TABLE invitations ADD CONSTRAINT invitations_token_hash_unique UNIQUE (token_hash);
ALTER TABLE invitations ADD CONSTRAINT invitations_token_hash_length CHECK (octet_length(token_hash) = 32);
ALTER TABLE invitations ADD CONSTRAINT invitations_acceptance_state_check CHECK (
    (accepted = FALSE AND accepted_at IS NULL AND accepted_by IS NULL)
    OR (accepted = TRUE AND accepted_at IS NOT NULL AND accepted_by IS NOT NULL)
) NOT VALID;
ALTER TABLE invitations ADD CONSTRAINT invitations_expiry_check
    CHECK (expires_at > created_at) NOT VALID;
ALTER TABLE invitations ADD CONSTRAINT invitations_role_check
    CHECK (role IN ('owner', 'admin', 'editor', 'viewer')) NOT VALID;
CREATE UNIQUE INDEX invitations_one_pending_email
    ON invitations(workspace_id, LOWER(invitee_email)) WHERE accepted = FALSE;

ALTER TABLE workspace_members ADD CONSTRAINT workspace_members_role_check
    CHECK (role IN ('owner', 'admin', 'editor', 'viewer')) NOT VALID;

ALTER TABLE encrypted_workspace_keys
    ADD COLUMN aad_version INTEGER NOT NULL DEFAULT 1;
ALTER TABLE encrypted_workspace_keys ADD CONSTRAINT encrypted_workspace_keys_nonce_length
    CHECK (octet_length(nonce) = 12) NOT VALID;
ALTER TABLE encrypted_workspace_keys ADD CONSTRAINT encrypted_workspace_keys_aad_version
    CHECK (aad_version = 1) NOT VALID;
ALTER TABLE encrypted_workspace_keys ADD CONSTRAINT encrypted_workspace_keys_envelope_size
    CHECK (octet_length(encrypted_key) BETWEEN 16 AND 65536) NOT VALID;

ALTER TABLE doc_updates ADD CONSTRAINT doc_updates_sender_device_fk
    FOREIGN KEY (sender_device_id) REFERENCES devices(id) NOT VALID;
ALTER TABLE doc_updates ADD CONSTRAINT doc_updates_nonce_length
    CHECK (octet_length(nonce) = 12) NOT VALID;
ALTER TABLE doc_updates ADD CONSTRAINT doc_updates_aad_version CHECK (aad_version = 1) NOT VALID;
ALTER TABLE doc_updates ADD CONSTRAINT doc_updates_envelope_size
    CHECK (octet_length(encrypted_update) BETWEEN 16 AND 1048576) NOT VALID;

ALTER TABLE doc_snapshots ADD CONSTRAINT doc_snapshots_nonce_length
    CHECK (octet_length(nonce) = 12) NOT VALID;
ALTER TABLE doc_snapshots ADD CONSTRAINT doc_snapshots_aad_version CHECK (aad_version = 1) NOT VALID;
ALTER TABLE doc_snapshots ADD CONSTRAINT doc_snapshots_envelope_size
    CHECK (octet_length(encrypted_snapshot) BETWEEN 16 AND 10485760) NOT VALID;

ALTER TABLE docs ADD CONSTRAINT docs_title_nonce_length CHECK (octet_length(title_nonce) = 12) NOT VALID;
ALTER TABLE docs ADD CONSTRAINT docs_title_envelope_size
    CHECK (octet_length(encrypted_title) BETWEEN 16 AND 16384) NOT VALID;
