//! Linux rseq registration ABI.
//!
//! Returning success is unsafe until context switch, migration, signal and
//! abort handling all maintain the userspace rseq area.  In particular, merely
//! seeding `cpu_id` at registration lets a migrated SMP task execute a critical
//! section with a stale CPU number.  Fail closed until that complete contract
//! is implemented.

use crate::syscall::errno::ENOSYS;

/// Reject rseq registration until the complete SMP execution contract exists.
pub fn sys_rseq(_addr: usize, _len: u32, _flags: usize, _sig: u32) -> isize {
    ENOSYS
}
