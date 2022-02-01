// Copyright (c) 2017-present, PingCAP, Inc. Licensed under Apache-2.0.

use std::io::{Result as IoResult, Write};
use std::os::unix::io::RawFd;
use std::sync::Arc;

use crate::common::file_system::WritableFile;
use nix::errno::Errno;
use nix::fcntl::{self, OFlag};
use nix::sys::stat::Mode;
use nix::sys::uio::{pread, pwrite};
use nix::unistd::{close, ftruncate, lseek, Whence};
use nix::NixPath;

use crate::common::{Error, Result};

const FILE_ALLOCATE_SIZE: usize = 2 * 1024 * 1024;
const MIN_ALLOCATE_SIZE: usize = 4 * 1024;

/// A `LogFd` is a RAII file that provides basic I/O functionality.
///
/// This implementation is a thin wrapper around `RawFd`, and primarily targets
/// UNIX-based systems.
pub struct RawFile(RawFd);

pub fn from_nix_error(e: nix::Error, custom: &'static str) -> std::io::Error {
    let kind = std::io::Error::from(e).kind();
    std::io::Error::new(kind, custom)
}

impl RawFile {
    pub fn open<P: ?Sized + NixPath>(path: &P) -> IoResult<Self> {
        let flags = OFlag::O_RDWR;
        // Permission 644
        let mode = Mode::S_IRUSR | Mode::S_IWUSR | Mode::S_IRGRP | Mode::S_IROTH;
        Ok(RawFile(
            fcntl::open(path, flags, mode).map_err(|e| from_nix_error(e, "open"))?,
        ))
    }

    pub fn create<P: ?Sized + NixPath>(path: &P) -> IoResult<Self> {
        // fail_point!("log_fd::create::err", |_| {
        //     Err(from_nix_error(nix::Error::EINVAL, "fp"))
        // });
        let flags = OFlag::O_RDWR | OFlag::O_CREAT;
        // Permission 644
        let mode = Mode::S_IRUSR | Mode::S_IWUSR | Mode::S_IRGRP | Mode::S_IROTH;
        let fd = fcntl::open(path, flags, mode).map_err(|e| from_nix_error(e, "open"))?;
        Ok(RawFile(fd))
    }

    pub fn close(&self) -> IoResult<()> {
        // fail_point!("log_fd::close::err", |_| {
        //     Err(from_nix_error(nix::Error::EINVAL, "fp"))
        // });
        close(self.0).map_err(|e| from_nix_error(e, "close"))
    }

    pub fn sync(&self) -> IoResult<()> {
        // fail_point!("log_fd::sync::err", |_| {
        //     Err(from_nix_error(nix::Error::EINVAL, "fp"))
        // });
        #[cfg(target_os = "linux")]
        {
            nix::unistd::fdatasync(self.0).map_err(|e| from_nix_error(e, "fdatasync"))
        }
        #[cfg(not(target_os = "linux"))]
        {
            nix::unistd::fsync(self.0).map_err(|e| from_nix_error(e, "fsync"))
        }
    }

    pub fn read(&self, mut offset: usize, buf: &mut [u8]) -> IoResult<usize> {
        let mut readed = 0;
        while readed < buf.len() {
            // fail_point!("log_fd::read::err", |_| {
            //     Err(from_nix_error(nix::Error::EINVAL, "fp"))
            // });
            let bytes = match pread(self.0, &mut buf[readed..], offset as i64) {
                Ok(bytes) => bytes,
                Err(e) if e == Errno::EAGAIN => continue,
                Err(e) => return Err(from_nix_error(e, "pread")),
            };
            // EOF
            if bytes == 0 {
                break;
            }
            readed += bytes;
            offset += bytes;
        }
        Ok(readed)
    }

    pub fn write(&self, mut offset: usize, content: &[u8]) -> IoResult<usize> {
        // fail_point!("log_fd::write::zero", |_| { Ok(0) });
        let mut written = 0;
        while written < content.len() {
            let bytes = match pwrite(self.0, &content[written..], offset as i64) {
                Ok(bytes) => bytes,
                Err(e) if e == Errno::EAGAIN => continue,
                Err(e) => return Err(from_nix_error(e, "pwrite")),
            };
            if bytes == 0 {
                break;
            }
            written += bytes;
            offset += bytes;
        }
        // fail_point!("log_fd::write::err", |_| {
        //     Err(from_nix_error(nix::Error::EINVAL, "fp"))
        // });
        Ok(written)
    }

