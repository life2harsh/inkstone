-- A public link is a read-only member with no login. Anyone holding its
-- token can read the vault's ciphertext; the key travels only in the
-- link's fragment, so the server still sees nothing readable.
ALTER TABLE users ADD COLUMN public_link_workspace_id UUID
    REFERENCES workspaces(id) ON DELETE CASCADE;
CREATE UNIQUE INDEX users_public_link_workspace_unique
    ON users(public_link_workspace_id) WHERE public_link_workspace_id IS NOT NULL;
ALTER TABLE users ADD CONSTRAINT users_public_link_has_no_login
    CHECK (public_link_workspace_id IS NULL OR email IS NULL);
