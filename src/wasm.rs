use anyhow::{Context, Result};
use log::{trace, debug, info, warn};
use serde::{Serialize, Deserialize};
use std::collections::HashMap;
use std::convert::TryInto;
use std::fs;
use std::io::{self, Seek, Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;

use wasmer::{LazyInit, Memory, Store, WasmerEnv, Cranelift, JIT, Export, Exportable, Val, ExternType, Function, ImportType, Resolver, Module, Instance, RuntimeError};
use wasmer_wasi::types::{
    __WASI_STDIN_FILENO,
    __WASI_STDOUT_FILENO,
    __WASI_STDERR_FILENO,
};

use mywasi::wasi_snapshot_preview1::WasiSnapshotPreview1;
use mywasi::types;
use wiggle::GuestPtr;
use wiggle_borrow::BorrowChecker;

struct FakeResolver {
    exports: Vec<Export>,
}
impl FakeResolver {
    fn new(store: &Store, imports: impl Iterator<Item=ImportType>, overrides: &HashMap<(&str, &str), Export>) -> Self {
        let exports = imports.map(|import| {
            let path = format!("{}:{}", import.module(), import.name());
            if let Some(ex) = overrides.get(&(import.module(), import.name())) {
                debug!("using override for {}", path);
                return ex.clone()
            }
            debug!("shimming import {:?}", import);
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
mod mywasi {
    use super::WasiCtx;

    use log::trace;
    use std::convert::{TryFrom, TryInto};
    pub use wasi_common::Error;

    wiggle::from_witx!({
        witx: ["$WASI_ROOT/phases/snapshot/witx/wasi_snapshot_preview1.witx"],
        ctx: WasiCtx,
        errors: { errno => Error },
    });

    use types::Errno;

    impl wiggle::GuestErrorType for Errno {
        fn success() -> Self {
            Self::Success
        }
    }

    impl types::GuestErrorConversion for WasiCtx {
        fn into_errno(&self, e: wiggle::GuestError) -> Errno {
            trace!("Guest error conversion: {:?}", e);
            e.into()
        }
    }

    impl types::UserErrorConversion for WasiCtx {
        fn errno_from_error(&self, e: Error) -> Result<Errno, wiggle::Trap> {
            trace!("Error conversion: {:?}", e);
            e.try_into()
        }
    }

    impl TryFrom<Error> for Errno {
        type Error = wiggle::Trap;
        fn try_from(e: Error) -> Result<Errno, wiggle::Trap> {
            match e {
                Error::Guest(e) => Ok(e.into()),
                Error::TryFromInt(_) => Ok(Errno::Overflow),
                Error::Utf8(_) => Ok(Errno::Ilseq),
                Error::UnexpectedIo(_) => Ok(Errno::Io),
                Error::GetRandom(_) => Ok(Errno::Io),
                Error::TooBig => Ok(Errno::TooBig),
                Error::Acces => Ok(Errno::Acces),
                Error::Badf => Ok(Errno::Badf),
                Error::Busy => Ok(Errno::Busy),
                Error::Exist => Ok(Errno::Exist),
                Error::Fault => Ok(Errno::Fault),
                Error::Fbig => Ok(Errno::Fbig),
                Error::Ilseq => Ok(Errno::Ilseq),
                Error::Inval => Ok(Errno::Inval),
                Error::Io => Ok(Errno::Io),
                Error::Isdir => Ok(Errno::Isdir),
                Error::Loop => Ok(Errno::Loop),
                Error::Mfile => Ok(Errno::Mfile),
                Error::Mlink => Ok(Errno::Mlink),
                Error::Nametoolong => Ok(Errno::Nametoolong),
                Error::Nfile => Ok(Errno::Nfile),
                Error::Noent => Ok(Errno::Noent),
                Error::Nomem => Ok(Errno::Nomem),
                Error::Nospc => Ok(Errno::Nospc),
                Error::Notdir => Ok(Errno::Notdir),
                Error::Notempty => Ok(Errno::Notempty),
                Error::Notsup => Ok(Errno::Notsup),
                Error::Overflow => Ok(Errno::Overflow),
                Error::Pipe => Ok(Errno::Pipe),
                Error::Perm => Ok(Errno::Perm),
                Error::Spipe => Ok(Errno::Spipe),
                Error::Notcapable => Ok(Errno::Notcapable),
                Error::Unsupported(feature) => {
                    Err(wiggle::Trap::String(format!("unsupported: {}", feature)))
                }
            }
        }
    }

    impl From<wiggle::GuestError> for Errno {
        fn from(err: wiggle::GuestError) -> Self {
            use wiggle::GuestError::*;
            match err {
                InvalidFlagValue { .. } => Self::Inval,
                InvalidEnumValue { .. } => Self::Inval,
                PtrOverflow { .. } => Self::Fault,
                PtrOutOfBounds { .. } => Self::Fault,
                PtrNotAligned { .. } => Self::Inval,
                PtrBorrowed { .. } => Self::Fault,
                InvalidUtf8 { .. } => Self::Ilseq,
                TryFromIntError { .. } => Self::Overflow,
                InFunc { .. } => Self::Inval,
                InDataField { .. } => Self::Inval,
                SliceLengthsDiffer { .. } => Self::Fault,
                BorrowCheckerOutOfHandles { .. } => Self::Fault,
            }
        }
    }
}

type WasiResult<T> = std::result::Result<T, mywasi::Error>;

struct MemoryWrapper<'a>(&'a Memory, &'a BorrowChecker);

unsafe impl wiggle::GuestMemory for MemoryWrapper<'_> {
    fn base(&self) -> (*mut u8, u32) {
        (self.0.data_ptr(), self.0.data_size().try_into().expect("memory too big"))
    }
    fn has_outstanding_borrows(&self) -> bool {
        self.1.has_outstanding_borrows()
    }
    fn is_mut_borrowed(&self, r: wiggle::Region) -> bool {
        self.1.is_mut_borrowed(r)
    }
    fn is_shared_borrowed(&self, r: wiggle::Region) -> bool {
        self.1.is_shared_borrowed(r)
    }
    fn mut_borrow(&self, r: wiggle::Region) -> Result<wiggle::BorrowHandle, wiggle::GuestError> {
        self.1.mut_borrow(r)
    }
    fn shared_borrow(&self, r: wiggle::Region) -> Result<wiggle::BorrowHandle, wiggle::GuestError> {
        self.1.shared_borrow(r)
    }
    fn mut_unborrow(&self, h: wiggle::BorrowHandle) {
        self.1.mut_unborrow(h)
    }
    fn shared_unborrow(&self, h: wiggle::BorrowHandle) {
        self.1.shared_unborrow(h)
    }
}

#[derive(Clone, WasmerEnv)]
pub struct WasiCtx {
    fs: Arc<Mutex<ROFilesystem>>,
    #[wasmer(export)]
    memory: LazyInit<Memory>,
    bc: Arc<Mutex<BorrowChecker>>,
}

impl WasiCtx {
    fn new(fs: ROFilesystem) -> Self {
        Self {
            fs: Arc::new(Mutex::new(fs)),
            memory: Default::default(),
            bc: Arc::new(Mutex::new(BorrowChecker::new())),
        }
    }
    fn fs(&self) -> MutexGuard<ROFilesystem> {
        self.fs.lock().unwrap()
    }
}

#[derive(Copy, Clone, Hash, Eq, PartialEq)]
pub struct Inode(u32);

pub struct FileCursor<'a> {
    pos: &'a mut u64,
    cur: io::Cursor<&'a Vec<u8>>,
}
impl Seek for FileCursor<'_> {
    fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> { self.cur.seek(pos) }
}
impl Read for FileCursor<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> { self.cur.read(buf) }
}
impl Drop for FileCursor<'_> {
    fn drop(&mut self) {
        *self.pos = self.cur.position()
    }
}

