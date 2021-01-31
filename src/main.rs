#![feature(duration_saturating_ops, duration_zero)]
#![recursion_limit="256"]

use anyhow::{Context, Result, anyhow, bail};
use async_std::task;
use chrono::Utc;
use decorum::R32;
use dotenv::dotenv;
use log::{info, warn};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::convert::TryInto;
use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::Path;
use std::str;
use std::thread;
use std::time;
use sqlx::prelude::*;
use sqlx::{query, query_as};

type SqliteConnection = sqlx::sqlite::SqlitePool;

mod scripts;
mod wasm;
mod models {
    use decorum::R32;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Eq, PartialEq)]
    pub struct Song {
        // TODO: make this u32
        pub key: i64,
        pub hash: Option<String>,
        pub tstamp: i64,
        pub deleted: bool,
        pub bsmeta: Option<Vec<u8>>,
    }

    #[derive(Debug, Eq, PartialEq)]
    pub struct SongData {
        // TODO: make this u32
        pub key: i64,
        pub zipdata: Vec<u8>,
        pub data: Vec<u8>,
        pub extra_meta: ExtraMeta,
    }

    #[derive(Debug, Eq, PartialEq)]
    #[derive(Serialize, Deserialize)]
    pub struct ExtraMeta {
        pub song_duration: Option<R32>,
        pub song_size: Option<u32>,
        pub zip_size: u32,
    }

    impl<DB: sqlx::Database> sqlx::Type<DB> for ExtraMeta
        where
            Vec<u8>: sqlx::Type<DB>
    {
        fn type_info() -> <DB as sqlx::Database>::TypeInfo {
            <Vec<u8> as sqlx::Type<DB>>::type_info()
        }
        fn compatible(ty: &<DB as sqlx::Database>::TypeInfo) -> bool {
            <Vec<u8> as sqlx::Type<DB>>::compatible(ty)
        }
    }

    impl<'q, DB: sqlx::Database> sqlx::Encode<'q, DB> for ExtraMeta
    where
        Vec<u8>: sqlx::Encode<'q, DB>
    {
        fn encode_by_ref(&self, buf: &mut <DB as sqlx::database::HasArguments<'q>>::ArgumentBuffer) -> sqlx::encode::IsNull {
            let bytes = serde_json::to_vec(&self).expect("failed to serialize extrameta");
            <Vec<u8> as sqlx::Encode<'q, DB>>::encode_by_ref(&bytes, buf)
        }
        fn encode(self, buf: &mut <DB as sqlx::database::HasArguments<'q>>::ArgumentBuffer) -> sqlx::encode::IsNull {
            let bytes = serde_json::to_vec(&self).expect("failed to serialize extrameta");
            <Vec<u8> as sqlx::Encode<'q, DB>>::encode(bytes, buf)
        }
    }
}

use models::*;

#[derive(Deserialize)]
struct InfoDat {
    #[serde(rename = "_songFilename")]
    song_filename: String,
    #[serde(rename = "_difficultyBeatmapSets")]
    difficulty_beatmap_sets: Vec<DifficultySet>,
}
#[derive(Deserialize)]
struct DifficultySet {
    #[serde(rename = "_difficultyBeatmaps")]
    difficulty_beatmaps: Vec<DifficultyBeatmap>,
}
#[derive(Deserialize)]
struct DifficultyBeatmap {
    #[serde(rename = "_beatmapFilename")]
    beatmap_filename: String,
}

#[derive(Deserialize)]
struct BeatSaverMap {
    key: String,
    hash: String,
    uploaded: String,
}

const INFO_PAUSE: time::Duration = time::Duration::from_secs(3);
const BSABER_DL_PAUSE: time::Duration = time::Duration::from_secs(10);
const BEATSAVER_DL_PAUSE: time::Duration = time::Duration::from_secs(120);

