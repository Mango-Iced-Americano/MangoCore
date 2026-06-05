use crate::mm::{UserPtr, VirtAddr};
use crate::syscall::errno::*;
use crate::task::threads::{
    do_futex_wait, do_futex_wait_bitset, do_futex_wait_bitset_shared, do_futex_wait_shared,
    do_futex_waitv, do_futex_waitv_shared, futex_requeue_shared, futex_wake_shared, FutexCmd,
    FutexWaitEntry,
};
use crate::task::{current_task, threads};
use crate::timer::{current_timespec, TimeSpec, NSEC_PER_SEC};
use alloc::vec::Vec;
use core::mem::size_of;
use log::{info, trace};
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
    vm: &crate::mm::AddressSpace<crate::mm::KernelPageTableImpl>,
    va: usize,
) -> Option<usize> {
    let va = VirtAddr::from(va);
    let vpn = va.floor();
    let offset = va.page_offset();
    vm.translate(vpn).map(|ppn| (ppn.0 << 12) + offset)
}

fn read_timeout(timeout: *const TimeSpec, token: usize) -> Result<Option<TimeSpec>, isize> {
    UserPtr::new(timeout).read_optional(token).and_then(|timeout| {
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
    let task = current_task().unwrap();
    let token = task.get_user_token();
    // uaddr is always used
    if uaddr.is_null() || uaddr.align_offset(4) != 0 {
        return EINVAL;
    }
    let futex_word = UserPtr::new(uaddr as *const u32);
    let futex_value = match futex_word.read(token) {
        Ok(value) => value,
        Err(errno) => return errno,
    };
    let cmd = threads::FutexCmd::from_primitive(futex_op & 0x7fu32);
    let option = FutexOption::from_bits_truncate(futex_op);
    let is_private = option.contains(FutexOption::PRIVATE);
    let private_key = uaddr as usize;
    if !is_private {
        trace!("[futex] process-shared futex, cmd={:?}", cmd);
    }
    info!(
        "[futex] uaddr: {:?}, futex_op: {:?}, option: {:?}, val: {:X}, timeout: {:?}, uaddr2: {:?}, val3: {:X}",
        uaddr, cmd, option, val, timeout, uaddr2, val3
    );

    match cmd {
        FutexCmd::Wait => {
            let timeout = match read_timeout(timeout, token) {
                Ok(timeout) => timeout,
                Err(errno) => return errno,
            };
            if !is_private {
                let vm_ref = task.process.vm();
                let vm = vm_ref.lock();
                let phys_key = match va_to_phys_key(&vm, uaddr as usize) {
                    Some(k) => k,
                    None => return EFAULT,
                };
                drop(vm);
                drop(task);
                do_futex_wait_shared(futex_word, token, val, timeout, phys_key)
            } else {
                drop(task);
                do_futex_wait(futex_word, token, private_key, val, timeout)
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
                let vm_ref = task.process.vm();
                let vm = vm_ref.lock();
                let phys_key = match va_to_phys_key(&vm, uaddr as usize) {
                    Some(k) => k,
                    None => return EFAULT,
                };
                drop(vm);
                drop(task);
                do_futex_wait_bitset_shared(futex_word, token, val, deadline, phys_key)
            } else {
                drop(task);
                do_futex_wait_bitset(futex_word, token, private_key, val, deadline)
            }
        }
        FutexCmd::Wake | FutexCmd::WakeBitset => {
            if val > i32::MAX as u32 {
                return EINVAL;
            }
            if cmd == FutexCmd::WakeBitset && val3 == 0 {
                return EINVAL;
            }
            if is_private {
                task.process.futex().lock().wake(private_key, val)
            } else {
                let vm_ref = task.process.vm();
                let vm = vm_ref.lock();
                let phys_key = match va_to_phys_key(&vm, uaddr as usize) {
                    Some(k) => k,
                    None => return EFAULT,
                };
                drop(vm);
                futex_wake_shared(phys_key, val)
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
            if cmd == FutexCmd::CmpRequeue && futex_value != val3 {
                return EAGAIN;
            }
            let val2 = timeout as usize;
            if val > i32::MAX as u32 || val2 > i32::MAX as usize {
                return EINVAL;
            }
            if is_private {
                task.process
                    .futex()
                    .lock()
                    .requeue(private_key, uaddr2 as usize, val, val2)
            } else {
                let phys_key = {
                    let vm_ref = task.process.vm();
                    let vm = vm_ref.lock();
                    match va_to_phys_key(&vm, uaddr as usize) {
                        Some(k) => k,
                        None => return EFAULT,
                    }
                };
                let phys_key2 = {
                    let vm_ref = task.process.vm();
                    let vm = vm_ref.lock();
                    match va_to_phys_key(&vm, uaddr2 as usize) {
                        Some(k) => k,
                        None => return EFAULT,
                    }
                };
                futex_requeue_shared(phys_key, phys_key2, val, val2)
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

    let task = current_task().unwrap();
    let token = task.get_user_token();
    let deadline = match futex_waitv_deadline(timeout, token, clockid) {
        Ok(deadline) => deadline,
        Err(errno) => return errno,
    };

    let mut entries = Vec::new();
    if entries.try_reserve(nr_futexes).is_err() {
        return ENOMEM;
    }
    let mut all_private = None;

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
        match all_private {
            Some(private) if private != is_private => return EINVAL,
            None => all_private = Some(is_private),
            _ => {}
        }

        let futex_key = if is_private {
            waiter.uaddr as usize
        } else {
            let vm_ref = task.process.vm();
            let vm = vm_ref.lock();
            match va_to_phys_key(&vm, waiter.uaddr as usize) {
                Some(key) => key,
                None => return EFAULT,
            }
        };

        entries.push(FutexWaitEntry {
            futex_word,
            futex_key,
            val: waiter.val as u32,
        });
    }

    let is_private = all_private.unwrap_or(true);
    drop(task);

    if is_private {
        do_futex_waitv(&entries, token, deadline)
    } else {
        do_futex_waitv_shared(&entries, token, deadline)
    }
}
