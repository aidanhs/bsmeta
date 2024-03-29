#![recursion_limit="256"]

use anyhow::{Context, Result, anyhow, bail};
use async_std::task;
use chrono::{DateTime, Utc};
use decorum::R32;
use dotenv::dotenv;
use log::{debug, info, warn};
use serde::{Serialize, Deserialize};
use std::cmp;
use std::collections::HashMap;
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
mod server;
mod wasm;
mod models {
    use decorum::R32;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Eq, PartialEq)]
    pub struct Song {
        // TODO: make this u32
        pub key: i64,
        pub deleted: bool,
        pub tstamp: i64,
    }

    #[derive(Debug, Eq, PartialEq)]
    pub struct SongMeta {
        // TODO: make this u32
        pub key: i64,
        pub hash: String,
        pub bsmeta: Vec<u8>,
    }

    #[derive(Debug, Eq, PartialEq)]
    pub struct SongData {
        // TODO: make this u32
        pub hash: String,
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
    #[serde(rename = "id")]
    key: String,
    description: String,
    metadata: BeatSaverMapMetadata,
    stats: BeatSaverMapStats,
    uploaded: chrono::DateTime<Utc>,
    uploader: BeatSaverMapUploader,
    #[serde(rename = "updatedAt")]
    updated_at: chrono::DateTime<Utc>,
    versions: Vec<BeatSaverMapVersion>,
}
impl BeatSaverMap {
    fn check(&self) {
        self.current_version(); // make sure assertions in here pass
        // TODO: relax this at some point?
        assert_eq!(self.versions.len(), 1);
    }

    fn current_version(&self) -> &BeatSaverMapVersion {
        assert_eq!(
            self.versions.iter()
                .filter(|v| v.state == BeatSaverMapVersionState::Published)
                .count(),
            1
        );
        self.versions.iter()
            .filter(|v| v.state == BeatSaverMapVersionState::Published)
            .next()
            .unwrap()
    }
}
#[derive(Deserialize)]
struct BeatSaverMapMetadata {
    #[serde(rename = "songName")]
    song_name: String,
    #[serde(rename = "songSubName")]
    song_sub_name: String,
}
#[derive(Deserialize)]
struct BeatSaverMapStats {
    upvotes: u32,
    downvotes: u32,
}
#[derive(Deserialize)]
struct BeatSaverMapUploader {
    name: String,
}
#[derive(Deserialize)]
struct BeatSaverMapVersion {
    hash: String,
    diffs: Vec<BeatSaverMapDifficulty>,
    state: BeatSaverMapVersionState,
}
#[derive(Deserialize)]
struct BeatSaverMapDifficulty {
    characteristic: String,
}
#[derive(Deserialize)]
#[derive(PartialEq, Eq)]
enum BeatSaverMapVersionState {
    Feedback,
    Published,
    Testplay,
    Uploaded,
}

const INFO_PAUSE: time::Duration = time::Duration::from_secs(3);
const BSABER_DL_PAUSE: time::Duration = time::Duration::from_secs(10);
const BEATSAVER_DL_PAUSE: time::Duration = time::Duration::from_secs(120);

// Additional padding to apply when ratelimited, to prove we're being a good citizen
const RATELIMIT_PADDING: time::Duration = time::Duration::from_secs(60);

