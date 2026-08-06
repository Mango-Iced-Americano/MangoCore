use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use lazy_static::lazy_static;
use log::{info, warn};
use spin::{Mutex, MutexGuard};

use crate::fs::dev::tty::{Termios, WinSize};
use crate::fs::dev::DEV_FS;
use crate::fs::vfs::event::{EPollEvent, EventWaitQueue};
use crate::fs::vfs::file::FileFlags;
use crate::fs::vfs::file_system::FileSystem as NewFileSystem;
use crate::fs::vfs::{
    FilePrivateData, FileType, IndexNode, InodeFlags, InodeId, InodeMode, Metadata,
};
use crate::task::WaitQueue;
use crate::timer::TimeSpec;
use crate::utils::error::SyscallErr;

// ioctl 基础命令值（去掉方向/大小编码位，取低16位 type << 8 | nr）
// musl 对标准 TTY ioctl 发送 raw 值，对 PTY ioctl 发送编码值
// 统一用 iocbase(cmd) 提取基础命令号
fn iocbase(cmd: u32) -> u32 {
    cmd & 0xFFFF
}

const PTY_BUF_SIZE: usize = 4096;
const MAX_SLAVE_OPENS: usize = 256;

const ONLCR: u32 = 0o000004;

const TCGETS: u32 = 0x5401;
const TCSETS: u32 = 0x5402;
const TCSETSW: u32 = 0x5403;
const TCSETSF: u32 = 0x5404;
const TCGETA: u32 = 0x5405;
const TCSETA: u32 = 0x5406;
const TCSETAW: u32 = 0x5407;
const TCSETAF: u32 = 0x5408;
const TCSBRK: u32 = 0x5409;
const TCXONC: u32 = 0x540A;
const TIOCGPGRP: u32 = 0x540F;
const TIOCSPGRP: u32 = 0x5410;
const TIOCGWINSZ: u32 = 0x5413;
const TIOCSWINSZ: u32 = 0x5414;
const FIONREAD: u32 = 0x541B;
const FIONBIO: u32 = 0x5421;
const TCSBRKP: u32 = 0x5425;

const TIOCGPTN: u32 = 0x5430;
const TIOCSPTLCK: u32 = 0x5431;
const TIOCGPTLCK: u32 = 0x5439;
const TIOCGPTPEER: u32 = 0x5441;
const TIOCSETD: u32 = 0x5423;
const TIOCGETD: u32 = 0x5424;
const TIOCVHANGUP: u32 = 0x5437;

// ─── RingBuffer ──────────────────────────────────────────────────────────

pub(crate) struct RingBuffer {
    buf: Vec<u8>,
    head: usize,
    len: usize,
}

impl RingBuffer {
    fn new(capacity: usize) -> Self {
        RingBuffer {
            buf: alloc::vec![0u8; capacity],
            head: 0,
            len: 0,
        }
    }

    fn available(&self) -> usize {
        self.len
    }

    fn free_space(&self) -> usize {
        self.buf.len() - self.len
    }

    fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }

    fn write(&mut self, data: &[u8]) -> usize {
        let cap = self.buf.len();
        let n = data.len().min(cap - self.len);
        for i in 0..n {
            self.buf[(self.head + self.len + i) % cap] = data[i];
        }
        self.len += n;
        n
    }

    fn read(&mut self, out: &mut [u8]) -> usize {
        let cap = self.buf.len();
        let n = out.len().min(self.len);
        for i in 0..n {
            out[i] = self.buf[(self.head + i) % cap];
        }
        self.head = (self.head + n) % cap;
        self.len -= n;
        n
    }
}

// ─── PtyInner ────────────────────────────────────────────────────────────

pub struct PtyInner {
    pub id: usize,
    pub locked: AtomicBool,
    pub master_closed: AtomicBool,
    pub slave_open_count: AtomicUsize,
    pub termios: Mutex<Termios>,
    pub winsize: Mutex<WinSize>,
    pub foreground_pgid: Mutex<u32>,
    pub master_to_slave: Mutex<RingBuffer>,
    pub slave_to_master: Mutex<RingBuffer>,
    pub slave_read_waiters: EventWaitQueue,
    pub master_read_waiters: EventWaitQueue,
    pub slave_write_waiters: EventWaitQueue,
    pub master_write_waiters: EventWaitQueue,
}

