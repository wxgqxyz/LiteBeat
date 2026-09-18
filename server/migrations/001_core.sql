-- LiteBeat 迁移 001：核心表与索引。
-- 外键由每个连接的 PRAGMA foreign_keys=ON 强制；本文件不重复设置。

CREATE TABLE users (
    id            INTEGER PRIMARY KEY,
    username      TEXT NOT NULL UNIQUE COLLATE NOCASE,
    password_hash TEXT NOT NULL,
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE sessions (
    token_hash  BLOB PRIMARY KEY,
    user_id     INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    csrf_token  BLOB NOT NULL UNIQUE,
    expires_at  TEXT NOT NULL
);
CREATE INDEX idx_sessions_expiry ON sessions(expires_at);

CREATE TABLE library_roots (
    id             INTEGER PRIMARY KEY,
    name           TEXT NOT NULL UNIQUE,
    canonical_path TEXT NOT NULL UNIQUE,
    enabled        INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1))
);

CREATE TABLE albums (
    id           INTEGER PRIMARY KEY,
    root_id      INTEGER NOT NULL REFERENCES library_roots(id) ON DELETE CASCADE,
    directory_key TEXT NOT NULL,
    title        TEXT NOT NULL,
    album_artist TEXT NOT NULL DEFAULT '',
    sort_title   TEXT NOT NULL,
    UNIQUE (root_id, directory_key, title, album_artist)
);
CREATE INDEX idx_albums_root_directory ON albums(root_id, directory_key);

CREATE TABLE tracks (
    id              INTEGER PRIMARY KEY,
    root_id         INTEGER NOT NULL REFERENCES library_roots(id) ON DELETE CASCADE,
    relative_path   TEXT NOT NULL,
    title           TEXT NOT NULL,
    artist          TEXT NOT NULL DEFAULT '',
    album_id        INTEGER REFERENCES albums(id) ON DELETE SET NULL,
    album_title     TEXT NOT NULL DEFAULT '',
    duration_ms     INTEGER,
    codec           TEXT,
    mime            TEXT,
    size_bytes      INTEGER NOT NULL CHECK (size_bytes >= 0),
    mtime_ns        INTEGER NOT NULL,
    sort_title      TEXT NOT NULL,
    sort_artist     TEXT NOT NULL,
    sort_album      TEXT NOT NULL,
    disc_no         INTEGER NOT NULL DEFAULT 0,
    track_no        INTEGER NOT NULL DEFAULT 0,
    available       INTEGER NOT NULL DEFAULT 1 CHECK (available IN (0, 1)),
    last_seen_scan_id INTEGER REFERENCES scan_jobs(id) ON DELETE SET NULL,
    UNIQUE (root_id, relative_path)
);
-- 计划要求短查询前缀与排序使用 NOCASE 规范化名称索引，id 为稳定次级键。
CREATE INDEX IF NOT EXISTS idx_tracks_title ON tracks(sort_title COLLATE NOCASE, id);
CREATE INDEX IF NOT EXISTS idx_tracks_artist ON tracks(sort_artist COLLATE NOCASE, id);
CREATE INDEX IF NOT EXISTS idx_tracks_album ON tracks(sort_album COLLATE NOCASE, id);
CREATE INDEX idx_tracks_album_id ON tracks(album_id, disc_no, track_no, id);
CREATE INDEX idx_tracks_available ON tracks(available, id);

-- FTS5 trigram：3 字符及以上片段搜索；1–2 字符走上面的前缀索引。
CREATE VIRTUAL TABLE tracks_fts USING fts5(
    sort_title,
    sort_artist,
    sort_album,
    content='tracks',
    content_rowid='id',
    tokenize='trigram'
);

CREATE TRIGGER tracks_fts_ai AFTER INSERT ON tracks BEGIN
    INSERT INTO tracks_fts(rowid, sort_title, sort_artist, sort_album)
    VALUES (new.id, new.sort_title, new.sort_artist, new.sort_album);
END;

CREATE TRIGGER tracks_fts_ad AFTER DELETE ON tracks BEGIN
    INSERT INTO tracks_fts(tracks_fts, rowid, sort_title, sort_artist, sort_album)
    VALUES ('delete', old.id, old.sort_title, old.sort_artist, old.sort_album);
END;

CREATE TRIGGER tracks_fts_au AFTER UPDATE OF sort_title, sort_artist, sort_album ON tracks BEGIN
    INSERT INTO tracks_fts(tracks_fts, rowid, sort_title, sort_artist, sort_album)
    VALUES ('delete', old.id, old.sort_title, old.sort_artist, old.sort_album);
    INSERT INTO tracks_fts(rowid, sort_title, sort_artist, sort_album)
    VALUES (new.id, new.sort_title, new.sort_artist, new.sort_album);
END;

CREATE TABLE favorites (
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    track_id   INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (user_id, track_id)
);
CREATE INDEX idx_favorites_user_time ON favorites(user_id, created_at DESC, track_id);

CREATE TABLE playlists (
    id         INTEGER PRIMARY KEY,
    owner_id   INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name       TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    version    INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX idx_playlists_owner ON playlists(owner_id, id);

CREATE TABLE playlist_items (
    id          INTEGER PRIMARY KEY,
    playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    track_id    INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    position    INTEGER NOT NULL CHECK (position >= 0),
    UNIQUE (playlist_id, position)
);
CREATE INDEX idx_playlist_items_order ON playlist_items(playlist_id, position, id);

CREATE TABLE play_history (
    id         INTEGER PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    track_id   INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    event_id   TEXT NOT NULL,
    played_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (user_id, event_id)
);
CREATE INDEX idx_history_user_time ON play_history(user_id, played_at DESC, id);

CREATE TABLE scan_jobs (
    id          INTEGER PRIMARY KEY,
    root_id     INTEGER NOT NULL REFERENCES library_roots(id) ON DELETE CASCADE,
    state       TEXT NOT NULL CHECK (state IN ('queued', 'running', 'completed', 'failed', 'cancelled', 'interrupted')),
    force       INTEGER NOT NULL DEFAULT 0 CHECK (force IN (0, 1)),
    scanned     INTEGER NOT NULL DEFAULT 0,
    updated     INTEGER NOT NULL DEFAULT 0,
    failed      INTEGER NOT NULL DEFAULT 0,
    started_at  TEXT,
    finished_at TEXT
);
CREATE INDEX idx_scan_jobs_root_state ON scan_jobs(root_id, state, id);

CREATE TABLE scan_errors (
    job_id        INTEGER NOT NULL REFERENCES scan_jobs(id) ON DELETE CASCADE,
    relative_path TEXT NOT NULL,
    code          TEXT NOT NULL,
    PRIMARY KEY (job_id, relative_path)
);

CREATE TABLE request_dedup (
    user_id       INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    key           TEXT NOT NULL,
    method        TEXT NOT NULL,
    path          TEXT NOT NULL,
    request_hash  BLOB NOT NULL,
    response_json TEXT NOT NULL,
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    expires_at    TEXT NOT NULL,
    PRIMARY KEY (user_id, key)
);
CREATE INDEX idx_request_dedup_expiry ON request_dedup(expires_at);
