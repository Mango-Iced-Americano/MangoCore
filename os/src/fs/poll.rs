use core::hint::spin_loop;

use crate::{
    config::PAGE_SIZE,
    fs::vfs::{event::EPollEvent, FdTable, File, PollWaitQueue},
    mm::{UserPtr, UserSlice},
    net::config::NET_INTERFACE,
    signal_type,
    syscall::errno::EFAULT,
    task::signal::Signals,
    timer::{get_clock_freq, get_time, TimeSpec, NSEC_PER_SEC},
    utils::error::SyscallErr,
};
use alloc::vec::Vec;

use crate::task::{current_task, has_actionable_signal, task_manager_counts, WaitQueue, WaitResult};
///  A scheduling  scheme  whereby  the  local  process  periodically  checks  until  the  pre-specified events (for example, read, write) have occurred.
/// The PollFd struct in 32-bit style.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PollFd {
    /// File descriptor
    fd: u32,
    /// Requested events
    events: PollEvent,
    /// Returned events
    revents: PollEvent,
}

bitflags! {
    /// Event types that can be polled for.
    ///
    /// These bits may be set in `events`(see `ppoll()`) to indicate the interesting event types;
    ///
    /// they will appear in `revents` to indicate the status of the file descriptor.
    struct PollEvent:u16 {
    /// There is data to read.
    const POLLIN = 0x001;
    /// There is urgent data to read.
    const POLLPRI = 0x002;
    /// Writing now will not block.
    const POLLOUT = 0x004;

    // These values are defined in XPG4.2.
    /// Normal data may be read.
    const POLLRDNORM = 0x040;
    /// Priority data may be read.
    const POLLRDBAND = 0x080;
    /// Writing now will not block.
    const POLLWRNORM = 0x100;
    /// Priority data may be written.
    const POLLWRBAND = 0x200;


    /// Linux Extension.
    const POLLMSG = 0x400;
    /// Linux Extension.
    const POLLREMOVE = 0x1000;
    /// Linux Extension.
    const POLLRDHUP = 0x2000;

    /* Event types always implicitly polled for.
    These bits need not be set in `events',
    but they will appear in `revents' to indicate the status of the file descriptor.*/

    /// Implicitly polled for only.
    /// Error condition.
    const POLLERR = 0x008;
    /// Implicitly polled for only.
    /// Hung up.
    const POLLHUP = 0x010;
    /// Implicitly polled for only.
    /// Invalid polling request.
    const POLLNVAL = 0x020;
    }
}

fn implicit_epoll_events() -> EPollEvent {
    EPollEvent::EPOLLERR | EPollEvent::EPOLLHUP | EPollEvent::EPOLLNVAL
}

fn pselect_read_events() -> EPollEvent {
    EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM | EPollEvent::EPOLLERR | EPollEvent::EPOLLHUP
}

fn pselect_write_events() -> EPollEvent {
    EPollEvent::EPOLLOUT | EPollEvent::EPOLLWRNORM | EPollEvent::EPOLLERR | EPollEvent::EPOLLHUP
}

fn pselect_except_events() -> EPollEvent {
    EPollEvent::EPOLLPRI | EPollEvent::EPOLLRDBAND | EPollEvent::EPOLLERR
}

fn poll_to_epoll(events: PollEvent) -> EPollEvent {
    EPollEvent::from_bits_truncate(events.bits() as usize)
}

fn epoll_to_poll(events: EPollEvent) -> PollEvent {
    PollEvent::from_bits_truncate(events.bits() as u16)
}

fn apply_temporary_sigmask(sigmask: *const Signals) -> Result<Option<Signals>, isize> {
    if sigmask.is_null() {
        return Ok(None);
    }
    if (sigmask as usize) < PAGE_SIZE {
        return Err(EFAULT);
    }

    let task = current_task().unwrap();
    let token = task.get_user_token();
    let bits = UserPtr::new(sigmask as *const u64).read(token)?;
    let mut new_mask = Signals::from_bits_truncate(bits as signal_type!());
    new_mask.remove(Signals::CAN_NOT_BE_MASKED);

    let mut inner = task.acquire_inner_lock();
    let old_mask = inner.sigmask;
    inner.sigmask = new_mask;
    Ok(Some(old_mask))
}

