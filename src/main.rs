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
use std::io::{self, Read};
use std::str;
use std::thread;
use std::time;

mod scripts;
mod schema;
mod models {
    use decorum::R32;
    use diesel::backend::Backend;
    use diesel::serialize::{self, Output, ToSql};
    use diesel::deserialize::{self, FromSql};
    use diesel::sql_types::Binary;
    use serde::{Deserialize, Serialize};
    use std::io::Write;
    use super::schema::{tSong, tSongData};

    #[derive(AsChangeset, Identifiable, Insertable, Queryable)]
    #[derive(Debug, Eq, PartialEq)]
    #[primary_key(key)]
    #[table_name="tSong"]
    #[changeset_options(treat_none_as_null="true")]
    pub struct Song {
        // TODO: make this u32
        pub key: i32,
        pub hash: Option<String>,
        pub tstamp: i64,
        pub deleted: bool,
        pub bsmeta: Option<Vec<u8>>,
    }

    #[derive(AsChangeset, Identifiable, Insertable, Queryable)]
    #[derive(Debug, Eq, PartialEq)]
    #[primary_key(key)]
    #[table_name="tSongData"]
    #[changeset_options(treat_none_as_null="true")]
    pub struct SongData {
        // TODO: make this u32
        pub key: i32,
        pub zipdata: Vec<u8>,
        pub data: Vec<u8>,
        pub extra_meta: ExtraMeta,
    }

