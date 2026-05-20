use alloc::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    vec::Vec,
};
use core::any::Any;

use spin::{Mutex, MutexGuard};

use crate::{
    config::PAGE_SIZE,
    fs::{
        dev::DEV_FS,
        vfs::{
            event::EPollEvent, File, FileFlags, FilePrivateData, FileType, FileSystem, IndexNode,
            InodeMode, Metadata, PollWaitQueue,
        },
    },
    mm::{UserPtr, UserSlice},
    net::config::NET_INTERFACE,
    signal_type,
    syscall::errno::{EFAULT, SUCCESS},
    task::{current_task, signal::Signals, WaitQueue, WaitResult},
    timer::TimeSpec,
    utils::error::SyscallErr,
};

const EPOLL_CTL_ADD: usize = 1;
const EPOLL_CTL_DEL: usize = 2;
const EPOLL_CTL_MOD: usize = 3;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct EpollUserEvent {
    pub events: u32,
    pub data: u64,
}

#[derive(Clone)]
struct EPollItem {
    file: Arc<File>,
    events: EPollEvent,
    data: u64,
    enabled: bool,
}

#[derive(Clone, Copy)]
struct ReadyEvent {
    fd: usize,
    events: EPollEvent,
    data: u64,
}

struct EventPollInner {
    items: BTreeMap<usize, EPollItem>,
    ready_list: VecDeque<ReadyEvent>,
}

pub struct EventPoll {
    inner: Mutex<EventPollInner>,
    wait_queue: Mutex<WaitQueue>,
}

pub struct EventPollFile {
    event_poll: Arc<EventPoll>,
    metadata: Metadata,
}

struct EPollScan {
    ready: Vec<ReadyEvent>,
    wait_queues: Vec<PollWaitQueue>,
}