fn restore_sigmask(old_mask: Option<Signals>) {
    if let Some(old_mask) = old_mask {
        if let Some(task) = current_task() {
            task.acquire_inner_lock().sigmask = old_mask;
        }
    }
}

fn collect_wait_queues(file: &File, wait_queues: &mut Vec<PollWaitQueue>) {
    if let Some(queue) = file.read_wait_queue() {
        wait_queues.push(queue);
    }
    if let Some(queue) = file.write_wait_queue() {
        wait_queues.push(queue);
    }
}

fn poll_wait(
    wait_queues: &[PollWaitQueue],
    deadline: Option<TimeSpec>,
    mut cond: impl FnMut() -> Option<isize>,
) -> WaitResult {
    if wait_queues.is_empty() {
        if let Some(result) = try_short_empty_poll(deadline, &mut cond) {
            return result;
        }
    }
    let queue_refs: Vec<&spin::Mutex<WaitQueue>> =
        wait_queues.iter().map(|queue| queue.queue()).collect();
    WaitQueue::wait_on_queues_interruptible_timeout(&queue_refs, cond, deadline)
}

fn poll_wait_empty(deadline: Option<TimeSpec>) -> WaitResult {
    if let Some(result) = try_short_empty_timeout(deadline) {
        return result;
    }
    WaitQueue::wait_on_queues_interruptible_timeout(&[], || None, deadline)
}

fn timespec_to_ticks(time: TimeSpec) -> usize {
    time.tv_sec
        .saturating_mul(get_clock_freq())
        .saturating_add(time.tv_nsec.saturating_mul(get_clock_freq()) / NSEC_PER_SEC)
}

fn try_short_empty_timeout(deadline: Option<TimeSpec>) -> Option<WaitResult> {
    let deadline = deadline?;
    let now = TimeSpec::now();
    if now >= deadline {
        return Some(WaitResult::TimedOut);
    }
    if deadline - now > TimeSpec::from_ms(50) {
        return None;
    }
    if task_manager_counts()
        .map(|(ready, _)| ready != 0)
        .unwrap_or(true)
    {
        return None;
    }

    let deadline_ticks = timespec_to_ticks(deadline);
    let task = current_task().unwrap();
    let mut spins = 0usize;
    loop {
        if get_time() >= deadline_ticks {
            return Some(WaitResult::TimedOut);
        }

        spins = spins.wrapping_add(1);
        if spins & 0x3ff == 0 && has_actionable_signal(&task) {
            return Some(WaitResult::Interrupted);
        }
        spin_loop();
    }
}

fn try_short_empty_poll<F>(deadline: Option<TimeSpec>, cond: &mut F) -> Option<WaitResult>
where
    F: FnMut() -> Option<isize>,
{
    let deadline = deadline?;
    let now = TimeSpec::now();
    if now >= deadline {
        return Some(WaitResult::TimedOut);
    }
    if deadline - now > TimeSpec::from_ms(50) {
        return None;
    }
    if task_manager_counts()
        .map(|(ready, _)| ready != 0)
        .unwrap_or(true)
    {
        return None;
    }

    if let Some(value) = cond() {
        return Some(WaitResult::Ready(value));
    }

    let task = current_task().unwrap();
    let mut spins = 0usize;
    loop {
        if TimeSpec::now() >= deadline {
            return Some(WaitResult::TimedOut);
        }
        if has_actionable_signal(&task) {
            return Some(WaitResult::Interrupted);
        }

        spins = spins.wrapping_add(1);
        if spins & 0x3ff == 0 {
            if let Some(value) = cond() {
                return Some(WaitResult::Ready(value));
            }
            if task_manager_counts()
                .map(|(ready, _)| ready != 0)
                .unwrap_or(true)
            {
                return None;
            }
        }
        spin_loop();
    }
}