impl PtyInner {
    fn new(id: usize) -> Arc<Self> {
        Arc::new(PtyInner {
            id,
            locked: AtomicBool::new(true),
            master_closed: AtomicBool::new(false),
            slave_open_count: AtomicUsize::new(0),
            termios: Mutex::new(Termios::default()),
            winsize: Mutex::new(WinSize::default()),
            foreground_pgid: Mutex::new(0),
            master_to_slave: Mutex::new(RingBuffer::new(PTY_BUF_SIZE)),
            slave_to_master: Mutex::new(RingBuffer::new(PTY_BUF_SIZE)),
            slave_read_waiters: EventWaitQueue::new(),
            master_read_waiters: EventWaitQueue::new(),
            slave_write_waiters: EventWaitQueue::new(),
            master_write_waiters: EventWaitQueue::new(),
        })
    }
}

impl core::fmt::Debug for PtyInner {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PtyInner").field("id", &self.id).finish()
    }
}

// ─── PtyManager ──────────────────────────────────────────────────────────

struct PtyManager {
    next_id: AtomicUsize,
    pairs: Mutex<BTreeMap<usize, Weak<PtyInner>>>,
}

impl PtyManager {
    fn new() -> Self {
        PtyManager {
            next_id: AtomicUsize::new(0),
            pairs: Mutex::new(BTreeMap::new()),
        }
    }

    fn create_pair(&self) -> (Arc<PtyInner>, String) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let inner = PtyInner::new(id);
        let path = alloc::format!("/dev/pts/{}", id);
        let mut pairs = self.pairs.lock();
        pairs.retain(|_, w| w.strong_count() > 0);
        pairs.insert(id, Arc::downgrade(&inner));
        (inner, path)
    }

    fn get_slave(&self, id: usize) -> Result<Arc<PtySlave>, SyscallErr> {
        let mut pairs = self.pairs.lock();
        pairs.retain(|_, w| w.strong_count() > 0);
        match pairs.get(&id).and_then(|w| w.upgrade()) {
            Some(inner) => Ok(Arc::new(PtySlave {
                inner,
                uid: AtomicU32::new(0),
            })),
            None => {
                pairs.remove(&id);
                Err(SyscallErr::ENOENT)
            }
        }
    }

    fn list_ids(&self) -> Vec<usize> {
        let mut pairs = self.pairs.lock();
        pairs.retain(|_, w| w.strong_count() > 0);
        pairs.keys().copied().collect()
    }
}

lazy_static! {
    static ref PTY_MANAGER: PtyManager = PtyManager::new();
}

// ─── PtySlave ────────────────────────────────────────────────────────────

pub struct PtySlave {
    inner: Arc<PtyInner>,
    uid: AtomicU32,
}

impl PtySlave {
    fn do_read(&self, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        if self.inner.master_closed.load(Ordering::Acquire) {
            let n = { self.inner.master_to_slave.lock().read(buf) };
            if n > 0 {
                return Ok(n);
            }
            return Ok(0);
        }
        let n = { self.inner.master_to_slave.lock().read(buf) };
        if n > 0 {
            self.inner
                .master_write_waiters
                .notify_events_at_most(EPollEvent::EPOLLOUT | EPollEvent::EPOLLWRNORM, 1);
        }
        Ok(n)
    }

    fn do_write(&self, buf: &[u8]) -> Result<usize, SyscallErr> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.inner.master_closed.load(Ordering::Acquire) {
            return Err(SyscallErr::EIO);
        }
        let onlcr = self.inner.termios.lock().oflag & ONLCR != 0;

        let mut rb = self.inner.slave_to_master.lock();
        let written = if onlcr {
            let mut w = 0;
            for &b in buf {
                let need = if b == b'\n' { 2 } else { 1 };
                if rb.free_space() < need {
                    break;
                }
                if b == b'\n' {
                    rb.write(b"\r\n");
                } else {
                    rb.write(&[b]);
                }
                w += 1;
            }
            w
        } else {
            rb.write(buf)
        };
        drop(rb);

        if written > 0 {
            self.inner
                .master_read_waiters
                .notify_events_at_most(EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM, 1);
        }
        Ok(written)
    }
}

impl core::fmt::Debug for PtySlave {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PtySlave")
            .field("id", &self.inner.id)
            .finish()
    }
}

