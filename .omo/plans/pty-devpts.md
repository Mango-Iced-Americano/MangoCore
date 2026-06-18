# PTY/devpts 实现计划

## 概述

为 MangoCore 增加最小可用的 pseudo terminal 支持。新增文件 `os/src/fs/dev/pty.rs`，修改 `os/src/fs/dev/mod.rs` 和 `os/src/fs/mod.rs`（注册端点），不修改 `tty.rs`。

## 整体架构

```
open("/dev/ptmx") ──→ PtyMaster (Arc<PtyInner>)
                       │
                       ├── master_to_slave_rb (RingBuffer)
                       ├── slave_to_master_rb (RingBuffer)
                       ├── termios: Termios
                       ├── winsize: WinSize
                       ├── locked: bool
                       ├── master_closed: bool
                       ├── slave_open_count: usize
                       ├── read_waiters (EventWaitQueue)  ← slave read wait
                       ├── write_waiters (EventWaitQueue)  ← master read wait
                       └── id: usize

open("/dev/pts/N") ──→ PtySlave (Arc<PtyInner>, same inner!)
```

PtyManager: 全局 BTreeMap<usize, Weak<PtyInner>> + 原子计数器分配 id。

## Phase 1: 核心数据结构 (`os/src/fs/dev/pty.rs`)

### 1.1 PtyInner

```rust
pub struct PtyInner {
    pub id: usize,
    pub locked: bool,
    pub master_closed: bool,
    pub slave_open_count: usize,
    pub termios: Termios,
    pub winsize: WinSize,
    pub foreground_pgid: u32,
    pub master_to_slave: RingBuffer,  // writer=master, reader=slave
    pub slave_to_master: RingBuffer,  // writer=slave, reader=master
    pub read_waiters: EventWaitQueue,   // for slave read / master read
    pub write_waiters: EventWaitQueue,  // for slave write / master write (buffer full)
}
```

RingBuffer: 直接用 `Vec<u8>` + head/tail 索引，容量 4096 字节（对标 Linux `N_TTY_BUF_SIZE`）。

```rust
struct RingBuffer {
    buf: Vec<u8>,
    head: usize, // next byte to read
    tail: usize, // next free slot (tail == head -> empty, (tail + 1) % cap == head -> full)
    capacity: usize,
}
```

方法: `write(&[u8]) -> usize`, `read(&mut [u8]) -> usize`, `available() -> usize`, `free_space() -> usize`, `is_empty() -> bool`, `is_full() -> bool`, `clear()`。

### 1.2 PtyManager

全局单例，管理所有 PTY 对：

```rust
static PTY_MANAGER: Lazy<Mutex<PtyManager>> = ...;

struct PtyManager {
    next_id: AtomicUsize,
    pairs: BTreeMap<usize, Weak<PtyInner>>,
}
```

- `create_pair() -> (Arc<PtyMaster>, String)`：分配 id，创建 PtyInner，注册到 pairs，返回 PtyMaster + slave 路径 "/dev/pts/N"。
- `get_slave(id: usize) -> Result<Arc<PtySlave>>`：从 pairs 查找 PtyInner，升级 Weak，构造 PtySlave。
- 定期清理 Weak 已失效的条目（在 create_pair 或 get_slave 时惰性清理）。

### 1.3 PtyMaster (实现 IndexNode)

```rust
pub struct PtyMaster {
    inner: Arc<PtyInner>,
}
```

**关键区别**: PtyMaster 和 PtySlave **共享同一个 `Arc<PtyInner>`**，所以 termios/winsize/锁定状态天然共享。

**read_at**: 从 `slave_to_master` ring buffer 读取。如果空且非阻塞→EAGAIN；空且阻塞→wait。然后执行换行转换（如果 ONLCR 开启，`\n` → `\r\n` 在输出处理中做，见下）。

**write_at**: 写入 `master_to_slave` ring buffer。如果满且非阻塞→EAGAIN；满且阻塞→wait。

**poll**: 检查 slave_to_master 有数据 → EPOLLIN；master_to_slave 有空间 → EPOLLOUT；master_closed → EPOLLHUP。

