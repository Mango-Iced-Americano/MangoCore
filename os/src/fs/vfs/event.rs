//! 统一 IO 事件位定义
//!
//! 值与 Linux `include/uapi/asm-generic/poll.h` 一致，对标 DragonOS 的
//! `EPollEventType`（epoll/mod.rs）及 `PollFlags`（poll.rs）。
//!
//! 整个内核（VFS、net、设备驱动）统一使用此类型作为 poll 事件位掩码。

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
    }
}