impl IndexNode for PtySlave {
    fn read_at(
        &self,
        _offset: usize,
        _len: usize,
        buf: &mut [u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        self.do_read(buf)
    }

    fn write_at(
        &self,
        _offset: usize,
        _len: usize,
        buf: &[u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        self.do_write(buf)
    }

    fn open(
        &self,
        _data: MutexGuard<FilePrivateData>,
        _flags: &FileFlags,
    ) -> Result<(), SyscallErr> {
        if self.inner.locked.load(Ordering::Acquire) {
            return Err(SyscallErr::EIO);
        }
        if self.inner.master_closed.load(Ordering::Acquire) {
            return Err(SyscallErr::EIO);
        }
        let count = self.inner.slave_open_count.fetch_add(1, Ordering::AcqRel);
        // Set uid on first open
        if count == 0 {
            if let Some(task) = crate::task::current_task() {
                let uid = task.acquire_inner_lock().euid;
                let _ = self
                    .uid
                    .compare_exchange(0, uid, Ordering::Relaxed, Ordering::Relaxed);
            }
        }
        Ok(())
    }

    fn close(&self, _data: MutexGuard<FilePrivateData>) -> Result<(), SyscallErr> {
        let prev = self.inner.slave_open_count.fetch_sub(1, Ordering::AcqRel);
        if prev == 1 {
            info!("[pty] slave pty{} last close, waking master", self.inner.id);
            self.inner
                .master_read_waiters
                .notify_events_all(EPollEvent::EPOLLIN | EPollEvent::EPOLLHUP);
            self.inner
                .master_write_waiters
                .notify_events_all(EPollEvent::EPOLLHUP);
        }
        Ok(())
    }

    fn metadata(&self) -> Result<Metadata, SyscallErr> {
        Ok(Metadata {
            dev_id: 0,
            inode_id: self.inner.id as InodeId + 1000,
            size: 0,
            blk_size: 0,
            blocks: 0,
            atime: TimeSpec::new(),
            mtime: TimeSpec::new(),
            ctime: TimeSpec::new(),
            file_type: FileType::CharDevice,
            mode: InodeMode::S_IFCHR | InodeMode::from_bits_truncate(0o620),
            nlinks: 1,
            uid: self.uid.load(Ordering::Relaxed),
            gid: 0,
            flags: InodeFlags::empty(),
            raw_dev: 0,
        })
    }

    fn is_stream(&self) -> bool {
        true
    }

    fn poll(&self, _private_data: &FilePrivateData) -> Result<usize, SyscallErr> {
        let mut events = EPollEvent::empty();
        if self.inner.master_to_slave.lock().available() > 0 {
            events |= EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM;
        }
        if self.inner.master_closed.load(Ordering::Acquire) {
            events |= EPollEvent::EPOLLHUP;
        }
        if self.inner.slave_to_master.lock().free_space() > 0 {
            events |= EPollEvent::EPOLLOUT | EPollEvent::EPOLLWRNORM;
        }
        Ok(events.bits())
    }

    fn ioctl(
        &self,
        cmd: u32,
        argp: usize,
        _private_data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        let token = crate::task::current_user_token();
        let inner = &self.inner;
        let b = iocbase(cmd);

        match b {
            TCGETS | TCGETA => {
                let t = inner.termios.lock();
                crate::mm::UserPtrMut::from_addr(argp)
                    .write(token, &*t)
                    .map_err(|_| SyscallErr::EFAULT)?;
                Ok(0)
            }
            TCSETS | TCSETSW | TCSETSF | TCSETA | TCSETAW | TCSETAF => {
                let new_t: Termios = crate::mm::UserPtr::from_addr(argp)
                    .read(token)
                    .map_err(|_| SyscallErr::EFAULT)?;
                if b == TCSETSF || b == TCSETAF {
                    inner.master_to_slave.lock().clear();
                }
                *inner.termios.lock() = new_t;
                Ok(0)
            }
            TIOCGWINSZ => {
                let ws = inner.winsize.lock();
                crate::mm::UserPtrMut::from_addr(argp)
                    .write(token, &*ws)
                    .map_err(|_| SyscallErr::EFAULT)?;
                Ok(0)
            }
            TIOCSWINSZ => {
                let ws: WinSize = crate::mm::UserPtr::from_addr(argp)
                    .read(token)
                    .map_err(|_| SyscallErr::EFAULT)?;
                *inner.winsize.lock() = ws;
                Ok(0)
            }
            TIOCGPGRP => {
                let pg = *inner.foreground_pgid.lock();
                crate::mm::UserPtrMut::from_addr(argp)
                    .write(token, &pg)
                    .map_err(|_| SyscallErr::EFAULT)?;
                Ok(0)
            }
            TIOCSPGRP => {
                let pg: u32 = crate::mm::UserPtr::from_addr(argp)
                    .read(token)
                    .map_err(|_| SyscallErr::EFAULT)?;
                *inner.foreground_pgid.lock() = pg;
                Ok(0)
            }
            TCXONC | TCSBRK | TCSBRKP | FIONBIO => Ok(0),
            FIONREAD => {
                let n = inner.master_to_slave.lock().available() as i32;
                crate::mm::UserPtrMut::from_addr(argp)
                    .write(token, &n)
                    .map_err(|_| SyscallErr::EFAULT)?;
                Ok(0)
            }
            TIOCSETD => {
                let disc: i32 = crate::mm::UserPtr::from_addr(argp)
                    .read(token)
                    .map_err(|_| SyscallErr::EFAULT)?;
                if disc == 0 {
                    Ok(0)
                } else {
                    Err(SyscallErr::EINVAL)
                }
            }
            TIOCGETD => {
                crate::mm::UserPtrMut::from_addr(argp)
                    .write(token, &0i32)
                    .map_err(|_| SyscallErr::EFAULT)?;
                Ok(0)
            }
            TIOCGPTN | TIOCSPTLCK | TIOCGPTLCK | TIOCGPTPEER | TIOCVHANGUP => {
                Err(SyscallErr::ENOTTY)
            }
            _ => {
                warn!("[pty-slave] unknown ioctl {:#X} (base {:#X})", cmd, b);
                Err(SyscallErr::ENOTTY)
            }
        }
    }

    fn read_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        Some(self.inner.slave_read_waiters.wait_queue())
    }

    fn read_event_queue(&self) -> Option<&EventWaitQueue> {
        Some(&self.inner.slave_read_waiters)
    }

    fn write_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        Some(self.inner.slave_write_waiters.wait_queue())
    }