// Additional padding to apply when ratelimited, to prove we're being a good citizen
const RATELIMIT_PADDING: time::Duration = time::Duration::from_secs(60);

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("bsmeta=info,warn")).init();
    env::set_var("ASYNC_STD_THREAD_COUNT", "3");

    let args: Vec<String> = env::args().collect();
    if args.len() != 2 {
        panic!("wrong num of args")
    }
    match args[1].as_str() {
        "script-loadjson" => scripts::loadjson(),
        "script-checkdeleted" => scripts::checkdeleted(),
        "script-getmissingbsmeta" => scripts::getmissingbsmeta(),
        "script-regenzipderived" => scripts::regenzipderived(),
        "unknown" => {
            println!("Considering unknown keys");
            println!("Unknown keys: {:?}", unknown_songs().len());
        },
        "dl" => dl_data(),
        "dlmeta" => dl_meta(),
        "analyse" => analyse_songs(),
        "test" => test().unwrap(),
        a => panic!("unknown arg {}", a),
    }
}

fn key_to_num<T: AsRef<str>>(k: &T) -> i64 {
    u32::from_str_radix(k.as_ref(), 16).expect("can't parse key").into()
}

fn num_to_key(n: i64) -> String {
    assert!(n >= 0);
    format!("{:x}", n) // format as hex
}

fn test() -> Result<()> {
    wasm::test()
}

fn unknown_songs() -> Vec<i64> {
    let conn = &establish_connection();

    let results = task::block_on(query!("SELECT key FROM tSong").fetch_all(conn)).expect("failed to select keys");
    let mut results: Vec<_> = results.into_iter().map(|res| res.key).collect();
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
    unknown
}

fn analyse_songs() {
    println!("Analysing songs");
    let conn = &establish_connection();

    let to_analyse = task::block_on(
        query!("SELECT s.key FROM tSong s, tSongData sd WHERE deleted = false AND s.key = sd.key").fetch_all(conn)
    ).expect("failed to select key");
    let to_analyse: Vec<_> = to_analyse.into_iter().map(|res| res.key).collect();

    // TODO: load from plugins list
    let analyses = &[
        wasm::load_plugin("parity", "js").expect("failed to load parity plugin"),
    ];

    let num_to_analyse = to_analyse.len();
    for (i, key) in to_analyse.into_iter().enumerate() {
        let key_str = num_to_key(key);
        info!("Considering song {}/{}: {}", i+1, num_to_analyse, key_str);
        for plugin in analyses {
            let plugin_name = plugin.name();
            let exists: bool = task::block_on(
                query!("SELECT count(*) as count FROM tSongAnalysis WHERE key = ? AND analysis_name = ?", key, plugin_name)
                    .fetch_one(conn)
            ).expect("failed to check if analysis exists").count > 0;
            if exists {
                continue
            }
            info!("Performing analysis {} on {}", plugin.name(), key_str);
            let data = task::block_on(
                query!("SELECT data FROM tSongData WHERE key = ?", key).fetch_one(conn)
            ).expect("failed to load zipdata").data;

            let mut map_dats: Option<HashSet<String>> = None;
            let mut ar = tar::Archive::new(&*data);
            let mut datas = HashMap::new();
            for entry in ar.entries().expect("failed to parse dat tar") {
                let mut entry = entry.expect("failed to decode dat entry");
                let path_bytes = entry.path_bytes();
                let path = str::from_utf8(&path_bytes).unwrap();
                let lower_path = path.to_lowercase();
                if lower_path == "info.dat" {
                    let info: InfoDat = serde_json::from_reader(entry).unwrap();
                    map_dats = Some(info.difficulty_beatmap_sets.into_iter()
                        .flat_map(|ds| ds.difficulty_beatmaps)
                        .map(|db| db.beatmap_filename)
                        .collect())
                } else {
                    let mut v = vec![];
                    let path = path.to_owned();
                    entry.read_to_end(&mut v).unwrap();
                    assert!(datas.insert(path, v).is_none())
                }
            }

            let map_dats = map_dats.unwrap();
            info!("Identified {} maps to analyse", map_dats.len());
            for (name, data) in datas {
                if !map_dats.contains(&name) {
                    continue
                }
                let results = match plugin.run(data) {
                    Ok(r) => r,
                    Err(e) => {
                        warn!("Failed to run analysis: {}", e);
                        continue
                    },
                };
                // Prefix results with plugin
                let results: HashMap<_, _> = results
                    .into_iter()
                    .map(|(k, v)| (format!("{}-{}", name, k), v))
                    .collect();
                let _ = results;
            }
            info!("Analysis complete");
        }
    }

    // TODO: loads all zipdata into memory
    //let to_analyse: Vec<(Song, SongData)> = {
    //    use schema::tSong::dsl::*;
    //    use schema::tSongData::dsl::*;
    //    tSong
    //        .inner_join(tSongData)
    //        .filter(deleted.eq(false))
    //        .load(conn).expect("failed to select keys")
    //};

    //for (_song, songdata) in to_analyse {
    //    let mut tar = tar::Archive::new(&*songdata.data);
    //    let mut names_lens = vec![];
    //    for entry in tar.entries().expect("couldn't iterate over entries") {
    //        let entry = entry.expect("error getting entry");
    //        let path = entry.path().expect("couldn't get path");
    //        let path_str = path.to_str().expect("couldn't convert path");
    //        names_lens.push((path_str.to_owned(), entry.size()))
    //    }
    //    println!("{:?}", names_lens);
    //}
}

