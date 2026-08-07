//! Linux rseq registration ABI.
//!
//! This implements the registration/unregistration handshake used by glibc
//! and other user-space runtimes.  The rseq area itself belongs to user space;
//! the kernel stores only the per-thread registration metadata and seeds the
//! initial logical CPU IDs.

use crate::mm::{copy_to_user, fault_in_user_range, UserAccess};
use crate::syscall::errno::{EBUSY, EFAULT, EINVAL};
use crate::task::{current_task, current_user_token, RseqRegistration};

const RSEQ_FLAG_UNREGISTER: usize = 1;
const RSEQ_MIN_SIZE: usize = 32;
const RSEQ_ALIGN: usize = 32;

/// Register or unregister the calling thread's rseq area.
///
/// Context-switch abort/update handling is not needed for the registration
/// ABI itself and remains a separate scheduler integration task.  Registration
/// initializes both CPU fields so glibc can use the area immediately.
pub fn sys_rseq(addr: usize, len: u32, flags: usize, sig: u32) -> isize {
    if flags == RSEQ_FLAG_UNREGISTER {
        if addr != 0 || len != 0 || sig != 0 {
            return EINVAL;
        }

        let task = match current_task() {
            Some(task) => task,
            None => return EFAULT,
        };
        let mut inner = task.acquire_inner_lock();
        if inner.rseq.take().is_none() {
            return EINVAL;
        }
        return 0;
    }

    if flags != 0 || addr == 0 || addr % RSEQ_ALIGN != 0 || (len as usize) < RSEQ_MIN_SIZE {
        return EINVAL;
    }

    let end = match addr.checked_add(RSEQ_MIN_SIZE) {
        Some(end) => end,
        None => return EFAULT,
    };
    if end <= addr {
        return EFAULT;
    }

    let task = match current_task() {
        Some(task) => task,
        None => return EFAULT,
    };
    {
        let inner = task.acquire_inner_lock();
        if inner.rseq.is_some() {
            return EBUSY;
        }
    }

    let token = current_user_token();
    if let Err(errno) =
        fault_in_user_range(token, addr as *const u8, RSEQ_MIN_SIZE, UserAccess::Write)
    {
        return errno;
    }

    let cpu_id = crate::smp::cpu_id() as u32;
    if let Err(errno) = copy_to_user(token, &cpu_id as *const u32, addr as *mut u32) {
        return errno;
    }
    if let Err(errno) = copy_to_user(
        token,
        &cpu_id as *const u32,
        (addr + core::mem::size_of::<u32>()) as *mut u32,
    ) {
        return errno;
    }

    let mut inner = task.acquire_inner_lock();
    if inner.rseq.is_some() {
        return EBUSY;
    }
    inner.rseq = Some(RseqRegistration {
        addr,
        len: len as usize,
        sig,
    });
    0
}