**ioctl**:
- TCGETS/TCGETA → 返回 inner.termios
- TCSETS/TCSETA → 立即设置
- TCSETSW/TCSETAW → 等同于 TCSETS（无输出缓冲区）
- TCSETSF/TCSETAF → 等同于 TCSETS（无输入缓冲区，但可以 flush master_to_slave）
- TIOCGWINSZ → 返回 inner.winsize
- TIOCSWINSZ → 设置 inner.winsize（同时通知 slave 端如果有的话）
- TIOCGPTN → 返回 inner.id
- TIOCSPTLCK → 设置/清除 inner.locked
- TIOCGPTLCK → 返回 inner.locked
- TIOCGPTPEER → ENOTTY（第一版不实现）
- TIOCGPGRP → 返回 inner.foreground_pgid
- TIOCSPGRP → 设置 inner.foreground_pgid
- TCXONC → no-op 成功
- TCSBRK/TCSBRKP → no-op 成功
- FIONREAD → 返回 slave_to_master.available()
- 其他 → ENOTTY

**metadata**: FileType::CharDevice, mode = S_IFCHR | 0600, uid=0, gid=0 (第一版简单处理)。

**open**: `master_closed` 设为 false。

**close**: `master_closed` 设为 true，通知 slave 端（wake read/write waiters，使其感知 HUP）。

**is_stream**: true。

### 1.4 PtySlave (实现 IndexNode)

```rust
pub struct PtySlave {
    inner: Arc<PtyInner>,
}
```

**read_at**: 从 `master_to_slave` ring buffer 读取。与 master 读写方向相反。同样处理空/非阻塞/阻塞。

**write_at**: 写入 `slave_to_master` ring buffer。**重要**: 需要做 termios 输出处理：
- 如果 ONLCR 设置且 output char 是 `\n`，写入 `\r\n` 两个字节
- 否则直接写入原始字节

同样处理满/非阻塞/阻塞。

**poll**: 检查 master_to_slave 有数据 → EPOLLIN；slave_to_master 有空间 → EPOLLOUT；master_closed → EPOLLHUP。

**ioctl**: 与 PtyMaster 完全相同（共享 PtyInner）：
- TCGETS/TCGETA → inner.termios
- TCSETS/TCSETA/TCSETSW/TCSETSF/TCSETAW/TCSETAF → 设置 inner.termios
- TIOCGWINSZ → inner.winsize
- TIOCSWINSZ → 设置 inner.winsize
- TIOCGPGRP → inner.foreground_pgid
- TIOCSPGRP → inner.foreground_pgid
- TCXONC → no-op 成功
- TCSBRK/TCSBRKP → no-op 成功
- TIOCGPTN → ENOTTY（只有 master 支持）
- TIOCSPTLCK → ENOTTY
- FIONREAD → master_to_slave.available()
- 其他 → ENOTTY

**metadata**: FileType::CharDevice, mode = S_IFCHR | 0620, uid=调用进程 uid, gid=0。
- uid 设置：在 `open()` 时记录当前 task 的 uid，用于 metadata。
- 如果 DevFS `metadata()` 无法动态设置 uid，可以在 PtySlave 中存储当前 uid。

**open**: 检查 `inner.locked`。如果 locked → EIO（匹配 Linux pty_open 行为）。否则 `slave_open_count += 1`。如果 master_closed → EIO。

**close**: `slave_open_count -= 1`。如果变为 0，通知 master 端（wake read/write waiters，方便 master 检测 EOF）。

**is_stream**: true。

## Phase 2: DevFS 注册 (`os/src/fs/dev/mod.rs` + `os/src/fs/mod.rs`)

### 2.1 注册 /dev/ptmx

在 `mount_common_filesystems()` 中：
```rust
devfs.add_dev("ptmx", alloc::sync::Arc::new(
    crate::fs::dev::pty::PtmxMaster
) as Arc<dyn IndexNode>);
```

需要一个新的 IndexNode: `PtmxMaster`（clone device），实现：

```rust
pub struct PtmxMaster;

impl IndexNode for PtmxMaster {
    fn open(&self, _data: ..., _flags: &FileFlags) -> Result<(), SyscallErr> {
        // 调用 PtyManager::create_pair()
        // 将 PtyMaster 替换到当前 fd 的 inode 中
        // 方案: 利用 FilePrivateData 存储 PtyMaster Arc
        Ok(())
    }
    
    fn read_at(...) -> Err(ENOSYS)
    fn write_at(...) -> Err(ENOSYS)
    // ...
    fn metadata() -> Metadata { /* char device, 0666 */ }
    fn as_any_ref() -> &dyn Any { self }
    fn fs() -> Arc<dyn FileSystem> { DEV_FS.clone() }
}
```

