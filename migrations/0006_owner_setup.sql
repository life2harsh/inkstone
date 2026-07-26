-- One-time, operator-issued owner setup links.
CREATE TABLE owner_setup (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    email TEXT NOT NULL,
    token_hash BYTEA UNIQUE,
    issued_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ,
    delivery_idempotency_key UUID,
    delivered_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    owner_user_id UUID UNIQUE REFERENCES users(id),
    CONSTRAINT owner_setup_state_check CHECK (
        (
            completed_at IS NULL
            AND owner_user_id IS NULL
            AND token_hash IS NOT NULL
            AND octet_length(token_hash) = 32
            AND issued_at IS NOT NULL
            AND expires_at > issued_at
            AND delivery_idempotency_key IS NOT NULL
        )
        OR (
            completed_at IS NOT NULL
            AND owner_user_id IS NOT NULL
            AND token_hash IS NULL
            AND expires_at IS NULL
        )
    )
);

-- Existing installations are already bootstrapped and must never become setup-eligible.
INSERT INTO owner_setup (email, completed_at, owner_user_id)
SELECT LOWER(email), NOW(), id
FROM users
WHERE email IS NOT NULL AND password_hash IS NOT NULL
ORDER BY created_at, id
LIMIT 1;
