use crate::fs::procfs::proc_read_bytes;
use crate::utils::error::SyscallErr;

/// Returns all zeros — no special page map support.
/// Supports seeking via offset/len parameters.
pub fn pid_pagemap_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    // Return zeros for any range requested
    let zeros = [0u8; 8192];
    proc_read_bytes(offset, len, buf, &zeros)
}
