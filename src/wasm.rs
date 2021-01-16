use anyhow::{Context, Result, anyhow, bail};
use std::cell::Cell;
use std::collections::HashMap;
use std::convert::TryInto;
use std::fs;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::Mutex;
use serde::{Deserialize, Serialize};

use wasmer::{Array, WasmPtr, ValueType, LazyInit, Memory, Store, WasmerEnv, LikeNamespace, Cranelift, JIT, Exports, Export, Exportable, Val, ExternType, Function, ImportType, Resolver, Module, Instance, Value, imports, RuntimeError};
use wasmer_wasi::{WasiFsError, WasiFile, WasiState, WasiFs, VIRTUAL_ROOT_FD, WasiEnv, WasiVersion};
use wasmer_wasi::ALL_RIGHTS;
use wasmer_wasi::types::{
    __wasi_iovec_t,
    __wasi_ciovec_t,
    __wasi_errno_t,
    __wasi_fdstat_t,
    __wasi_prestat_t,
    __wasi_prestat_u,
    __wasi_prestat_u_dir_t,
    __wasi_fd_t,
    __wasi_rights_t,
    __wasi_fdflags_t,
    __wasi_lookupflags_t,
    __wasi_oflags_t,
    __wasi_timestamp_t,
    __wasi_filesize_t,
    __wasi_whence_t,
    __wasi_filedelta_t,

    __WASI_WHENCE_CUR,
    __WASI_WHENCE_END,
    __WASI_WHENCE_SET,

    __WASI_LOOKUP_SYMLINK_FOLLOW,

    __WASI_FILETYPE_DIRECTORY,

    __WASI_PREOPENTYPE_DIR,

    __WASI_ESUCCESS,
    __WASI_EINVAL,
    __WASI_EFAULT,
    __WASI_EBADF,
    __WASI_EIO,
    __WASI_EACCES,
    __WASI_EISDIR,

    __WASI_STDIN_FILENO,
    __WASI_STDOUT_FILENO,
    __WASI_STDERR_FILENO,
};