pub struct ROFilesystem {
    ino: Inode,
    next_fd: u32,
    next_ino: u32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    fds: HashMap<types::Fd, (Inode, u64)>,
    parent: HashMap<Inode, Inode>,
    data: HashMap<Inode, Vec<u8>>,
    children: HashMap<Inode, HashMap<Vec<u8>, Inode>>,
    preopens: HashMap<types::Fd, Vec<u8>>,
}
impl ROFilesystem {
    fn new() -> Self {
        let ino = Inode(1);
        let mut children = HashMap::new();
        children.insert(ino, Default::default());
        Self {
            ino, next_fd: 3, next_ino: 2,
            stdout: Default::default(), stderr: Default::default(),
            fds: Default::default(),
            parent: Default::default(),
            data: Default::default(),
            children,
            preopens: Default::default(),
        }
    }
    fn new_fd(&mut self, ino: Inode) -> types::Fd {
        let fd = self.next_fd.into();
        self.next_fd += 1;
        assert!(self.fds.insert(fd, (ino, 0)).is_none());
        fd
    }
    fn root(&self) -> Inode { self.ino }

    fn get_file_cursor(&mut self, fd: types::Fd) -> WasiResult<FileCursor> {
        let (ino, pos) = self.fds.get_mut(&fd).ok_or(mywasi::Error::Badf)?;
        let data = self.data.get(&ino).ok_or(mywasi::Error::Isdir)?;
        let mut cur = io::Cursor::new(data);
        cur.set_position(*pos);
        Ok(FileCursor { pos, cur })
    }

    // It checks fds after 2 until it gets EBADF and treats each of them as a preopen
    // https://github.com/WebAssembly/wasi-libc/blob/5b148b6131f36770f110c24d61adfb1e17fea06a/libc-bottom-half/sources/preopens.c#L201
    fn calculate_preopens(&mut self) {
        let children = self.children.get(&self.root()).expect("no root").clone();
        for (name, ino) in children {
            assert!(self.children.contains_key(&ino), "preopen is not a dir");
            let fd = self.new_fd(ino);
            let mut preopen_name = Vec::with_capacity(name.len()+1);
            preopen_name.push(b'/');
            preopen_name.extend(name);
            assert!(self.preopens.insert(fd, preopen_name).is_none())
        }
    }