fn fdset_has_requested_fds(set: &Option<FdSet>, nfds: usize) -> bool {
    let Some(set) = set else {
        return false;
    };
    let limit = nfds.min(1024);
    for fd in 0..limit {
        if set.is_set(fd) {
            return true;
        }
    }
    false
}

fn pselect_has_requested_fds(
    nfds: usize,
    read_fds: &Option<FdSet>,
    write_fds: &Option<FdSet>,
    exception_fds: &Option<FdSet>,
) -> bool {
    fdset_has_requested_fds(read_fds, nfds)
        || fdset_has_requested_fds(write_fds, nfds)
        || fdset_has_requested_fds(exception_fds, nfds)
}

fn checked_timeout_deadline(timeout: Option<TimeSpec>) -> Result<Option<TimeSpec>, isize> {
    match timeout {
        Some(timeout) => {
            if timeout.tv_sec > isize::MAX as usize || timeout.tv_nsec >= NSEC_PER_SEC {
                Err(-(SyscallErr::EINVAL as isize))
            } else {
                Ok(Some(timeout + TimeSpec::now()))
            }
        }
        None => Ok(None),
    }
}

struct PPollScan {
    ready: isize,
    wait_queues: Vec<PollWaitQueue>,
}

fn scan_ppoll(fds: &spin::Mutex<FdTable>, poll_fds: &mut [PollFd], collect_wait: bool) -> PPollScan {
    NET_INTERFACE.poll();
    let fd_table = fds.lock();
    let mut ready = 0;
    let mut wait_queues = Vec::new();

    for poll_fd in poll_fds.iter_mut() {
        poll_fd.revents = PollEvent::empty();
        if (poll_fd.fd as i32) < 0 {
            continue;
        }
        match fd_table.get_ref(poll_fd.fd as usize) {
            Ok(file) => {
                let requested = poll_to_epoll(poll_fd.events);
                let observed = file.poll_events();
                let returned = observed & (requested | implicit_epoll_events());
                poll_fd.revents = epoll_to_poll(returned);
                if !poll_fd.revents.is_empty() {
                    ready += 1;
                } else if collect_wait {
                    collect_wait_queues(file, &mut wait_queues);
                }
            }
            Err(_) => {
                poll_fd.revents = PollEvent::POLLNVAL;
                ready += 1;
            }
        }
    }

    PPollScan { ready, wait_queues }
}

struct PSelectScan {
    ready: isize,
    wait_queues: Vec<PollWaitQueue>,
    read_fds: Option<FdSet>,
    write_fds: Option<FdSet>,
    exception_fds: Option<FdSet>,
    error: Option<isize>,
}

