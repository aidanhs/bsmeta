/// This file is for on-off scripts that will not be used regularly
use async_std::task;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::thread;
use sqlx::prelude::*;
use sqlx::query;

use super::INFO_PAUSE;
use super::{key_to_num, num_to_key};

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
    let currently_deleted = task::block_on(
        query!("SELECT key FROM tSong WHERE deleted = true").fetch_all(conn)
    ).expect("failed to select keys");
    let currently_deleted: Vec<_> = currently_deleted.into_iter().map(|res| res.key).collect();
    let num_to_check = currently_deleted.len();
    println!("Checking {} deleted songs", num_to_check);
    for (i, key) in currently_deleted.into_iter().enumerate() {
        let key_str = num_to_key(key);
        println!("Considering song {} ({}/{})", key_str, i+1, num_to_check);
        if deleteds.contains_key(&key_str) {
            continue
        }
        let is_deleted = super::get_map_meta(client, key).expect("failed to get map detail").is_none();
        assert!(deleteds.insert(key_str, is_deleted).is_none());
        save_deleteds(&deleteds);

        thread::sleep(INFO_PAUSE);
    }

    let needs_undeleting: Vec<_> = deleteds.into_iter().filter_map(|(k, deleted)| if !deleted { Some(k) } else { None }).collect();
    let needs_undeleting_i64s: Vec<_> = needs_undeleting.iter().map(key_to_num).collect();
    println!("The following keys are marked as deleted but need undeleting: {:?}", needs_undeleting);
    println!("(as integers: {:?})", needs_undeleting_i64s);
}

/// Regenerate all extrameta and infodats
pub fn regenzipderived() {
    use super::zip_to_dats_tar;

    let conn = &super::establish_connection();

    println!("Finding all song data");
    let needs_regenerating = task::block_on(query!("SELECT hash FROM tSongData").fetch_all(conn)).expect("failed to select hashes");
    let needs_regenerating: Vec<_> = needs_regenerating.into_iter().map(|res| res.hash).collect();

    let num_to_regenerate = needs_regenerating.len();
    println!("Regenerating data for {} songs", num_to_regenerate);
    for (i, hash) in needs_regenerating.into_iter().enumerate() {
        println!("Regenerating derived data for {} ({}/{})", hash, i+1, num_to_regenerate);
        let zip = task::block_on(
            query!("SELECT zipdata FROM tSongData WHERE hash = ?", hash).fetch_one(conn)
        ).expect("failed to load zipdata").zipdata;
        let (newdata, new_extra_meta) = zip_to_dats_tar(&zip).expect("failed to reprocess zip");
        let res = task::block_on(query!("
            UPDATE tSongData
            SET data = ?, extra_meta = ?
            WHERE hash = ?
        ", newdata, new_extra_meta, hash).execute(conn)).expect("error saving data");
        assert_eq!(res.rows_affected(), 1, "insert {}", hash)
    }
}
