#![recursion_limit="256"]

#[macro_use]
extern crate diesel;

use chrono::Utc;
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use dotenv::dotenv;
use serde::Deserialize;
use std::collections::HashSet;
use std::env;
use std::fs;
use std::io::{self, BufReader, Read};
use std::thread;
use std::time;

mod schema;
use schema::tSong;

#[derive(AsChangeset, Identifiable, Insertable, Queryable)]
#[derive(Debug, Eq, PartialEq)]
#[primary_key(key)]
#[table_name="tSong"]
#[changeset_options(treat_none_as_null="true")]
struct Song {
    key: String,
    hash: String,
    tstamp: i64,
    data: Option<Vec<u8>>,
    deleted: bool,
}

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

fn main() {
    //loadjson();
    println!("Considering missing keys");
    println!("Missing keys: {:?}", missing_songs().len());

    dl_deets();
    //analyse_songs();
}

fn loadjson() {
    let conn = establish_connection();

    let f = fs::File::open("../songsdata.json").unwrap();
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
        upsert_song(&conn, song_key, song_hash, post_status != "publish", None);
    }
}

fn key_to_num<T: AsRef<str>>(k: &T) -> u64 {
    u64::from_str_radix(k.as_ref(), 16).expect("can't parse key")
}

fn num_to_key(n: u64) -> String {
    format!("{:x}", n)
}

fn missing_songs() -> Vec<String> {
    use schema::tSong::dsl::*;
    let conn = establish_connection();

    let mut results: Vec<String> = tSong.select(key).load(&conn).expect("failed to select keys");
    println!("Loaded keys");
    results.sort_by_key(key_to_num);

    let mut missing = vec![];
    let mut cur = 1;
    results.reverse();
    let mut next_target = results.pop();
    while let Some(target) = &next_target {
        let target_num = key_to_num(target);
        assert!(cur <= target_num);
        if cur == target_num {
            next_target = results.pop()
        } else {
            missing.push(cur);
        }
        cur += 1;
    }
    missing.into_iter().map(num_to_key).collect()
}

fn analyse_songs() {
    println!("Analysing songs");
    let conn = &establish_connection();

    let to_analyse: Vec<Song> = {
        use schema::tSong::dsl::*;
        tSong
            .filter(data.is_not_null())
            .filter(deleted.eq(false))
            .load(conn).expect("failed to select keys")
    };

    for song in to_analyse {
        let data = song.data.expect("no song data");
        let mut tar = tar::Archive::new(&*data);
        let mut names_lens = vec![];
        for entry in tar.entries().expect("couldn't iterate over entries") {
            let entry = entry.expect("error getting entry");
            let path = entry.path().expect("couldn't get path");
            let path_str = path.to_str().expect("couldn't convert path");
            names_lens.push((path_str.to_owned(), entry.size()))
        }
        println!("{:?}", names_lens);
    }
}

fn dl_deets() {
    let conn = &establish_connection();

    println!("Finding songs to download");
    let mut to_download: Vec<Song> = {
        use schema::tSong::dsl::*;
        tSong
            .filter(data.is_null())
            .filter(deleted.eq(false))
            .load(conn).expect("failed to select keys")
    };
    println!("Got {} to download", to_download.len());
    to_download.sort_by_key(|s| key_to_num(&s.key));
    to_download.reverse();

    let client = reqwest::blocking::Client::builder()
        .user_agent("aidanhsmetaclient/0.1 (@aidanhs on discord, aidanhs@cantab.net)")
        .build().expect("failed to create reqwest client");

    //let song = to_download.into_iter().find(|s| s.key == "11b01").unwrap();
    for song in to_download {
        println!("Considering song {}", song.key);
        println!("Getting song zip for {} {}", song.key, song.hash);
        let zipdata = get_song_zip(&client, &song.key, &song.hash);
        println!("Converting zip for {} to dats tar", song.key);
        let tardata = zip_to_dats_tar(&zipdata);
        let key = song.key.clone();
        upsert_song(conn, song.key, song.hash, song.deleted, Some(tardata));
        println!("Finished getting song {}", key);
        thread::sleep(time::Duration::from_secs(60));
    }
}

