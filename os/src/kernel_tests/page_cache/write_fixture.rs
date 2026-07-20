use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use crate::fs::PageCacheBackend;
use crate::utils::error::SyscallErr;

pub(super) struct BeforeCopyBackend {
    writes: AtomicUsize,
    written_byte: AtomicU8,
}

impl BeforeCopyBackend {
    pub(super) fn new() -> Self {
        Self {
            writes: AtomicUsize::new(0),
            written_byte: AtomicU8::new(0),
        }
    }

    pub(super) fn writes(&self) -> usize {
        self.writes.load(Ordering::SeqCst)
    }

    pub(super) fn written_byte(&self) -> u8 {
        self.written_byte.load(Ordering::SeqCst)
    }
}

impl PageCacheBackend for BeforeCopyBackend {
    fn read_page(&self, _index: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        buf.fill(0);
        Ok(buf.len())
    }

    fn write_page(&self, _index: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
        let first_byte = *buf.first().ok_or(SyscallErr::EIO)?;
        self.written_byte.store(first_byte, Ordering::SeqCst);
        self.writes.fetch_add(1, Ordering::SeqCst);
        Ok(buf.len())
    }

    fn npages(&self) -> usize {
        2
    }
}
