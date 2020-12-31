table! {
    tSong (key) {
        key -> Text,
        hash -> Text,
        tstamp -> BigInt,
        deleted -> Bool,
        data -> Nullable<Binary>,
        extra_meta -> Nullable<Binary>,
    }
}