    fn write_event_queue(&self) -> Option<&EventWaitQueue> {
        Some(&self.inner.slave_write_waiters)
    }

    fn fs(&self) -> Arc<dyn NewFileSystem> {
        DEV_FS.clone()
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}

// ─── PtmxMasterInode ─────────────────────────────────────────────────────

pub struct PtmxMasterInode;

impl PtmxMasterInode {
    fn extract_inner<'a>(
        data: &'a MutexGuard<FilePrivateData>,
    ) -> Result<&'a Arc<PtyInner>, SyscallErr> {
        match &**data {
            FilePrivateData::PtyMaster { inner } => Ok(inner),
            _ => Err(SyscallErr::EIO),
        }
    }

    fn master_read(
        &self,
        buf: &mut [u8],
        data: &MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        let inner = Self::extract_inner(data)?;
        if inner.master_closed.load(Ordering::Acquire) {
            return Ok(0);
        }
        let n = { inner.slave_to_master.lock().read(buf) };
        if n > 0 {
            inner
                .slave_write_waiters
                .notify_events_at_most(EPollEvent::EPOLLOUT | EPollEvent::EPOLLWRNORM, 1);
        }
        Ok(n)
    }

    fn master_write(
        &self,
        buf: &[u8],
        data: &MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        let inner = Self::extract_inner(data)?;
        if buf.is_empty() {
            return Ok(0);
        }
        if inner.master_closed.load(Ordering::Acquire) {
            return Err(SyscallErr::EIO);
        }
        let n = { inner.master_to_slave.lock().write(buf) };
        if n > 0 {
            inner
                .slave_read_waiters
                .notify_events_at_most(EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM, 1);
        }
        Ok(n)
    }
}

impl core::fmt::Debug for PtmxMasterInode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PtmxMasterInode").finish()
    }
}

impl IndexNode for PtmxMasterInode {
    fn read_at(
        &self,
        _offset: usize,
        _len: usize,
        buf: &mut [u8],
        data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        self.master_read(buf, &data)
    }

    fn write_at(
        &self,
        _offset: usize,
        _len: usize,
        buf: &[u8],
        data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        self.master_write(buf, &data)
    }