**关键设计**: `PtmxMaster::open()` 不能简单返回。实际上需要让后续的 read/write/ioctl 操作路由到新创建的 PtyMaster。

**实现方案 A（推荐）**: 在 `PtmxMaster::open()` 中创建 PtyMaster，将 PtyMaster 的 Arc 存储到 `FilePrivateData` 中。然后在 `PtmxMaster` 的 `read_at/write_at/ioctl` 中根据 `FilePrivateData` 获取真正的 PtyMaster 并委托给它。

实际上：`PtmxMaster` 的 `open()` 创建一对 PTY → PtyMaster 节点需要被创建。更简洁的做法是：

**实现方案 B（更简洁）**: `PtmxMaster` 本身不实现 I/O。它的 `open()` 是在 open syscall 路径中被调用的，但这个 open 应该**替换 fd 的 inode**。这在当前架构下比较困难。

**实现方案 C（最简单可行）**: PtyMaster **本身就是** `PtmxMaster`。即只有一个结构体，`open()` 是创建操作。我们将 PtyMaster 的 IndexNode 实现作为 ptmx 的 inode，但是 `open()` 方法会：
1. 调用 PtyManager::create_pair()
2. 将创建的 PtyInner Arc 存入 self（或通过内部 Mutex）

实际上最简单的设计：**让 PtyMaster 本身同时充当 ptmx clone device**。

```rust
pub struct PtyMaster {
    inner: Arc<Mutex<Option<Arc<PtyInner>>>>,
}

impl IndexNode for PtyMaster {
    fn open(&self, _data: ..., _flags: &FileFlags) -> Result<(), SyscallErr> {
        let (inner, slave_path) = PtyManager::create_pair();
        *self.inner.lock() = Some(inner);
        Ok(())
    }
    
    // 所有 read_at/write_at/ioctl/poll 都从 inner 获取真正的 PtyInner
}
```

但这有个问题：多个 fd 打开同一个 /dev/ptmx 时共享同一个 PtyMaster inode，第二个 open 会覆盖第一个 PTY pair。

**实现方案 D（最佳）**: 让 `PtmxMaster` 是一个单独的 "clone device" inode，它的 `open()` 创建 PtyMaster 并**返回**。问题在于 IndexNode::open() 不返回新的 IndexNode，只返回 `Result<()>`。

检查代码: `File::new()` 调用 `inode.open(private_data, &flags)`。File 持有原始 inode 的 Arc。所以 PtmxMaster::open() 需要某种机制来替换 File 中的 inode 引用。

实际上最简单的 hack：让 `PtmxMaster` 拥有一个 `Mutex<Option<Arc<dyn IndexNode>>>` 来存储当前活动的 PTY。当 `open()` 被调用时，创建新的 PtyMaster 节点并存入。后续所有 read/write 委托给当前活动的节点。

但这对于并发 open 不对——两个 fd 同时打开 /dev/ptmx 会冲突。

**实现方案 E（最终推荐——clone device 模式）**:

在 syscall 层处理 `/dev/ptmx` 的特殊性。在 `sys_openat` 中检测打开的 inode 是否为 PtmxMaster，如果是，创建 PtyMaster 并替换 File 的 inode。

但这样侵入性太大。

**实现方案 F（实际可行）**: 不用 clone device 模式。改为：

1. `/dev/ptmx` 是一个普通的字符设备，PtmxMasterInode 实现 IndexNode。
2. `PtmxMasterInode::open()` 创建 PtyInner，但不替换自身。
3. `PtmxMasterInode::read_at/write_at/ioctl/poll` **都使用 `FilePrivateData`** 来获取 PtyInner。
4. 在 `File::new()` 中，`inode.open()` 被调用，此时 private_data 还是默认值。
5. 需要在 `open()` 中将 PtyInner 存入 private_data。

检查 `FilePrivateData` 枚举：只有 `Unused`、`Memfd`、`Pipe`、`SocketCreate`。

可以添加：`Pty { inner: Arc<PtyInner> }`。

在 `open()` 中：
```rust
fn open(&self, mut data: MutexGuard<FilePrivateData>, _flags: &FileFlags) -> Result<(), SyscallErr> {
    let (inner, _) = PtyManager::create_pair();
    *data = FilePrivateData::Pty { inner };
    Ok(())
}
```

