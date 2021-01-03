-- Your SQL goes here

PRAGMA encoding = 'UTF-8';

CREATE TABLE tSong (
    key        TEXT PRIMARY KEY NOT NULL  CHECK (typeof(key) = 'text'),
    hash       TEXT NOT NULL              CHECK (typeof(hash) = 'text'),
    tstamp     BIGINT NOT NULL            CHECK (typeof(tstamp) = 'integer'),
    deleted    BOOLEAN NOT NULL DEFAULT 0 CHECK (typeof(deleted) = 'integer'),
    -- A tar of just the .dat files
    data       BLOB                       CHECK (typeof(data) = 'blob' OR typeof(data) = null),
    extra_meta BLOB                       CHECK (typeof(extra_meta) = 'blob' OR typeof(extra_meta) = null),
    -- Raw zip data
    zipdata    BLOB                       CHECK (typeof(zipdata) = 'blob' OR typeof(zipdata) = null),
    -- Beatsaver JSON
    bsmeta     BLOB                       CHECK (typeof(bsmeta) = 'blob' OR typeof(bsmeta) = null),
    CHECK (deleted = 0 OR deleted = 1),
    CHECK ((data IS NULL AND extra_meta IS NULL AND zipdata IS NULL) OR (data IS NOT NULL AND extra_meta IS NOT NULL AND zipdata IS NOT NULL))
);

-- alter table tsong add column bsmeta BLOB CHECK (typeof(bsmeta) = 'blob' OR typeof(bsmeta) = null);
