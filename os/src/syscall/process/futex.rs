use crate::mm::{UserPtr, VirtAddr};
use crate::syscall::errno::*;
use crate::task::threads::{
    do_futex_wait, do_futex_wait_bitset, do_futex_wait_bitset_shared, do_futex_wait_shared,
    do_futex_waitv, do_futex_waitv_shared, futex_requeue_shared, futex_wake_shared, FutexCmd,
    FutexWaitSpec,
};
use crate::task::{current_task, current_user_token, threads, TaskControlBlock};
use crate::timer::{current_timespec, TimeSpec, NSEC_PER_SEC};
use alloc::vec::Vec;
use core::mem::size_of;
use num_enum::FromPrimitive;

bitflags! {
    pub struct FutexOption: u32 {
        const PRIVATE = 128;
        const CLOCK_REALTIME = 256;
    }
}

const FUTEX_WAITV_MAX: usize = 128;
const FUTEX2_SIZE_MASK: u32 = 0x03;
const FUTEX_32: u32 = 0x02;
const FUTEX_WAITV_SUPPORTED_FLAGS: u32 = FUTEX_32 | FutexOption::PRIVATE.bits();
const CLOCK_REALTIME: usize = 0;
const CLOCK_MONOTONIC: usize = 1;

#[derive(Clone, Copy)]
#[repr(C)]
pub struct FutexWaitV {
    val: u64,
    uaddr: u64,
    flags: u32,
    __reserved: u32,
}

