-- Your SQL goes here

CREATE TABLE tSong (
    key     TEXT PRIMARY KEY NOT NULL  CHECK (typeof(key) = 'text'),
    hash    TEXT NOT NULL              CHECK (typeof(hash) = 'text'),
    tstamp  BIGINT NOT NULL            CHECK (typeof(tstamp) = 'integer'),
    data    BLOB                       CHECK (typeof(data) = 'blob' OR typeof(data) = null),
    deleted BOOLEAN NOT NULL DEFAULT 0 CHECK (typeof(deleted) = 'integer'),
    CHECK (deleted = 0 OR deleted = 1)
);
