CREATE TABLE IF NOT EXISTS clips (
    id           INTEGER PRIMARY KEY,
    kind         TEXT    NOT NULL,
    hash         BLOB    NOT NULL UNIQUE,
    preview      TEXT    NOT NULL,
    thumbnail    BLOB,
    source_app   TEXT,
    pinned       INTEGER NOT NULL DEFAULT 0,
    created_at   INTEGER NOT NULL,
    last_used_at INTEGER NOT NULL,
    size_bytes   INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS formats (
    clip_id INTEGER NOT NULL REFERENCES clips(id) ON DELETE CASCADE,
    mime    TEXT    NOT NULL,
    data    BLOB    NOT NULL,
    PRIMARY KEY (clip_id, mime)
);

CREATE VIRTUAL TABLE IF NOT EXISTS clips_fts USING fts5(
    preview, content = 'clips', content_rowid = 'id'
);

CREATE TRIGGER IF NOT EXISTS clips_ai AFTER INSERT ON clips BEGIN
    INSERT INTO clips_fts(rowid, preview) VALUES (new.id, new.preview);
END;

CREATE TRIGGER IF NOT EXISTS clips_ad AFTER DELETE ON clips BEGIN
    INSERT INTO clips_fts(clips_fts, rowid, preview) VALUES ('delete', old.id, old.preview);
END;

CREATE TRIGGER IF NOT EXISTS clips_au AFTER UPDATE OF preview ON clips BEGIN
    INSERT INTO clips_fts(clips_fts, rowid, preview) VALUES ('delete', old.id, old.preview);
    INSERT INTO clips_fts(rowid, preview) VALUES (new.id, new.preview);
END;
