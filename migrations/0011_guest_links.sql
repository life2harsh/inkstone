ALTER TABLE workspaces ADD COLUMN link_guests_can_edit BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE users ADD COLUMN guest_workspace_id UUID
    REFERENCES workspaces(id) ON DELETE CASCADE;
ALTER TABLE users ADD COLUMN encrypted_guest_name BYTEA;
ALTER TABLE users ADD COLUMN guest_name_nonce BYTEA;
CREATE INDEX users_guest_workspace_idx
    ON users(guest_workspace_id) WHERE guest_workspace_id IS NOT NULL;
ALTER TABLE users ADD CONSTRAINT users_guest_has_no_login
    CHECK (guest_workspace_id IS NULL OR email IS NULL);
ALTER TABLE users ADD CONSTRAINT users_guest_name_shape CHECK (
    (guest_workspace_id IS NULL AND encrypted_guest_name IS NULL AND guest_name_nonce IS NULL)
    OR (guest_workspace_id IS NOT NULL
        AND octet_length(encrypted_guest_name) BETWEEN 17 AND 512
        AND octet_length(guest_name_nonce) = 12)
);
