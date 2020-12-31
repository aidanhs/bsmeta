#![recursion_limit="256"]

#[macro_use]
extern crate diesel;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use decorum::R32;
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use dotenv::dotenv;
use serde::Deserialize;
use std::collections::HashMap;
use std::convert::TryInto;
use std::env;
use std::fs;
use std::io::{self, BufReader, Read};
use std::thread;
use std::time;

mod schema;
mod models {
    use decorum::R32;
    use diesel::backend::Backend;
    use diesel::serialize::{self, Output, ToSql};
    use diesel::deserialize::{self, FromSql};
    use diesel::sql_types::Binary;
    use serde::{Deserialize, Serialize};
    use std::io::Write;
    use super::schema::tSong;

    #[derive(AsChangeset, Identifiable, Insertable, Queryable)]
    #[derive(Debug, Eq, PartialEq)]
    #[primary_key(key)]
    #[table_name="tSong"]
    #[changeset_options(treat_none_as_null="true")]
    pub struct Song {
        pub key: String,
        pub hash: String,
        pub tstamp: i64,
        pub deleted: bool,
        pub data: Option<Vec<u8>>,
        pub extra_meta: Option<ExtraMeta>,
    }

    #[derive(Debug, Eq, PartialEq)]
    #[derive(Serialize, Deserialize)]
    #[derive(FromSqlRow, AsExpression)]
    #[sql_type = "Binary"]
    pub struct ExtraMeta {
        pub song_duration: R32,
        pub song_size: u32,
        pub zip_size: u32,
    }

    impl<DB> ToSql<Binary, DB> for ExtraMeta
    where
        DB: Backend,
        Vec<u8>: ToSql<Binary, DB>
    {
        fn to_sql<W: Write>(&self, out: &mut Output<W, DB>) -> serialize::Result {
            let bytes = serde_json::to_vec(&self).expect("failed to serialize extrameta");
            bytes.to_sql(out)
        }
    }

    impl<DB> FromSql<Binary, DB> for ExtraMeta
    where
        DB: Backend,
        Vec<u8>: FromSql<Binary, DB>,
    {
        fn from_sql(bytes: Option<&DB::RawValue>) -> deserialize::Result<Self> {
            let bs = Vec::<u8>::from_sql(bytes)?;
            Ok(serde_json::from_slice(&bs).expect("failed to deserialize extrameta"))
        }
    }
}

use models::*;
use schema::tSong;


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
    let args: Vec<String> = env::args().collect();
    if args.len() != 2 {
        panic!("wrong num of args")
    }
    match args[1].as_str() {
        "loadjson" => loadjson(),
        "unknown" => {
            println!("Considering unknown keys");
            println!("Unknown keys: {:?}", unknown_songs().len());
        },
        "dl" => dl_deets(),
        "analyse" => analyse_songs(),
        a => panic!("unknown arg {}", a),
    }
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
        upsert_song(&conn, song_key, song_hash, post_status != "publish", None, None);
    }
}

fn key_to_num<T: AsRef<str>>(k: &T) -> u64 {
    u64::from_str_radix(k.as_ref(), 16).expect("can't parse key")
}

fn num_to_key(n: u64) -> String {
    format!("{:x}", n)
}

fn unknown_songs() -> Vec<String> {
    use schema::tSong::dsl::*;
    let conn = establish_connection();

    let mut results: Vec<String> = tSong.select(key).load(&conn).expect("failed to select keys");
    println!("Loaded keys");
    results.sort_by_key(key_to_num);

    let mut unknown = vec![];
    let mut cur = 1;
    results.reverse();
    let mut next_target = results.pop();
    while let Some(target) = &next_target {
        let target_num = key_to_num(target);
        assert!(cur <= target_num);
        if cur == target_num {
            next_target = results.pop()
        } else {
            unknown.push(cur);
        }
        cur += 1;
    }
    unknown.into_iter().map(num_to_key).collect()
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

    fn load_blacklist() -> HashMap<String, String> {
        serde_json::from_reader(fs::File::open("blacklist.json").expect("blacklist open failed")).expect("blacklist load failed")
    }
    fn save_blacklist(blacklist: &HashMap<String, String>) {
        serde_json::to_writer(fs::File::create("blacklist.json").expect("blacklist open failed"), &blacklist).expect("blacklist save failed")
    }
    let mut blacklisted_keys = load_blacklist();

    //let song = to_download.into_iter().find(|s| s.key == "11b01").unwrap();
    for song in to_download {
        if let Some(reason) = blacklisted_keys.get(&song.key) {
            println!("Skipping song {} - previous failure: {}", song.key, reason);
            continue
        }
        println!("Considering song {}", song.key);
        println!("Getting song zip for {} {}", song.key, song.hash);
        let zipdata = match get_song_zip(&client, &song.key, &song.hash) {
            Ok(zd) => zd,
            Err(e) => {
                blacklisted_keys.insert(song.key, format!("get song zip failed: {}", e));
                save_blacklist(&blacklisted_keys);
                continue
            },
        };
        println!("Converting zip for {} to dats tar", song.key);
        let (tardata, extra_meta) = match zip_to_dats_tar(&zipdata) {
            Ok((td, em)) => (td, em),
            Err(e) => {
                blacklisted_keys.insert(song.key, format!("zip to dats tar failed: {}", e));
                save_blacklist(&blacklisted_keys);
                continue
            },
        };
        let key = song.key.clone();
        upsert_song(conn, song.key, song.hash, song.deleted, Some(tardata), Some(extra_meta));
        println!("Finished getting song {}", key);
        thread::sleep(time::Duration::from_secs(60));
    }
}

