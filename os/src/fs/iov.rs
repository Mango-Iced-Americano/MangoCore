// os/src/fs/iov.rs

/// Linux struct iovec (scatter/gather array element)
#[repr(C)]
#[derive(Clone, Copy)]
pub struct IOVec {
    pub iov_base: *const u8,
    pub iov_len: usize,
}