    fn mknod(&mut self, parent: Inode, name: Vec<u8>) -> Inode {
        let ino = Inode(self.next_ino);
        self.next_ino += 1;
        assert!(self.children.get_mut(&parent).expect("no parent").insert(name, ino).is_none());
        assert!(self.parent.insert(ino, parent).is_none());
        ino
    }
    fn mkdir(&mut self, parent: Inode, name: Vec<u8>) -> Inode {
        let ino = self.mknod(parent, name);
        assert!(self.children.insert(ino, Default::default()).is_none());
        ino
    }
    fn mkfile(&mut self, parent: Inode, name: Vec<u8>, data: Vec<u8>) -> Inode {
        let ino = self.mknod(parent, name);
        assert!(self.data.insert(ino, data).is_none());
        ino
    }
}

impl<'a> WasiSnapshotPreview1 for WasiCtx {
    fn args_get<'b>(
        &self,
        _argv: &GuestPtr<'b, GuestPtr<'b, u8>>,
        _argv_buf: &GuestPtr<'b, u8>,
    ) -> WasiResult<()> {
        Ok(())
    }

    fn args_sizes_get(&self) -> WasiResult<(types::Size, types::Size)> {
        Ok((0, 0))
    }

    fn environ_get<'b>(
        &self,
        _environ: &GuestPtr<'b, GuestPtr<'b, u8>>,
        _environ_buf: &GuestPtr<'b, u8>,
    ) -> WasiResult<()> {
        todo!()
        //self.env.write_to_guest(environ_buf, environ)
    }

    fn environ_sizes_get(&self) -> WasiResult<(types::Size, types::Size)> {
        Ok((0, 0))
    }

    fn clock_res_get(
        &self,
        _id: types::Clockid
    ) -> WasiResult<types::Timestamp> {
        unimplemented!("clock_res_get")
    }

    fn clock_time_get(
        &self,
        _id: types::Clockid,
        _precision: types::Timestamp,
    ) -> WasiResult<types::Timestamp> {
        Ok(0)
    }

    fn fd_advise(
        &self,
        _fd: types::Fd,
        _offset: types::Filesize,
        _len: types::Filesize,
        _advice: types::Advice,
    ) -> WasiResult<()> {
        unimplemented!("fd_advise")
    }

    fn fd_allocate(
        &self,
        _fd: types::Fd,
        _offset: types::Filesize,
        _len: types::Filesize,
    ) -> WasiResult<()> {
        todo!()
        //let required_rights = HandleRights::from_base(types::Rights::FD_ALLOCATE);
        //let entry = self.get_entry(fd)?;
        //entry.as_handle(&required_rights)?.allocate(offset, len)
    }

    fn fd_close(
        &self,
        fd: types::Fd
    ) -> WasiResult<()> {
        let mut fs = self.fs();
        if fs.preopens.contains_key(&fd) {
            return Err(mywasi::Error::Notsup)
        }
        match fs.fds.remove(&fd) {
            Some(_) => Ok(()),
            None => Err(mywasi::Error::Badf)
        }
    }

    fn fd_datasync(
        &self,
        _fd: types::Fd
    ) -> WasiResult<()> {
        unimplemented!("fd_datasync")
    }

    fn fd_fdstat_get(
        &self,
        fd: types::Fd
    ) -> WasiResult<types::Fdstat> {
        let fs = self.fs();
        let fs_filetype = match fd.into() {
            __WASI_STDIN_FILENO |
            __WASI_STDOUT_FILENO |
            __WASI_STDERR_FILENO => types::Filetype::CharacterDevice,
            _ => {
                let (ino, _) = fs.fds.get(&fd).ok_or(mywasi::Error::Badf)?;
                if fs.data.contains_key(ino) {
                    types::Filetype::RegularFile
                } else if fs.children.contains_key(ino) {
                    types::Filetype::Directory
                } else {
                    return Err(mywasi::Error::Noent)
                }
            },
        };
        Ok(types::Fdstat {
            fs_filetype,
            fs_flags: types::Fdflags::empty(),
            fs_rights_base: types::Rights::all(),
            fs_rights_inheriting: types::Rights::all(),
        })
    }

    fn fd_fdstat_set_flags(
        &self,
        _fd: types::Fd,
        _flags: types::Fdflags
    ) -> WasiResult<()> {
        todo!()
        //let required_rights = HandleRights::from_base(types::Rights::FD_FDSTAT_SET_FLAGS);
        //let entry = self.get_entry(fd)?;
        //entry.as_handle(&required_rights)?.fdstat_set_flags(flags)
    }

    fn fd_fdstat_set_rights(
        &self,
        _fd: types::Fd,
        _fs_rights_base: types::Rights,
        _fs_rights_inheriting: types::Rights,
    ) -> WasiResult<()> {
        todo!()
        //let rights = HandleRights::new(fs_rights_base, fs_rights_inheriting);
        //let entry = self.get_entry(fd)?;
        //if !entry.get_rights().contains(&rights) {
        //    return Err(Error::Notcapable);
        //}
        //entry.set_rights(rights);
        //Ok(())
    }

    fn fd_filestat_get(
        &self,
        _fd: types::Fd
    ) -> WasiResult<types::Filestat> {
        todo!()
        //let required_rights = HandleRights::from_base(types::Rights::FD_FILESTAT_GET);
        //let entry = self.get_entry(fd)?;
        //let host_filestat = entry.as_handle(&required_rights)?.filestat_get()?;
        //Ok(host_filestat)
    }

    fn fd_filestat_set_size(
        &self,
        _fd: types::Fd,
        _size: types::Filesize
    ) -> WasiResult<()> {
        todo!()
        //let required_rights = HandleRights::from_base(types::Rights::FD_FILESTAT_SET_SIZE);
        //let entry = self.get_entry(fd)?;
        //entry.as_handle(&required_rights)?.filestat_set_size(size)
    }

    fn fd_filestat_set_times(
        &self,
        _fd: types::Fd,
        _atim: types::Timestamp,
        _mtim: types::Timestamp,
        _fst_flags: types::Fstflags,
    ) -> WasiResult<()> {
        todo!()
        //let required_rights = HandleRights::from_base(types::Rights::FD_FILESTAT_SET_TIMES);
        //let entry = self.get_entry(fd)?;
        //entry
        //    .as_handle(&required_rights)?
        //    .filestat_set_times(atim, mtim, fst_flags)
    }

    fn fd_pread(
        &self,
        _fd: types::Fd,
        _iovs: &types::IovecArray<'_>,
        _offset: types::Filesize,
    ) -> WasiResult<types::Size> {
        todo!()
        //let mut guest_slices: Vec<GuestSliceMut<'_, u8>> = Vec::new();
        //for iov_ptr in iovs.iter() {
        //    let iov_ptr = iov_ptr?;
        //    let iov: types::Iovec = iov_ptr.read()?;
        //    guest_slices.push(iov.buf.as_array(iov.buf_len).as_slice_mut()?);
        //}

        //let required_rights =
        //    HandleRights::from_base(types::Rights::FD_READ | types::Rights::FD_SEEK);
        //let entry = self.get_entry(fd)?;
        //if offset > i64::max_value() as u64 {
        //    return Err(Error::Io);
        //}

        //let host_nread = {
        //    let mut buf = guest_slices
        //        .iter_mut()
        //        .map(|s| io::IoSliceMut::new(&mut *s))
        //        .collect::<Vec<io::IoSliceMut<'_>>>();
        //    entry
        //        .as_handle(&required_rights)?
        //        .preadv(&mut buf, offset)?
        //        .try_into()?
        //};
        //Ok(host_nread)
    }

    fn fd_prestat_get(
        &self,
        fd: types::Fd
    ) -> WasiResult<types::Prestat> {
        let fs = self.fs();
        let name = fs.preopens.get(&fd).ok_or(mywasi::Error::Badf)?;
        Ok(types::Prestat::Dir(types::PrestatDir { pr_name_len: name.len().try_into()? }))
    }

    fn fd_prestat_dir_name(
        &self,
        fd: types::Fd,
        path: &GuestPtr<u8>,
        path_len: types::Size,
    ) -> WasiResult<()> {
        let fs = self.fs();
        let name = fs.preopens.get(&fd).ok_or(mywasi::Error::Badf)?;
        let name_len: u32 = name.len().try_into()?;
        if path_len != name_len {
            return Err(mywasi::Error::Inval)
        }
        path.as_array(name_len).copy_from_slice(&name)?;
        Ok(())
    }

    fn fd_pwrite(
        &self,
        _fd: types::Fd,
        _ciovs: &types::CiovecArray<'_>,
        _offset: types::Filesize,
    ) -> WasiResult<types::Size> {
        todo!()
        //let mut guest_slices = Vec::new();
        //for ciov_ptr in ciovs.iter() {
        //    let ciov_ptr = ciov_ptr?;
        //    let ciov: types::Ciovec = ciov_ptr.read()?;
        //    guest_slices.push(ciov.buf.as_array(ciov.buf_len).as_slice()?);
        //}

        //let required_rights =
        //    HandleRights::from_base(types::Rights::FD_WRITE | types::Rights::FD_SEEK);
        //let entry = self.get_entry(fd)?;

        //if offset > i64::max_value() as u64 {
        //    return Err(Error::Io);
        //}

        //let host_nwritten = {
        //    let buf: Vec<io::IoSlice> =
        //        guest_slices.iter().map(|s| io::IoSlice::new(&*s)).collect();
        //    entry
        //        .as_handle(&required_rights)?
        //        .pwritev(&buf, offset)?
        //        .try_into()?
        //};
        //Ok(host_nwritten)
    }

    fn fd_read(
        &self,
        fd: types::Fd,
        iovs: &types::IovecArray<'_>
    ) -> WasiResult<types::Size> {
        match fd.into() {
            __WASI_STDOUT_FILENO |
            __WASI_STDERR_FILENO => return Err(mywasi::Error::Inval),
            __WASI_STDIN_FILENO => return Ok(0),
            _ => (),
        }

        let mut fs = self.fs();
        let mut cur = fs.get_file_cursor(fd)?;
        let mut guest_slices = vec![];
        for iov_ptr in iovs.iter() {
            let iov_ptr = iov_ptr?;
            let iov: types::Iovec = iov_ptr.read()?;
            guest_slices.push(iov.buf.as_array(iov.buf_len).as_slice_mut()?)
        }
        let mut slices: Vec<_> = guest_slices.iter_mut().map(|s| io::IoSliceMut::new(s)).collect();

        let nwritten = cur.read_vectored(&mut slices).map_err(|_| mywasi::Error::Io)?;
        Ok(nwritten.try_into()?)
    }

    fn fd_readdir(
        &self,
        _fd: types::Fd,
        _buf: &GuestPtr<u8>,
        _buf_len: types::Size,
        _cookie: types::Dircookie,
    ) -> WasiResult<types::Size> {
        todo!()
        //let required_rights = HandleRights::from_base(types::Rights::FD_READDIR);
        //let entry = self.get_entry(fd)?;

        //let mut bufused = 0;
        //let mut buf = buf.clone();
        //for pair in entry.as_handle(&required_rights)?.readdir(cookie)? {
        //    let (dirent, name) = pair?;
        //    let dirent_raw = dirent.as_bytes()?;
        //    let dirent_len: types::Size = dirent_raw.len().try_into()?;
        //    let name_raw = name.as_bytes();
        //    let name_len = name_raw.len().try_into()?;
        //    let offset = dirent_len.checked_add(name_len).ok_or(Error::Overflow)?;

        //    // Copy as many bytes of the dirent as we can, up to the end of the buffer.
        //    let dirent_copy_len = min(dirent_len, buf_len - bufused);
        //    buf.as_array(dirent_copy_len)
        //        .copy_from_slice(&dirent_raw[..dirent_copy_len as usize])?;

        //    // If the dirent struct wasn't copied entirely, return that we
        //    // filled the buffer, which tells libc that we're not at EOF.
        //    if dirent_copy_len < dirent_len {
        //        return Ok(buf_len);
        //    }

        //    buf = buf.add(dirent_copy_len)?;

        //    // Copy as many bytes of the name as we can, up to the end of the buffer.
        //    let name_copy_len = min(name_len, buf_len - bufused);
        //    buf.as_array(name_copy_len)
        //        .copy_from_slice(&name_raw[..name_copy_len as usize])?;

        //    // If the dirent struct wasn't copied entirely, return that we
        //    // filled the buffer, which tells libc that we're not at EOF.
        //    if name_copy_len < name_len {
        //        return Ok(buf_len);
        //    }

        //    buf = buf.add(name_copy_len)?;

        //    bufused += offset;
        //}

        //Ok(bufused)
    }

    fn fd_renumber(
        &self,
        _from: types::Fd,
        _to: types::Fd
    ) -> WasiResult<()> {
        todo!()
        //if !self.contains_entry(from) {
        //    return Err(Error::Badf);
        //}

        //// Don't allow renumbering over a pre-opened resource.
        //// TODO: Eventually, we do want to permit this, once libpreopen in
        //// userspace is capable of removing entries from its tables as well.
        //if let Ok(from_fe) = self.get_entry(from) {
        //    if from_fe.preopen_path.is_some() {
        //        return Err(Error::Notsup);
        //    }
        //}
        //if let Ok(to_fe) = self.get_entry(to) {
        //    if to_fe.preopen_path.is_some() {
        //        return Err(Error::Notsup);
        //    }
        //}
        //let fe = self.remove_entry(from)?;
        //self.insert_entry_at(to, fe);
        //Ok(())
    }

    fn fd_seek(
        &self,
        fd: types::Fd,
        offset: types::Filedelta,
        whence: types::Whence,
    ) -> WasiResult<types::Filesize> {
        let seek = match whence {
            types::Whence::Cur => io::SeekFrom::Current(offset),
            types::Whence::End => io::SeekFrom::End(offset),
            types::Whence::Set => io::SeekFrom::Start(offset as u64),
        };
        let fs = &mut *self.fs();
        let mut cur = fs.get_file_cursor(fd)?;
        let pos = cur.seek(seek)?;
        Ok(pos)
    }

    fn fd_sync(
        &self,
        _fd: types::Fd
    ) -> WasiResult<()> {
        todo!()
        //let required_rights = HandleRights::from_base(types::Rights::FD_SYNC);
        //let entry = self.get_entry(fd)?;
        //entry.as_handle(&required_rights)?.sync()
    }

    fn fd_tell(
        &self,
        _fd: types::Fd
    ) -> WasiResult<types::Filesize> {
        todo!()
        //let required_rights = HandleRights::from_base(types::Rights::FD_TELL);
        //let entry = self.get_entry(fd)?;
        //let host_offset = entry
        //    .as_handle(&required_rights)?
        //    .seek(SeekFrom::Current(0))?;
        //Ok(host_offset)
    }

    fn fd_write(
        &self,
        fd: types::Fd,
        ciovs: &types::CiovecArray<'_>
    ) -> WasiResult<types::Size> {
        let mut guest_slices = vec![];
        for ciov_ptr in ciovs.iter() {
            let ciov_ptr = ciov_ptr?;
            let ciov: types::Ciovec = ciov_ptr.read()?;
            guest_slices.push(ciov.buf.as_array(ciov.buf_len).as_slice()?)
        }
        let slices: Vec<_> = guest_slices.iter().map(|s| io::IoSlice::new(s)).collect();

        let mut fs = self.fs();
        let nwritten = match fd.into() {
            __WASI_STDOUT_FILENO => fs.stdout.write_vectored(&slices),
            __WASI_STDERR_FILENO => fs.stderr.write_vectored(&slices),
            _ => return Err(mywasi::Error::Badf),
        }.map_err(|_| mywasi::Error::Io)?;
        let nwritten = nwritten.try_into()?;
        Ok(nwritten)
    }

    fn path_create_directory(
        &self,
        _dirfd: types::Fd,
        _path: &GuestPtr<'_, str>
    ) -> WasiResult<()> {
        todo!()
        //let required_rights = HandleRights::from_base(
        //    types::Rights::PATH_OPEN | types::Rights::PATH_CREATE_DIRECTORY,
        //);
        //let entry = self.get_entry(dirfd)?;
        //let path = path.as_str()?;
        //let (dirfd, path) = path::get(
        //    &entry,
        //    &required_rights,
        //    types::Lookupflags::empty(),
        //    path.deref(),
        //    false,
        //)?;
        //dirfd.create_directory(&path)
    }

    fn path_filestat_get(
        &self,
        _dirfd: types::Fd,
        _flags: types::Lookupflags,
        _path: &GuestPtr<'_, str>,
    ) -> WasiResult<types::Filestat> {
        todo!()
        //let required_rights = HandleRights::from_base(types::Rights::PATH_FILESTAT_GET);
        //let entry = self.get_entry(dirfd)?;
        //let path = path.as_str()?;
        //let (dirfd, path) = path::get(&entry, &required_rights, flags, path.deref(), false)?;
        //let host_filestat =
        //    dirfd.filestat_get_at(&path, flags.contains(&types::Lookupflags::SYMLINK_FOLLOW))?;
        //Ok(host_filestat)
    }

    fn path_filestat_set_times(
        &self,
        _dirfd: types::Fd,
        _flags: types::Lookupflags,
        _path: &GuestPtr<'_, str>,
        _atim: types::Timestamp,
        _mtim: types::Timestamp,
        _fst_flags: types::Fstflags,
    ) -> WasiResult<()> {
        todo!()
        //let required_rights = HandleRights::from_base(types::Rights::PATH_FILESTAT_SET_TIMES);
        //let entry = self.get_entry(dirfd)?;
        //let path = path.as_str()?;
        //let (dirfd, path) = path::get(&entry, &required_rights, flags, path.deref(), false)?;
        //dirfd.filestat_set_times_at(
        //    &path,
        //    atim,
        //    mtim,
        //    fst_flags,
        //    flags.contains(&types::Lookupflags::SYMLINK_FOLLOW),
        //)?;
        //Ok(())
    }

    fn path_link(
        &self,
        _old_fd: types::Fd,
        _old_flags: types::Lookupflags,
        _old_path: &GuestPtr<'_, str>,
        _new_fd: types::Fd,
        _new_path: &GuestPtr<'_, str>,
    ) -> WasiResult<()> {
        todo!()
        //let required_rights = HandleRights::from_base(types::Rights::PATH_LINK_SOURCE);
        //let old_entry = self.get_entry(old_fd)?;
        //let (old_dirfd, old_path) = {
        //    // Borrow old_path for just this scope
        //    let old_path = old_path.as_str()?;
        //    path::get(
        //        &old_entry,
        //        &required_rights,
        //        types::Lookupflags::empty(),
        //        old_path.deref(),
        //        false,
        //    )?
        //};
        //let required_rights = HandleRights::from_base(types::Rights::PATH_LINK_TARGET);
        //let new_entry = self.get_entry(new_fd)?;
        //let (new_dirfd, new_path) = {
        //    // Borrow new_path for just this scope
        //    let new_path = new_path.as_str()?;
        //    path::get(
        //        &new_entry,
        //        &required_rights,
        //        types::Lookupflags::empty(),
        //        new_path.deref(),
        //        false,
        //    )?
        //};
        //old_dirfd.link(
        //    &old_path,
        //    new_dirfd,
        //    &new_path,
        //    old_flags.contains(&types::Lookupflags::SYMLINK_FOLLOW),
        //)
    }

    fn path_open(
        &self,
        dirfd: types::Fd,
        dirflags: types::Lookupflags,
        path: &GuestPtr<'_, str>,
        oflags: types::Oflags,
        _fs_rights_base: types::Rights,
        _fs_rights_inheriting: types::Rights,
        fdflags: types::Fdflags,
    ) -> WasiResult<types::Fd> {
        if dirflags != types::Lookupflags::SYMLINK_FOLLOW {
            unimplemented!()
        }
        if oflags != types::Oflags::empty() {
            unimplemented!()
        }
        if fdflags != types::Fdflags::empty() {
            unimplemented!()
        }
        let path_slice = path.as_bytes().as_slice()?;
        let mut fs = self.fs();
        let (dir_ino, _) = fs.fds.get(&dirfd).unwrap();
        let path_ino = *fs.children.get(dir_ino).unwrap().get(&*path_slice).ok_or(mywasi::Error::Noent)?;
        Ok(fs.new_fd(path_ino))
    }

    fn path_readlink(
        &self,
        _dirfd: types::Fd,
        _path: &GuestPtr<'_, str>,
        _buf: &GuestPtr<u8>,
        _buf_len: types::Size,
    ) -> WasiResult<types::Size> {
        todo!()
        //let required_rights = HandleRights::from_base(types::Rights::PATH_READLINK);
        //let entry = self.get_entry(dirfd)?;
        //let (dirfd, path) = {
        //    // borrow path for just this scope
        //    let path = path.as_str()?;
        //    path::get(
        //        &entry,
        //        &required_rights,
        //        types::Lookupflags::empty(),
        //        path.deref(),
        //        false,
        //    )?
        //};
        //let mut slice = buf.as_array(buf_len).as_slice_mut()?;
        //let host_bufused = dirfd.readlink(&path, &mut *slice)?.try_into()?;
        //Ok(host_bufused)
    }

    fn path_remove_directory(
        &self,
        _dirfd: types::Fd,
        _path: &GuestPtr<'_, str>
    ) -> WasiResult<()> {
        unimplemented!("path_remove_directory")
    }

    fn path_rename(
        &self,
        _old_fd: types::Fd,
        _old_path: &GuestPtr<'_, str>,
        _new_fd: types::Fd,
        _new_path: &GuestPtr<'_, str>,
    ) -> WasiResult<()> {
        unimplemented!("path_rename")
    }

    fn path_symlink(
        &self,
        _old_path: &GuestPtr<'_, str>,
        _dirfd: types::Fd,
        _new_path: &GuestPtr<'_, str>,
    ) -> WasiResult<()> {
        unimplemented!("path_symlink")
    }

    fn path_unlink_file(
        &self,
        _dirfd: types::Fd,
        _path: &GuestPtr<'_, str>
    ) -> WasiResult<()> {
        unimplemented!("path_unlink_file")
    }

    fn poll_oneoff(
        &self,
        _in_: &GuestPtr<types::Subscription>,
        _out: &GuestPtr<types::Event>,
        _nsubscriptions: types::Size,
    ) -> WasiResult<types::Size> {
        unimplemented!("poll_oneoff")
    }

    fn proc_exit(
        &self,
        _rval: types::Exitcode
    ) -> wiggle::Trap {
        // proc_exit is special in that it's expected to unwind the stack, which
        // typically requires runtime-specific logic.
        unimplemented!("runtimes are expected to override this implementation")
    }

    fn proc_raise(
        &self,
        _sig: types::Signal
    ) -> WasiResult<()> {
        unimplemented!("proc_raise")
    }

    fn sched_yield(&self) -> WasiResult<()> {
        std::thread::yield_now();
        Ok(())
    }

    fn random_get(
        &self,
        _buf: &GuestPtr<u8>,
        _buf_len: types::Size
    ) -> WasiResult<()> {
        unimplemented!("random_get")
    }

    fn sock_recv(
        &self,
        _fd: types::Fd,
        _ri_data: &types::IovecArray<'_>,
        _ri_flags: types::Riflags,
    ) -> WasiResult<(types::Size, types::Roflags)> {
        unimplemented!("sock_recv")
    }

    fn sock_send(
        &self,
        _fd: types::Fd,
        _si_data: &types::CiovecArray<'_>,
        _si_flags: types::Siflags,
    ) -> WasiResult<types::Size> {
        unimplemented!("sock_send")
    }

    fn sock_shutdown(
        &self,
        _fd: types::Fd,
        _how: types::Sdflags
    ) -> WasiResult<()> {
        unimplemented!("sock_shutdown")
    }
}