impl EventPoll {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(EventPollInner {
                items: BTreeMap::new(),
                ready_list: VecDeque::new(),
            }),
            wait_queue: Mutex::new(WaitQueue::new()),
        }
    }

    fn control_mask() -> EPollEvent {
        EPollEvent::EPOLLET | EPollEvent::EPOLLONESHOT
    }

    fn unsupported_mask() -> EPollEvent {
        EPollEvent::EPOLLEXCLUSIVE | EPollEvent::EPOLLWAKEUP
    }

    fn implicit_mask() -> EPollEvent {
        EPollEvent::EPOLLERR | EPollEvent::EPOLLHUP
    }

    fn returned_events(observed: EPollEvent, requested: EPollEvent) -> EPollEvent {
        let interest = requested & !Self::control_mask();
        observed & (interest | Self::implicit_mask())
    }

    fn snapshot_items(&self) -> Vec<(usize, EPollItem)> {
        self.inner
            .lock()
            .items
            .iter()
            .map(|(fd, item)| (*fd, item.clone()))
            .collect()
    }

    fn collect_wait_queues(file: &File, wait_queues: &mut Vec<PollWaitQueue>) {
        if let Some(queue) = file.read_wait_queue() {
            wait_queues.push(queue);
        }
        if let Some(queue) = file.write_wait_queue() {
            wait_queues.push(queue);
        }
    }

    fn scan(&self, collect_wait: bool) -> EPollScan {
        NET_INTERFACE.poll();
        let items = self.snapshot_items();
        let mut ready = Vec::new();
        let mut wait_queues = Vec::new();

        for (fd, item) in items {
            if !item.enabled {
                continue;
            }
            let observed = item.file.poll_events();
            let returned = Self::returned_events(observed, item.events);
            if returned.is_empty() {
                if collect_wait {
                    Self::collect_wait_queues(&item.file, &mut wait_queues);
                }
                continue;
            }

            ready.push(ReadyEvent {
                fd,
                events: returned,
                data: item.data,
            });
        }

        let mut inner = self.inner.lock();
        inner.ready_list.clear();
        for event in ready.iter().copied() {
            inner.ready_list.push_back(event);
        }

        EPollScan { ready, wait_queues }
    }

    fn disable_oneshot(&self, delivered: &[ReadyEvent]) {
        let mut inner = self.inner.lock();
        for event in delivered {
            if let Some(item) = inner.items.get_mut(&event.fd) {
                if !item.events.contains(EPollEvent::EPOLLONESHOT) {
                    continue;
                }
                item.enabled = false;
            }
        }
    }

    fn has_ready(&self) -> bool {
        !self.scan(false).ready.is_empty()
    }

    fn add(&self, fd: usize, file: File, events: EPollEvent, data: u64) -> Result<(), SyscallErr> {
        if events.intersects(Self::unsupported_mask()) {
            return Err(SyscallErr::EINVAL);
        }

        let mut inner = self.inner.lock();
        if inner.items.contains_key(&fd) {
            return Err(SyscallErr::EEXIST);
        }
        inner.items.insert(
            fd,
            EPollItem {
                file: Arc::new(file),
                events,
                data,
                enabled: true,
            },
        );
        drop(inner);
        self.wait_queue.lock().wake_all();
        Ok(())
    }

    fn modify(&self, fd: usize, events: EPollEvent, data: u64) -> Result<(), SyscallErr> {
        if events.intersects(Self::unsupported_mask()) {
            return Err(SyscallErr::EINVAL);
        }

        let mut inner = self.inner.lock();
        let item = inner.items.get_mut(&fd).ok_or(SyscallErr::ENOENT)?;
        item.events = events;
        item.data = data;
        item.enabled = true;
        drop(inner);
        self.wait_queue.lock().wake_all();
        Ok(())
    }

    fn delete(&self, fd: usize) -> Result<(), SyscallErr> {
        let mut inner = self.inner.lock();
        if inner.items.remove(&fd).is_none() {
            return Err(SyscallErr::ENOENT);
        }
        inner.ready_list.retain(|event| event.fd != fd);
        Ok(())
    }

    fn wait(&self, maxevents: usize, timeout: isize) -> Result<Vec<ReadyEvent>, isize> {
        let deadline = if timeout < -1 {
            return Err(-(SyscallErr::EINVAL as isize));
        } else if timeout == -1 {
            None
        } else if timeout == 0 {
            let scan = self.scan(false);
            let ready: Vec<ReadyEvent> = scan.ready.into_iter().take(maxevents).collect();
            self.disable_oneshot(&ready);
            return Ok(ready);
        } else {
            Some(TimeSpec::now() + TimeSpec::from_ms(timeout as usize))
        };

        let scan = self.scan(true);
        if !scan.ready.is_empty() {
            let ready: Vec<ReadyEvent> = scan.ready.into_iter().take(maxevents).collect();
            self.disable_oneshot(&ready);
            return Ok(ready);
        }

        let mut queue_refs: Vec<&Mutex<WaitQueue>> =
            scan.wait_queues.iter().map(|queue| queue.queue()).collect();
        queue_refs.push(&self.wait_queue);

        match WaitQueue::wait_on_queues_interruptible_timeout(
            &queue_refs,
            || {
                let scan = self.scan(false);
                if scan.ready.is_empty() {
                    None
                } else {
                    Some(scan.ready.len() as isize)
                }
            },
            deadline,
        ) {
            WaitResult::Ready(_) => {
                let scan = self.scan(false);
                let ready: Vec<ReadyEvent> = scan.ready.into_iter().take(maxevents).collect();
                self.disable_oneshot(&ready);
                Ok(ready)
            }
            WaitResult::Interrupted => Err(-(SyscallErr::EINTR as isize)),
            WaitResult::TimedOut => Ok(Vec::new()),
        }
    }
}

impl core::fmt::Debug for EventPollFile {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EventPollFile").finish()
    }
}

impl EventPollFile {
    fn new() -> Self {
        Self {
            event_poll: Arc::new(EventPoll::new()),
            metadata: Metadata::new(
                FileType::File,
                InodeMode::S_IFREG | InodeMode::from_bits_truncate(0o600),
            ),
        }
    }

    fn event_poll(&self) -> Arc<EventPoll> {
        self.event_poll.clone()
    }
}

