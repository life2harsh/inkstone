ALTER TABLE docs
    ADD COLUMN encrypted_path BYTEA,
    ADD COLUMN path_nonce BYTEA,
    ADD COLUMN title_projection_seq BIGINT NOT NULL DEFAULT 0,
    ALTER COLUMN encrypted_title DROP NOT NULL,
    ALTER COLUMN title_nonce DROP NOT NULL;

-- Before this migration encrypted_title carried the path projection.
UPDATE docs
SET encrypted_path = encrypted_title,
    path_nonce = title_nonce,
    encrypted_title = NULL,
    title_nonce = NULL;

ALTER TABLE docs
    ALTER COLUMN encrypted_path SET NOT NULL,
    ALTER COLUMN path_nonce SET NOT NULL,
    ADD CONSTRAINT docs_title_projection_pair
        CHECK ((encrypted_title IS NULL) = (title_nonce IS NULL)),
    ADD CONSTRAINT docs_path_envelope_size
        CHECK (octet_length(encrypted_path) >= 16 AND octet_length(encrypted_path) <= 16384),
    ADD CONSTRAINT docs_path_nonce_length
        CHECK (octet_length(path_nonce) = 12),
    ADD CONSTRAINT docs_title_projection_seq_nonnegative
        CHECK (title_projection_seq >= 0);