fn make_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .user_agent("aidanhsmetaclient/0.1 (@aidanhs#1789 on discord, aidanhs@cantab.net, https://github.com/aidanhs/bsmeta)")
        .build().expect("failed to create reqwest client")
}

fn dl_meta() {
    let conn = &establish_connection();
    let client = &make_client();

    dl_latest_meta(conn, client);

    dl_unknown_meta(conn, client);
}

fn dl_latest_meta(conn: &SqliteConnection, client: &reqwest::blocking::Client) {
    println!("Identifying new songs");
    let mut page = 0;
    // Don't go too far if we've missed lots, we have the ability to backfill songs
    const MAX_PAGE: usize = 10;
    let mut maps: Vec<(BeatSaverMap, Vec<u8>)> = vec![];
    loop {
        let res = get_latest_maps(client, page).expect("failed to get latest maps");

        let mut num_new = 0;
        let num_maps = res.docs.len();
        for (map, map_value) in res.docs {
            if get_db_song(conn, key_to_num(&map.key)).is_none() {
                println!("Found a new map: {}", map.key);
                num_new += 1
            } else {
                println!("Map {} already in db", map.key);
            }
            maps.push((map, map_value.get().as_bytes().to_owned()))
        }
        println!("Got {} new maps", num_new);
        if num_new < num_maps / 2 {
            println!("Looks like we have all the new maps now, breaking");
            break
        }
        page += 1;
        if page == MAX_PAGE {
            break
        }
        thread::sleep(INFO_PAUSE);
    }

    println!("Upserting {} map metas", maps.len());
    // Deliberately go oldest first, so if there's an error we can resume
    while let Some((BeatSaverMap { key, hash, uploaded: _ }, raw_meta)) = maps.pop() {
        println!("Upserting {}", key);
        upsert_song(conn, key_to_num(&key), Some(hash), false, Some(raw_meta))
    }
}

// These are keys that beatsaver seems to redirect to another map - I'm not sure why
const REDIRECTING_KEYS: &[i64] = &[0x9707];