fn scan_pselect(
    fds: &spin::Mutex<FdTable>,
    nfds: usize,
    read_fds: &Option<FdSet>,
    write_fds: &Option<FdSet>,
    exception_fds: &Option<FdSet>,
    collect_wait: bool,
) -> PSelectScan {
    NET_INTERFACE.poll();
    let fd_table = fds.lock();
    let mut ready = 0;
    let mut wait_queues = Vec::new();
    let mut out_read = read_fds.map(|_| FdSet::empty());
    let mut out_write = write_fds.map(|_| FdSet::empty());
    let mut out_exception = exception_fds.map(|_| FdSet::empty());

    for fd in 0..1024 {
        let want_read = read_fds.as_ref().map(|set| set.is_set(fd)).unwrap_or(false);
        let want_write = write_fds.as_ref().map(|set| set.is_set(fd)).unwrap_or(false);
        let want_exception = exception_fds
            .as_ref()
            .map(|set| set.is_set(fd))
            .unwrap_or(false);
        if !want_read && !want_write && !want_exception {
            continue;
        }

        let file = match fd_table.get_ref(fd) {
            Ok(file) => file,
            Err(_) => {
                return PSelectScan {
                    ready: 0,
                    wait_queues,
                    read_fds: out_read,
                    write_fds: out_write,
                    exception_fds: out_exception,
                    error: Some(-(SyscallErr::EBADF as isize)),
                };
            }
        };
        if fd >= nfds {
            continue;
        }

        let observed = file.poll_events();
        let mut fd_ready = false;
        if want_read && observed.intersects(pselect_read_events()) {
            if let Some(set) = out_read.as_mut() {
                set.set(fd);
            }
            ready += 1;
            fd_ready = true;
        }
        if want_write && observed.intersects(pselect_write_events()) {
            if let Some(set) = out_write.as_mut() {
                set.set(fd);
            }
            ready += 1;
            fd_ready = true;
        }
        if want_exception && observed.intersects(pselect_except_events()) {
            if let Some(set) = out_exception.as_mut() {
                set.set(fd);
            }
            ready += 1;
            fd_ready = true;
        }
        if !fd_ready && collect_wait {
            collect_wait_queues(file, &mut wait_queues);
        }
    }

    PSelectScan {
        ready,
        wait_queues,
        read_fds: out_read,
        write_fds: out_write,
        exception_fds: out_exception,
        error: None,
    }
}

/// Wait for one of the events in `poll_fd_p` to happen, or the time limit to run out if any.
/// Unlike the function family of `select()` which are basically AND'S,
/// `poll()`'s act like OR's for polling the files.
/// # Arguments
/// * `poll_fd`: The USER pointer to the array of file descriptors to be polled
/// * `nfds`: The number stored in the previous array.
/// * `time_spec`: The time, see `timer::TimeSpec` for information. NOT SUPPORTED and will be ignored!
/// * `sigmask`: The pointer to the sigmask in use during the poll.
/// # Note
/// * `POLLHUP`, `POLLNVAL` and `POLLERR` are ALWAYS polled for all given files,
///   regardless of whether it is set in the array.
/// # Unsupported Features
/// * Other implementations are supported by specific files and may not be used by
/// * Currently only user space structs are supported.
/// # Return Conditions
/// The call will block until either:
/// * a file descriptor becomes ready;
/// * the call is interrupted by a signal handler; or
/// * the timeout expires.
/// # Return Values and Side-effects
/// * On success, a positive number is returned; this is the number of structures
///   which have nonzero revents fields (in other words, those descriptors
///   with events or errors reported).
/// * A value of 0 indicates that the call timed out and no file descriptors were ready.
/// * On error, -1 is returned, and errno is set appropriately.
/// * The observed event is written back to the array, with others cleared.
pub fn ppoll(
    fds: *mut PollFd,
    nfds: usize,
    tmo_p: *const TimeSpec,
    sigmask: *const Signals,
) -> isize {
    if nfds > 4096 {
        return crate::syscall::errno::EINVAL;
    }
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let timeout: Option<TimeSpec> = match UserPtr::new(tmo_p).read_optional(token) {
        Ok(tmo) => match checked_timeout_deadline(tmo) {
            Ok(deadline) => deadline,
            Err(errno) => return errno,
        },
        Err(errno) => return errno,
    };
    let files = task.process.files();
    drop(task);
    let old_mask = match apply_temporary_sigmask(sigmask) {
        Ok(old_mask) => old_mask,
        Err(errno) => return errno,
    };

    let mut poll_fd = alloc::vec![
        PollFd {
            fd: 0,
            events: PollEvent::empty(),
            revents: PollEvent::empty(),
        };
        nfds
    ];

    let mut done: isize;
    let mut interrupted = false;
    if UserSlice::new(fds as *const PollFd, nfds)
        .read_array_into(token, &mut poll_fd)
        .is_err()
    {
        log::error!(
            "[ppoll] Error copy_from_user_array(_, fds: {:?}, poll_fd.as_mut_ptr():{:?}, _)",
            fds,
            poll_fd.as_mut_ptr()
        );
        done = EFAULT;
    } else {
        if nfds == 0 {
            done = 0;
            if timeout
                .map(|deadline| TimeSpec::now() < deadline)
                .unwrap_or(true)
            {
                match poll_wait_empty(timeout) {
                    WaitResult::Ready(value) => done = value,
                    WaitResult::Interrupted => interrupted = true,
                    WaitResult::TimedOut => done = 0,
                }
            }
        } else {
            let scan = scan_ppoll(&files, &mut poll_fd, true);
            done = scan.ready;
            if done == 0
                && timeout
                    .map(|deadline| TimeSpec::now() < deadline)
                    .unwrap_or(true)
            {
                match poll_wait(&scan.wait_queues, timeout, || {
                    let scan = scan_ppoll(&files, &mut poll_fd, false);
                    if scan.ready > 0 {
                        Some(scan.ready)
                    } else {
                        None
                    }
                }) {
                    WaitResult::Ready(value) => done = value,
                    WaitResult::Interrupted => interrupted = true,
                    WaitResult::TimedOut => done = 0,
                }
            }
        }

        log::trace!("[ppoll] result: {:?}", poll_fd);
        if let Err(_) = UserSlice::new(fds as *const PollFd, nfds)
            .write_array_from(token, &poll_fd)
        {
            done = EFAULT;
            interrupted = false;
        }
    }
    restore_sigmask(old_mask);
    if interrupted {
        return -(SyscallErr::EINTR as isize);
    }
    done
}

