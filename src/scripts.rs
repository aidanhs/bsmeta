/// This file is for on-off scripts that will not be used regularly
use diesel::prelude::*;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::io::BufReader;
use std::thread;

use super::INFO_PAUSE;
use super::schema;
use super::{key_to_num, num_to_key};

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
        super::insert_song(&conn, key_to_num(&song_key), Some(song_hash), post_status != "publish", None);
    }
}


pub fn checkdeleted() {
    let conn = &super::establish_connection();
    let client = super::make_client();

    fn load_deleteds() -> BTreeMap<String, bool> {
        if !Path::new("deleteds.json").is_file() {
            fs::write("deleteds.json", b"{}").expect("failed to created empty deleteds file")
        }
        serde_json::from_reader(fs::File::open("deleteds.json").expect("deleteds open failed")).expect("deleteds load failed")
    }
    fn save_deleteds(deleteds: &BTreeMap<String, bool>) {
        serde_json::to_writer(fs::File::create("deleteds.json").expect("deleteds open failed"), &deleteds).expect("deleteds save failed")
    }

    let mut deleteds = load_deleteds();
    let currently_deleted: Vec<i32> = {
        use schema::tSong::dsl::*;
        tSong
            .select(key)
            .filter(deleted.eq(true))
            .load(conn).expect("failed to select keys")
    };
    let num_to_check = currently_deleted.len();
    for (i, key) in currently_deleted.into_iter().enumerate() {
        let key_str = num_to_key(key);
        println!("Considering song {} ({}/{})", key_str, i+1, num_to_check);
        if deleteds.contains_key(&key_str) {
            continue
        }
        let is_deleted = super::get_map(&client, key).expect("failed to get map detail").is_none();
        assert!(deleteds.insert(key_str, is_deleted).is_none());
        save_deleteds(&deleteds);

        thread::sleep(INFO_PAUSE);
    }

    let needs_undeleting: Vec<_> = deleteds.into_iter().filter_map(|(k, deleted)| if !deleted { Some(k) } else { None }).collect();
    let needs_undeleting_i32s: Vec<_> = needs_undeleting.iter().map(key_to_num).collect();
    println!("The following keys are marked as deleted but need undeleting: {:?}", needs_undeleting);
    println!("(as integers: {:?})", needs_undeleting_i32s);
}