所有 read_at/write_at/ioctl/poll 从 `data` (FilePrivateData) 中提取 PtyInner 并操作。

**这完全可行！**FilePrivateData 是 File 的 per-fd 状态，每个 fd 有独立的 private_data。PtmxMasterInode 本身只是一个容器，所有操作通过 private_data 路由。

### 2.2 注册 /dev/pts/ 目录

需要一个特殊的 inode 来处理 `/dev/pts/N` 的动态查找。

方案：创建一个 `PtsDirInode` 作为 `/dev/pts` 目录。它的 `find()` 方法解析数字 N 并从 PtyManager 查找对应 PtyInner，返回 PtySlave。

```rust
struct PtsDirInode;

impl IndexNode for PtsDirInode {
    fn find(&self, name: &str) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        let id: usize = name.parse().map_err(|_| SyscallErr::ENOENT)?;
        PtyManager::get_slave(id)
    }
    
    fn metadata() -> Result<Metadata, SyscallErr> {
        // Dir, mode 0755
    }
    
    fn list() -> Result<Vec<String>, SyscallErr> {
        PtyManager::list_pts()  // 列出所有活跃的 PTY id
    }
    
    fn open(...) -> Ok(())  // no-op
    fn fs() -> Arc<dyn FileSystem> { DEV_FS.clone() }
    fn as_any_ref() -> &dyn Any { self }
}
```

在 `mount_common_filesystems()` 中：
```rust
devfs.add_dir("pts", InodeMode::from_bits_truncate(0o755))
    .expect("devfs: failed to create /dev/pts");
// 然后用自定义 IndexNode 替换 /dev/pts 目录
// DevFS 的 LockedDevFSInode 支持 add_dev，但不支持替换
// 需要扩展 DevFS 支持自定义子目录 inode
```

问题：`DevFS::add_dir()` 返回 `Arc<LockedDevFSInode>`，它实现了 IndexNode（通过 BTreeMap 查找子节点），但我们需要 `PtsDirInode` 的 find() 做动态查找。

方案：
1. **扩展 LockedDevFSInode**：添加一个 `fallback: Option<Arc<dyn IndexNode>>` 字段，在 `find()` 中如果 children 未命中则委托给 fallback。
2. **更简单**: 让 `PtsDirInode` 内部维护 PtyManager 查询，直接将 PtsDirInode 作为 /dev/pts 的 inode 插入 DevFS children map。

实际上 `devfs.add_dev("pts", arc_pstdir)` 就可以，因为 `add_dev` 接受 `Arc<dyn IndexNode>`。`PtsDirInode` 实现 IndexNode（包括 `find()`），返回 FileType::Dir 的 metadata，就是合法的目录节点。

### 2.3 slave 节点 metadata 的 uid

LTP `common.h` 检查：
```c
// stat sb for /dev/pts/N
// sb.st_uid == getuid()
// sb.st_mode == S_IFCHR | 0600 or 0620
```

PtySlave 的 metadata 需要设置 uid 为打开它的进程的 uid。

在 PtySlave 中维护一个 `uid: AtomicU32`，在 `open()` 中：
```rust
fn open(&self, _data: ..., _flags: &FileFlags) -> Result<(), SyscallErr> {
    if self.inner.locked {
        return Err(SyscallErr::EACCES);
    }
    // 设置 uid 为当前进程 uid（仅在首次 open 时？）
    self.uid.compare_exchange(0, current_uid, ...);
    self.inner.slave_open_count += 1;
    Ok(())
}
```

实际上 Linux 的 devpts 行为更复杂（涉及 mount options `gid=`, `mode=`, `ptmxmode=` 等），第一版可以简单处理：
- PtySlave metadata: mode = S_IFCHR | 0620, uid = 当前进程 euid, gid = 0
- 如果 LTP 检查 mode 为 0600 失败，改为 0600

## Phase 3: RingBuffer 实现

```rust
const PTY_BUF_SIZE: usize = 4096;

struct RingBuffer {
    buf: Vec<u8>,
    head: usize,
    tail: usize,
}
```

- `new()` → `buf = vec![0; PTY_BUF_SIZE + 1], head = 0, tail = 0`
- 容量 = PTY_BUF_SIZE（一个字节用作区分空/满的哨兵）
- `write(data: &[u8]) -> usize`：最大写入 free_space() 字节
- `read(buf: &mut [u8]) -> usize`：最大读取 available() 字节
- `available()`: `(tail - head + cap) % cap`
- `free_space()`: `cap - 1 - available()`
- `clear()`: head = tail = 0