// This may be unsafe since the size of bits is undefined.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
/// Bitmap used by `pselect()` and `select` to indicate the event to wait for.
pub struct FdSet {
    bits: [u64; 16],
}
use crate::lang_items::Bytes;
#[allow(unused)]
impl FdSet {
    /// Return an empty bitmap for further manipulation
    pub fn empty() -> Self {
        Self { bits: [0; 16] }
    }
    /// Divide `d` by 64 to decide the `u64` in `bits` to visit.
    fn fd_elt(d: usize) -> usize {
        d >> 6
    }
    /// Mod `d` by 64 for the position of `d` in the `fd_elt()` bitmap.
    fn fd_mask(d: usize) -> u64 {
        1 << (d & 0x3F)
    }
    /// Clear the current struct.
    pub fn clr_all(&mut self) {
        for i in 0..16 {
            self.bits[i] = 0;
        }
    }
    /// Collect all fds with their bits set.
    pub fn get_fd_vec(&self) -> Vec<usize> {
        let mut v = Vec::new();
        for i in 0..1024 {
            if self.is_set(i) {
                v.push(i);
            }
        }
        v
    }
    /// The total number of set bits.
    pub fn set_num(&self) -> u32 {
        let mut sum: u32 = 0;
        for i in self.bits.iter() {
            sum += i.count_ones();
        }
        sum
    }
    pub fn set(&mut self, d: usize) {
        self.bits[Self::fd_elt(d)] |= Self::fd_mask(d);
    }
    /// Clear a certain bit `d` to stop waiting for the event of the correspond fd.
    pub fn clr(&mut self, d: usize) {
        self.bits[Self::fd_elt(d)] &= !Self::fd_mask(d);
    }
    /// Predicate for whether the bit is set for the `d`
    pub fn is_set(&self, d: usize) -> bool {
        (Self::fd_mask(d) & self.bits[Self::fd_elt(d)]) != 0
    }
}
impl Bytes<FdSet> for FdSet {
    fn as_bytes(&self) -> &[u8] {
        let size = core::mem::size_of::<FdSet>();
        unsafe { core::slice::from_raw_parts(self as *const _ as *const u8, size) }
    }