pub fn test() -> Result<()> {
    println!("doing wasm things");

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
    struct MyFsEnv {
        dirfd: __wasi_fd_t,
        fs: Arc<Mutex<WasiFs>>,
        #[wasmer(export)]
        memory: LazyInit<Memory>,
    }
    impl MyFsEnv {
        fn new(fs: WasiFs, dirfd: __wasi_fd_t) -> Self {
            MyFsEnv { dirfd, fs: Arc::new(Mutex::new(fs)), memory: Default::default() }
        }
    }
    #[derive(Clone, WasmerEnv)]
    struct ScriptEnv {
        script: Vec<u8>,
        script_len: u32,
        #[wasmer(export)]
        memory: LazyInit<Memory>,
    }


    // Steal these from wasi for minimal consistency
    fn write_bytes_inner<T: Write>(
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
    fn write_bytes<T: Write>(
        mut write_loc: T,
        memory: &Memory,
        iovs_arr_cell: &[Cell<__wasi_ciovec_t>],
    ) -> Result<u32, u16> {
        // TODO: limit amount written
        let result = write_bytes_inner(&mut write_loc, memory, iovs_arr_cell);
        write_loc.flush().expect("no write!");
        result
    }
    fn read_bytes<T: Read>(
        mut reader: T,
        memory: &Memory,
        iovs_arr_cell: &[Cell<__wasi_iovec_t>],
    ) -> Result<u32, __wasi_errno_t> {
        let mut bytes_read = 0;
        for iov in iovs_arr_cell {
            let iov_inner = iov.get();
            let bytes = iov_inner.buf.deref(memory, 0, iov_inner.buf_len)?;
            let mut raw_bytes: &mut [u8] =
                unsafe { &mut *(bytes as *const [_] as *mut [_] as *mut [u8]) };
            bytes_read += reader.read(raw_bytes).map_err(|_| __WASI_EIO)? as u32;
        }
        Ok(bytes_read)
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
    const WORKDIR_FD: u32 = 3;

    #[derive(Debug, Serialize, Deserialize)]
    struct ROVirtualFile {
        data: Vec<u8>,
        pos: u64,
    }
    impl ROVirtualFile {
        fn with_cursor<T>(&mut self, f: impl FnOnce(&mut io::Cursor<&[u8]>) -> T) -> T {
            let mut cursor = io::Cursor::new(self.data.as_slice());
            cursor.set_position(self.pos);
            let ret = f(&mut cursor);
            self.pos = cursor.position();
            ret
        }
    }
    #[typetag::serde]
    impl WasiFile for ROVirtualFile {
        fn last_accessed(&self) -> __wasi_timestamp_t { 0 }
        fn last_modified(&self) -> __wasi_timestamp_t { 0 }
        fn created_time(&self) -> __wasi_timestamp_t { 0 }
        fn size(&self) -> __wasi_timestamp_t { self.data.len() as u64 }
        fn set_len(&mut self, new_size: __wasi_filesize_t) -> Result<(), WasiFsError> { Err(WasiFsError::PermissionDenied) }
        fn unlink(&mut self) -> Result<(), WasiFsError> { Err(WasiFsError::PermissionDenied) }
        fn bytes_available(&self) -> Result<usize, WasiFsError> { Ok(self.pos as usize - self.data.len()) }
    }
    impl io::Seek for ROVirtualFile {
        fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> { self.with_cursor(|data| data.seek(pos)) }
    }
    impl Read for ROVirtualFile {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> { self.with_cursor(|data| data.read(buf)) }
    }
    impl Write for ROVirtualFile {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> { Err(io::Error::new(io::ErrorKind::PermissionDenied, "rofile write")) }
        fn flush(&mut self) -> io::Result<()> { Err(io::Error::new(io::ErrorKind::PermissionDenied, "rofile flush")) }
    }

    let mut fs = WasiFs::new(&[], &[]).expect("failed to create fs");
    let dirfd = unsafe {
        fs.open_dir_all(VIRTUAL_ROOT_FD, "work".to_owned(), ALL_RIGHTS, ALL_RIGHTS, 0).expect("failed to create work dir")
    };
    for &(mapfrom, mapto) in &[
        ("bs-parity/scripts/main.js", "bs-parity-main.js"),
        ("../beatmaps/7f0356d54ded74ed2dbf56e7290a29fde002c0af/ExpertPlusStandard.dat", "7f0356d54ded74ed2dbf56e7290a29fde002c0af-ExpertPlusStandard.dat"),
    ] {
        let rofile = ROVirtualFile { data: fs::read(mapfrom).expect("failed to read data"), pos: 0 };
        fs.open_file_at(dirfd, Box::new(rofile), 0, mapto.to_owned(), ALL_RIGHTS, ALL_RIGHTS, 0).expect("failed to create virtual file");
    }
    let fsenv = MyFsEnv::new(fs, dirfd);
    println!("created a virtualfs, /work is at {}", dirfd);

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
        // FS related things
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
            ("wasi_snapshot_preview1", "fd_read"),
            Function::new_native_with_env(&store, fsenv.clone(), move |env: &MyFsEnv, fd: __wasi_fd_t, iovs: WasmPtr<__wasi_iovec_t, Array>, iovs_len: u32, nread: WasmPtr<u32>| -> __wasi_errno_t {
                println!("wasi-ish>> fd_read {}", fd);
                let memory = env.memory_ref().expect("memory not set up");
                let mut fs = env.fs.lock().unwrap();
                let iovs = mytry!(iovs.deref(memory, 0, iovs_len).ok_or(__WASI_EFAULT));
                let nread = mytry!(nread.deref(memory).ok_or(__WASI_EFAULT));
                let count = match fd {
                    __WASI_STDOUT_FILENO => return __WASI_EINVAL,
                    __WASI_STDERR_FILENO => return __WASI_EINVAL,
                    __WASI_STDIN_FILENO => return __WASI_EINVAL,
                    _ => {
                        let fd_entry = mytry!(fs.fd_map.get_mut(&fd).ok_or(__WASI_EBADF));
                        let offset = fd_entry.offset as usize;
                        let inode_idx = fd_entry.inode;
                        let inode = &mut fs.inodes[inode_idx];

                        let bytes_read = match &mut inode.kind {
                            wasmer_wasi::Kind::File { handle, .. } => {
                                if let Some(handle) = handle {
                                    handle.seek(std::io::SeekFrom::Start(offset as u64)).expect("no seek!");
                                    mytry!(read_bytes(handle, memory, iovs))
                                } else {
                                    return __WASI_EINVAL;
                                }
                            }
                            wasmer_wasi::Kind::Dir { .. } | wasmer_wasi::Kind::Root { .. } => return __WASI_EISDIR,
                            wasmer_wasi::Kind::Symlink { .. } => unimplemented!("Symlinks in wasi::fd_read"),
                            wasmer_wasi::Kind::Buffer { buffer } => {
                                mytry!(read_bytes(&buffer[offset..], memory, iovs))
                            }
                        };

                        // reborrow
                        let fd_entry = mytry!(fs.fd_map.get_mut(&fd).ok_or(__WASI_EBADF));
                        fd_entry.offset += bytes_read as u64;

                        bytes_read
                    },
                };
                nread.set(count);
                __WASI_ESUCCESS
            }).to_export(),
        ),
        (
            ("wasi_snapshot_preview1", "fd_fdstat_get"),
            Function::new_native_with_env(&store, fsenv.clone(), move |env: &MyFsEnv, fd: __wasi_fd_t, fdstat: WasmPtr<__wasi_fdstat_t>| -> __wasi_errno_t {
                println!("wasi-ish>> fd_fdstat_get {}", fd);
                let memory = env.memory_ref().expect("memory not set up");
                let fdstat = mytry!(fdstat.deref(memory).ok_or(__WASI_EFAULT));
                fdstat.set(match fd {
                    WORKDIR_FD => {
                        __wasi_fdstat_t {
                            fs_filetype: __WASI_FILETYPE_DIRECTORY,
                            fs_flags: 0,
                            fs_rights_base: ALL_RIGHTS,
                            fs_rights_inheriting: ALL_RIGHTS,
                        }
                    },
                    fd => mytry!(env.fs.lock().unwrap().fdstat(fd)),
                });
                __WASI_ESUCCESS
            }).to_export(),
        ),
        (
            ("wasi_snapshot_preview1", "path_open"),
            Function::new_native_with_env(&store, fsenv.clone(), move |env: &MyFsEnv, dirfd: __wasi_fd_t, dirflags: __wasi_lookupflags_t, path: WasmPtr<u8, Array>, path_len: u32, o_flags: __wasi_oflags_t, fs_rights_base: __wasi_rights_t, fs_rights_inheriting: __wasi_rights_t, fs_flags: __wasi_fdflags_t, fd: WasmPtr<__wasi_fd_t>| -> __wasi_errno_t {
                println!("wasi-ish>> path_open");

                let memory = env.memory_ref().expect("memory not set up");
                let path = mytry!(unsafe { path.get_utf8_str(memory, path_len) }.ok_or(__WASI_EINVAL));
                println!("wasi-ish>> path_open deets {} -> {}", dirfd, path);
                let fd = mytry!(fd.deref(memory).ok_or(__WASI_EFAULT));

                match dirfd {
                    WORKDIR_FD => {
                        let mut fs = env.fs.lock().unwrap();
                        // Note the use of env.dirfd - we map our fixed fd to the fd in the virtual fs
                        let ino = mytry!(fs.get_inode_at_path(env.dirfd, path, dirflags & __WASI_LOOKUP_SYMLINK_FOLLOW != 0));
                        let newfd = mytry!(fs.create_fd(fs_rights_base, fs_rights_inheriting, fs_flags, 0, ino));
                        fd.set(newfd)
                    },
                    _ => return __WASI_EACCES,
                }
                __WASI_ESUCCESS
            }).to_export(),
        ),
        (
            ("wasi_snapshot_preview1", "fd_close"),
            Function::new_native_with_env(&store, fsenv.clone(), move |env: &MyFsEnv, fd: __wasi_fd_t| -> __wasi_errno_t {
                println!("wasi-ish>> fd_close");
                let mut fs = env.fs.lock().unwrap();
                mytry!(fs.close_fd(fd));
                __WASI_ESUCCESS
            }).to_export(),
        ),
        (
            ("wasi_snapshot_preview1", "fd_seek"),
            Function::new_native_with_env(&store, fsenv.clone(), move |env: &MyFsEnv, fd: __wasi_fd_t, offset: __wasi_filedelta_t, whence: __wasi_whence_t, newoffset: WasmPtr<__wasi_filesize_t>| -> __wasi_errno_t {
                println!("wasi-ish>> fd_seek {}", fd);
                let memory = env.memory_ref().expect("memory not set up");
                let newoffset = mytry!(newoffset.deref(memory).ok_or(__WASI_EFAULT));
                let mut fs = env.fs.lock().unwrap();
                let fd_entry = mytry!(fs.fd_map.get_mut(&fd).ok_or(__WASI_EBADF));
                match whence {
                    __WASI_WHENCE_CUR => fd_entry.offset = (fd_entry.offset as i64 + offset) as u64,
                    __WASI_WHENCE_END => {
                        let inode_idx = fd_entry.inode;
                        match fs.inodes[inode_idx].kind {
                            wasmer_wasi::Kind::File { ref mut handle, .. } => {
                                if let Some(handle) = handle {
                                    let end = mytry!(handle.seek(io::SeekFrom::End(0)).ok().ok_or(__WASI_EIO));
                                    // TODO: handle case if fd_entry.offset uses 64 bits of a u64

                                    // reborrow
                                    let fd_entry = mytry!(fs.fd_map.get_mut(&fd).ok_or(__WASI_EBADF));
                                    fd_entry.offset = (end as i64 + offset) as u64;
                                } else {
                                    return __WASI_EINVAL;
                                }
                            }
                            wasmer_wasi::Kind::Symlink { .. } => {
                                unimplemented!("wasi::fd_seek not implemented for symlinks")
                            }
                            wasmer_wasi::Kind::Dir { .. } | wasmer_wasi::Kind::Root { .. } => {
                                // TODO: check this
                                return __WASI_EINVAL;
                            }
                            wasmer_wasi::Kind::Buffer { .. } => {
                                // seeking buffers probably makes sense
                                // TODO: implement this
                                return __WASI_EINVAL;
                            }
                        }
                    }
                    __WASI_WHENCE_SET => fd_entry.offset = offset as u64,
                    _ => return __WASI_EINVAL,
                }
                // reborrow
                let fd_entry = mytry!(fs.fd_map.get_mut(&fd).ok_or(__WASI_EBADF));
                newoffset.set(fd_entry.offset);
                __WASI_ESUCCESS
            }).to_export(),
        ),
        // Preopen handling
        // https://github.com/WebAssembly/wasi-libc/blob/5b148b6131f36770f110c24d61adfb1e17fea06a/libc-bottom-half/sources/preopens.c#L201
        (
            ("wasi_snapshot_preview1", "fd_prestat_dir_name"),
            Function::new_native_with_env(&store, MyEnv::default(), move |env: &MyEnv, fd: __wasi_fd_t, path: WasmPtr<u8, Array>, path_len: u32| -> __wasi_errno_t {
                println!("wasi-ish>> fd_prestat_dir_name {}", fd);
                match fd {
                    WORKDIR_FD => {
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
                    WORKDIR_FD => {
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