fn dl_unknown_meta(conn: &SqliteConnection, client: &reqwest::blocking::Client) {
    println!("Finding song metas to download");
    let unknown = unknown_songs();
    let num_unknown = unknown.len();
    println!("Found {} unknown songs to download", num_unknown);
    for (i, key) in unknown.into_iter().enumerate() {
        let key_str = num_to_key(key);
        if REDIRECTING_KEYS.iter().find(|&&k| k == key).is_some() {
            println!("Skipping key {}", key_str);
            continue
        }
        println!("Getting meta for song {} ({}/{})", key_str, i+1, num_unknown);
        match get_map_meta(client, key).expect("failed to get map for song") {
            Some((m, raw)) => {
                assert_eq!(m.key, key_str);
                upsert_song(conn, key, Some(m.hash), false, Some(raw.get().as_bytes().to_owned()))
            },
            None => {
                upsert_song(conn, key, None, true, None)
            },
        }
        thread::sleep(INFO_PAUSE)
    }
}

fn dl_data() {
    let conn = &establish_connection();

    println!("Finding songs to download");
    let mut to_download = task::block_on(
        query_as!(Song, "
            SELECT s.*
            FROM tSong s
                LEFT OUTER JOIN tSongData sd ON s.key = sd.key
            WHERE s.hash IS NOT NULL AND s.deleted = false AND sd.key IS NULL
        ").fetch_all(conn)
    ).expect("failed to select keys");
    println!("Got {} not yet downloaded", to_download.len());
    to_download.sort_by_key(|s| s.key);
    to_download.reverse();

    fn load_blacklist() -> HashMap<String, String> {
        if !Path::new("blacklist.json").is_file() {
            fs::write("blacklist.json", b"{}").expect("failed to created empty blacklist file")
        }
        serde_json::from_reader(fs::File::open("blacklist.json").expect("blacklist open failed")).expect("blacklist load failed")
    }
    fn save_blacklist(blacklist: &HashMap<String, String>) {
        serde_json::to_writer(fs::File::create("blacklist.json").expect("blacklist open failed"), &blacklist).expect("blacklist save failed")
    }

    let mut blacklisted_keys = load_blacklist();
    println!("Got {} blacklisted_keys", blacklisted_keys.len());

    let to_download: Vec<_> = to_download.into_iter()
        .filter(|s| !blacklisted_keys.contains_key(&num_to_key(s.key)))
        .collect();
    let num_to_download = to_download.len();
    println!("Got {} to try and download", num_to_download);

    let client = &make_client();
    let mut last_bsaber_dl = time::Instant::now();
    let mut last_beatsaver_dl = time::Instant::now();
    for (i, song) in to_download.into_iter().enumerate() {
        let key_str = num_to_key(song.key);
        let hash = song.hash.expect("non-null hash was None");
        println!("Considering song {} ({}/{})", key_str, i+1, num_to_download);
        if let Some(reason) = blacklisted_keys.get(&key_str) {
            println!("Skipping song {} - previous failure: {}", key_str, reason);
            continue
        }
        // Skip recent songs to give bsaber.org a chance to cache, to spread out our downloads off
        // beatsaver
        let bsmeta = song.bsmeta.as_ref().expect("no bsmeta available in song");
        let bsm: BeatSaverMap = serde_json::from_slice(bsmeta).expect("failed to parse bsmeta");
        let uploaded = chrono::DateTime::parse_from_rfc3339(&bsm.uploaded).expect("failed to parse bsmeta uploaded");
        if chrono::Utc::now().signed_duration_since(uploaded).num_days() < 1 {
            println!("Skipping song - uploaded within last 24 hours");
            continue
        }
        println!("Getting song zip for {} {}", song.key, hash);
        let zipdata = match get_song_zip(client, &key_str, &hash, &mut last_bsaber_dl, &mut last_beatsaver_dl) {
            Ok(zd) => zd,
            Err(e) => {
                blacklisted_keys.insert(key_str.clone(), format!("get song zip failed: {}", e));
                save_blacklist(&blacklisted_keys);
                continue
            },
        };
        println!("Converting zip for {} to dats tar", key_str);
        let (tardata, extra_meta) = match zip_to_dats_tar(&zipdata) {
            Ok((td, em)) => (td, em),
            Err(e) => {
                blacklisted_keys.insert(key_str.clone(), format!("zip to dats tar failed: {}", e));
                save_blacklist(&blacklisted_keys);
                continue
            },
        };
        set_song_data(conn, song.key, tardata, extra_meta, zipdata);
        println!("Finished getting song {}", key_str)
    }
}

// TODO: do this by implementing a Deserializer that creates a visitor that dispatches to two sub
// visitors
fn splitde<'de, D>(de: D) -> Result<Vec<(BeatSaverMap, Box<serde_json::value::RawValue>)>, D::Error> where D: serde::Deserializer<'de> {
    use serde::de::Error;
    let raws = Vec::<Box<serde_json::value::RawValue>>::deserialize(de)?;
    let all: Vec<_> = raws.into_iter()
        .map(|rv| {
            serde_json::from_str(rv.get())
                .map(|m| (m, rv))
                .map_err(|e| D::Error::custom(e))
        })
        .collect::<Result<_, _>>()?;
    Ok(all)
}

#[derive(Deserialize)]
struct BeatSaverLatestResponse {
    #[serde(deserialize_with = "splitde")]
    docs: Vec<(BeatSaverMap, Box<serde_json::value::RawValue>)>,
}

macro_rules! retry {
    ($t:expr, $e:expr) => {{
        println!("request failure: {}", $e);
        thread::sleep($t);
        continue
    }};
}

const RATELIMIT_RESET_AFTER_HEADER: &str = "x-ratelimit-reset-after";

fn get_song_zip(client: &reqwest::blocking::Client, key_str: &str, hash: &str, last_bsaber_dl: &mut time::Instant, last_beatsaver_dl: &mut time::Instant) -> Result<Vec<u8>> {
    //let url = format!("https://beatsaver.com/cdn/{}/{}.zip", key_str, hash);
    //println!("Retrieving {} from url {}", key_str, url);
    //let mut res = client.get(&url).send().expect("failed to send request");
    //println!("Got response {}", res.status());
    //if res.status() == reqwest::StatusCode::UNAUTHORIZED {
    //    let url = format!("https://bsaber.org/files/cache/zip/{}.zip", key_str);
    //    println!("Retrying with url {}", url);
    //    res = client.get(&url).send().expect("failed to send request");
    //    println!("Got response {}", res.status());
    //}

    let mut attempts = 2;
    loop {
        if attempts == 0 {
            break
        }
        attempts -= 1;

        let pause_remaining = BSABER_DL_PAUSE.saturating_sub(last_bsaber_dl.elapsed());
        if !pause_remaining.is_zero() {
            thread::sleep(pause_remaining)
        }
        *last_bsaber_dl = time::Instant::now();
        println!("Retrieving {} from bsaber.org", key_str);
        let (res, headers) = match do_req(client, &format!("https://bsaber.org/files/cache/zip/{}.zip", key_str)) {
            Ok(r) => r,
            Err(e) => retry!(BSABER_DL_PAUSE, format!("failed to send request: {}", e)),
        };
        println!("Got response {}", res.status());

        if res.status() == reqwest::StatusCode::NOT_FOUND {
            break
        }
        if !res.status().is_success() {
            retry!(BSABER_DL_PAUSE, format!("non-success response: {:?} {:?}", headers, res.bytes()))
        }
        let bytes = match res.bytes() {
            Ok(bs) => bs,
            Err(e) => retry!(BSABER_DL_PAUSE, format!("failed to get bytes: {}, response headers: {:?}", e, headers)),
        };

        return Ok(bytes.as_ref().to_owned())
    }

    // TODO: ratelimit pause
    println!("Falling back to beatsaver.com for song");
    let mut attempts = 2;
    loop {
        if attempts == 0 {
            bail!("multiple attempts to retrieve failed, from both bsaber.org and beatsaver.com")
        }
        attempts -= 1;

        let pause_remaining = BEATSAVER_DL_PAUSE.saturating_sub(last_beatsaver_dl.elapsed());
        if !pause_remaining.is_zero() {
            thread::sleep(pause_remaining)
        }
        *last_beatsaver_dl = time::Instant::now();
        println!("Retrieving {} from beatsaver.com", key_str);
        let (res, headers) = match do_req(client, &format!("https://beatsaver.com/cdn/{}/{}.zip", key_str, hash)) {
            Ok(r) => r,
            Err(e) => retry!(BEATSAVER_DL_PAUSE, format!("failed to send request: {}", e)),
        };
        println!("Got response {}", res.status());

        if res.status() == reqwest::StatusCode::NOT_FOUND {
            bail!("song not found on bsaber.org or beatsaver.com")
        }
        if !res.status().is_success() {
            retry!(BEATSAVER_DL_PAUSE, format!("non-success response: {:?} {:?}", headers, res.bytes()))
        }
        let bytes = match res.bytes() {
            Ok(bs) => bs,
            Err(e) => retry!(BEATSAVER_DL_PAUSE, format!("failed to get bytes: {}, response headers: {:?}", e, headers)),
        };

        return Ok(bytes.as_ref().to_owned())
    }
}

fn get_latest_maps(client: &reqwest::blocking::Client, page: usize) -> Result<BeatSaverLatestResponse> {
    println!("Getting maps from page {}", page);
    let (res, headers) = do_req(client, &format!("https://beatsaver.com/api/maps/latest/{}?automapper=1", page))?;
    println!("Got response {}", res.status());

    if res.status() == reqwest::StatusCode::NOT_FOUND {
        bail!("latest maps page not found from beatsaver")
    }
    if !res.status().is_success() {
        bail!("non-success response: {:?} {:?}", headers, res.bytes())
    }
    let bytes = match res.bytes() {
        Ok(bs) => bs,
        Err(e) => bail!("failed to get bytes: {}, response headers: {:?}", e, headers),
    };

    let bytes = bytes.as_ref();
    let res = serde_json::from_slice(bytes)
        .with_context(|| format!("failed to deserialize maps response: {:?}", String::from_utf8_lossy(bytes)))?;
    Ok(res)
}

fn get_map_meta(client: &reqwest::blocking::Client, key: i64) -> Result<Option<(BeatSaverMap, Box<serde_json::value::RawValue>)>> {
    let mut did_ratelimit = false;

    loop {
        let key_str = num_to_key(key);
        println!("Getting map detail for {}", key_str);
        let (res, headers) = do_req(client, &format!("https://beatsaver.com/api/maps/detail/{}", key_str))?;
        println!("Got response {}", res.status());

        if res.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None)
        }
        if res.status() == reqwest::StatusCode::TOO_MANY_REQUESTS && !did_ratelimit {
            did_ratelimit = true;
            let pause = ratelimit_pause(&headers)?;
            retry!(pause, format!("hit ratelimit, waiting {}s: {:?}", pause.as_secs(), headers))
        }
        if !res.status().is_success() {
            bail!("non-success response: {:?} {:?}", headers, res.bytes())
        }
        let bytes = match res.bytes() {
            Ok(bs) => bs,
            Err(e) => bail!("failed to get bytes: {}, response headers: {:?}", e, headers),
        };

        let bytes = bytes.as_ref();
        let raw_res: Box<serde_json::value::RawValue> = serde_json::from_slice(bytes)
            .with_context(|| format!("failed to deserialize aw maps response: {:?}", String::from_utf8_lossy(bytes)))?;
        let res = serde_json::from_str(raw_res.get())
            .with_context(|| format!("failed to deserialize maps response: {:?}", String::from_utf8_lossy(bytes)))?;
        return Ok(Some((res, raw_res)))
    }
}