    fn open(
        &self,
        mut data: MutexGuard<FilePrivateData>,
        _flags: &FileFlags,
    ) -> Result<(), SyscallErr> {
        let (inner, slave_path) = PTY_MANAGER.create_pair();
        info!(
            "[ptmx] created PTY pair: master pty{} -> slave {}",
            inner.id, slave_path
        );
        *data = FilePrivateData::PtyMaster { inner };
        Ok(())
    }

    fn close(&self, data: MutexGuard<FilePrivateData>) -> Result<(), SyscallErr> {
        if let Ok(inner) = Self::extract_inner(&data) {
            info!("[ptmx] closing master pty{}", inner.id);
            if inner.slave_open_count.load(Ordering::Acquire) > 0 {
                inner.master_closed.store(true, Ordering::Release);
            }
            inner
                .slave_read_waiters
                .notify_events_all(EPollEvent::EPOLLIN | EPollEvent::EPOLLHUP);
            inner
                .slave_write_waiters
                .notify_events_all(EPollEvent::EPOLLHUP);
        }
        Ok(())
    }

    fn metadata(&self) -> Result<Metadata, SyscallErr> {
        Ok(Metadata {
            dev_id: 0,
            inode_id: 0,
            size: 0,
            blk_size: 0,
            blocks: 0,
            atime: TimeSpec::new(),
            mtime: TimeSpec::new(),
            ctime: TimeSpec::new(),
            file_type: FileType::CharDevice,
            mode: InodeMode::S_IFCHR | InodeMode::from_bits_truncate(0o666),
            nlinks: 1,
            uid: 0,
            gid: 0,
            flags: InodeFlags::empty(),
            raw_dev: crate::makedev!(0x88, 0),
        })
    }

    fn is_stream(&self) -> bool {
        true
    }

    fn poll(&self, private_data: &FilePrivateData) -> Result<usize, SyscallErr> {
        let inner = match &*private_data {
            FilePrivateData::PtyMaster { inner } => inner,
            _ => return Err(SyscallErr::EIO),
        };
        let mut events = EPollEvent::empty();
        if inner.slave_to_master.lock().available() > 0 {
            events |= EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM;
        }
        if inner.master_closed.load(Ordering::Acquire) {
            events |= EPollEvent::EPOLLHUP;
        }
        if inner.master_to_slave.lock().free_space() > 0 {
            events |= EPollEvent::EPOLLOUT | EPollEvent::EPOLLWRNORM;
        }
        Ok(events.bits())
    }

    fn ioctl(
        &self,
        cmd: u32,
        argp: usize,
        data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        let inner = Self::extract_inner(&data)?;
        let token = crate::task::current_user_token();
        let b = iocbase(cmd);

        match b {
            TIOCGPTN => {
                let n = inner.id as u32;
                crate::mm::UserPtrMut::from_addr(argp)
                    .write(token, &n)
                    .map_err(|_| SyscallErr::EFAULT)?;
                Ok(0)
            }
            TIOCSPTLCK => {
                let val: i32 = crate::mm::UserPtr::from_addr(argp)
                    .read(token)
                    .map_err(|_| SyscallErr::EFAULT)?;
                inner.locked.store(val != 0, Ordering::Release);
                info!("[ptmx] pty{} slave lock: {}", inner.id, val != 0);
                Ok(0)
            }
            TIOCGPTLCK => {
                let locked: i32 = if inner.locked.load(Ordering::Acquire) {
                    1
                } else {
                    0
                };
                crate::mm::UserPtrMut::from_addr(argp)
                    .write(token, &locked)
                    .map_err(|_| SyscallErr::EFAULT)?;
                Ok(0)
            }
            TIOCGPTPEER => Err(SyscallErr::ENOTTY),

            TCGETS | TCGETA => {
                let t = inner.termios.lock();
                crate::mm::UserPtrMut::from_addr(argp)
                    .write(token, &*t)
                    .map_err(|_| SyscallErr::EFAULT)?;
                Ok(0)
            }
            TCSETS | TCSETSW | TCSETSF | TCSETA | TCSETAW | TCSETAF => {
                let new_t: Termios = crate::mm::UserPtr::from_addr(argp)
                    .read(token)
                    .map_err(|_| SyscallErr::EFAULT)?;
                if b == TCSETSF || b == TCSETAF {
                    inner.master_to_slave.lock().clear();
                }
                *inner.termios.lock() = new_t;
                Ok(0)
            }
            TIOCGWINSZ => {
                let ws = inner.winsize.lock();
                crate::mm::UserPtrMut::from_addr(argp)
                    .write(token, &*ws)
                    .map_err(|_| SyscallErr::EFAULT)?;
                Ok(0)
            }
            TIOCSWINSZ => {
                let ws: WinSize = crate::mm::UserPtr::from_addr(argp)
                    .read(token)
                    .map_err(|_| SyscallErr::EFAULT)?;
                *inner.winsize.lock() = ws;
                Ok(0)
            }
            TIOCGPGRP => {
                let pg = *inner.foreground_pgid.lock();
                crate::mm::UserPtrMut::from_addr(argp)
                    .write(token, &pg)
                    .map_err(|_| SyscallErr::EFAULT)?;
                Ok(0)
            }
            TIOCSPGRP => {
                let pg: u32 = crate::mm::UserPtr::from_addr(argp)
                    .read(token)
                    .map_err(|_| SyscallErr::EFAULT)?;
                *inner.foreground_pgid.lock() = pg;
                Ok(0)
            }
            TCXONC | TCSBRK | TCSBRKP | FIONBIO => Ok(0),
            FIONREAD => {
                let n = inner.slave_to_master.lock().available() as i32;
                crate::mm::UserPtrMut::from_addr(argp)
                    .write(token, &n)
                    .map_err(|_| SyscallErr::EFAULT)?;
                Ok(0)
            }
            TIOCSETD => {
                let disc: i32 = crate::mm::UserPtr::from_addr(argp)
                    .read(token)
                    .map_err(|_| SyscallErr::EFAULT)?;
                if disc == 0 {
                    Ok(0)
                } else {
                    Err(SyscallErr::EINVAL)
                }
            }
            TIOCGETD => {
                crate::mm::UserPtrMut::from_addr(argp)
                    .write(token, &0i32)
                    .map_err(|_| SyscallErr::EFAULT)?;
                Ok(0)
            }
            TIOCVHANGUP => Err(SyscallErr::ENOTTY),
            _ => {
                warn!("[ptmx] unknown ioctl {:#X} (base {:#X})", cmd, b);
                Err(SyscallErr::ENOTTY)
            }
        }
    }