## Phase 4: read/write 阻塞语义

```rust
// Master read (从 slave_to_master 读)
fn read_at(&self, _offset: usize, _len: usize, buf: &mut [u8], data: MutexGuard<FilePrivateData>) -> Result<usize, SyscallErr> {
    let inner = get_inner(&data);
    if inner.master_closed {
        return Ok(0); // EOF
    }
    
    // 检查 master_to_slave 是否也需要考虑？不需要，master read 是读 slave 写入的数据
    let available = inner.slave_to_master.available();
    if available > 0 {
        let n = inner.slave_to_master.read(buf);
        inner.write_waiters.notify_events_at_most_if_unlocked(EPollEvent::EPOLLOUT, 1);
        return Ok(n);
    }
    
    // No data
    if is_nonblock(data) {
        return Err(SyscallErr::EAGAIN);
    }
    
    // Blocking wait
    loop {
        // check signals
        let wq = inner.read_waiters.wait_queue();
        WaitQueue::wait_until_interruptible(wq, || {
            inner.slave_to_master.available() > 0 || inner.master_closed
        });
        
        if inner.master_closed {
            return Ok(0);
        }
        let available = inner.slave_to_master.available();
        if available > 0 {
            let n = inner.slave_to_master.read(buf);
            inner.write_waiters.notify_events_at_most_if_unlocked(EPollEvent::EPOLLOUT, 1);
            return Ok(n);
        }
        // interrupted by signal? return EINTR
    }
}
```

类似的，master write、slave read、slave write 也遵循类似模式，只是方向和 buffer 不同。

**换行转换 (ONLCR)**:
在 slave write（写入 slave_to_master buffer）时：
- 如果 inner.termios.oflag & ONLCR 非零：
  - 扫描输入，将 `\n` 替换为 `\r\n` 后写入 ring buffer
  - 如果 buffer 空间不足放不下展开后的内容，则返回实际写入的字节数（可能是原始数据中的 n-1 个完整字节）

简化版：先尝试写入原始数据，如果数据中包含 `\n` 且 ONLCR 开启，做逐字节展开写入。

## Phase 5: Poll 实现

```rust
// Both master and slave poll
fn poll(&self, data: &FilePrivateData) -> Result<usize, SyscallErr> {
    let inner = get_inner(data);
    let mut events = EPollEvent::empty();
    
    // 检查对端写入方向是否有数据可读
    let has_data = match self.role {
        Master => inner.slave_to_master.available() > 0,
        Slave => inner.master_to_slave.available() > 0,
    };
    if has_data {
        events |= EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM;
    }
    
    // 检查写方向是否有空间（PTY 的可写方向几乎总是就绪，除非 buffer 满）
    let has_space = match self.role {
        Master => inner.master_to_slave.free_space() > 0,
        Slave => inner.slave_to_master.free_space() > 0,
    };
    if has_space {
        events |= EPollEvent::EPOLLOUT | EPollEvent::EPOLLWRNORM;
    }
    
    // Hangup detection
    if inner.master_closed && slave_to_master.available() == 0 {
        events |= EPollEvent::EPOLLHUP;
    }
    
    Ok(events.bits())
}
```

明确区分 `PtyMaster` 和 `PtySlave` 的 poll 行为（用 role 区分）：
- Master poll: 读 = slave_to_master, 写 = master_to_slave
- Slave poll: 读 = master_to_slave, 写 = slave_to_master

## Phase 6: IOCTLs

额外需要的 TTY ioctl 常量（补充到 `pty.rs` 或复用 `tty.rs` 的）：