    pub fn file_size(&self) -> IoResult<usize> {
        // fail_point!("log_fd::file_size::err", |_| {
        //     Err(from_nix_error(nix::Error::EINVAL, "fp"))
        // });
        lseek(self.0, 0, Whence::SeekEnd)
            .map(|n| n as usize)
            .map_err(|e| from_nix_error(e, "lseek"))
    }

    pub fn truncate(&self, offset: usize) -> IoResult<()> {
        // fail_point!("log_fd::truncate::err", |_| {
        //     Err(from_nix_error(nix::Error::EINVAL, "fp"))
        // });
        ftruncate(self.0, offset as i64).map_err(|e| from_nix_error(e, "ftruncate"))
    }

    #[allow(unused_variables)]
    pub fn allocate(&self, offset: usize, size: usize) -> IoResult<()> {
        // fail_point!("log_fd::allocate::err", |_| {
        //     Err(from_nix_error(nix::Error::EINVAL, "fp"))
        // });
        #[cfg(target_os = "linux")]
        {
            fcntl::fallocate(
                self.0,
                fcntl::FallocateFlags::empty(),
                offset as i64,
                size as i64,
            )
            .map_err(|e| from_nix_error(e, "fallocate"))
        }
        #[cfg(not(target_os = "linux"))]
        {
            Ok(())
        }
    }
}

impl Drop for RawFile {
    fn drop(&mut self) {
        if let Err(_e) = self.close() {
            // error!("error while closing file: {}", e);
        }
    }
}

/// A `WritableFile` is a `RawFile` wrapper that implements `Seek`, `Write` and `Read`.
pub struct PosixWritableFile {
    inner: Arc<RawFile>,
    offset: usize,
    capacity: usize,
}

impl PosixWritableFile {
    pub fn open<P: ?Sized + NixPath>(path: &P) -> IoResult<Self> {
        let fd = RawFile::open(path)?;
        let file_size = fd.file_size()?;
        Ok(Self::new(Arc::new(fd), file_size))
    }

    pub fn create<P: ?Sized + NixPath>(path: &P) -> IoResult<Self> {
        let fd = RawFile::create(path)?;
        let file_size = fd.file_size()?;
        Ok(Self::new(Arc::new(fd), file_size))
    }

    pub fn new(fd: Arc<RawFile>, capacity: usize) -> Self {
        Self {
            inner: fd,
            offset: 0,
            capacity,
        }
    }
}

impl WritableFile for PosixWritableFile {
    fn append(&mut self, data: &[u8]) -> Result<()> {
        self.write_all(data).map_err(|e| Error::Io(Box::new(e)))
    }

    fn truncate(&mut self, offset: u64) -> Result<()> {
        self.inner
            .truncate(offset as usize)
            .map_err(|e| Error::Io(Box::new(e)))
    }

    fn allocate(&mut self, offset: u64, len: u64) -> Result<()> {
        let new_written = offset + len;
        if new_written > self.capacity as u64 {
            let mut real_alloc = MIN_ALLOCATE_SIZE;
            let alloc = new_written as usize - self.capacity;
            while real_alloc < alloc {
                real_alloc *= 2;
            }
            self.inner.allocate(self.capacity, real_alloc)?;
        }
        Ok(())
    }

    fn sync(&self) -> Result<()> {
        self.inner.sync().map_err(|e| Error::Io(Box::new(e)))
    }

    fn fsync(&self) -> Result<()> {
        self.inner.sync().map_err(|e| Error::Io(Box::new(e)))
    }
}

impl Write for PosixWritableFile {
    fn write(&mut self, buf: &[u8]) -> IoResult<usize> {
        let new_written = self.offset + buf.len();
        if new_written > self.capacity {
            let alloc = std::cmp::max(new_written - self.capacity, FILE_ALLOCATE_SIZE);
            let mut real_alloc = FILE_ALLOCATE_SIZE;
            while real_alloc < alloc {
                real_alloc *= 2;
            }
            self.inner.allocate(self.capacity, real_alloc)?;
            self.capacity += real_alloc;
        }
        let len = self.inner.write(self.offset, buf)?;
        self.offset += len;
        Ok(len)
    }

    fn flush(&mut self) -> IoResult<()> {
        Ok(())
    }
}