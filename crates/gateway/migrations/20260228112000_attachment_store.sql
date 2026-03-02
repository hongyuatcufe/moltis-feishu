CREATE TABLE IF NOT EXISTS attachment_blobs (
    sha256            TEXT PRIMARY KEY,
    media_type        TEXT    NOT NULL,
    ext               TEXT    NOT NULL,
    size_bytes        INTEGER NOT NULL,
    storage_path      TEXT    NOT NULL,
    created_at        INTEGER NOT NULL,
    last_accessed_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS attachment_refs (
    id            TEXT PRIMARY KEY,
    session_key   TEXT    NOT NULL,
    channel_type  TEXT    NOT NULL,
    account_id    TEXT    NOT NULL,
    chat_id       TEXT    NOT NULL,
    message_id    TEXT,
    blob_sha256   TEXT    NOT NULL,
    original_name TEXT,
    created_at    INTEGER NOT NULL,
    FOREIGN KEY(blob_sha256) REFERENCES attachment_blobs(sha256)
);

CREATE INDEX IF NOT EXISTS idx_attachment_refs_session_created
    ON attachment_refs(session_key, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_attachment_refs_channel_chat_created
    ON attachment_refs(channel_type, account_id, chat_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_attachment_refs_blob
    ON attachment_refs(blob_sha256);