```rust
// IOCTL values verified against Linux include/uapi/asm-generic/ioctls.h
// _IO('T', N) = N | 0x5400 for the 'T' (0x54) prefix
const TIOCGPTN: u32 = 0x5430;         // _IOR('T', 0x30, uint) = 0x5430
const TIOCSPTLCK: u32 = 0x5431;      // _IOW('T', 0x31, int)  = 0x5431
const TIOCGPTLCK: u32 = 0x5439;      // _IOR('T', 0x39, int)  = 0x5439 (musl uses)
const TIOCGPTPEER: u32 = 0x5441;     // _IO('T', 0x41)        = 0x5441 (glibc >= 2.29)
const TIOCGPKT: u32 = 0x5438;        // _IOR('T', 0x38, int)  = 0x5438
const TIOCSIG: u32 = 0x5436;         // _IOW('T', 0x36, int)  = 0x5436
const TCSBRK: u32 = 0x5409;          // tcsendbreak (no parameter)
const TCSBRKP: u32 = 0x5425;         // tcsendbreak with duration
const TIOCPKT: u32 = 0x5420;         // set PTY packet mode
const TIOCGETD: u32 = 0x5424;         // get line discipline
const TIOCSETD: u32 = 0x5423;         // set line discipline → 对不支持discipline返回EINVAL
const TIOCVHANGUP: u32 = 0x5437;     // virtual hangup → 第一版返回ENOTTY
const FIONREAD: u32 = 0x541B;         // get bytes available
const ONLCR: u32 = 0o000004;          // oflag: map NL to CR-NL
```

**TIOCVHANGUP**: 返回 ENOTTY（第一版）。
**TIOCSETD**: 对其他 line discipline 返回 EINVAL，对 N_TTY(0) 返回成功（no-op）。

## Phase 7: grantpt / unlockpt / ptsname 支持

### grantpt(masterfd)
- glibc: 调用 `ioctl(masterfd, TIOCGPTN, &n)` 获取 PTY number，然后 `chown /dev/pts/N`, `chmod /dev/pts/N 0620`
- musl: 在 musl 中是 no-op（返回 0）
- 我们需要：`TIOCGPTN` 返回正确的 PTY number，`/dev/pts/N` 的 uid/mode 能被正确设置

### unlockpt(masterfd)
- glibc: 调用 `ioctl(masterfd, TIOCSPTLCK, &unlock_arg)` 其中 unlock_arg = 0
- 支持 `TIOCSPTLCK`：arg = 0 → unlocked, arg = 1 → locked
- 需要 `TIOCGPTLCK`（musl 可能用）

### ptsname(masterfd)
- glibc: 调用 `ioctl(masterfd, TIOCGPTN, &n)` 获取 PTY number，然后构造 `/dev/pts/N`
- 不需要特殊内核支持，只要 TIOCGPTN 工作即可

### ptsname_r(masterfd, buf, len)
- 同样依赖 TIOCGPTN

## Phase 8: Close / Hangup 行为

### Slave close
1. `PtySlave::close()`: `inner.slave_open_count -= 1`
2. 如果计数归零：wake master 的 read/write waiters
3. Master 端 poll 会检测到 `slave_open_count == 0` 并返回 EPOLLHUP
4. Master read 在 slave 关闭后返回 0 (EOF) 或 EIO

### Master close
1. `PtyMaster::close()`: `inner.master_closed = true`
2. Wake slave 的 read/write waiters
3. Slave poll 检测 `master_closed` 返回 EPOLLHUP
4. Slave read/write 返回 EIO 或 0

### hangup01 预期行为
- 父进程在 master 上 poll/read
- 子进程反复 open slave / write / close
- 不 panic、不死锁

关键：
- Slave close 时正确唤醒 master 的等待者
- 多次 open/close slave 不会造成 refcount 问题
- 并发 fork + open slave 不会竞态

## Phase 9: 与现有 /dev/tty 的隔离

- 现有 `/dev/tty` 和 `/dev/console` 保持指向同一个 `Teletype`
- PTY master/slave 是完全独立的 PtyInner
- 不引入 controlling terminal 概念
- 不修改 `tty.rs`

## Phase 10: 实现文件清单

### 新建
- `os/src/fs/dev/pty.rs` (~800行)
  - RingBuffer
  - PtyInner
  - PtyManager
  - PtyMaster (IndexNode impl)
  - PtySlave (IndexNode impl)
  - PtmxMasterInode (IndexNode impl, 用于 /dev/ptmx 注册)
  - PtsDirInode (IndexNode impl, 用于 /dev/pts/ 目录)

### 修改
- `os/src/fs/dev/mod.rs`: 添加 `pub mod pty;`
- `os/src/fs/mod.rs`: `mount_common_filesystems()` 中注册 /dev/ptmx 和 /dev/pts
- `os/src/fs/vfs/mod.rs`: `FilePrivateData` 添加 `Pty { inner: Arc<PtyInner> }` 变体
- `os/src/syscall/fs.rs`: FIONREAD 的 PTY 处理（需要在 inode 层处理，或者在 syscall 层检查是否为 PtyMaster/PtySlave）