fn do_req(client: &reqwest::blocking::Client, url: &str) -> Result<(reqwest::blocking::Response, reqwest::header::HeaderMap)> {
    client.get(url).send()
        .map(|res| {
            let headers = res.headers().to_owned();
            (res, headers)
        })
        .context("failed to send request")
}

fn ratelimit_pause(headers: &reqwest::header::HeaderMap) -> Result<time::Duration> {
    let r = headers.get(RATELIMIT_RESET_AFTER_HEADER)
        .ok_or_else(|| anyhow!("no ratelimit header with too many requests response: {:?}", headers))?;
    let r = r.to_str().with_context(|| format!("ratelimit header couldn't be interpreted as ascii: {:?}", headers))?;
    let r: u64 = r.parse().with_context(|| format!("failed to parse ratelimit as i64: {:?}", headers))?;
    Ok(2*time::Duration::from_millis(r) + RATELIMIT_PADDING) // pad for safety
}


fn zip_to_dats_tar(zipdata: &[u8]) -> Result<(Vec<u8>, ExtraMeta)> {
    let zipreader = io::Cursor::new(zipdata);

    let mut infodat: Option<(String, InfoDat)> = None;
    let mut zip = zip::ZipArchive::new(zipreader).expect("failed to load zip");
    for zip_index in 0..zip.len() {
        let entry = zip.by_index(zip_index).context("failed to get entry from zip")?;
        let name_bytes = entry.name_raw();
        if name_bytes.eq_ignore_ascii_case(b"info.dat") {
            if infodat.is_some() {
                bail!("multiple info.dat candidates")
            }
            infodat = Some((
                String::from_utf8(name_bytes.to_owned()).expect("info.dat name not ascii"),
                serde_json::from_reader(entry).context("failed to parse info.dat")?,
            ));
            break
        }
    }

    let (infodat_name, infodat) = infodat.ok_or_else(|| anyhow!("no info.dat found in zip"))?;
    let mut dat_names: Vec<_> = infodat.difficulty_beatmap_sets.into_iter()
        .flat_map(|ds| ds.difficulty_beatmaps)
        .map(|db| db.beatmap_filename)
        .collect();
    // Put info.dat at the front, sort and dedup the rest
    dat_names.sort();
    dat_names.dedup();
    dat_names.reverse();
    dat_names.push(infodat_name);
    dat_names.reverse();

    println!("Got dat names: {:?}", dat_names);

    let mut tar = tar::Builder::new(vec![]);
    for dat_name in dat_names {
        if !dat_name.is_ascii() {
            bail!("non-ascii dat name")
        }

        let mut dat = None;
        // This nested loop isn't ideal, but `by_name` take a str and we want to avoid decoding as utf8
        for zip_index in 0..zip.len() {
            let mut entry = zip.by_index(zip_index).context("failed to get candidate dat entry from zip")?;
            if entry.name_raw() == dat_name.as_bytes() {
                if dat.is_some() {
                    bail!("duplicate entry for dat")
                }
                let mut data = vec![];
                entry.read_to_end(&mut data).context("failed to read dat from zip")?;
                dat = Some(data)
            }
        }
        let dat = dat.ok_or_else(|| anyhow!("failed to get dat out of zip"))?;

        let mut header = tar::Header::new_old();
        header.set_path(dat_name).expect("failed to set path");
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(dat.len().try_into().expect("dat length conversion failed"));
        header.set_cksum();
        tar.append(&header, &*dat).expect("failed to append data to tar");
    }
    let tardata = tar.into_inner().expect("failed to finish tar");

    let (song_duration, song_size) = ogg_duration_from_zip(&infodat.song_filename, zip);
    let zip_size = zipdata.len().try_into().expect("zip size too big");
    let extra_meta = ExtraMeta { song_duration, song_size, zip_size };

    Ok((tardata, extra_meta))
}

