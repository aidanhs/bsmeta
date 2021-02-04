-- Your SQL goes here

PRAGMA encoding = 'UTF-8';

CREATE TABLE tSong (
    key        INTEGER PRIMARY KEY NOT NULL  CHECK (typeof(key) = 'integer'),
    -- TODO: there's a hash in here that's just '-' ???
    hash       TEXT                          CHECK (typeof(hash) = 'text' OR hash IS NULL),
    tstamp     BIGINT NOT NULL               CHECK (typeof(tstamp) = 'integer'),
    deleted    BOOLEAN NOT NULL DEFAULT 0    CHECK (typeof(deleted) = 'integer'),
    -- Beatsaver JSON
    bsmeta     BLOB                          CHECK (typeof(bsmeta) = 'blob' OR bsmeta IS NULL),
    -- TODO: once I have old metas, augment this check constraint to have a hash+bsmeta when deleted = 0
    CHECK (deleted = 0 OR deleted = 1)
);
-- TODO: figure why this can't be unique
CREATE INDEX iSong1 ON tSong(hash);

CREATE TABLE tSongData (
    key        INTEGER PRIMARY KEY NOT NULL CHECK (typeof(key) = 'integer'),
    -- Raw zip data
    zipdata    BLOB                NOT NULL CHECK (typeof(zipdata) = 'blob'),
    -- A tar of just the .dat files
    data       BLOB                NOT NULL CHECK (typeof(data) = 'blob'),
    -- My derived extra meta
    extra_meta BLOB                NOT NULL CHECK (typeof(extra_meta) = 'blob'),
    FOREIGN KEY (key) REFERENCES tSong(key)
);

CREATE TABLE tSongAnalysis (
    key           INTEGER NOT NULL CHECK (typeof(key) = 'integer'),
    analysis_name TEXT NOT NULL    CHECK (typeof(analysis_name) = 'text'),
    result        BLOB NOT NULL    CHECK (typeof(result) = 'blob'),
    PRIMARY KEY (key, analysis_name),
    FOREIGN KEY (key) REFERENCES tSong(key)
);
