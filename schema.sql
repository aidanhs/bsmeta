-- Your SQL goes here

PRAGMA encoding = 'UTF-8';
PRAGMA foreign_keys = ON;

CREATE TABLE tSong (
    key     INTEGER PRIMARY KEY NOT NULL CHECK (typeof(key) = 'integer'),
    deleted BOOLEAN NOT NULL DEFAULT 0   CHECK (typeof(deleted) = 'integer' AND (deleted = 0 OR deleted = 1)),
    tstamp  BIGINT NOT NULL              CHECK (typeof(tstamp) = 'integer')
);

-- These may not exist if the song has been deleted
CREATE TABLE tSongMeta (
    key    INTEGER PRIMARY KEY NOT NULL  CHECK (typeof(key) = 'integer'),
    hash   TEXT NOT NULL                 CHECK (typeof(hash) = 'text'),
    -- Beatsaver JSON
    bsmeta BLOB NOT NULL                 CHECK (typeof(bsmeta) = 'blob'),
    FOREIGN KEY (key) REFERENCES tSong(key)
);
CREATE INDEX iSongMeta1 ON tSongMeta(hash);

-- TODO: these aren't really freestanding, but hash is not a primary key of tSongMeta
-- so we can't reference it as a foreign key
CREATE TABLE tSongData (
    hash       TEXT PRIMARY KEY NOT NULL CHECK (typeof(hash) = 'text'),
    -- Raw zip data
    zipdata    BLOB NOT NULL             CHECK (typeof(zipdata) = 'blob'),
    -- A tar of just the .dat files
    data       BLOB NOT NULL             CHECK (typeof(data) = 'blob'),
    -- My derived extra meta
    extra_meta BLOB NOT NULL             CHECK (typeof(extra_meta) = 'blob')
);

CREATE TABLE tSongAnalysis (
    hash          TEXT NOT NULL CHECK (typeof(hash) = 'text'),
    analysis_name TEXT NOT NULL CHECK (typeof(analysis_name) = 'text'),
    result        BLOB NOT NULL CHECK (typeof(result) = 'blob'),
    PRIMARY KEY (hash, analysis_name),
    FOREIGN KEY (hash) REFERENCES tSongData(hash)
);