fn va_to_phys_key(
    vm: &crate::mm::AddressSpaceInner<crate::mm::KernelPageTableImpl>,
    va: usize,
) -> Option<usize> {
    let va = VirtAddr::from(va);
    let vpn = va.floor();
    let offset = va.page_offset();
    vm.translate(vpn).map(|ppn| (ppn.0 << 12) + offset)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FutexKey {
    Private(usize),
    Shared(usize),
}

fn futex_key_for(
    task: &TaskControlBlock,
    uaddr: usize,
    is_private: bool,
) -> Result<FutexKey, isize> {
    if is_private {
        return Ok(FutexKey::Private(uaddr));
    }

    let vm_ref = task.process.vm();
    vm_ref.read(|vm| {
        if vm.futex_uses_shared_key(VirtAddr::from(uaddr))? {
            va_to_phys_key(vm, uaddr)
                .map(FutexKey::Shared)
                .ok_or(EFAULT)
        } else {
            Ok(FutexKey::Private(uaddr))
        }
    })
}

fn current_futex_key(uaddr: usize, is_private: bool) -> Result<FutexKey, isize> {
    let task = current_task().unwrap();
    futex_key_for(&task, uaddr, is_private)
}

fn read_timeout(timeout: *const TimeSpec, token: usize) -> Result<Option<TimeSpec>, isize> {
    UserPtr::new(timeout)
        .read_optional(token)
        .and_then(|timeout| {
            if let Some(timeout) = timeout {
                if timeout.tv_sec > isize::MAX as usize || timeout.tv_nsec >= NSEC_PER_SEC {
                    return Err(EINVAL);
                }
            }
            Ok(timeout)
        })
}

fn realtime_deadline(deadline: TimeSpec) -> TimeSpec {
    let now_realtime = current_timespec();
    let duration = if deadline > now_realtime {
        deadline - now_realtime
    } else {
        TimeSpec::new()
    };
    TimeSpec::now() + duration
}

fn futex_bitset_deadline(
    timeout: *const TimeSpec,
    token: usize,
    option: FutexOption,
) -> Result<Option<TimeSpec>, isize> {
    read_timeout(timeout, token).map(|timeout| {
        timeout.map(|deadline| {
            if option.contains(FutexOption::CLOCK_REALTIME) {
                realtime_deadline(deadline)
            } else {
                deadline
            }
        })
    })
}

fn futex_waitv_deadline(
    timeout: *const TimeSpec,
    token: usize,
    clockid: usize,
) -> Result<Option<TimeSpec>, isize> {
    if timeout.is_null() {
        return Ok(None);
    }
    let timeout = match read_timeout(timeout, token)? {
        Some(timeout) => timeout,
        None => return Ok(None),
    };
    match clockid {
        CLOCK_MONOTONIC => Ok(Some(timeout)),
        CLOCK_REALTIME => Ok(Some(realtime_deadline(timeout))),
        _ => Err(EINVAL),
    }
}

/// # 描述
/// fast user-space locking
/// # 参数
/// * `uaddr`: `usize`, the address to the futex word;
/// * `futex_op`: `u32`, the operation to perform on the futex;
/// The remaining arguments (val, timeout, uaddr2, and val3) are re‐
/// quired only for certain of the futex  operations  described
/// below.  Where one of these arguments is not required, it is
/// ignored.
/// * `val`: `u32`, the argument to futex_op
/// * `timeout`: `*const TimeSpec`,
/// * `uaddr2`: `usize`,
/// * `val3`: `u32`,
pub fn sys_futex(
    uaddr: *mut u32,
    futex_op: u32,
    val: u32,
    timeout: *const TimeSpec,
    uaddr2: *mut u32,
    val3: u32,
) -> isize {
    let token = current_user_token();
    // uaddr is always used
    if uaddr.is_null() || uaddr.align_offset(4) != 0 {
        return EINVAL;
    }
    let futex_word = UserPtr::new(uaddr as *const u32);
    let cmd = threads::FutexCmd::from_primitive(futex_op & 0x7fu32);
    let option = FutexOption::from_bits_truncate(futex_op);
    let is_private = option.contains(FutexOption::PRIVATE);
    let private_key = uaddr as usize;
    match cmd {
        FutexCmd::Wait => {
            let timeout = match read_timeout(timeout, token) {
                Ok(timeout) => timeout,
                Err(errno) => return errno,
            };
            if !is_private {
                if let Err(errno) = futex_word.read(token) {
                    return errno;
                }
            }
            match current_futex_key(private_key, is_private) {
                Ok(FutexKey::Shared(phys_key)) => {
                    do_futex_wait_shared(futex_word, token, val, timeout, phys_key)
                }
                Ok(FutexKey::Private(key)) => do_futex_wait(futex_word, token, key, val, timeout),
                Err(errno) => errno,
            }
        }
        FutexCmd::WaitBitset => {
            if val3 == 0 {
                return EINVAL;
            }
            let deadline = match futex_bitset_deadline(timeout, token, option) {
                Ok(deadline) => deadline,
                Err(errno) => return errno,
            };
            if !is_private {
                if let Err(errno) = futex_word.read(token) {
                    return errno;
                }
            }
            match current_futex_key(private_key, is_private) {
                Ok(FutexKey::Shared(phys_key)) => {
                    do_futex_wait_bitset_shared(futex_word, token, val, deadline, phys_key)
                }
                Ok(FutexKey::Private(key)) => {
                    do_futex_wait_bitset(futex_word, token, key, val, deadline)
                }
                Err(errno) => errno,
            }
        }
        FutexCmd::Wake | FutexCmd::WakeBitset => {
            if val > i32::MAX as u32 {
                return EINVAL;
            }
            if cmd == FutexCmd::WakeBitset && val3 == 0 {
                return EINVAL;
            }
            if !is_private {
                if let Err(errno) = futex_word.read(token) {
                    return errno;
                }
            }
            match current_futex_key(private_key, is_private) {
                Ok(FutexKey::Private(key)) => current_task()
                    .unwrap()
                    .process
                    .futex()
                    .lock()
                    .wake(key, val),
                Ok(FutexKey::Shared(phys_key)) => futex_wake_shared(phys_key, val),
                Err(errno) => errno,
            }
        }
        FutexCmd::Requeue | FutexCmd::CmpRequeue => {
            if uaddr2.is_null() || uaddr2.align_offset(4) != 0 {
                return EINVAL;
            }
            match UserPtr::new(uaddr2 as *const u32).read(token) {
                Ok(_) => {}
                Err(errno) => return errno,
            };
            if cmd == FutexCmd::CmpRequeue {
                match futex_word.read(token) {
                    Ok(value) if value == val3 => {}
                    Ok(_) => return EAGAIN,
                    Err(errno) => return errno,
                }
            } else if !is_private {
                if let Err(errno) = futex_word.read(token) {
                    return errno;
                }
            }
            let val2 = timeout as usize;
            if val > i32::MAX as u32 || val2 > i32::MAX as usize {
                return EINVAL;
            }
            let key = match current_futex_key(private_key, is_private) {
                Ok(key) => key,
                Err(errno) => return errno,
            };
            let key2 = match current_futex_key(uaddr2 as usize, is_private) {
                Ok(key) => key,
                Err(errno) => return errno,
            };
            match (key, key2) {
                (FutexKey::Private(key), FutexKey::Private(key2)) => current_task()
                    .unwrap()
                    .process
                    .futex()
                    .lock()
                    .requeue(key, key2, val, val2),
                (FutexKey::Shared(key), FutexKey::Shared(key2)) => {
                    futex_requeue_shared(key, key2, val, val2)
                }
                _ => EINVAL,
            }
        }
        FutexCmd::Invalid => EINVAL,
        _ => EINVAL, // Unsupported command
    }
}

pub fn sys_futex_waitv(
    waiters: *const FutexWaitV,
    nr_futexes: usize,
    flags: u32,
    timeout: *const TimeSpec,
    clockid: usize,
) -> isize {
    if flags != 0 || nr_futexes == 0 || nr_futexes > FUTEX_WAITV_MAX {
        return EINVAL;
    }
    if waiters.is_null() {
        return EFAULT;
    }

    let token = current_user_token();
    let deadline = match futex_waitv_deadline(timeout, token, clockid) {
        Ok(deadline) => deadline,
        Err(errno) => return errno,
    };

    let mut entries = Vec::new();
    if entries.try_reserve(nr_futexes).is_err() {
        return ENOMEM;
    }
    let mut requested_private = None;
    let mut private_table = None;

    for index in 0..nr_futexes {
        let waiter_addr = match (waiters as usize).checked_add(index * size_of::<FutexWaitV>()) {
            Some(addr) => addr,
            None => return EFAULT,
        };
        let waiter = match UserPtr::<FutexWaitV>::from_addr(waiter_addr).read(token) {
            Ok(waiter) => waiter,
            Err(errno) => return errno,
        };

        if waiter.__reserved != 0
            || (waiter.flags & !FUTEX_WAITV_SUPPORTED_FLAGS) != 0
            || (waiter.flags & FUTEX2_SIZE_MASK) != FUTEX_32
            || waiter.val > u32::MAX as u64
        {
            return EINVAL;
        }
        if waiter.uaddr == 0 {
            return EFAULT;
        }
        if (waiter.uaddr as usize) & (core::mem::align_of::<u32>() - 1) != 0 {
            return EINVAL;
        }
        let futex_word = UserPtr::<u32>::from_addr(waiter.uaddr as usize);
        if let Err(errno) = futex_word.read(token) {
            return errno;
        }

        let is_private = (waiter.flags & FutexOption::PRIVATE.bits()) != 0;
        match requested_private {
            Some(private) if private != is_private => return EINVAL,
            None => requested_private = Some(is_private),
            _ => {}
        }

        let (uses_private_table, futex_key) =
            match current_futex_key(waiter.uaddr as usize, is_private) {
                Ok(FutexKey::Private(key)) => (true, key),
                Ok(FutexKey::Shared(key)) => (false, key),
                Err(errno) => return errno,
            };
        match private_table {
            Some(private) if private != uses_private_table => return EINVAL,
            None => private_table = Some(uses_private_table),
            _ => {}
        };

        entries.push(FutexWaitSpec {
            futex_word,
            futex_key,
            val: waiter.val as u32,
        });
    }

    let use_private_table = private_table.unwrap_or(true);

    if use_private_table {
        do_futex_waitv(&entries, token, deadline)
    } else {
        do_futex_waitv_shared(&entries, token, deadline)
    }
}
