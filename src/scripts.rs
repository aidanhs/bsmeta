/// This file is for on-off scripts that will not be used regularly
use serde::Deserialize;
use std::fs;
use std::io::BufReader;

#[derive(Deserialize)]
struct RawSongData {
    song: RawSong,
    post: Option<RawPost>,
}

#[derive(Deserialize)]
struct RawSong {
    song_key: String,
    song_hash: String,
}

#[derive(Deserialize)]
struct RawPost {
    post_status: String,
}

pub fn loadjson() {
    let conn = super::establish_connection();

    let f = fs::File::open("songsdata.json").unwrap();
    let buf = BufReader::new(f);
    let song_data: Vec<RawSongData> = serde_json::from_reader(buf).unwrap();
    for (i, RawSongData { song, post }) in song_data.into_iter().enumerate() {
        if i % 100 == 0 {
            println!("At song: {}", i+1)
        }
        let RawSong { song_key, song_hash } = song;
        if post.is_none() { continue }
        let RawPost { post_status } = post.unwrap();
        assert!(post_status == "publish" || post_status == "draft" || post_status == "trash" || post_status == "private",
                "{} {}", song_key, post_status);
        super::insert_song(&conn, super::key_to_num(&song_key), Some(song_hash), post_status != "publish", None);
    }
}