fn load_module(path: impl AsRef<Path>) -> Result<wasmer::Module> {
    let module_bytes = fs::read(path)?;

    // TODO: wasmer/examples/tunables_limit_memory.rs
    let mut cranelift = Cranelift::new();
    cranelift.enable_simd(false);
    let engine = JIT::new(cranelift).engine();
    let store = Store::new(&engine);
    let module = Module::from_binary(&store, &module_bytes)?;
    debug!("{:?}", module);
    debug!("imports:");
    for import in module.imports() {
        debug!("{:?}", import)
    }
    debug!("exports:");
    for export in module.exports() {
        debug!("{:?}", export)
    }

    Ok(module)
}

fn run_plugin(module: Module, mut plugin: tar::Archive<impl Read>, map_dat: Vec<u8>) -> Result<HashMap<String, AnalysisValue>> {
    let store = module.store();

    let mut rofs = ROFilesystem::new();
    let work_ino = rofs.mkdir(rofs.root(), b"work".to_vec());
    let data_ino = rofs.mkdir(rofs.root(), b"data".to_vec());

    for entry in plugin.entries().context("couldn't read entries from tar")? {
        let mut entry = entry.context("reading entry failed")?;
        let path = entry.path_bytes().into_owned();
        let mut data = vec![];
        entry.read_to_end(&mut data).context("failed to extract data from tar")?;
        rofs.mkfile(work_ino, path, data);
    }

    rofs.mkfile(data_ino, b"map.dat".to_vec(), map_dat);

    rofs.calculate_preopens();
    debug!("created a virtualfs with preopens: {:?}", rofs.preopens);

    let wasi_ctx = WasiCtx::new(rofs);

    macro_rules! gen {
        ($name:ident, $( $arg:ident ),*) => {
            (
                ("wasi_snapshot_preview1", stringify!($name)),
                Function::new_native_with_env(&store, wasi_ctx.clone(), move |env: &WasiCtx, $( $arg ),*| {
                    trace!("wasicall >> {} {:?}", stringify!($name), ($( $arg ),*));
                    let bc = env.bc.lock().unwrap();
                    let memory = MemoryWrapper(env.memory_ref().expect("memory not set up"), &bc);
                    mywasi::wasi_snapshot_preview1::$name(env, &memory, $( $arg ),*).expect("wasi call trapped")
                }).to_export(),
            )
        };
    }

    let overrides = vec![
        gen!(args_get, argv, argv_buf),
        gen!(args_sizes_get, argc, argv_buf_size),
        gen!(clock_time_get, clock_id, precision, time),
        gen!(environ_sizes_get, environ_count, environ_buf_size),

        gen!(path_open, dirfd, dirflags, path_ptr, path_len, o_flags, fs_rights_base, fs_rights_inheriting, fs_flags, fd),
        gen!(fd_prestat_get, fd, buf_ptr),
        gen!(fd_prestat_dir_name, fd, path, path_len),
        gen!(fd_fdstat_get, fd, stat_ptr),
        gen!(fd_write, fd, iovs_ptr, iovs_len, nwritten_ptr),
        gen!(fd_seek, fd, offset, whence, newoffset_ptr),
        gen!(fd_read, fd, iovs_ptr, iovs_len, nread_ptr),
        gen!(fd_close, fd),
    ].into_iter().collect();

    let importobj = FakeResolver::new(&store, module.imports(), &overrides);

    let instance = Instance::new(&module, &importobj)?;

    debug!("running: _start");
    let f = instance.exports.get_function("_start")?;
    let ret = f.call(&[]);
    let fs = wasi_ctx.fs();
    debug!("stdout:{{{}}}", String::from_utf8_lossy(&fs.stdout));
    debug!("stderr:{{{}}}", String::from_utf8_lossy(&fs.stderr));
    match ret {
        Ok(vals) => {
            debug!("success: {:?}", vals);
            Ok(serde_json::from_slice(&fs.stdout).context("couldn't parse script output")?)
        },
        Err(re) => {
            warn!("_start runtime fail: {}", re);
            debug!("trace: {:#?}", re.trace());
            Err(re).context("failed to run analysis")
        }
    }
}