fn main() {
    dotenv().ok();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("bsmeta=info,warn")).init();
    env::set_var("ASYNC_STD_THREAD_COUNT", "3");

    let args: Vec<String> = env::args().collect();
    if args.len() != 2 {
        panic!("wrong num of args")
    }
    match args[1].as_str() {
        "script-checkdeleted" => scripts::checkdeleted(),
        "script-regenzipderived" => scripts::regenzipderived(),
        "unknown" => {
            println!("Considering unknown keys");
            println!("Unknown keys: {:?}", unknown_songs().len());
        },
        "dl" => dl_data(),
        "dlmeta" => dl_meta(),
        "analyse" => analyse_songs(),
        "update-search" => update_search(),
        "serve" => server::serve(),
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
        query!("SELECT s.key, sm.hash FROM tSong s, tSongMeta sm, tSongData sd WHERE s.deleted = false AND s.key = sm.key AND sm.hash = sd.hash").fetch_all(conn)
    ).expect("failed to select keys and hashes");

    let pluginlist_file = fs::File::open("plugins/dist/pluginlist.json").expect("failed to open plugin list");
    let pluginlist: HashMap<String, String> = serde_json::from_reader(pluginlist_file).expect("failed to parse plugin list");
    let mut analyses = vec![];
    for (name, interp) in pluginlist.into_iter() {
        println!("Loading plugin {}", name);
        let interp_path = format!("plugins/dist/{}.wasm", interp);
        let plugin_path = format!("plugins/dist/{}.tar", name);
        let plugin = wasm::load_plugin(&name, interp_path.as_ref(), plugin_path.as_ref()).expect("failed to load plugin");
        analyses.push(plugin)
    }

    let num_to_analyse = to_analyse.len();
    for (i, res) in to_analyse.into_iter().enumerate() {
        let key_str = num_to_key(res.key);
        info!("Considering song {}/{}: {}", i+1, num_to_analyse, key_str);
        for plugin in analyses.iter() {
            let plugin_name = plugin.name();
            let exists: bool = task::block_on(
                query!("SELECT count(*) as count FROM tSongAnalysis WHERE hash = ? AND analysis_name = ?", res.hash, plugin_name)
                    .fetch_one(conn)
            ).expect("failed to check if analysis exists").count > 0;
            if exists {
                continue
            }
            info!("Performing analysis {:?} on {}", plugin.name(), key_str);
            let dats = load_dats_for_analysis(conn, &res.hash);

            info!("Analysing {} dats", dats.len());
            let results = match plugin.run(dats) {
                Ok((_stderr, Ok(r))) => r,
                Ok((_, Err(e))) |
                Err(e) => {
                    warn!("Failed to run analysis: {}", e);
                    continue
                },
            };

            let result_json = serde_json::to_vec(&results).expect("failed to convert results to json");
            insert_song_analysis(conn, res.hash.clone(), plugin.name(), result_json)
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

fn load_dats_for_analysis(conn: &SqliteConnection, hash: &str) -> HashMap<String, Vec<u8>> {
    let data = task::block_on(
        query!("SELECT data FROM tSongData WHERE hash = ?", hash).fetch_one(conn)
    ).expect("failed to load dat data").data;

    let mut ar = tar::Archive::new(&*data);
    let mut dats = HashMap::new();
    for entry in ar.entries().expect("failed to parse dat tar") {
        let mut entry = entry.expect("failed to decode dat entry");
        let path_bytes = entry.path_bytes();
        // Standardise for info.dat to always be lowercase
        let path = if path_bytes.eq_ignore_ascii_case(b"info.dat") {
            "info.dat"
        } else {
            str::from_utf8(&path_bytes).unwrap()
        }.to_owned();
        let mut v = vec![];
        let path = path.to_owned();
        entry.read_to_end(&mut v).unwrap();
        assert!(dats.insert(path, v).is_none())
    }
    assert!(!dats.is_empty());

    dats
}

fn update_search() {
    use meilisearch_sdk::document::Document;
    use meilisearch_sdk::client::Client;

    const ID_KEY: &str = "key";
    const SEARCH_KEYS: &[&str] = &["name", "sub_name", "description"];
    const _FILTER_KEYS: &[&str] = &["total_votes", "pct_upvoted", "uploaded_at_tstamp"];
    const FACET_KEYS: &[&str] = &["uploader"];
    const FACET_GROUP_KEYS: &[&str] = &["modes"];
    const _VIEW_KEYS: &[&str] = &[];

    #[derive(Serialize, Deserialize)]
    #[derive(Debug)]
    struct MeiliSong {
        // ID
        key: String,

        // Search keys
        name: String,
        sub_name: String,
        // TODO: use description from bsaber.com instead
        description: String,

        // Filter keys
        total_votes: u32,
        pct_upvoted: u8,
        uploaded_at_tstamp: i64,
        // TODO: difficulties
        // TODO: categories from bsaber.com?

        // Facet keys
        uploader: String,

        // Facet group keys
        modes: Vec<String>,
        // TODO: categories from bsaber.com

        // Just for viewing
        #[serde(flatten)]
        analyses: HashMap<String, serde_json::Value>,
        // TODO: bsaber.com post id
    }

    impl Document for MeiliSong {
        type UIDType = String;
        fn get_uid(&self) -> &Self::UIDType { &self.key }
    }

    async fn wait_progress_complete(progress: meilisearch_sdk::progress::Progress) {
        loop {
            match progress.get_status().await.expect("meilisearch returned error") {
                meilisearch_sdk::progress::UpdateStatus::Processed { content } => {
                    assert!(content.error.is_none());
                    break
                },
                meilisearch_sdk::progress::UpdateStatus::Failed { content } => {
                    panic!("{:?}", content)
                }
                meilisearch_sdk::progress::UpdateStatus::Processing { content: _ } |
                meilisearch_sdk::progress::UpdateStatus::Enqueued { content: _ } => {
                    task::sleep(time::Duration::from_secs(1)).await
                },
            }
        }
    }

    let conn = &establish_connection();

    let meili_url = env::var("MEILI_URL").expect("no meili url");
    let meili_masterkey = env::var("MEILI_PRIVATEKEY").expect("no meili masterkey");
    let client = Client::new(&meili_url, &meili_masterkey);

    const BATCH_SIZE: usize = 1000;

    task::block_on(async {
        match client.delete_index("songs").await {
            Ok(()) => (),
            Err(meilisearch_sdk::errors::Error::MeiliSearchError { error_code: meilisearch_sdk::errors::ErrorCode::IndexNotFound, .. }) => (),
            Err(e) => panic!("failed to delete index: {}", e),
        }

        let idx = client.create_index("songs", Some(ID_KEY)).await.expect("failed to create index");

        let searchable_attributes = SEARCH_KEYS.into_iter().chain(FACET_KEYS).chain(FACET_GROUP_KEYS)
            .map(|&s| s.to_owned())
            .collect();
        let filterable_attributes = FACET_KEYS.into_iter().chain(FACET_GROUP_KEYS)
            .map(|&s| s.to_owned())
            .collect();
        let progress = idx.set_settings(&meilisearch_sdk::settings::Settings {
            synonyms: None,
            stop_words: None,
            ranking_rules: None,
            filterable_attributes: Some(filterable_attributes),
            sortable_attributes: None,
            distinct_attribute: None,
            searchable_attributes: Some(searchable_attributes),
            displayed_attributes: Some(vec!["*".to_owned()]),
        }).await.expect("failed to set settings");
        wait_progress_complete(progress).await;

        println!("Waiting for meilisearch to apply index settings");

        let songs = query!("SELECT s.key, sm.hash FROM tSong s, tSongMeta sm WHERE s.deleted = false AND s.key = sm.key").fetch_all(conn).await.expect("failed to select songs");
        let mut songs: Vec<_> = songs.into_iter().map(|res| (res.key, res.hash)).collect();
        println!("Loaded keys");
        songs.sort_by_key(|(key, _hash)| *key);

        let num_songs = songs.len();
        let mut batch = vec![];
        for (i, (key, hash)) in songs.into_iter().enumerate() {
            let key_str = num_to_key(key);
            info!("Considering song {}/{}: {}", i+1, num_songs, key_str);
            if batch.len() == BATCH_SIZE {
                let progress = idx.add_or_replace(&batch, Some(ID_KEY)).await.expect("failed to send batch of songs for addition");
                wait_progress_complete(progress).await;
                batch.clear()
            }
            let song_meta = get_db_song_meta(conn, key).expect("song went missing from db");
            let bsmeta: BeatSaverMap = serde_json::from_slice(&song_meta.bsmeta).expect("failed to deserialize bsmeta");

            let mut analyses = HashMap::new();
            let analysis_results: Vec<_> =
                query!("SELECT analysis_name, result FROM tSongAnalysis WHERE hash = ?", hash).fetch_all(conn).await.expect("failed to retrieve analyses");
            for ar in analysis_results {
                let analysis_results_map: HashMap<String, serde_json::Value> = serde_json::from_slice(&ar.result).expect("couldn't parse analysis result");
                // Prefix results with plugin
                analyses.extend(analysis_results_map.into_iter().map(|(k, v)| (format!("{}-{}", ar.analysis_name, k), v)));
            }

            let total_votes = bsmeta.stats.upvotes + bsmeta.stats.downvotes;
            let pct_upvoted = if total_votes == 0 { 100. } else { (100. * f64::from(bsmeta.stats.upvotes) / f64::from(total_votes)).round() };
            assert!(0. <= pct_upvoted && pct_upvoted <= 100.);
            let pct_upvoted = pct_upvoted as u8;
            let modes = bsmeta.current_version().diffs.iter().map(|d| d.characteristic.clone()).collect();
            let ms = MeiliSong {
                key: key_str,
                name: bsmeta.metadata.song_name,
                sub_name: bsmeta.metadata.song_sub_name,
                description: bsmeta.description,
                total_votes,
                pct_upvoted,
                uploaded_at_tstamp: bsmeta.uploaded.timestamp(),
                uploader: bsmeta.uploader.name,
                modes,
                analyses,
            };
            batch.push(ms)
        }
        let progress = idx.add_or_replace(&batch, Some(ID_KEY)).await.expect("failed to send batch of songs for addition");
        wait_progress_complete(progress).await;
        batch.clear()
    });
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
    let mut before = chrono::Utc::now();
    // Don't go too far if we've missed lots, we have the ability to backfill songs
    const MAX_PAGE: usize = 20;
    let mut maps: Vec<(BeatSaverMap, Vec<u8>)> = vec![];
    let one_sec = chrono::Duration::seconds(1);
    loop {
        let res = get_latest_maps(client, before + one_sec).expect("failed to get latest maps");

        let mut num_new = 0;
        let num_maps = res.docs.len();
        for (map, map_value) in res.docs {
            if get_db_song_meta(conn, key_to_num(&map.key)).is_none() {
                println!("Found a new map: {}", map.key);
                num_new += 1
            } else {
                println!("Map {} already in db", map.key);
            }
            before = cmp::min(map.updated_at, before);
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
    while let Some((map, raw_meta)) = maps.pop() {
        println!("Upserting {}", map.key);
        map.check();
        upsert_song(conn, key_to_num(&map.key), Some((map.current_version().hash.clone(), raw_meta)))
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
                m.check();
                upsert_song(conn, key, Some((m.current_version().hash.clone(), raw.get().as_bytes().to_owned())))
            },
            None => {
                upsert_song(conn, key, None)
            },
        }
        thread::sleep(INFO_PAUSE)
    }
}

fn dl_data() {
    let conn = &establish_connection();

    println!("Finding songs to download");
    let mut to_download = task::block_on(
        query!("
            SELECT s.key, sm.hash, sm.bsmeta
            FROM tSong s
                INNER JOIN tSongMeta sm ON s.key = sm.key
                LEFT OUTER JOIN tSongData sd ON sm.hash = sd.hash
            WHERE s.deleted = false AND sd.hash IS NULL
        ").fetch_all(conn)
    ).expect("failed to select keys");
    println!("Got {} not yet downloaded", to_download.len());
    to_download.sort_by_key(|res| res.key);
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

    let mut blacklisted_hashes = load_blacklist();
    println!("Got {} blacklisted_hashes", blacklisted_hashes.len());

    let to_download: Vec<_> = to_download.into_iter()
        .filter(|res| !blacklisted_hashes.contains_key(&res.hash))
        .collect();
    let num_to_download = to_download.len();
    println!("Got {} to try and download", num_to_download);

    let client = &make_client();
    let mut last_bsaber_dl = time::Instant::now();
    let mut last_beatsaver_dl = time::Instant::now();
    for (i, res) in to_download.into_iter().enumerate() {
        let key_str = num_to_key(res.key);
        println!("Considering song {} ({}/{})", key_str, i+1, num_to_download);
        if let Some(reason) = blacklisted_hashes.get(&res.hash) {
            println!("Skipping song {} ({}) - previous failure: {}", key_str, res.hash, reason);
            continue
        }
        // Skip recent songs to give bsaber.org a chance to cache, to spread out our downloads off
        // beatsaver
        let bsm: BeatSaverMap = serde_json::from_slice(&res.bsmeta).expect("failed to parse bsmeta");
        if chrono::Utc::now().signed_duration_since(bsm.uploaded).num_days() < 1 {
            println!("Skipping song - uploaded within last 24 hours");
            continue
        }
        println!("Getting song zip for {} {}", key_str, res.hash);
        let zipdata: Vec<u8> = match get_song_zip(client, &res.hash, &mut last_bsaber_dl, &mut last_beatsaver_dl) {
            Ok(zd) => zd,
            Err(e) => {
                blacklisted_hashes.insert(res.hash.clone(), format!("get song zip failed: {}", e));
                save_blacklist(&blacklisted_hashes);
                continue
            },
        };
        println!("Converting zip for {} to dats tar", key_str);
        let (tardata, extra_meta) = match zip_to_dats_tar(&zipdata) {
            Ok((td, em)) => (td, em),
            Err(e) => {
                blacklisted_hashes.insert(res.hash.clone(), format!("zip to dats tar failed: {}", e));
                save_blacklist(&blacklisted_hashes);
                continue
            },
        };
        set_song_data(conn, res.hash, tardata, extra_meta, zipdata);
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

fn get_song_zip(client: &reqwest::blocking::Client, hash: &str, last_bsaber_dl: &mut time::Instant, last_beatsaver_dl: &mut time::Instant) -> Result<Vec<u8>> {
    //let url = format!("https://cdn.beatsaver.com/{}.zip", hash);
    //println!("Retrieving {} from url {}", hash, url);
    //let mut res = client.get(&url).send().expect("failed to send request");
    //println!("Got response {}", res.status());
    //if res.status() == reqwest::StatusCode::UNAUTHORIZED {
    //    let url = format!("https://bsaber.org/files/cache/zip/{}.zip", hash);
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
        println!("Retrieving {} from bsaber.org", hash);
        let (res, headers) = match do_req(client, &format!("https://bsaber.org/files/cache/zip/{}.zip", hash)) {
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
        println!("Retrieving {} from beatsaver.com", hash);
        let (res, headers) = match do_req(client, &format!("https://cdn.beatsaver.com/{}.zip", hash)) {
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

fn get_latest_maps(client: &reqwest::blocking::Client, before: DateTime<Utc>) -> Result<BeatSaverLatestResponse> {
    println!("Getting maps before {}", before);
    let before = before.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let (res, headers) = do_req(client, &format!("https://api.beatsaver.com/maps/latest?automapper=true&sort=UPDATED&before={}", before))?;
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
        let (res, headers) = do_req(client, &format!("https://api.beatsaver.com/maps/id/{}", key_str))?;
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
    debug!("request to url: {}", url);
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
    let mut zip = zip::ZipArchive::new(zipreader).context("failed to load zip")?;
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

// https://github.com/launchbadge/sqlx/issues/328 - for inserting a Song struct
fn set_song_data(conn: &SqliteConnection, hash: String, data: Vec<u8>, extra_meta: ExtraMeta, zipdata: Vec<u8>) {
    let res = task::block_on(
        query!("INSERT INTO tSongData (hash, data, extra_meta, zipdata) VALUES (?, ?, ?, ?)", hash, data, extra_meta, zipdata)
            .execute(conn)
    ).expect("error updating song data");
    assert_eq!(res.rows_affected(), 1, "{:?}", (hash, data, extra_meta, zipdata))
}

// TODO: should only need to pass a &str here but sqlx 0.4 has a 'static bound and we can't upgrade
// to 0.5 (see Cargo.toml)
fn upsert_song(conn: &SqliteConnection, key: i64, hash_and_meta: Option<(String, Vec<u8>)>) {
    let tstamp = Utc::now().timestamp_millis();
    task::block_on(async move {
        let mut conn = conn.acquire().await.unwrap();
        conn.transaction::<_, _, sqlx::Error>(move |conn| Box::pin(async move {
            let deleted = hash_and_meta.is_none();
            let res = query!("
                INSERT INTO tSong (key, deleted, tstamp) VALUES (?, ?, ?)
                ON CONFLICT (key) DO UPDATE SET deleted=excluded.deleted, tstamp=excluded.tstamp
            ", key, deleted, tstamp)
                .execute(&mut *conn).await?;
            assert_eq!(res.rows_affected(), 1, "upsert song {}", key);
            if let Some((hash, bsmeta)) = hash_and_meta {
                let res = query!("
                    INSERT INTO tSongMeta (key, hash, bsmeta) VALUES (?, ?, ?)
                    ON CONFLICT (key) DO UPDATE SET hash=excluded.hash, bsmeta=excluded.bsmeta
                ", key, hash, bsmeta)
                    .execute(&mut *conn).await?;
                assert_eq!(res.rows_affected(), 1, "upsert meta {}", key);
            }
            Ok(())
        })).await
    }).expect("error upserting song")
}

fn insert_song_analysis(conn: &SqliteConnection, hash: String, analysis_name: &str, result: Vec<u8>) {
    let res = task::block_on(
        query!("INSERT INTO tSongAnalysis (hash, analysis_name, result) VALUES (?, ?, ?)", hash, analysis_name, result)
            .execute(conn)
    ).expect("error saving song analysis");
    assert_eq!(res.rows_affected(), 1, "insert {}", hash)
}

fn get_db_song_meta(conn: &SqliteConnection, song_key: i64) -> Option<SongMeta> {
    task::block_on(
        query_as!(SongMeta, "SELECT * FROM tSongMeta WHERE key = ?", song_key)
            .fetch_optional(conn)
    ).expect("failed to load song meta")
}

pub fn establish_connection() -> SqliteConnection {
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
