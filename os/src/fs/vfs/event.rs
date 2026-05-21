//! 统一 IO 事件位定义
//!
//! 值与 Linux `include/uapi/asm-generic/poll.h` 一致，对标 DragonOS 的
//! `EPollEventType`（epoll/mod.rs）及 `PollFlags`（poll.rs）。
//!
//! 整个内核（VFS、net、设备驱动）统一使用此类型作为 poll 事件位掩码。

use alloc::{
    sync::Weak,
    vec::Vec,
};
use spin::{Mutex, MutexGuard};

use crate::task::WaitQueue;

bitflags! {
    /// IO 就绪事件位，用作 `IndexNode::poll()` 返回值及 socket pollee 缓存。
    pub struct EPollEvent: usize {
        const EPOLLIN       = 0x001;
        const EPOLLPRI      = 0x002;
        const EPOLLOUT      = 0x004;
        const EPOLLERR      = 0x008;
        const EPOLLHUP      = 0x010;
        const EPOLLNVAL     = 0x020;
        const EPOLLRDNORM   = 0x040;
        const EPOLLRDBAND   = 0x080;
        const EPOLLWRNORM   = 0x100;
        const EPOLLWRBAND   = 0x200;
        const EPOLLMSG      = 0x400;
        const EPOLLREMOVE   = 0x1000;
        const EPOLLRDHUP    = 0x2000;
        const EPOLLEXCLUSIVE = 1usize << 28;
        const EPOLLWAKEUP    = 1usize << 29;
        const EPOLLONESHOT   = 1usize << 30;
        const EPOLLET        = 1usize << 31;
    }
}

pub trait EventListener: Send + Sync {
    fn on_event(&self, key: usize, events: EPollEvent);
}

#[derive(Clone)]
struct EventListenerEntry {
    listener_id: usize,
    key: usize,
    interest: EPollEvent,
    listener: Weak<dyn EventListener>,
}

pub struct EventWaitQueue {
    wait_queue: Mutex<WaitQueue>,
    listeners: Mutex<Vec<EventListenerEntry>>,
}

impl EventWaitQueue {
    pub fn new() -> Self {
        Self {
            wait_queue: Mutex::new(WaitQueue::new()),
            listeners: Mutex::new(Vec::new()),
        }
    }

    pub fn wait_queue(&self) -> &Mutex<WaitQueue> {
        &self.wait_queue
    }

    pub fn lock(&self) -> MutexGuard<WaitQueue> {
        self.wait_queue.lock()
    }

    pub fn try_lock(&self) -> Option<MutexGuard<WaitQueue>> {
        self.wait_queue.try_lock()
    }

    pub fn register(
        &self,
        listener_id: usize,
        key: usize,
        interest: EPollEvent,
        listener: Weak<dyn EventListener>,
    ) {
        let mut listeners = self.listeners.lock();
        if let Some(entry) = listeners
            .iter_mut()
            .find(|entry| entry.listener_id == listener_id && entry.key == key)
        {
            entry.interest = interest;
            entry.listener = listener;
            return;
        }
        listeners.push(EventListenerEntry {
            listener_id,
            key,
            interest,
            listener,
        });
    }

    pub fn unregister(&self, listener_id: usize, key: usize) {
        self.listeners
            .lock()
            .retain(|entry| entry.listener_id != listener_id || entry.key != key);
    }

    pub fn notify_events_all(&self, events: EPollEvent) -> usize {
        self.notify_listeners(events);
        self.wait_queue.lock().wake_all()
    }

    pub fn notify_events_at_most(&self, events: EPollEvent, limit: usize) -> usize {
        self.notify_listeners(events);
        self.wait_queue.lock().wake_at_most(limit)
    }

    /// 非阻塞版 notify：listener 总是通知，task 唤醒仅在 wait_queue 未被锁定时生效。
    /// 避免在 WaitQueue::wait_until_interruptible 的 cond 闭包内自死锁。
    pub fn notify_events_all_if_unlocked(&self, events: EPollEvent) -> usize {
        self.notify_listeners(events);
        match self.wait_queue.try_lock() {
            Some(mut guard) => guard.wake_all(),
            None => 0,
        }
    }

    pub fn notify_events_at_most_if_unlocked(&self, events: EPollEvent, limit: usize) -> usize {
        self.notify_listeners(events);
        match self.wait_queue.try_lock() {
            Some(mut guard) => guard.wake_at_most(limit),
            None => 0,
        }
    }

    fn notify_listeners(&self, events: EPollEvent) {
        if events.is_empty() {
            return;
        }

        let mut deliver = Vec::new();
        {
            let mut listeners = self.listeners.lock();
            listeners.retain(|entry| entry.listener.strong_count() > 0);
            for entry in listeners.iter() {
                let returned = Self::returned_events(events, entry.interest);
                if returned.is_empty() {
                    continue;
                }
                if let Some(listener) = entry.listener.upgrade() {
                    deliver.push((listener, entry.key, returned));
                }
            }
        }

        for (listener, key, returned) in deliver {
            listener.on_event(key, returned);
        }
    }

    fn returned_events(observed: EPollEvent, interest: EPollEvent) -> EPollEvent {
        let control = EPollEvent::EPOLLET
            | EPollEvent::EPOLLONESHOT
            | EPollEvent::EPOLLEXCLUSIVE
            | EPollEvent::EPOLLWAKEUP;
        let implicit = EPollEvent::EPOLLERR | EPollEvent::EPOLLHUP;
        observed & ((interest & !control) | implicit)
    }
}
