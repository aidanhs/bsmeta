table! {
    tSong (key) {
        key -> Text,
        hash -> Text,
        tstamp -> BigInt,
        data -> Nullable<Binary>,
        deleted -> Bool,
    }
}