### FIONREAD 特殊处理
当前 `sys_ioctl` 对 FIONREAD 做了硬编码处理（使用 `metadata().size - offset`）。对 PTY 这个逻辑是错误的——PTY 的 `size` 是 0，`offset` 也无意义。

**方案**: 在 FIONREAD 处理中，先尝试调用 `inode.ioctl(FIONREAD, ...)`，如果返回 ENOSYS 则回退到现有逻辑。PtyMaster 和 PtySlave 的 ioctl 实现会正确返回 ring buffer 的可读字节数。

具体修改：
```rust
// syscall/fs.rs
if cmd == FIONREAD {
    // 先让 inode 尝试处理（PTY 可以用内部 buffer 大小覆盖）
    match file.inode.ioctl(cmd, arg, file.private_data()) {
        Ok(n) => return n as isize,
        Err(SyscallErr::ENOSYS) => { /* fall through to default logic */ }
        Err(e) => return -(e as isize),
    }
    // default FIONREAD logic...
}
```

## Phase 11: 测试策略

### 阶段 A (第一轮) — 基础功能验证
1. 编译: `make rv64-kernel-build-only && make la64-kernel-build-only`
2. 配置: `ltp_include=pty01,ptem01,ptem02,ptem03,ptem04`, `ltp_runner=inline`
3. 运行: `make rv64-run` 观察 LTP 输出

验收标准:
- pty01: open /dev/ptmx → grantpt → unlockpt → ptsname → open slave → 双向读写 ✓
- ptem01: slave 上 TCGETS/TCSETS 系列成功 ✓
- ptem02: master/slave 共享 winsize ✓
- ptem03: tcsendbreak 成功 ✓
- ptem04: 连续 10 次 open /dev/pts/N ✓

### 阶段 B (第二轮) — 进阶功能
1. 配置: `ltp_include=pty01,pty02,ptem01,ptem02,ptem03,ptem04,hangup01`
2. 运行验证

验收标准:
- pty02: FIONREAD + termios 保存/读取 + 不挂死 ✓
- hangup01: master poll 收到数据 + slave close 不阻塞 + fork 不 panic ✓

### 阶段 C (最终) — 完整 suite
1. 配置: `ltp_suites=pty`, `ltp_runner=suite`
2. 运行验证

目标: pty01, ptem01-04 通过。pty02, hangup01 尽量通过。pty03/pty04 不 panic。

## 风险与注意事项

1. **RingBuffer 并发安全**: PtyInner 的 Mutex 保护所有字段。read_at/write_at 获取锁，操作 buffer，释放锁。
2. **死锁风险**: 确保 wait_until 在 lock 外部调用。模式: lock → 检查可用 → unlock → 如果不可用则 wait → 重新 lock → 检查 → 读取。
3. **FilePrivateData 生命周期**: PtyInner 通过 Arc 共享，File 的 private_data 持有 Arc，File drop 时 private_data drop，Arc refcount 减 1。PtyManager 持有 Weak，不影响生命周期。
4. **slave 并发 open**: 多个进程同时 open `/dev/pts/N` 时，需要原子增加 slave_open_count。
5. **ONLCR 输出转换**: pty01 明确要求 slave write "\n" 后 master read 读到 "\r\n"。必须在 slave write 路径实现。
6. **LTP common.h stat 检查**: `/dev/pts/N` 的 st_uid 必须等于当前进程 uid，否则测试 TBROK。在 slave open 时设置。
7. **la64 兼容性**: RingBuffer 不涉及架构特定代码，pty.rs 应该双架构通用。
8. **不破坏现有测试**: 只新增代码，不修改 tty.rs 和 console 逻辑。

## 执行顺序

1. 实现 `os/src/fs/dev/pty.rs` 完整内容
2. 修改 `FilePrivateData` 添加 Pty 变体
3. 在 `mount_common_filesystems()` 注册 /dev/ptmx 和 /dev/pts
4. 修改 `sys_ioctl` 中的 FIONREAD 处理
5. 双架构编译验证
6. QEMU LTP 测试（阶段 A）
7. 根据测试结果修复问题
8. 阶段 B 测试
9. 阶段 C 测试
