/// This file is for on-off scripts that will not be used regularly
use diesel::prelude::*;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::io::BufReader;
use std::thread;


use super::INFO_PAUSE;
use super::models::*;
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

/// Load JSON from a dump of the bsaber.com DB that I've manually munged
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


/// Validate that every song marked as deleted, is in fact deleted
pub fn checkdeleted() {
    let conn = &super::establish_connection();
    let client = &super::make_client();

    fn load_deleteds() -> BTreeMap<String, bool> {
        if !Path::new("deleteds.json").is_file() {
            fs::write("deleteds.json", b"{}").expect("failed to created empty deleteds file")
        }
        serde_json::from_reader(fs::File::open("deleteds.json").expect("deleteds open failed")).expect("deleteds load failed")
    }
    fn save_deleteds(deleteds: &BTreeMap<String, bool>) {
        serde_json::to_writer(fs::File::create("deleteds.json").expect("deleteds open failed"), &deleteds).expect("deleteds save failed")
    }

    println!("Loading all deleted songs from db");
    let mut deleteds = load_deleteds();
    let currently_deleted: Vec<i32> = {
        use schema::tSong::dsl::*;
        tSong
            .select(key)
            .filter(deleted.eq(true))
            .load(conn).expect("failed to select keys")
    };
    let num_to_check = currently_deleted.len();
    println!("Checking {} deleted songs", num_to_check);
    for (i, key) in currently_deleted.into_iter().enumerate() {
        let key_str = num_to_key(key);
        println!("Considering song {} ({}/{})", key_str, i+1, num_to_check);
        if deleteds.contains_key(&key_str) {
            continue
        }
        let is_deleted = super::get_map(client, key).expect("failed to get map detail").is_none();
        assert!(deleteds.insert(key_str, is_deleted).is_none());
        save_deleteds(&deleteds);

        thread::sleep(INFO_PAUSE);
    }

    let needs_undeleting: Vec<_> = deleteds.into_iter().filter_map(|(k, deleted)| if !deleted { Some(k) } else { None }).collect();
    let needs_undeleting_i32s: Vec<_> = needs_undeleting.iter().map(key_to_num).collect();
    println!("The following keys are marked as deleted but need undeleting: {:?}", needs_undeleting);
    println!("(as integers: {:?})", needs_undeleting_i32s);
}

/// For any song without a bsmeta and that isn't deleted, update it with one or the other
pub fn getmissingbsmeta() {
    let conn = &super::establish_connection();
    let client = &super::make_client();

    println!("Loading all songs with missing meta from DB");
    let missing_meta: Vec<Song> = {
        use schema::tSong::dsl::*;
        tSong
            .filter(deleted.eq(false))
            .filter(bsmeta.is_null())
            .load(conn).expect("failed to select keys")
    };

    let num_to_update = missing_meta.len();
    println!("Updating {} songs with missing meta", num_to_update);
    for (i, song) in missing_meta.into_iter().enumerate() {
        let key_str = num_to_key(song.key);
        println!("Considering song {} ({}/{})", key_str, i+1, num_to_update);

        assert!(song.bsmeta.is_none());
        match super::get_map(client, song.key).expect("failed to get map detail") {
            Some((m, raw)) => {
                assert_eq!(m.key, key_str);
                if let Some(hash) = song.hash {
                    assert_eq!(m.hash, hash);
                }
                super::upsert_song(conn, song.key, Some(m.hash), false, Some(raw.get().as_bytes().to_owned()))
            },
            None => {
                super::upsert_song(conn, song.key, song.hash, true, song.bsmeta)
            },
        }

        thread::sleep(INFO_PAUSE)
    }
}

/// Regenerate all extrameta and infodats
pub fn regenzipderived() {
    use super::zip_to_dats_tar;

    let conn = &super::establish_connection();

    println!("Finding all song data");
    let needs_regenerating: Vec<i32> = {
        schema::tSongData::table
            .select(schema::tSongData::key)
            .load(conn).expect("failed to select keys")
    };

    let num_to_regenerate = needs_regenerating.len();
    println!("Regenerating data for {} songs", num_to_regenerate);
    for (i, key) in needs_regenerating.into_iter().enumerate() {
        let key_str = num_to_key(key);
        println!("Regenerating derived data for {} ({}/{})", key_str, i+1, num_to_regenerate);
        let zip: Vec<u8> = {
            schema::tSongData::table.find(key)
                .select(schema::tSongData::zipdata)
                .get_result(conn)
                .expect("failed to load zipdata")
        };
        let (newdata, new_extra_meta) = zip_to_dats_tar(&zip).expect("failed to reprocess zip");
        diesel::update(schema::tSongData::table.find(key))
            .set((
                schema::tSongData::data.eq(newdata),
                schema::tSongData::extra_meta.eq(new_extra_meta),
            ))
            .execute(conn)
            .expect("error saving data");
    }
}
