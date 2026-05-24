use crate::mm::{UserPtr, VirtAddr};
use crate::syscall::errno::*;
use crate::task::threads::{
    do_futex_wait, do_futex_wait_bitset, do_futex_wait_bitset_shared, do_futex_wait_shared,
    futex_requeue_shared, futex_wake_shared, FutexCmd,
};
use crate::task::{current_task, threads};
use crate::timer::{current_timespec, TimeSpec, NSEC_PER_SEC};
use log::{info, trace};
use num_enum::FromPrimitive;

bitflags! {
    pub struct FutexOption: u32 {
        const PRIVATE = 128;
        const CLOCK_REALTIME = 256;
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

    // 计算用户地址对应的物理地址 key（用于 process-shared futex）
    // 分解为独立函数避免闭包捕获 task 的借用问题
    fn va_to_phys_key(
        vm: &crate::mm::AddressSpace<crate::mm::KernelPageTableImpl>,
        va: usize,
    ) -> Option<usize> {
        let va = VirtAddr::from(va);
        let vpn = va.floor();
        let offset = va.page_offset();
        vm.translate(vpn).map(|ppn| (ppn.0 << 12) + offset)
    }

    fn read_timeout(
        timeout: *const TimeSpec,
        token: usize,
    ) -> Result<Option<TimeSpec>, isize> {
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