fn ogg_duration_from_zip(ogg_name: &str, mut zip: zip::ZipArchive<impl Read + io::Seek>) -> (Option<R32>, Option<u32>) {
    macro_rules! try_return {
        ($e:expr, $r:expr) => {
            match $e {
                Ok(v) => v,
                Err(e) => {
                    println!("error getting ogg size and/or duration: {}", e);
                    return $r
                },
            }
        };
    }
    let mut entry = try_return!(zip.by_name(&ogg_name), (None, None));
    let mut oggdata = vec![];
    try_return!(entry.read_to_end(&mut oggdata), (None, None));
    let song_size = Some(oggdata.len().try_into().expect("song size too big"));
    let song_duration = Some(try_return!(ogg_duration(&oggdata), (None, song_size)));
    (song_duration, song_size)
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

// https://github.com/launchbadge/sqlx/issues/328 - for inserting a Song
fn set_song_data(conn: &SqliteConnection, key: i64, data: Vec<u8>, extra_meta: ExtraMeta, zipdata: Vec<u8>) {
    let res = task::block_on(
        query!("INSERT INTO tSongData (key, data, extra_meta, zipdata) VALUES (?, ?, ?, ?)", key, data, extra_meta, zipdata)
            .execute(conn)
    ).expect("error updating song data");
    assert_eq!(res.rows_affected(), 1, "{:?}", (key, data, extra_meta, zipdata))
}

fn upsert_song(conn: &SqliteConnection, key: i64, hash: Option<String>, deleted: bool, bsmeta: Option<Vec<u8>>) {
    let tstamp = Utc::now().timestamp_millis();
    let res = task::block_on(
        query!("
            INSERT INTO tSong (key, hash, tstamp, deleted, bsmeta) VALUES (?, ?, ?, ?, ?)
            ON CONFLICT (key) DO UPDATE SET hash=excluded.hash, tstamp=excluded.tstamp, deleted=excluded.deleted, bsmeta=excluded.bsmeta
        ", key, hash, tstamp, deleted, bsmeta)
            .execute(conn)
    ).expect("error upserting song");
    assert_eq!(res.rows_affected(), 1, "upsert {}", key)
}

fn insert_song(conn: &SqliteConnection, key: i64, hash: Option<String>, deleted: bool, bsmeta: Option<Vec<u8>>) {
    let tstamp = Utc::now().timestamp_millis();
    let res = task::block_on(
        query!("INSERT INTO tSong (key, hash, tstamp, deleted, bsmeta) VALUES (?, ?, ?, ?, ?)", key, hash, tstamp, deleted, bsmeta)
            .execute(conn)
    ).expect("error saving song");
    assert_eq!(res.rows_affected(), 1, "insert {}", key)
}

fn get_db_song(conn: &SqliteConnection, song_key: i64) -> Option<Song> {
    task::block_on(
        query_as!(Song, "SELECT * FROM tSong WHERE key = ?", song_key)
            .fetch_optional(conn)
    ).expect("failed to load song")
}

pub fn establish_connection() -> SqliteConnection {
    dotenv().ok();

    let database_url = env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set");
    // We don't actually use this as a pool (yet) - it's just so we can pass it as a reference!
    let conn = task::block_on(sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .min_connections(1)
        .after_connect(|conn| Box::pin(async move {
            conn.execute("
                PRAGMA journal_mode = WAL;   -- better write-concurrency
                PRAGMA synchronous = NORMAL; -- fsync only in critical moments
                PRAGMA busy_timeout = 250;   -- sleep if the database is busy
                PRAGMA foreign_keys = ON;    -- enforce foreign keys
            ").await?;
            Ok(())
        }))
        .connect(&database_url))
        .expect(&format!("Error connecting to {}", database_url));

    task::block_on(conn.execute("
        PRAGMA wal_checkpoint(TRUNCATE); -- free some space by truncating possibly massive WAL files from the last run.
    ")).expect("couldn't set up connection");

    conn
}