fn get_song_zip(client: &reqwest::blocking::Client, key: &str, hash: &str) -> Result<Vec<u8>> {
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

    macro_rules! retry {
        () => {{ thread::sleep(time::Duration::from_secs(60)); continue }}
    }
    let mut attempts = 2;
    while attempts > 0 {
        attempts -= 1;
        let url = format!("https://bsaber.org/files/cache/zip/{}.zip", key);
        println!("Retrieving {} from {}", key, url);
        let res = client.get(&url).send().expect("failed to send request");
        println!("Got response {}", res.status());

        let headers = res.headers().to_owned();
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            bail!("song not found from bsaber.org")
        }
        if !res.status().is_success() {
            println!("Response headers: {:?}", headers);
            retry!()
        }
        let bytes = match res.bytes() {
            Ok(bs) => bs,
            Err(e) => {
                println!("Failed to get bytes: {}", e);
                println!("Response headers: {:?}", headers);
                retry!()
            },
        };
        return Ok(bytes.as_ref().to_owned())
    }

    panic!("Failed to retrieve data");

    //println!("Retrieving {} from fs", key);
    //let path = format!("../beatmaps/{}.zip", hash);
    //let zipdata = fs::read(path).unwrap();
    //zipdata
}

fn zip_to_dats_tar(zipdata: &[u8]) -> Result<(Vec<u8>, ExtraMeta)> {
    let zipreader = io::Cursor::new(zipdata);
    let mut zip = zip::ZipArchive::new(zipreader).expect("failed to load zip");
    let mut dat_names: Vec<String> = vec![];
    for name in zip.file_names() {
        assert!(name.is_ascii(), "{}", name);
        assert!(name.find("/").is_none() && name.find("\\").is_none(), "{}", name);
        let lower_name = name.to_lowercase();
        if lower_name.ends_with(".egg") || lower_name.ends_with(".jpg") || lower_name.ends_with(".jpeg") || lower_name.ends_with(".png") {
            continue
        } else if !lower_name.ends_with(".dat") {
            bail!("odd file in zip: {}", name)
        }
        dat_names.push(name.to_owned())
    }
    println!("Got dat names: {:?}", dat_names);

    let mut tar = tar::Builder::new(vec![]);
    for dat_name in dat_names {
        let dat = zip.by_name(&dat_name).expect("failed to get dat name out of zip");
        let mut header = tar::Header::new_old();
        header.set_path(dat_name).expect("failed to set path");
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(dat.size());
        header.set_cksum();
        tar.append(&header, dat).expect("failed to append data to tar");
    }
    let tardata = tar.into_inner().expect("failed to finish tar");

    let ogg_names: Vec<_> = zip.file_names().filter(|name| name.ends_with(".egg")).collect();
    assert_eq!(ogg_names.len(), 1, "{:?}", ogg_names);
    let ogg_name = ogg_names[0].to_owned();
    let mut oggdata = vec![];
    zip.by_name(&ogg_name).expect("failed to find ogg").read_to_end(&mut oggdata).expect("failed to read ogg");
    let song_duration = ogg_duration(&oggdata)?;
    let song_size = oggdata.len().try_into().expect("song size too big");
    let zip_size = zipdata.len().try_into().expect("zup size too big");
    let extra_meta = ExtraMeta { song_duration, song_size, zip_size };

    Ok((tardata, extra_meta))
}

fn ogg_duration(ogg: &[u8]) -> Result<R32> {
    let ogg = io::Cursor::new(ogg);
    let mut srr = lewton::inside_ogg::OggStreamReader::new(ogg).expect("failed to create ogg stream reader");

    println!("Sample rate: {}", srr.ident_hdr.audio_sample_rate);

    let mut n = 0;
    let mut len_play = R32::from_inner(0.0);
    while let Some(pck) = srr.read_dec_packet().context("failed to read packet")? {
        n += 1;
        // This is guaranteed by the docs
        assert_eq!(pck.len(), srr.ident_hdr.audio_channels as usize);
        len_play += pck[0].len() as f32 / srr.ident_hdr.audio_sample_rate as f32;
    }
    println!("The piece is {} s long ({} packets).", len_play, n);
    Ok(len_play)
}

fn upsert_song(conn: &SqliteConnection, key: String, hash: String, deleted: bool, data: Option<Vec<u8>>, extra_meta: Option<ExtraMeta>) {
    let tstamp = Utc::now().timestamp_millis();
    let new_song = Song {
        key, hash, tstamp, deleted, data, extra_meta,
    };

    let nrows = if let Some(mut cur_song) = get_db_song(conn, &new_song.key) {
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

fn get_db_song(conn: &SqliteConnection, song_key: &str) -> Option<Song> {
    use schema::tSong::dsl::*;

    let mut results: Vec<Song> = tSong
        .filter(key.eq(song_key))
        .load(conn)
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