    #[derive(Debug, Eq, PartialEq)]
    #[derive(Serialize, Deserialize)]
    #[derive(FromSqlRow, AsExpression)]
    #[sql_type = "Binary"]
    pub struct ExtraMeta {
        pub song_duration: R32,
        pub song_size: u32,
        pub zip_size: u32,
        pub zip_files: Vec<String>,
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
use schema::{tSong, tSongData};

const INFO_PAUSE_SECONDS: u64 = 3;
const DL_PAUSE_SECONDS: u64 = 10;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 2 {
        panic!("wrong num of args")
    }
    match args[1].as_str() {
        "loadjson" => scripts::loadjson(),
        "unknown" => {
            println!("Considering unknown keys");
            println!("Unknown keys: {:?}", unknown_songs().len());
        },
        "dl" => dl_data(),
        "dlmeta" => dl_meta(),
        "analyse" => analyse_songs(),
        a => panic!("unknown arg {}", a),
    }
}

fn key_to_num<T: AsRef<str>>(k: &T) -> i32 {
    u32::from_str_radix(k.as_ref(), 16).expect("can't parse key").try_into().expect("too big for i32")
}

fn num_to_key(n: i32) -> String {
    assert!(n >= 0);
    format!("{:x}", n)
}

fn unknown_songs() -> Vec<String> {
    use schema::tSong::dsl::*;
    let conn = establish_connection();

    let mut results: Vec<i32> = tSong.select(key).load(&conn).expect("failed to select keys");
    println!("Loaded keys");
    results.sort();

    let mut unknown = vec![];
    let mut cur = 1;
    results.reverse();
    let mut next_target = results.pop();
    while let Some(target) = next_target {
        assert!(cur <= target);
        if cur == target {
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

    let to_analyse: Vec<(Song, SongData)> = {
        use schema::tSong::{self, dsl::*};
        use schema::tSongData::{self, dsl::*};
        tSong
            .inner_join(tSongData)
            .filter(deleted.eq(false))
            .load(conn).expect("failed to select keys")
    };

    for (_song, songdata) in to_analyse {
        let mut tar = tar::Archive::new(&*songdata.data);
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

fn make_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .user_agent("aidanhsmetaclient/0.1 (@aidanhs#1789 on discord, aidanhs@cantab.net, https://github.com/aidanhs/bsmeta)")
        .build().expect("failed to create reqwest client")
}

fn dl_meta() {
    let conn = &establish_connection();

    fn get_latest_maps(client: &reqwest::blocking::Client, page: usize) -> Result<Vec<u8>> {
        let url = format!("https://beatsaver.com/api/maps/latest/{}", page);
        let res = match client.get(&url).send() {
            Ok(r) => r,
            Err(e) => bail!("failed to send request: {}", e),
        };
        println!("Got response {}", res.status());

        let headers = res.headers().to_owned();
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            bail!("song not found from bsaber.com")
        }
        if !res.status().is_success() {
            bail!("non-success response: {:?}", headers)
        }
        let bytes = match res.bytes() {
            Ok(bs) => bs,
            Err(e) => bail!("failed to get bytes: {}, response headers: {:?}", e, headers),
        };
        return Ok(bytes.as_ref().to_owned())
    }

    #[derive(Deserialize)]
    struct LatestResponse<'a> {
        #[serde(borrow)]
        docs: Vec<&'a serde_json::value::RawValue>
    }
    #[derive(Deserialize)]
    struct Map {
        key: String,
        hash: String,
    }

    println!("Identifying new songs");
    let client = make_client();
    let mut page = 0;
    let mut new_maps: Vec<(Map, Vec<u8>)> = vec![];
    loop {
        println!("Getting maps from page {}", page);
        let maps_data = get_latest_maps(&client, page).expect("failed to get latest maps");
        let res: LatestResponse = serde_json::from_slice(&maps_data).expect("failed to deserialize maps response");

        let mut num_new = 0;
        let num_maps = res.docs.len();
        for map_value in res.docs {
            let map: Map = serde_json::from_str(map_value.get()).expect("failed to interpret map value");
            if get_db_song(conn, key_to_num(&map.key)).is_none() {
                println!("Found a new map: {}", map.key);
                num_new += 1;
                new_maps.push((map, map_value.get().as_bytes().to_owned()))
            } else {
                println!("Map {} already in db", map.key);
                continue
            }
        }
        println!("Got {} new maps", num_new);
        if num_new < num_maps / 2 {
            println!("Looks like we have all the new maps now, breaking");
            break
        }
        page += 1;
        thread::sleep(time::Duration::from_secs(INFO_PAUSE_SECONDS));
    }

    println!("Found {} new metas", new_maps.len());
    while let Some((Map { key, hash }, raw_meta)) = new_maps.pop() {
        println!("Upserting {}", key);
        upsert_song(conn, key_to_num(&key), Some(hash), false, Some(raw_meta))
    }

    //println!("Finding song metas to download");
    //let (first_id, to_download): (String, Vec<String>) = {
    //    use schema::tSong::dsl::*;
    //    (
    //        tSong.select(max(key)),
    //        tSong
    //            .select(key)
    //            .filter(bsmeta.is_null())
    //            .filter(deleted.eq(false))
    //            .load::<String>(conn).expect("failed to select keys")
    //    )
    //};
    //println!("Got {} to download", to_download.len());
    //let to_download: BTreeSet<_> = to_download.into_iter().map(key_to_num).collect();
    //while let Some(target) = to_download.pop_last() {

    //    // loop
    //    //   get a page
    //    //     (handle everything)
    //    //   if it included target, break
    //    //   guess how far back/forward we need to go
    //    //
    //}
}

fn dl_data() {
    let conn = &establish_connection();

    println!("Finding songs to download");
    let to_download: Vec<(Song, Option<SongData>)> = {
        use schema::tSong::{self, dsl::*};
        use schema::tSongData::{self, dsl::*};
        tSong
            .left_join(tSongData)
            .filter(tSong::hash.is_not_null())
            .filter(tSongData::key.is_null())
            .filter(deleted.eq(false))
            .load(conn).expect("failed to select keys")
    };
    let mut to_download: Vec<Song> = to_download.into_iter().map(|(s, _sd)| s).collect();
    println!("Got {} to download", to_download.len());
    to_download.sort_by_key(|s| s.key);
    to_download.reverse();

    fn load_blacklist() -> HashMap<String, String> {
        serde_json::from_reader(fs::File::open("blacklist.json").expect("blacklist open failed")).expect("blacklist load failed")
    }
    fn save_blacklist(blacklist: &HashMap<String, String>) {
        serde_json::to_writer(fs::File::create("blacklist.json").expect("blacklist open failed"), &blacklist).expect("blacklist save failed")
    }

    let mut blacklisted_keys = load_blacklist();
    let client = make_client();
    for song in to_download {
        let key_str = num_to_key(song.key);
        let hash = song.hash.expect("non-null hash was None");
        if let Some(reason) = blacklisted_keys.get(&key_str) {
            println!("Skipping song {} - previous failure: {}", song.key, reason);
            continue
        }
        println!("Considering song {}", song.key);
        println!("Getting song zip for {} {}", song.key, hash);
        let zipdata = match dl_song_zip(&client, &key_str, &hash) {
            Ok(zd) => zd,
            Err(e) => {
                blacklisted_keys.insert(key_str.clone(), format!("get song zip failed: {}", e));
                save_blacklist(&blacklisted_keys);
                continue
            },
        };
        println!("Converting zip for {} to dats tar", song.key);
        let (tardata, extra_meta) = match zip_to_dats_tar(&zipdata) {
            Ok((td, em)) => (td, em),
            Err(e) => {
                blacklisted_keys.insert(key_str.clone(), format!("zip to dats tar failed: {}", e));
                save_blacklist(&blacklisted_keys);
                continue
            },
        };
        set_song_data(conn, song.key, tardata, extra_meta, zipdata);
        println!("Finished getting song {}", song.key);
        thread::sleep(time::Duration::from_secs(DL_PAUSE_SECONDS));
    }
}

fn dl_song_zip(client: &reqwest::blocking::Client, key: &str, hash: &str) -> Result<Vec<u8>> {
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

    let mut attempts = 2;
    macro_rules! retry {
        ($e:expr) => {{
            println!("request failure: {}", $e);
            attempts -= 1;
            if attempts == 0 {
                panic!("failed to retrieve data")
            }
            thread::sleep(time::Duration::from_secs(DL_PAUSE_SECONDS));
            continue
        }};
    }
    loop {
        let url = format!("https://bsaber.org/files/cache/zip/{}.zip", key);
        println!("Retrieving {} from {}", key, url);
        let res = match client.get(&url).send() {
            Ok(r) => r,
            Err(e) => retry!(format!("failed to send request: {}", e)),
        };
        println!("Got response {}", res.status());

        let headers = res.headers().to_owned();
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            bail!("song not found from bsaber.org")
        }
        if !res.status().is_success() {
            retry!(format!("non-success response: {:?}", headers))
        }
        let bytes = match res.bytes() {
            Ok(bs) => bs,
            Err(e) => retry!(format!("failed to get bytes: {}, response headers: {:?}", e, headers)),
        };
        return Ok(bytes.as_ref().to_owned())
    }

    //println!("Retrieving {} from fs", key);
    //let path = format!("../beatmaps/{}.zip", hash);
    //let zipdata = fs::read(path).unwrap();
    //zipdata
}

const IMAGE_EXTS: &[&str] = &[".jpg", ".jpeg", ".png"];
const OGG_EXTS: &[&str] = &[".egg"];

fn zip_to_dats_tar(zipdata: &[u8]) -> Result<(Vec<u8>, ExtraMeta)> {
    let zipreader = io::Cursor::new(zipdata);
    let mut zip = zip::ZipArchive::new(zipreader).expect("failed to load zip");
    let mut dat_names: Vec<String> = vec![];
    let mut all_names: Vec<String> = vec![];
    for zip_index in 0..zip.len() {
        let entry = zip.by_index(zip_index).context("failed to get entry from zip")?;
        let name_bytes = entry.name_raw();
        let name = str::from_utf8(name_bytes).context("failed to interpret name as utf8")?;
        if !name.is_ascii() {
            bail!("non-ascii name in zip: {}", name)
        } else if entry.is_dir() || name.contains("/") || name.contains("\\") {
            bail!("dir entry in zip: {}", name)
        }

        all_names.push(name.to_owned());
        let lower_name = name.to_lowercase();
        if lower_name.ends_with(".dat") {
            dat_names.push(name.to_owned())
        } else if !(IMAGE_EXTS.into_iter().chain(OGG_EXTS).any(|ext| lower_name.ends_with(ext))) {
            bail!("odd file in zip: {}", name)
        }
    }
    println!("Got dat names: {:?}, all names: {:?}", dat_names, all_names);

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

    let ogg_names: Vec<_> = all_names.iter().filter(|name| OGG_EXTS.into_iter().any(|ext| name.to_lowercase().ends_with(ext))).collect();
    if ogg_names.len() != 1 {
        bail!("multiple oggs: {:?}", ogg_names)
    }
    let ogg_name = ogg_names[0].to_owned();
    let mut oggdata = vec![];
    zip.by_name(&ogg_name).expect("failed to find ogg").read_to_end(&mut oggdata).expect("failed to read ogg");
    let song_duration = ogg_duration(&oggdata)?;
    let song_size = oggdata.len().try_into().expect("song size too big");
    let zip_size = zipdata.len().try_into().expect("zip size too big");
    let extra_meta = ExtraMeta { song_duration, song_size, zip_size, zip_files: all_names };

    Ok((tardata, extra_meta))
}

fn ogg_duration(ogg: &[u8]) -> Result<R32> {
    let ogg = io::Cursor::new(ogg);
    let mut srr = lewton::inside_ogg::OggStreamReader::new(ogg).context("failed to create ogg stream reader")?;

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

fn set_song_data(conn: &SqliteConnection, key: i32, data: Vec<u8>, extra_meta: ExtraMeta, zipdata: Vec<u8>) {
    let songdata = SongData { key, zipdata, data, extra_meta };
    let nrows = diesel::insert_into(tSongData::table)
        .values(&songdata)
        .execute(conn)
        .expect("error updating song data");
    assert_eq!(nrows, 1, "{:?}", songdata)
}

fn upsert_song(conn: &SqliteConnection, key: i32, hash: Option<String>, deleted: bool, bsmeta: Option<Vec<u8>>) {
    let new_song = Song { key, hash, tstamp: Utc::now().timestamp_millis(), deleted, bsmeta };
    let nrows = diesel::replace_into(tSong::table)
        .values(&new_song)
        .execute(conn)
        .expect("error saving song");
    assert_eq!(nrows, 1, "{}", new_song.key)
}

fn insert_song(conn: &SqliteConnection, key: i32, hash: Option<String>, deleted: bool, bsmeta: Option<Vec<u8>>) {
    let new_song = Song { key, hash, tstamp: Utc::now().timestamp_millis(), deleted, bsmeta };
    let nrows = diesel::insert_into(tSong::table)
        .values(&new_song)
        .execute(conn)
        .expect("error saving song");
    assert_eq!(nrows, 1, "{}", new_song.key)
}

fn get_db_song(conn: &SqliteConnection, song_key: i32) -> Option<Song> {
    use schema::tSong::dsl::*;
    tSong
        .find(song_key)
        .first(conn).optional()
        .expect("failed to load song")
}

pub fn establish_connection() -> SqliteConnection {
    use diesel::connection::SimpleConnection;
    dotenv().ok();

    let database_url = env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set");
    let conn = SqliteConnection::establish(&database_url)
        .expect(&format!("Error connecting to {}", database_url));

    conn.batch_execute("
        PRAGMA journal_mode = WAL;          -- better write-concurrency
        PRAGMA synchronous = NORMAL;        -- fsync only in critical moments
        PRAGMA wal_autocheckpoint = 1000;   -- write WAL changes back every 1000 pages, for an in average 1MB WAL file. May affect readers if number is increased
        PRAGMA wal_checkpoint(TRUNCATE);    -- free some space by truncating possibly massive WAL files from the last run.
        PRAGMA busy_timeout = 250;          -- sleep if the database is busy
        PRAGMA foreign_keys = ON;           -- enforce foreign keys
    ").expect("couldn't set up connection");

    conn
}
