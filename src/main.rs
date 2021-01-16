#![recursion_limit="256"]

#[macro_use]
extern crate diesel;

use anyhow::{Context, Result, anyhow, bail};
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
use std::path::Path;
use std::str;
use std::thread;
use std::time;

mod scripts;
#[allow(non_snake_case)]
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

const INFO_PAUSE: time::Duration = time::Duration::from_secs(3);
const DL_PAUSE: time::Duration = time::Duration::from_secs(10);

// Additional padding to apply when ratelimited, to prove we're being a good citizen
const RATELIMIT_PADDING: time::Duration = time::Duration::from_secs(5);

fn main() {
    env_logger::init();
    let args: Vec<String> = env::args().collect();
    if args.len() != 2 {
        panic!("wrong num of args")
    }
    match args[1].as_str() {
        "script-loadjson" => scripts::loadjson(),
        "script-checkdeleted" => scripts::checkdeleted(),
        "script-getmissingbsmeta" => scripts::getmissingbsmeta(),
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

fn key_to_num<T: AsRef<str>>(k: &T) -> i32 {
    u32::from_str_radix(k.as_ref(), 16).expect("can't parse key").try_into().expect("too big for i32")
}

fn num_to_key(n: i32) -> String {
    assert!(n >= 0);
    format!("{:x}", n)
}

fn test() -> Result<()> {
    println!("doing wasm things");
    use wasmer::{Array, WasmPtr, ValueType, LazyInit, Memory, Store, WasmerEnv, LikeNamespace, Cranelift, JIT, Exports, Export, Exportable, Val, ExternType, Function, ImportType, Resolver, Module, Instance, Value, imports, RuntimeError};
    use wasmer_wasi::{WasiState, WasiEnv, WasiVersion};

    //let module_bytes = fs::read("quickjs/qjs.wasm")?;
    let module_bytes = fs::read("out.wasm")?;

    // TODO: wasmer/examples/tunables_limit_memory.rs
    let mut cranelift = Cranelift::new();
    cranelift.enable_simd(false);
    let engine = JIT::new(cranelift).engine();
    let store = Store::new(&engine);
    let module = Module::from_binary(&store, &module_bytes)?;
    println!("{:?}", module);
    println!("imports:");
    for import in module.imports() {
        println!("{:?}", import)
    }
    println!("exports:");
    for export in module.exports() {
        println!("{:?}", export)
    }

    struct FakeResolver {
        exports: Vec<Export>,
    }
    impl FakeResolver {
        fn new(store: &Store, imports: impl Iterator<Item=ImportType>, overrides: &HashMap<(&str, &str), Export>) -> Self {
            let exports = imports.map(|import| {
                let path = format!("{}:{}", import.module(), import.name());
                if let Some(ex) = overrides.get(&(import.module(), import.name())) {
                    println!("using override for {}", path);
                    return ex.clone()
                }
                println!("shimming import {:?}", import);
                let ty: &ExternType = import.ty();
                match ty {
                    ExternType::Function(ft) => {
                        let errfn = move |_vals: &[Val]| -> Result<Vec<Val>, RuntimeError> {
                            Err(RuntimeError::new(path.clone()))
                        };
                        let f = Function::new(store, ft, errfn);
                        f.to_export()
                    },
                    other => {
                        todo!("unable to shim {:?}", other)
                    },
                }
            }).collect();
            Self { exports }
        }
    }
    impl Resolver for FakeResolver {
        fn resolve(&self, index: u32, _module: &str, _field: &str) -> Option<Export> {
            Some(self.exports[index as usize].clone())
        }
    }

    #[derive(Default, Clone, WasmerEnv)]
    struct MyEnv {
        #[wasmer(export)]
        memory: LazyInit<Memory>,
    }
    #[derive(Clone, WasmerEnv)]
    struct ScriptEnv {
        script: Vec<u8>,
        script_len: u32,
        #[wasmer(export)]
        memory: LazyInit<Memory>,
    }


    // Steal these from wasi for minimal consistency
    use std::cell::Cell;
    use std::sync::Arc;
    use wasmer_wasi::ALL_RIGHTS;
    use wasmer_wasi::types::{
        __wasi_ciovec_t,
        __wasi_errno_t,
        __wasi_fdstat_t,
        __wasi_prestat_t,
        __wasi_prestat_u,
        __wasi_prestat_u_dir_t,
        __wasi_fd_t,

        __WASI_FILETYPE_DIRECTORY,

        __WASI_PREOPENTYPE_DIR,

        __WASI_ESUCCESS,
        __WASI_EINVAL,
        __WASI_EFAULT,
        __WASI_EBADF,
        __WASI_EIO,

        __WASI_STDIN_FILENO,
        __WASI_STDOUT_FILENO,
        __WASI_STDERR_FILENO,
    };
    fn write_bytes_inner<T: io::Write>(
        mut write_loc: T,
        memory: &Memory,
        iovs_arr_cell: &[Cell<__wasi_ciovec_t>],
    ) -> Result<u32, u16> {
        let mut bytes_written = 0;
        for iov in iovs_arr_cell {
            let iov_inner = iov.get();
            let bytes = iov_inner.buf.deref(memory, 0, iov_inner.buf_len)?;
            write_loc
                .write_all(&bytes.iter().map(|b_cell| b_cell.get()).collect::<Vec<u8>>())
                .map_err(|_| __WASI_EIO)?;

            // TODO: handle failure more accurately
            bytes_written += iov_inner.buf_len;
        }
        Ok(bytes_written)
    }
    fn write_bytes<T: io::Write>(
        mut write_loc: T,
        memory: &Memory,
        iovs_arr_cell: &[Cell<__wasi_ciovec_t>],
    ) -> Result<u32, u16> {
        // TODO: limit amount written
        let result = write_bytes_inner(&mut write_loc, memory, iovs_arr_cell);
        write_loc.flush();
        result
    }
    macro_rules! mytry {
        ($e:expr) => {{
            match $e {
                Ok(v) => v,
                Err(e) => return e,
            }
        }};
    }

    let script = fs::read("plugintest.js").expect("failed to open plugin");
    let script_len: u32 = script.len().try_into().expect("script too big");
    let script_env = ScriptEnv { script, script_len, memory: Default::default() };

    const WORKDIR: &[u8] = b"/work";
    let overrides = vec![
        (
            ("env", "get_script_size"),
            Function::new_native_with_env(&store, script_len, move |&script_len_env: &u32| -> u32 { script_len_env }).to_export(),
        ),
        (
            ("env", "get_script_data"),
            Function::new_native_with_env(&store, script_env, move |env: &ScriptEnv, data: WasmPtr<u8, Array>| -> () {
                let memory = env.memory_ref().expect("memory not set up");
                let data = mytry!(data.deref(memory, 0, env.script_len).ok_or(()));
                for (cell, &b) in data.into_iter().zip(env.script.iter()) {
                    cell.set(b)
                }
            }).to_export(),
        ),
        (
            ("wasi_snapshot_preview1", "args_get"),
            Function::new_native_with_env(&store, MyEnv::default(), move |env: &MyEnv, argv: WasmPtr<WasmPtr<u8, Array>, Array>, argv_buf: WasmPtr<u8, Array>| -> __wasi_errno_t {
                println!("wasi-ish>> args_get");
                __WASI_ESUCCESS
            }).to_export(),
        ),
        (
            ("wasi_snapshot_preview1", "args_sizes_get"),
            Function::new_native_with_env(&store, MyEnv::default(), move |env: &MyEnv, argc: WasmPtr<u32>, argv_buf_size: WasmPtr<u32>| -> __wasi_errno_t {
                println!("wasi-ish>> args_sizes_get");
                let memory = env.memory_ref().expect("memory not set up");
                let argc = mytry!(argc.deref(memory).ok_or(__WASI_EFAULT));
                let argv_buf_size = mytry!(argv_buf_size.deref(memory).ok_or(__WASI_EFAULT));
                argc.set(0);
                argv_buf_size.set(0);
                __WASI_ESUCCESS
            }).to_export(),
        ),
        (
            ("wasi_snapshot_preview1", "clock_time_get"),
            Function::new_native_with_env(&store, MyEnv::default(), move |env: &MyEnv, _clock_id: u32, _precision: u64, time: WasmPtr<u64>| -> __wasi_errno_t {
                println!("wasi-ish>> clock_time_get");
                let memory = env.memory_ref().expect("memory not set up");
                let out_addr = mytry!(time.deref(memory).ok_or(__WASI_EFAULT));
                out_addr.set(0);
                __WASI_ESUCCESS
            }).to_export(),
        ),
        (
            ("wasi_snapshot_preview1", "environ_sizes_get"),
            Function::new_native_with_env(&store, MyEnv::default(), move |env: &MyEnv, environ_count: WasmPtr<u32>, environ_buf_size: WasmPtr<u32>| -> __wasi_errno_t {
                println!("wasi-ish>> environ_sizes_get");
                let memory = env.memory_ref().expect("memory not set up");
                let environ_count = mytry!(environ_count.deref(memory).ok_or(__WASI_EFAULT));
                let environ_buf_size = mytry!(environ_buf_size.deref(memory).ok_or(__WASI_EFAULT));
                environ_count.set(0);
                environ_buf_size.set(0);
                __WASI_ESUCCESS
            }).to_export(),
        ),
        (
            ("wasi_snapshot_preview1", "fd_write"),
            Function::new_native_with_env(&store, MyEnv::default(), move |env: &MyEnv, fd: __wasi_fd_t, iovs: WasmPtr<__wasi_ciovec_t, Array>, iovs_len: u32, nwritten: WasmPtr<u32>| -> __wasi_errno_t {
                println!("wasi-ish>> fd_write {}", fd);
                let memory = env.memory_ref().expect("memory not set up");
                let iovs = mytry!(iovs.deref(memory, 0, iovs_len).ok_or(__WASI_EFAULT));
                let nwritten = mytry!(nwritten.deref(memory).ok_or(__WASI_EFAULT));
                let count = mytry!(match fd {
                    __WASI_STDOUT_FILENO => write_bytes(io::stdout().lock(), &memory, iovs),
                    __WASI_STDERR_FILENO => write_bytes(io::stderr().lock(), &memory, iovs),
                    _ => return __WASI_EINVAL,
                });
                nwritten.set(count);
                __WASI_ESUCCESS
            }).to_export(),
        ),
        (
            ("wasi_snapshot_preview1", "fd_fdstat_get"),
            Function::new_native_with_env(&store, MyEnv::default(), move |env: &MyEnv, fd: __wasi_fd_t, fdstat: WasmPtr<__wasi_fdstat_t>| -> __wasi_errno_t {
                println!("wasi-ish>> fd_fdstat_get {}", fd);
                match fd {
                    3 => {
                        let memory = env.memory_ref().expect("memory not set up");
                        let fdstat = mytry!(fdstat.deref(memory).ok_or(__WASI_EFAULT));
                        fdstat.set(__wasi_fdstat_t {
                            fs_filetype: __WASI_FILETYPE_DIRECTORY,
                            fs_flags: 0,
                            fs_rights_base: ALL_RIGHTS,
                            fs_rights_inheriting: ALL_RIGHTS,
                        });
                    },
                    _ => return __WASI_EBADF,
                }
                __WASI_ESUCCESS
            }).to_export(),
        ),
        // Path and preopen handling
        // https://github.com/WebAssembly/wasi-libc/blob/5b148b6131f36770f110c24d61adfb1e17fea06a/libc-bottom-half/sources/preopens.c#L201
        (
            ("wasi_snapshot_preview1", "fd_prestat_dir_name"),
            Function::new_native_with_env(&store, MyEnv::default(), move |env: &MyEnv, fd: __wasi_fd_t, path: WasmPtr<u8, Array>, path_len: u32| -> __wasi_errno_t {
                println!("wasi-ish>> fd_prestat_dir_name {}", fd);
                match fd {
                    3 => {
                        assert_eq!(path_len as usize, WORKDIR.len());
                        let memory = env.memory_ref().expect("memory not set up");
                        let path = mytry!(path.deref(memory, 0, path_len).ok_or(__WASI_EFAULT));
                        for (cell, &b) in path.into_iter().zip(WORKDIR.iter()) {
                            cell.set(b)
                        }
                    },
                    _ => return __WASI_EBADF,
                }
                __WASI_ESUCCESS
            }).to_export(),
        ),
        (
            ("wasi_snapshot_preview1", "fd_prestat_get"),
            Function::new_native_with_env(&store, MyEnv::default(), move |env: &MyEnv, fd: __wasi_fd_t, prestat: WasmPtr<__wasi_prestat_t>| -> __wasi_errno_t {
                println!("wasi-ish>> fd_prestat_get {}", fd);
                match fd {
                    3 => {
                        let memory = env.memory_ref().expect("memory not set up");
                        let prestat = mytry!(prestat.deref(memory).ok_or(__WASI_EFAULT));
                        prestat.set(__wasi_prestat_t {
                            pr_type: __WASI_PREOPENTYPE_DIR,
                            u: __wasi_prestat_u {
                                dir: __wasi_prestat_u_dir_t {
                                    pr_name_len: WORKDIR.len().try_into().expect("workdir too long"),
                                },
                            },
                        })
                    },
                    _ => return __WASI_EBADF,
                }
                __WASI_ESUCCESS
            }).to_export(),
        ),
        (
            ("wasi_snapshot_preview1", "fd_prestat_get"),
            Function::new_native_with_env(&store, MyEnv::default(), move |env: &MyEnv, fd: __wasi_fd_t, prestat: WasmPtr<__wasi_prestat_t>| -> __wasi_errno_t {
                println!("wasi-ish>> fd_prestat_get {}", fd);
                match fd {
                    3 => {
                        let memory = env.memory_ref().expect("memory not set up");
                        let prestat = mytry!(prestat.deref(memory).ok_or(__WASI_EFAULT));
                        prestat.set(__wasi_prestat_t {
                            pr_type: __WASI_PREOPENTYPE_DIR,
                            u: __wasi_prestat_u {
                                dir: __wasi_prestat_u_dir_t {
                                    pr_name_len: WORKDIR.len().try_into().expect("workdir too long"),
                                },
                            },
                        })
                    },
                    _ => return __WASI_EBADF,
                }
                __WASI_ESUCCESS
            }).to_export(),
        ),
    ].into_iter().collect();
    let importobj = FakeResolver::new(&store, module.imports(), &overrides);

    //let fake_namespace: Exports = module.imports()
    //    .filter(|import| import.module() == "env")
    //    .map(|import| {
    //        let ty: &ExternType = import.ty();
    //        let ex = match ty {
    //            ExternType::Function(ft) => {
    //                let path = format!("{}:{}", import.module(), import.name());
    //                let errfn = move |_vals: &[Val]| -> Result<Vec<Val>, RuntimeError> {
    //                    Err(RuntimeError::new(path.clone()))
    //                };
    //                let f = Function::new(&store, ft, errfn);
    //                f.into()
    //            },
    //            other => {
    //                todo!("unable to shim {:?}", other)
    //            },
    //        };
    //        (import.name().to_owned(), ex)
    //    })
    //    .collect();

    //let wasi_state = WasiState::new("testy")
    //    .args(&["--std", "--script", "/plugintest.js"])
    //    .preopen_dir("/tmp")?
    //    //.preopen_dir("/home/aidanhs/Desktop/per/bsaber/bsmeta")?
    //    .build()?;
    //let wasi_env = WasiEnv::new(wasi_state);
    //let wasi_vsn = wasmer_wasi::get_wasi_version(&module, false).unwrap();
    //let mut importobj = wasmer_wasi::generate_import_object_from_env(&store, wasi_env, wasi_vsn);
    //println!("wasi generated importobj");
    //for (path, _export) in importobj.clone() {
    //    println!("{:?}", path)
    //}
    //importobj.register("env", fake_namespace);

    let instance = Instance::new(&module, &importobj)?;

    println!("running: _start");
    let f = instance.exports.get_function("_start")?;
    match f.call(&[]) {
        Ok(vals) => println!("success: {:?}", vals),
        Err(re) => {
            println!("fail: {}", re);
            println!("trace: {:#?}", re.trace());
            bail!("oh no")
        }
    }
    println!("running: do_analysis");
    let f = instance.exports.get_function("do_analysis")?;
    match f.call(&[]) {
        Ok(vals) => println!("success: {:?}", vals),
        Err(re) => {
            println!("fail: {}", re);
            println!("trace: {:#?}", re.trace());
            bail!("oh no")
        }
    }
    Ok(())
}

fn unknown_songs() -> Vec<i32> {
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
    unknown
}

fn analyse_songs() {
    println!("Analysing songs");
    let conn = &establish_connection();

    let to_analyse: Vec<(Song, SongData)> = {
        use schema::tSong::dsl::*;
        use schema::tSongData::dsl::*;
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
    while let Some((BeatSaverMap { key, hash }, raw_meta)) = maps.pop() {
        println!("Upserting {}", key);
        upsert_song(conn, key_to_num(&key), Some(hash), false, Some(raw_meta))
    }
}

fn dl_unknown_meta(conn: &SqliteConnection, client: &reqwest::blocking::Client) {
    println!("Finding song metas to download");
    let unknown = unknown_songs();
    let num_unknown = unknown.len();
    println!("Found {} unknown songs to download", num_unknown);
    for (i, key) in unknown.into_iter().enumerate() {
        let key_str = num_to_key(key);
        println!("Getting meta for song {} ({}/{})", key_str, i+1, num_unknown);
        match get_map(client, key).expect("failed to get map for song") {
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
    let to_download: Vec<(Song, Option<SongData>)> = {
        use schema::tSong::dsl::*;
        use schema::tSongData::dsl::*;
        tSong
            .left_join(tSongData)
            .filter(schema::tSong::hash.is_not_null())
            .filter(schema::tSongData::key.is_null())
            .filter(deleted.eq(false))
            .load(conn).expect("failed to select keys")
    };
    let mut to_download: Vec<Song> = to_download.into_iter().map(|(s, _sd)| s).collect();
    println!("Got {} to download", to_download.len());
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
    let client = &make_client();
    for song in to_download {
        let key_str = num_to_key(song.key);
        let hash = song.hash.expect("non-null hash was None");
        if let Some(reason) = blacklisted_keys.get(&key_str) {
            println!("Skipping song {} - previous failure: {}", song.key, reason);
            continue
        }
        println!("Considering song {}", song.key);
        println!("Getting song zip for {} {}", song.key, hash);
        let zipdata = match get_song_zip(client, &key_str, &hash) {
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
        thread::sleep(DL_PAUSE);
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
#[derive(Deserialize)]
struct BeatSaverMap {
    key: String,
    hash: String,
}

macro_rules! retry {
    ($t:expr, $e:expr) => {{
        println!("request failure: {}", $e);
        thread::sleep($t);
        continue
    }};
}

const RATELIMIT_RESET_AFTER_HEADER: &str = "x-ratelimit-reset-after";

fn get_song_zip(client: &reqwest::blocking::Client, key_str: &str, hash: &str) -> Result<Vec<u8>> {
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
            panic!("failed to retrieve data")
        }
        attempts -= 1;

        println!("Retrieving {} from bsaber", key_str);
        let (res, headers) = match do_req(client, &format!("https://bsaber.org/files/cache/zip/{}.zip", key_str)) {
            Ok(r) => r,
            Err(e) => retry!(DL_PAUSE, format!("failed to send request: {}", e)),
        };
        println!("Got response {}", res.status());

        if res.status() == reqwest::StatusCode::NOT_FOUND {
            bail!("song not found from bsaber.org")
        }
        if !res.status().is_success() {
            retry!(DL_PAUSE, format!("non-success response: {:?} {:?}", headers, res.bytes()))
        }
        let bytes = match res.bytes() {
            Ok(bs) => bs,
            Err(e) => retry!(DL_PAUSE, format!("failed to get bytes: {}, response headers: {:?}", e, headers)),
        };

        return Ok(bytes.as_ref().to_owned())
    }

    //println!("Retrieving {} from fs", key_str);
    //let path = format!("../beatmaps/{}.zip", hash);
    //let zipdata = fs::read(path).unwrap();
    //zipdata
}

fn get_latest_maps(client: &reqwest::blocking::Client, page: usize) -> Result<BeatSaverLatestResponse> {
    println!("Getting maps from page {}", page);
    let (res, headers) = do_req(client, &format!("https://beatsaver.com/api/maps/latest/{}", page))?;
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

fn get_map(client: &reqwest::blocking::Client, key: i32) -> Result<Option<(BeatSaverMap, Box<serde_json::value::RawValue>)>> {
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
    Ok(time::Duration::from_millis(r) + RATELIMIT_PADDING) // pad for safety
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
    let nrows = diesel::insert_into(schema::tSongData::table)
        .values(&songdata)
        .execute(conn)
        .expect("error updating song data");
    assert_eq!(nrows, 1, "{:?}", songdata)
}

fn upsert_song(conn: &SqliteConnection, key: i32, hash: Option<String>, deleted: bool, bsmeta: Option<Vec<u8>>) {
    let new_song = Song { key, hash, tstamp: Utc::now().timestamp_millis(), deleted, bsmeta };
    let nrows = diesel::replace_into(schema::tSong::table)
        .values(&new_song)
        .execute(conn)
        .expect("error saving song");
    assert_eq!(nrows, 1, "{}", new_song.key)
}

fn insert_song(conn: &SqliteConnection, key: i32, hash: Option<String>, deleted: bool, bsmeta: Option<Vec<u8>>) {
    let new_song = Song { key, hash, tstamp: Utc::now().timestamp_millis(), deleted, bsmeta };
    let nrows = diesel::insert_into(schema::tSong::table)
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
