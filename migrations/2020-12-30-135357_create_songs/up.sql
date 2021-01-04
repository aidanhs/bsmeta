-- Your SQL goes here

PRAGMA encoding = 'UTF-8';

CREATE TABLE tSong (
    key        TEXT PRIMARY KEY NOT NULL  CHECK (typeof(key) = 'text'),
    hash       TEXT                       CHECK (typeof(hash) = 'text' OR hash IS NULL),
    tstamp     BIGINT NOT NULL            CHECK (typeof(tstamp) = 'integer'),
    deleted    BOOLEAN NOT NULL DEFAULT 0 CHECK (typeof(deleted) = 'integer'),
    -- A tar of just the .dat files
    data       BLOB                       CHECK (typeof(data) = 'blob' OR data IS NULL),
    extra_meta BLOB                       CHECK (typeof(extra_meta) = 'blob' OR extra_meta IS NULL),
    -- Raw zip data
    zipdata    BLOB                       CHECK (typeof(zipdata) = 'blob' OR zipdata IS NULL),
    -- Beatsaver JSON
    bsmeta     BLOB                       CHECK (typeof(bsmeta) = 'blob' OR bsmeta IS NULL),
    -- TODO: once I have old metas, augment this check constraint to have a bsmeta when deleted = 0
    CHECK (deleted = 0 OR deleted = 1),
    CHECK ((data IS NULL AND extra_meta IS NULL AND zipdata IS NULL) OR (data IS NOT NULL AND extra_meta IS NOT NULL AND zipdata IS NOT NULL))
);