pub struct AnalysisPlugin {
    module: Module,
    tar_data: Vec<u8>,
    name: String,
}

#[derive(Debug)]
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub enum AnalysisValue {
    String(String),
    Number(serde_json::Number),
}

impl AnalysisPlugin {
    pub fn run(&self, map_dat: Vec<u8>) -> Result<HashMap<String, AnalysisValue>> {
        let ar = tar::Archive::new(&*self.tar_data);
        run_plugin(self.module.clone(), ar, map_dat)
    }
    pub fn name(&self) -> &str {
        &self.name
    }
}

pub fn load_plugin(plugin_name: &str, interp: &str) -> Result<AnalysisPlugin> {
    let interp_path = format!("plugins/dist/{}.wasm", interp);
    let module = load_module(interp_path).with_context(|| format!("failed to load interp module {}", interp))?;
    let plugin_path = format!("plugins/dist/{}.tar", plugin_name);
    let tar_data = fs::read(&plugin_path).with_context(|| format!("failed to read {}", plugin_path))?;
    Ok(AnalysisPlugin {
        module,
        tar_data,
        name: plugin_name.to_owned(),
    })
}

pub fn test() -> Result<()> {
    let plugin = load_plugin("parity", "js")?;

    for map_dat_path in &[
        "../beatmaps/7f0356d54ded74ed2dbf56e7290a29fde002c0af/ExpertPlusStandard.dat",
        "../beatmaps/9a1d001995cc0a2014352aa7148cbcbf2e489d89/Hard.dat",
        "../beatmaps/28c746c1bbdaa7f10e894b5054c2e80a647ef1f6/ExpertPlusStandard.dat",
    ] {
        info!("considering {}", map_dat_path);
        let ret = plugin.run(fs::read(map_dat_path)?)?;
        info!("output: {:?} {}", ret, serde_json::to_string(&ret).unwrap())
    }
    Ok(())
}
