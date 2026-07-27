-- Durable account security, recovery envelopes, device revocation, and key requests.
ALTER TABLE users ADD COLUMN email_verified_at TIMESTAMPTZ;

CREATE TABLE auth_tokens (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    token_hash BYTEA NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
    purpose TEXT NOT NULL CHECK (purpose IN ('email_verification', 'password_reset')),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    email TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL CHECK (expires_at > created_at),
    consumed_at TIMESTAMPTZ,
    delivery_idempotency_key UUID NOT NULL UNIQUE
);
CREATE INDEX auth_tokens_expiry_idx ON auth_tokens(expires_at);
CREATE INDEX auth_tokens_user_purpose_idx ON auth_tokens(user_id, purpose);

CREATE TABLE rate_limits (
    scope TEXT NOT NULL,
    client_key_hash BYTEA NOT NULL CHECK (octet_length(client_key_hash) = 32),
    window_started_at TIMESTAMPTZ NOT NULL,
    attempts INTEGER NOT NULL CHECK (attempts > 0),
    expires_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (scope, client_key_hash)
);
CREATE INDEX rate_limits_expiry_idx ON rate_limits(expires_at);

ALTER TABLE devices ADD COLUMN revoked_at TIMESTAMPTZ;
CREATE INDEX devices_active_user_idx ON devices(user_id) WHERE revoked_at IS NULL;

CREATE TABLE account_recovery_envelopes (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('passphrase', 'recovery_key')),
    version BIGINT NOT NULL CHECK (version > 0),
    ciphertext BYTEA NOT NULL CHECK (octet_length(ciphertext) BETWEEN 16 AND 65536),
    nonce BYTEA NOT NULL CHECK (octet_length(nonce) = 12),
    aad_version INTEGER NOT NULL CHECK (aad_version > 0),
    wrapping_version INTEGER NOT NULL CHECK (wrapping_version > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (user_id, kind)
);

ALTER TABLE encrypted_workspace_keys ADD COLUMN version BIGINT NOT NULL DEFAULT 1;
ALTER TABLE encrypted_workspace_keys ADD COLUMN updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW();

ALTER TABLE invitations ADD COLUMN revoked_at TIMESTAMPTZ;
ALTER TABLE invitations ADD COLUMN last_sent_at TIMESTAMPTZ;
ALTER TABLE invitations ADD COLUMN delivery_idempotency_key UUID;
DROP INDEX IF EXISTS invitations_one_pending_email;
CREATE UNIQUE INDEX invitations_one_pending_email ON invitations(workspace_id, LOWER(invitee_email))
    WHERE accepted = FALSE AND revoked_at IS NULL;

CREATE TABLE invitation_key_requests (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    invitation_id UUID NOT NULL UNIQUE REFERENCES invitations(id) ON DELETE CASCADE,
    workspace_id UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    public_key BYTEA NOT NULL CHECK (octet_length(public_key) = 32),
    key_version INTEGER NOT NULL CHECK (key_version > 0),
    requested_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    approved_at TIMESTAMPTZ,
    approved_by UUID REFERENCES users(id),
    UNIQUE (workspace_id, user_id)
);
CREATE INDEX invitation_key_requests_pending_idx ON invitation_key_requests(workspace_id)
    WHERE approved_at IS NULL;

-- Existing acceptance semantics are replaced by approval after an opaque envelope is uploaded.
ALTER TABLE invitations DROP CONSTRAINT IF EXISTS invitations_acceptance_state_check;

CREATE INDEX auth_sessions_device_id_idx ON auth_sessions(device_id);