fn get_song_zip(client: &reqwest::blocking::Client, key: &str, hash: &str) -> Vec<u8> {
    //let url = format!("https://beatsaver.com/cdn/{}/{}.zip", key, hash);
    //println!("Retrieving {} from url {}", key, url);
    //let mut res = client.get(&url).send().expect("failed to send request");
    //println!("Got response {}", res.status());
    //if res.status() == reqwest::StatusCode::UNAUTHORIZED {
    //    let url = format!("https://bsaber.org/files/cache/zip/{}.zip", key);
    //    println!("Retrying with url {}", url);
    //    res = client.get(&url).send().expect("failed to send request");
    //    println!("Got response {}", res.status());
    //}
    let url = format!("https://bsaber.org/files/cache/zip/{}.zip", key);
    println!("Retrieving {} from url {}", key, url);
    let mut res = client.get(&url).send().expect("failed to send request");
    println!("Got response {}", res.status());

    let headers = res.headers().to_owned();
    if !res.status().is_success() {
        println!("Response headers: {:?}", headers);
        panic!("request failure")
    }
    let bytes = match res.bytes() {
        Ok(bs) => bs,
        Err(e) => {
            println!("Failed to get bytes: {}", e);
            println!("Response headers: {:?}", headers);
            panic!("request failure")
        },
    };
    bytes.as_ref().to_owned()

    //println!("Retrieving {} from fs", key);
    //let path = format!("../beatmaps/{}.zip", hash);
    //let zipdata = fs::read(path).unwrap();
    //zipdata
}

fn zip_to_dats_tar(zipdata: &[u8]) -> Vec<u8> {
    let zipreader = io::Cursor::new(zipdata);
    let mut zip = zip::ZipArchive::new(zipreader).expect("failed to load zip");
    let names: Vec<String> = zip.file_names()
        .filter(|name| {
            assert!(name.is_ascii(), "{}", name);
            assert!(name.find("/").is_none() && name.find("\\").is_none(), "{}", name);
            let name = name.to_lowercase();
            if name.ends_with(".egg") || name.ends_with(".jpg") || name.ends_with(".png") {
                false
            } else {
                assert!(name.ends_with(".dat"), "{}", name);
                true
            }
        })
        .map(str::to_owned).collect();
    println!("Got names: {:?}", names);

    let mut tar = tar::Builder::new(vec![]);
    for dat_name in names {
        let dat = zip.by_name(&dat_name).expect("failed to get dat name out of zip");
        let mut header = tar::Header::new_old();
        header.set_path(dat_name).expect("failed to set path");
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(dat.size());
        header.set_cksum();
        tar.append(&header, dat).expect("failed to append data to tar");
    }
    tar.into_inner().expect("failed to finish tar")
}

fn upsert_song(conn: &SqliteConnection, key: String, hash: String, deleted: bool, data: Option<Vec<u8>>) {
    let tstamp = Utc::now().timestamp_millis();
    let new_song = Song { key, hash, tstamp, deleted, data };

    let nrows = if let Some(mut cur_song) = get_song(&new_song.key) {
        assert_eq!((&new_song.key, &new_song.hash), (&cur_song.key, &cur_song.hash));
        cur_song.tstamp = tstamp;
        if cur_song == new_song {
            return
        }
        diesel::update(&new_song).set(&new_song)
            .execute(conn)
            .expect("error updating song")
    } else {
        diesel::insert_into(tSong::table)
            .values(&new_song)
            .execute(conn)
            .expect("error saving song")
    };
    assert_eq!(nrows, 1, "{}", new_song.key)
}

fn get_song(song_key: &str) -> Option<Song> {
    use schema::tSong::dsl::*;
    let conn = establish_connection();

    let mut results: Vec<Song> = tSong
        .filter(key.eq(song_key))
        .load(&conn)
        .expect("failed to load song");
    assert!(results.len() == 1 || results.is_empty());
    results.pop()
}

pub fn establish_connection() -> SqliteConnection {
    dotenv().ok();

    let database_url = env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set");
    SqliteConnection::establish(&database_url)
        .expect(&format!("Error connecting to {}", database_url))
}