    fn read_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        None
    }
    fn write_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        None
    }

    fn fs(&self) -> Arc<dyn NewFileSystem> {
        DEV_FS.clone()
    }
    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}

// ─── PtsDirInode ─────────────────────────────────────────────────────────

pub struct PtsDirInode;

impl core::fmt::Debug for PtsDirInode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PtsDirInode").finish()
    }
}

impl IndexNode for PtsDirInode {
    fn find(&self, name: &str) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        let id: usize = name.parse().map_err(|_| SyscallErr::ENOENT)?;
        let slave = PTY_MANAGER.get_slave(id)?;
        Ok(slave as Arc<dyn IndexNode>)
    }

    fn list(&self) -> Result<Vec<String>, SyscallErr> {
        Ok(PTY_MANAGER
            .list_ids()
            .into_iter()
            .map(|id| alloc::format!("{}", id))
            .collect())
    }

    fn list_dirents(&self) -> Result<Vec<(String, InodeId, FileType)>, SyscallErr> {
        Ok(PTY_MANAGER
            .list_ids()
            .into_iter()
            .map(|id| {
                (
                    alloc::format!("{}", id),
                    id as InodeId + 1000,
                    FileType::CharDevice,
                )
            })
            .collect())
    }

    fn open(
        &self,
        _data: MutexGuard<FilePrivateData>,
        _flags: &FileFlags,
    ) -> Result<(), SyscallErr> {
        Ok(())
    }
    fn close(&self, _data: MutexGuard<FilePrivateData>) -> Result<(), SyscallErr> {
        Ok(())
    }

    fn metadata(&self) -> Result<Metadata, SyscallErr> {
        Ok(Metadata {
            dev_id: 0,
            inode_id: 999,
            size: 0,
            blk_size: 0,
            blocks: 0,
            atime: TimeSpec::new(),
            mtime: TimeSpec::new(),
            ctime: TimeSpec::new(),
            file_type: FileType::Dir,
            mode: InodeMode::S_IFDIR | InodeMode::from_bits_truncate(0o755),
            nlinks: 2,
            uid: 0,
            gid: 0,
            flags: InodeFlags::empty(),
            raw_dev: 0,
        })
    }

    fn fs(&self) -> Arc<dyn NewFileSystem> {
        DEV_FS.clone()
    }
    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}
