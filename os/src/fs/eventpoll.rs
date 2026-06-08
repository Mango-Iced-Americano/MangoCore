use alloc::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    vec::Vec,
};
use core::any::Any;
use core::sync::atomic::{AtomicUsize, Ordering};

use spin::{Mutex, MutexGuard};

use crate::{
    config::PAGE_SIZE,
    fs::{
        dev::DEV_FS,
        vfs::{
            event::{EPollEvent, EventListener},
            EventQueueHandle, File, FileFlags, FileMode, FilePrivateData, FileType, FileSystem,
            IndexNode, InodeMode, Metadata, PollWaitQueue,
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
const EPOLL_MAX_NESTS: usize = 4;

static NEXT_EVENTPOLL_ID: AtomicUsize = AtomicUsize::new(1);

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
    last_ready: EPollEvent,
    event_queues: Vec<EventQueueHandle>,
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
    id: usize,
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
            id: NEXT_EVENTPOLL_ID.fetch_add(1, Ordering::Relaxed),
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

    fn collect_event_queues(file: &File) -> Vec<EventQueueHandle> {
        let mut event_queues = Vec::new();
        if let Some(queue) = file.read_event_queue() {
            event_queues.push(queue);
        }
        if let Some(queue) = file.write_event_queue() {
            let exists = event_queues.iter().any(|item| {
                core::ptr::eq(item.queue() as *const _, queue.queue() as *const _)
            });
            if !exists {
                event_queues.push(queue);
            }
        }
        event_queues
    }

    fn scan(&self, collect_wait: bool) -> EPollScan {
        NET_INTERFACE.poll();
        let items = self.snapshot_items();
        let mut wait_queues = Vec::new();

        self.reset_level_ready_list();

        for (fd, item) in items {
            if !item.enabled {
                continue;
            }
            let observed = item.file.poll_events();
            if Self::returned_events(observed, item.events).is_empty() {
                if collect_wait {
                    Self::collect_wait_queues(&item.file, &mut wait_queues);
                }
                self.clear_ready_state(fd);
                continue;
            }
            self.record_observed_event(fd, observed);
        }

        let ready = self.ready_snapshot();

        EPollScan { ready, wait_queues }
    }

    fn reset_level_ready_list(&self) {
        let mut inner = self.inner.lock();
        let edge_fds: Vec<usize> = inner
            .items
            .iter()
            .filter_map(|(fd, item)| {
                if item.events.contains(EPollEvent::EPOLLET) {
                    Some(*fd)
                } else {
                    None
                }
            })
            .collect();
        inner
            .ready_list
            .retain(|event| edge_fds.iter().any(|fd| *fd == event.fd));
        for item in inner.items.values_mut() {
            if !item.events.contains(EPollEvent::EPOLLET) {
                item.last_ready = EPollEvent::empty();
            }
        }
    }

    fn ready_snapshot(&self) -> Vec<ReadyEvent> {
        self.inner.lock().ready_list.iter().copied().collect()
    }

    fn clear_ready_state(&self, fd: usize) {
        let mut inner = self.inner.lock();
        if let Some(item) = inner.items.get_mut(&fd) {
            item.last_ready = EPollEvent::empty();
        }
        inner.ready_list.retain(|event| event.fd != fd);
    }

    fn push_ready_locked(inner: &mut EventPollInner, ready: ReadyEvent) {
        if let Some(existing) = inner
            .ready_list
            .iter_mut()
            .find(|event| event.fd == ready.fd)
        {
            existing.events |= ready.events;
            existing.data = ready.data;
            return;
        }
        inner.ready_list.push_back(ready);
    }

    fn record_observed_event(&self, fd: usize, observed: EPollEvent) {
        let mut inner = self.inner.lock();
        let Some(item) = inner.items.get_mut(&fd) else {
            return;
        };
        if !item.enabled {
            return;
        }

        let returned = Self::returned_events(observed, item.events);
        if returned.is_empty() {
            item.last_ready = EPollEvent::empty();
            inner.ready_list.retain(|event| event.fd != fd);
            return;
        }

        if item.events.contains(EPollEvent::EPOLLET) {
            let new_bits = returned & !item.last_ready;
            if new_bits.is_empty() {
                return;
            }
        }
        item.last_ready = returned;
        let data = item.data;
        Self::push_ready_locked(
            &mut inner,
            ReadyEvent {
                fd,
                events: returned,
                data,
            },
        );
    }

    fn take_ready(&self, maxevents: usize) -> Vec<ReadyEvent> {
        let mut inner = self.inner.lock();
        let mut ready = Vec::new();
        while ready.len() < maxevents {
            let Some(event) = inner.ready_list.pop_front() else {
                break;
            };
            if event.events.is_empty() {
                continue;
            }
            if inner
                .items
                .get(&event.fd)
                .map(|item| item.enabled)
                .unwrap_or(false)
            {
                ready.push(event);
            }
        }
        ready
    }

    fn listener(self: &Arc<Self>) -> alloc::sync::Weak<dyn EventListener> {
        let listener: Arc<dyn EventListener> = self.clone();
        Arc::downgrade(&listener)
    }

    fn register_event_queues(self: &Arc<Self>, fd: usize, item: &EPollItem) {
        let listener = self.listener();
        for queue in item.event_queues.iter() {
            queue
                .queue()
                .register(self.id, fd, item.events, listener.clone());
        }
    }

    fn unregister_event_queues(&self, fd: usize, item: &EPollItem) {
        for queue in item.event_queues.iter() {
            queue.queue().unregister(self.id, fd);
        }
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
        self.scan(false);
        !self.inner.lock().ready_list.is_empty()
    }

    fn add(
        self: &Arc<Self>,
        fd: usize,
        file: Arc<File>,
        events: EPollEvent,
        data: u64,
    ) -> Result<(), SyscallErr> {
        if events.intersects(Self::unsupported_mask()) {
            return Err(SyscallErr::EINVAL);
        }

        let event_queues = Self::collect_event_queues(&file);
        let item = EPollItem {
            file,
            events,
            data,
            enabled: true,
            last_ready: EPollEvent::empty(),
            event_queues,
        };
        let initial_file = item.file.clone();
        let mut inner = self.inner.lock();
        if inner.items.contains_key(&fd) {
            return Err(SyscallErr::EEXIST);
        }
        self.register_event_queues(fd, &item);
        inner.items.insert(fd, item);
        drop(inner);
        self.record_observed_event(fd, initial_file.poll_events());
        self.wait_queue.lock().wake_all();
        Ok(())
    }

    fn modify(self: &Arc<Self>, fd: usize, events: EPollEvent, data: u64) -> Result<(), SyscallErr> {
        if events.intersects(Self::unsupported_mask()) {
            return Err(SyscallErr::EINVAL);
        }

        let mut inner = self.inner.lock();
        let item = inner.items.get_mut(&fd).ok_or(SyscallErr::ENOENT)?;
        item.events = events;
        item.data = data;
        item.enabled = true;
        item.last_ready = EPollEvent::empty();
        self.register_event_queues(fd, item);
        let file = item.file.clone();
        inner.ready_list.retain(|event| event.fd != fd);
        drop(inner);
        self.record_observed_event(fd, file.poll_events());
        self.wait_queue.lock().wake_all();
        Ok(())
    }

    fn delete(&self, fd: usize) -> Result<(), SyscallErr> {
        let mut inner = self.inner.lock();
        let item = inner.items.remove(&fd).ok_or(SyscallErr::ENOENT)?;
        self.unregister_event_queues(fd, &item);
        inner.ready_list.retain(|event| event.fd != fd);
        Ok(())
    }

    fn check_nested_epoll(self: &Arc<Self>, target: &Arc<EventPoll>) -> Result<(), SyscallErr> {
        let mut seen = Vec::new();
        let depth = target.nested_depth_to(self.id, &mut seen)?;
        if depth + 1 > EPOLL_MAX_NESTS {
            return Err(SyscallErr::EINVAL);
        }
        Ok(())
    }

    fn nested_depth_to(
        &self,
        ancestor_id: usize,
        seen: &mut Vec<usize>,
    ) -> Result<usize, SyscallErr> {
        if self.id == ancestor_id {
            return Err(SyscallErr::ELOOP);
        }
        if seen.iter().any(|id| *id == self.id) {
            return Ok(0);
        }
        seen.push(self.id);

        let items = self.snapshot_items();
        let mut max_depth = 0;
        for (_, item) in items {
            let Some(child) = eventpoll_from_file(&item.file) else {
                continue;
            };
            let child_depth = child.nested_depth_to(ancestor_id, seen)? + 1;
            max_depth = max_depth.max(child_depth);
            if max_depth >= EPOLL_MAX_NESTS {
                break;
            }
        }
        seen.pop();
        Ok(max_depth)
    }

    fn wait(&self, maxevents: usize, timeout: isize) -> Result<Vec<ReadyEvent>, isize> {
        let deadline = if timeout < -1 {
            return Err(-(SyscallErr::EINVAL as isize));
        } else if timeout == -1 {
            None
        } else if timeout == 0 {
            self.scan(false);
            let ready = self.take_ready(maxevents);
            self.disable_oneshot(&ready);
            return Ok(ready);
        } else {
            Some(TimeSpec::now() + TimeSpec::from_ms(timeout as usize))
        };

        let scan = self.scan(true);
        let ready = self.take_ready(maxevents);
        if !ready.is_empty() {
            self.disable_oneshot(&ready);
            return Ok(ready);
        }

        let mut queue_refs: Vec<&Mutex<WaitQueue>> =
            scan.wait_queues.iter().map(|queue| queue.queue()).collect();
        queue_refs.push(&self.wait_queue);

        match WaitQueue::wait_on_queues_interruptible_timeout(
            &queue_refs,
            || {
                self.scan(false);
                let len = self.inner.lock().ready_list.len();
                if len == 0 {
                    None
                } else {
                    Some(len as isize)
                }
            },
            deadline,
        ) {
            WaitResult::Ready(_) => {
                self.scan(false);
                let ready = self.take_ready(maxevents);
                self.disable_oneshot(&ready);
                Ok(ready)
            }
            WaitResult::Interrupted => Err(-(SyscallErr::EINTR as isize)),
            WaitResult::TimedOut => Ok(Vec::new()),
        }
    }
}

impl EventListener for EventPoll {
    fn on_event(&self, key: usize, events: EPollEvent) {
        self.record_observed_event(key, events);
        self.wait_queue.lock().wake_all();
    }
}

impl Drop for EventPoll {
    fn drop(&mut self) {
        let inner = self.inner.lock();
        for (fd, item) in inner.items.iter() {
            self.unregister_event_queues(*fd, item);
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

    fn read_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        Some(&self.event_poll.wait_queue)
    }

    fn is_stream(&self) -> bool {
        true
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

    let epoll_file = match fd_table.get_file(epfd) {
        Ok(file) => file,
        Err(err) => return -(err as isize),
    };
    let epoll = match eventpoll_from_file(&*epoll_file) {
        Some(epoll) => epoll,
        None => return -(SyscallErr::EINVAL as isize),
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
            let target_epoll = eventpoll_from_file(&*file);
            if let Some(target_epoll) = target_epoll.as_ref() {
                if let Err(err) = epoll.check_nested_epoll(target_epoll) {
                    return -(err as isize);
                }
            } else if !file.mode().contains(FileMode::FMODE_STREAM) {
                return -(SyscallErr::EPERM as isize);
            }
            let event = user_event.unwrap();
            let events = EPollEvent::from_bits_truncate(event.events as usize);
            match epoll.add(fd, file, events, event.data) {
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
        let epoll_file = match fd_table.get_file(epfd) {
            Ok(file) => file,
            Err(err) => return -(err as isize),
        };
        match eventpoll_from_file(&*epoll_file) {
            Some(epoll) => epoll,
            None => return -(SyscallErr::EINVAL as isize),
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

pub fn sys_epoll_pwait2(
    epfd: usize,
    events: *mut EpollUserEvent,
    maxevents: isize,
    timeout: *const TimeSpec,
    sigmask: *const Signals,
) -> isize {
    let timeout_ms = if timeout.is_null() {
        -1
    } else {
        let task = current_task().unwrap();
        let token = task.get_user_token();
        let ts = match UserPtr::new(timeout).read(token) {
            Ok(ts) => ts,
            Err(errno) => return errno,
        };
        if ts.tv_sec > isize::MAX as usize || ts.tv_nsec >= 1_000_000_000 {
            return -(SyscallErr::EINVAL as isize);
        }
        let sec_ms = match (ts.tv_sec as isize).checked_mul(1000) {
            Some(v) => v,
            None => return -(SyscallErr::EINVAL as isize),
        };
        let nsec_ms = ((ts.tv_nsec as isize) + 999_999) / 1_000_000;
        match sec_ms.checked_add(nsec_ms) {
            Some(v) => v,
            None => return -(SyscallErr::EINVAL as isize),
        }
    };

    sys_epoll_pwait(epfd, events, maxevents, timeout_ms, sigmask)
}
