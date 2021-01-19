table! {
    tSong (key) {
        key -> Integer,
        hash -> Nullable<Text>,
        tstamp -> BigInt,
        deleted -> Bool,
        bsmeta -> Nullable<Binary>,
    }
}

table! {
    tSongAnalysis (key, analysis_name) {
        key -> Integer,
        analysis_name -> Text,
        result -> Binary,
    }
}

table! {
    tSongData (key) {
        key -> Integer,
        zipdata -> Binary,
        data -> Binary,
        extra_meta -> Binary,
    }
}

joinable!(tSongAnalysis -> tSong (key));
joinable!(tSongData -> tSong (key));

allow_tables_to_appear_in_same_query!(
    tSong,
    tSongAnalysis,
    tSongData,
);