    fn as_bytes_mut(&mut self) -> &mut [u8] {
        let size = core::mem::size_of::<FdSet>();
        unsafe { core::slice::from_raw_parts_mut(self as *mut _ as *mut u8, size) }
    }
}
pub fn pselect(
    nfds: usize,
    read_fds: &mut Option<FdSet>,
    write_fds: &mut Option<FdSet>,
    exception_fds: &mut Option<FdSet>,
    timeout: &Option<TimeSpec>,
    sigmask: *const Signals,
) -> isize {
    let timeout: Option<TimeSpec> = match checked_timeout_deadline(*timeout) {
        Ok(deadline) => deadline,
        Err(errno) => return errno,
    };

    let old_mask = match apply_temporary_sigmask(sigmask) {
        Ok(old_mask) => old_mask,
        Err(errno) => return errno,
    };

    if nfds > 1024 {
        restore_sigmask(old_mask);
        return -(SyscallErr::EINVAL as isize);
    }

    let has_requested_fds = pselect_has_requested_fds(nfds, read_fds, write_fds, exception_fds);
    let files = current_task().unwrap().process.files();
    let initial_scan = if has_requested_fds {
        scan_pselect(&files, nfds, read_fds, write_fds, exception_fds, true)
    } else {
        PSelectScan {
            ready: 0,
            wait_queues: Vec::new(),
            read_fds: read_fds.as_ref().map(|_| FdSet::empty()),
            write_fds: write_fds.as_ref().map(|_| FdSet::empty()),
            exception_fds: exception_fds.as_ref().map(|_| FdSet::empty()),
            error: None,
        }
    };
    let mut done = initial_scan.ready;
    let mut interrupted = false;
    let mut error = initial_scan.error;
    let mut ready_sets = if error.is_none() && done > 0 {
        Some((
            initial_scan.read_fds,
            initial_scan.write_fds,
            initial_scan.exception_fds,
        ))
    } else {
        None
    };

    if error.is_none()
        && done == 0
        && timeout
            .map(|deadline| TimeSpec::now() < deadline)
            .unwrap_or(true)
    {
        let wait_result = if has_requested_fds {
            poll_wait(&initial_scan.wait_queues, timeout, || {
                let scan = scan_pselect(&files, nfds, read_fds, write_fds, exception_fds, false);
                if let Some(error) = scan.error {
                    Some(error)
                } else if scan.ready > 0 {
                    ready_sets = Some((scan.read_fds, scan.write_fds, scan.exception_fds));
                    Some(scan.ready)
                } else {
                    None
                }
            })
        } else {
            poll_wait_empty(timeout)
        };
        match wait_result {
            WaitResult::Ready(value) if value < 0 => error = Some(value),
            WaitResult::Ready(value) => done = value,
            WaitResult::Interrupted => {
                interrupted = true;
                log::info!("[pselect] Interrupted by signal(s)");
            }
            WaitResult::TimedOut => done = 0,
        }
    }

    // ============================================================
    // Step 1: Always restore sigmask first, regardless of outcome
    // ============================================================
    restore_sigmask(old_mask);

    // ============================================================
    // Step 2: Priority-based result dispatch
    // ============================================================

    // 1. EBADF / other errors
    if let Some(err_code) = error {
        return err_code;
    }

    // 2. Signal interruption
    if interrupted {
        log::info!("[pselect] Interrupted by signal(s), returning EINTR");
        return -(SyscallErr::EINTR as isize);
    }

    // 3. Normal FD set writeback
    if let Some((new_read, new_write, new_exception)) = ready_sets {
        *read_fds = new_read;
        *write_fds = new_write;
        *exception_fds = new_exception;
    } else {
        if let Some(read_fds) = read_fds.as_mut() {
            *read_fds = FdSet::empty();
        }
        if let Some(write_fds) = write_fds.as_mut() {
            *write_fds = FdSet::empty();
        }
        if let Some(exception_fds) = exception_fds {
            *exception_fds = FdSet::empty();
        }
    }
    log::debug!(
        "[pselect] read_fds: {:?}, write_fds: {:?}, exception_fds: {:?}",
        read_fds,
        write_fds,
        exception_fds
    );
    // sigmask already restored at Step 1 above (before dispatch)
    done as isize
}