impl IndexNode for EventPollFile {
    fn read_at(
        &self,
        _offset: usize,
        _len: usize,
        _buf: &mut [u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        Err(SyscallErr::EINVAL)
    }

    fn write_at(
        &self,
        _offset: usize,
        _len: usize,
        _buf: &[u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        Err(SyscallErr::EINVAL)
    }

    fn metadata(&self) -> Result<Metadata, SyscallErr> {
        Ok(self.metadata.clone())
    }

    fn poll(&self, _private_data: &FilePrivateData) -> Result<usize, SyscallErr> {
        if self.event_poll.has_ready() {
            Ok(EPollEvent::EPOLLIN.bits())
        } else {
            Ok(0)
        }
    }

    fn fs(&self) -> Arc<dyn FileSystem> {
        DEV_FS.clone()
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}

fn eventpoll_from_file(file: &File) -> Option<Arc<EventPoll>> {
    file.inode_as_any_ref()
        .downcast_ref::<EventPollFile>()
        .map(|file| file.event_poll())
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

pub fn sys_epoll_create1(flags: usize) -> isize {
    let cloexec_flag = FileFlags::O_CLOEXEC.bits() as usize;
    if flags & !cloexec_flag != 0 {
        return -(SyscallErr::EINVAL as isize);
    }

    let file_flags = FileFlags::O_RDWR
        | if flags & cloexec_flag != 0 {
            FileFlags::O_CLOEXEC
        } else {
            FileFlags::empty()
        };
    let inode = Arc::new(EventPollFile::new()) as Arc<dyn IndexNode>;
    let file = match File::new(inode, file_flags) {
        Ok(file) => file,
        Err(err) => return -(err as isize),
    };

    let task = current_task().unwrap();
    let files = task.process.files();
    let ret = match files
        .lock()
        .alloc_fd(file, flags & cloexec_flag != 0)
    {
        Ok(fd) => fd as isize,
        Err(err) => -(err as isize),
    };
    ret
}

pub fn sys_epoll_ctl(epfd: usize, op: usize, fd: usize, event: *const EpollUserEvent) -> isize {
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let files = task.process.files();
    let fd_table = files.lock();

    let epoll = match fd_table
        .get_file(epfd)
        .ok()
        .and_then(eventpoll_from_file)
    {
        Some(epoll) => epoll,
        None => return -(SyscallErr::EBADF as isize),
    };

    if epfd == fd {
        return -(SyscallErr::EINVAL as isize);
    }

    if op != EPOLL_CTL_DEL {
        if event.is_null() {
            return -(SyscallErr::EFAULT as isize);
        }
    }

    let user_event = if op == EPOLL_CTL_DEL {
        None
    } else {
        match UserPtr::new(event).read(token) {
            Ok(event) => Some(event),
            Err(errno) => return errno,
        }
    };

    match op {
        EPOLL_CTL_ADD => {
            let file = match fd_table.get_file(fd) {
                Ok(file) => file,
                Err(err) => return -(err as isize),
            };
            if eventpoll_from_file(file).is_some() {
                return -(SyscallErr::EINVAL as isize);
            }
            let cloned = match file.try_clone() {
                Some(file) => file,
                None => return -(SyscallErr::EBADF as isize),
            };
            let event = user_event.unwrap();
            let events = EPollEvent::from_bits_truncate(event.events as usize);
            match epoll.add(fd, cloned, events, event.data) {
                Ok(()) => SUCCESS,
                Err(err) => -(err as isize),
            }
        }
        EPOLL_CTL_MOD => {
            let event = user_event.unwrap();
            let events = EPollEvent::from_bits_truncate(event.events as usize);
            match epoll.modify(fd, events, event.data) {
                Ok(()) => SUCCESS,
                Err(err) => -(err as isize),
            }
        }
        EPOLL_CTL_DEL => match epoll.delete(fd) {
            Ok(()) => SUCCESS,
            Err(err) => -(err as isize),
        },
        _ => -(SyscallErr::EINVAL as isize),
    }
}

pub fn sys_epoll_pwait(
    epfd: usize,
    events: *mut EpollUserEvent,
    maxevents: isize,
    timeout: isize,
    sigmask: *const Signals,
) -> isize {
    if maxevents <= 0 {
        return -(SyscallErr::EINVAL as isize);
    }
    if events.is_null() {
        return -(SyscallErr::EFAULT as isize);
    }

    let task = current_task().unwrap();
    let token = task.get_user_token();
    let files = task.process.files();
    let epoll = {
        let fd_table = files.lock();
        match fd_table
            .get_file(epfd)
            .ok()
            .and_then(eventpoll_from_file)
        {
            Some(epoll) => epoll,
            None => return -(SyscallErr::EBADF as isize),
        }
    };
    drop(task);

    let old_mask = match apply_temporary_sigmask(sigmask) {
        Ok(old_mask) => old_mask,
        Err(errno) => return errno,
    };

    let ready = epoll.wait(maxevents as usize, timeout);
    restore_sigmask(old_mask);

    let ready = match ready {
        Ok(events) => events,
        Err(errno) => return errno,
    };

    let mut out = Vec::new();
    if out.try_reserve(ready.len()).is_err() {
        return -(SyscallErr::ENOMEM as isize);
    }
    for event in ready {
        out.push(EpollUserEvent {
            events: event.events.bits() as u32,
            data: event.data,
        });
    }

    if UserSlice::new(events as *const EpollUserEvent, out.len())
        .write_array_from(token, &out)
        .is_err()
    {
        return -(SyscallErr::EFAULT as isize);
    }

    out.len() as isize
}
