use alloc::sync::Arc;
use core::any::Any;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::fs::dev::DEV_FS;
use crate::fs::vfs::file_system::FileSystem as NewFileSystem;
use crate::fs::vfs::{FilePrivateData, FileType, IndexNode, InodeFlags, InodeMode, Metadata};
use crate::timer::TimeSpec;
use crate::utils::error::SyscallErr;

#[derive(Debug)]
pub struct Urandom {
    allow_insecure_fallback: bool,
}

pub const URANDOM: Urandom = Urandom {
    allow_insecure_fallback: true,
};

pub const RANDOM: Urandom = Urandom {
    allow_insecure_fallback: false,
};

static URANDOM_FALLBACK: AtomicU64 = AtomicU64::new(0x4d41_4e47_4f55_524e);

fn fallback_fill(buf: &mut [u8]) {
    let mut state = URANDOM_FALLBACK.fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed);
    for byte in buf {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *byte = (state >> 32) as u8;
    }
}

impl IndexNode for Urandom {
    fn read_at(
        &self,
        _offset: usize,
        _len: usize,
        buf: &mut [u8],
        _data: spin::MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        if crate::random::fill_bytes(buf).is_err() {
            if !self.allow_insecure_fallback {
                return Err(SyscallErr::EAGAIN);
            }
            if crate::random::fill_insecure_bytes(buf).is_err() {
                fallback_fill(buf);
            }
        }
        Ok(buf.len())
    }

    fn write_at(
        &self,
        _offset: usize,
        _len: usize,
        buf: &[u8],
        _data: spin::MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        // Linux accepts writes as supplemental pool input but does not credit
        // caller-controlled bytes as entropy.
        crate::random::mix_untrusted(buf);
        Ok(buf.len())
    }

    fn metadata(&self) -> Result<Metadata, SyscallErr> {
        Ok(Metadata {
            dev_id: 0,
            inode_id: 0,
            size: 0,
            blk_size: 0,
            blocks: 0,
            atime: TimeSpec::new(),
            mtime: TimeSpec::new(),
            ctime: TimeSpec::new(),
            file_type: FileType::CharDevice,
            mode: InodeMode::S_IFCHR | InodeMode::from_bits_truncate(0o666),
            nlinks: 1,
            uid: 0,
            gid: 0,
            flags: InodeFlags::empty(),
            raw_dev: crate::makedev!(1, 9),
        })
    }

    fn is_stream(&self) -> bool {
        true
    }

    fn fs(&self) -> Arc<dyn NewFileSystem> {
        DEV_FS.clone()
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}
