---
title: "DevFS 设备文件系统"
module: fs/dev
category: fs
status: draft
owner: "MangoCore Team"
last_updated: "2026-07-18"
code_paths:
  - "os/src/fs/dev/mod.rs"
  - "os/src/fs/dev/null.rs"
  - "os/src/fs/dev/zero.rs"
  - "os/src/fs/dev/urandom.rs"
  - "os/src/fs/dev/full.rs"
  - "os/src/fs/dev/pipe.rs"
  - "os/src/fs/dev/tty.rs"
  - "os/src/fs/dev/pty.rs"
  - "os/src/fs/dev/rtc.rs"
  - "os/src/fs/dev/block.rs"
  - "os/src/drivers/block/partition.rs"
  - "os/src/syscall/fs.rs"
  - "os/src/fs/page_cache.rs"
  - "os/src/fs/mod.rs"
entry_points:
  - "DEV_FS"
  - "DevFS"
  - "add_dev"
  - "add_dir"
  - "LockedDevFSInode"
arch:
  rv64: supported
  la64: supported
tests:
  ltp:
    - "open05"
    - "fcntl01"
    - "getdents01"
  oscomp:
    - "basic"
related_docs:
  - "docs/03_fs/architecture.md"
  - "docs/03_fs/vfs-core.md"
  - "docs/03_fs/init-and-rootfs.md"
  - "docs/09_debug/la64_on_board/260717/10-tty-smolagent-interactive-fix.md"
---

## 概述

DevFS（设备文件系统）管理 `/dev/` 目录下的所有设备节点。它是一个纯内存虚拟文件系统，不关联任何块设备，所有 inode 通过 `add_dev` / `add_dir` 接口在初始化时或运行时动态注册。DevFS 的设计参考了 DragonOS 的 DevFS 实现，遵循 VFS 层 `IndexNode` trait 契约。

DevFS 位于 VFS 四层模型的最底层，负责将字符设备、块设备、伪终端、管道等 I/O 操作接口统一呈现为文件系统目录树中的 inode 节点。

## 数据结构

### DevFSInode

DevFSInode 是 DevFS 的目录 inode 内部数据，通过 `LockedDevFSInode`（`Arc<Mutex<DevFSInode>>`）提供并发访问：

```rust
pub struct DevFSInode {
    parent: Weak<LockedDevFSInode>,
    self_ref: Weak<LockedDevFSInode>,
    children: BTreeMap<String, Arc<dyn IndexNode>>,
    metadata: Metadata,
    fs: Weak<DevFS>,
}
```

核心字段说明：

- **parent / self_ref** — 弱引用，避免循环引用，用于 `..` 和 `.` 目录项查找
- **children** — 子节点 BTreeMap，键为设备名（如"null"、"zero"），值为实现了 `IndexNode` 的具体设备 inode
- **metadata** — 标准 VFS Metadata，包含 inode_id、file_type、mode 等
- **fs** — 所属 DevFS 实例的弱引用

DevFS 本身只存储根目录 inode，所有动态注册的设备 inode 通过 Arc 托管，直接插入根目录的 children map。

### 全局实例

```rust
lazy_static! {
    pub static ref DEV_FS: Arc<DevFS> = DevFS::new();
}
```

`DEV_FS` 是全局共享的单例，在 `mount_common_filesystems` 中通过 `DEV_FS.add_dev` 注册静态设备，在 `mount_boot_block_devices` 中注册块设备分区节点。

## 设备注册

### add_dev

向 DevFS 根目录注册一个设备 inode。设备名作为 key 插入 children BTreeMap，后续通过 `find` 按名查找返回：

```rust
devfs.add_dev("null", Arc::new(Null) as Arc<dyn IndexNode>)
```

如果 name 已存在返回 `SyscallErr::EEXIST`，当前 inode 不是目录返回 `SyscallErr::ENOTDIR`。

### add_dir

在 DevFS 根目录下创建子目录 inode。每个子目录也是一个 `LockedDevFSInode`，独立持有自己的 children BTreeMap。典型用途是 `add_dir("misc")` 创建 `/dev/misc` 以容纳次要设备：

```rust
let misc_dir = devfs.add_dir("misc", InodeMode::from_bits_truncate(0o755))?;
misc_dir.add_dev("rtc", Arc::new(Rtc) as Arc<dyn IndexNode>)?;
```

### IndexNode for LockedDevFSInode

目录 inode 实现了 `find`、`list`、`list_dirents` 等标准 VFS 方法，将 lookup 委托给 `children` map 查找。非目录操作（`read_at`、`write_at`）返回 `ENOSYS`。

## 设备列表

### /dev/null

数据汇。读总是返回 EOF（0 字节），写丢弃所有数据并返回写入长度。实现了 `is_discard_write`（true）和 `supports_user_buffer_io`（true），通过 `read_at_user` / `write_at_user` 路径直接操作用户缓冲区。`resize` 是空操作。主次设备号 makedev!(1, 3)。

### /dev/zero

零源。读用 0 填充缓冲区并返回长度，写丢弃所有数据（同 null 语义）。实现了 `read_at_user`（通过 `dst.fill_at` 快速填零）和 `supports_user_buffer_io`。主次设备号 makedev!(1, 5)。

### /dev/urandom 和 /dev/random

两者共享内核 ChaCha20 CSPRNG。QEMU 由 VirtIO RNG 播种，2K1000LA 由片上 APB RNG 播种；启动样本通过基本重复/卡死健康检查后，随机池才进入 ready 状态。读操作返回请求长度的安全随机字节，可信熵源初始化失败时返回 `EAGAIN`，不会回退到全零或时间种子。写入数据会混入私有状态，但不会提高 ready 状态或被计为可信熵。`/dev/random` 当前仍是 `/dev/urandom` 的同实现别名，主次设备号为 makedev!(1, 9)。

### /dev/full

总是满的假设备。读语义同 /dev/zero（填零返回），写始终返回 `ENOSPC`。用于测试程序在磁盘满时的行为。主次设备号 makedev!(1, 7)。

### /dev/tty 和 /dev/console

控制台终端。`/dev/console` 是 TTY 的别名。

**输入生产与通知**：调度器从物理 UART 取字符，先经过 magic-key 识别，再放入 trace
stash；`Teletype::receive_stashed()` 将字符送入 line discipline。只有生产侧在字符真正使
TTY 可读后才通知普通 read waiter 和 epoll listener；`read_at()` 只消费数据，`poll()` 只
查询状态，两者都不会反向通知自己的读等待队列。这个约束避免 `WaitQueue` 在持锁重查
read 条件时，TTY 消费路径再次获取同一非重入 `spin::Mutex` 形成单核自锁。

**输入变换与规范模式**：输入先应用 `IGNCR` / `ICRNL` / `INLCR`。默认 `ICRNL` 因而会
把串口 Enter 的 `CR` 转为用户态 `NL`。`ICANON` 下使用固定 1024 字节环形队列保存完整
记录和当前可编辑行，支持换行、`VEOL` / `VEOL2`、`VEOF`、`VERASE`、`VKILL`，并按
`ECHO` / `ECHOE` / `ECHOK` / `ECHONL` / `ECHOKE` 做最小回显。规范 read 最多返回一条
记录；在 Enter 前输入普通字符只进入当前行，不会提前返回给 Python `input()`。

**非规范模式**：清除 `ICANON` 后支持 `VMIN` / `VTIME` 四种基本组合。每个有效字节
都会在生产侧唤醒 waiter，`VTIME` 由现有 wait-I/O 兜底定时复查。当前计时状态仍属于
整个 `Teletype`，而不是每个 open/read；多个并发 reader 的完整 Linux N_TTY 语义尚未
实现。

**控制字符**：`ISIG` 下的 VINTR（默认 Ctrl-C）会按 `NOFLSH` 决定是否清空输入，并在
释放 TTY 内部锁后向前台进程组投递 SIGINT，避免持 TTY 锁扫描全局 task/process 表。
没有可用 foreground pgid 时仍保留调度器场景的 interruptible-task fallback。

**写**：将 UTF-8 字符串直接输出到串口（`print!`）。

**ioctl**：支持 `TCGETS` / `TCSETS` / `TCGETA` / `TCSETA` 系列（termios 读写）、`TCXONC`（空操作）、`TIOCGPGRP` / `TIOCSPGRP`（前台进程组）、`TIOCGWINSZ` / `TIOCSWINSZ`（窗口大小）。`TIOCGPGRP` 在 foreground_pgid 从未设置时返回调用者的 pgid（Linux 兼容）。`TCSETSF` / `TCSETAF` 会清空输入；模式切换若让已缓冲数据从不可读变为可读，会在释放内部锁后通知 read/epoll waiter。

### Pipe（匿名管道）

管道由 `make_pipe` 创建一对 `(read_end, write_end)`，共享同一个 `PipeRingBuffer`（64KB 环形缓冲区）。关键行为：

- **读**：从环形缓冲区读取数据。缓冲区为空且写端已关闭返回 EOF（0）；缓冲区为空且写端打开返回 `EAGAIN`；读取后通知写端 `EPOLLOUT`。
- **写**：写入环形缓冲区。读端已关闭发送 SIGPIPE 并返回 `EPIPE`；缓冲区满返回 `EAGAIN`；写入后通知读端 `EPOLLIN`。
- **poll**：基于环形缓冲区状态和端对关闭情况计算可读/可写/挂起事件位。
- **ioctl**：`FIONREAD` 用于读取当前缓冲区中可用字节数。

PipeRingBuffer 状态机为 FULL / EMPTY / NORMAL。支持 `F_SETPIPE_SZ` 调整容量（需 `CAP_SYS_RESOURCE` 权限，上限 2GB 实际受 64KB 硬限制）。资源使用支持原子计数器跟踪。

**Named FIFO**：通过 `fifo_open` 在全局 `FIFO_REGISTRY` 中以 `(dev_id, inode_id)` 标识建立管道端点。支持 `compact_fifo_registry` 周期回收两端已关闭的陈旧条目。

### PTY（伪终端）

Pty 系统由 `PtyManager` 管理，每对 PTY 包含一个 master（`PtmxMasterInode`）和一个 slave（`PtySlave`），通过 `PtyInner` 共享两个方向独立的 `RingBuffer`（各 4KB）。

**Master /dev/ptmx**：

- `open` 创建新的 PTY pair，初始化 `PtyInner` 并分配唯一 ID
- 读（master_read）：从 slave_to_master 环形缓冲区取数据；读后通知 slave 写端
- 写（master_write）：向 master_to_slave 环形缓冲区写数据；写后通知 slave 读端
- 支持 ioctl：`TIOCGPTN`（获取从设备号）、`TIOCSPTLCK` / `TIOCGPTLCK`（锁定控制）

**Slave /dev/pts/N**：

- `open` 检查 master 是否锁定、master 是否关闭；更新打开计数，首次打开记录 uid
- `close` 递减打开计数，最后一个 slave 关闭时唤醒 master 的读/写等待队列并通知 HUP
- 读：从 master_to_slave 取数据。如果 master 已关闭且无数据返回 0（EOF）。
- 写：向 slave_to_master 写数据。如果 master 已关闭返回 EIO。ONLCR 模式下 `\n` 自动扩展为 `\r\n`。
- 支持 termios / winsize / foreground_pgid 全套 ioctl

`PtsDirInode` 作为 `/dev/pts` 的动态目录，`find` 时根据数字 ID 从 `PtyManager` 获取对应 slave。

### /dev/rtc

实时时钟。仅支持 `RTC_RD_TIME` ioctl，将 `current_time_safe()` UNIX 时间戳转换为 `RtcTime` 结构（tm_sec / tm_min / tm_hour / tm_mday / tm_mon / tm_year / tm_wday / tm_yday / tm_isdst），闰年和月份天数正确处理。主次设备号 makedev!(10, 135)。

多个 RTC 入口并存：`/dev/rtc`（devfs 根）和 `/dev/misc/rtc`（misc 子目录共享同类型的 Rtc inode）。

### BlockDevInode（块设备节点）

BlockDevInode 包装 `Arc<dyn BlockDevice>`，提供原始块设备访问。主设备号固定为 254（VIRTIO_BLK_MAJOR）。

**读**：按 BLOCK_SZ 块对齐分片读取，通过 `read_block` 获取整块数据后拷贝子区间。超出设备大小返回 0。

**写**：按块对齐写入。非整块写入时先读后写（read-modify-write）。超出设备大小返回 `ENOSPC`。

**ioctl**：

- `BLKGETSIZE64`：获取设备字节大小
- `BLKSSZGET`：获取逻辑扇区大小（固定返回 512）

**动态注册路径**：`mount_boot_block_devices` 先注册原始设备，再对未识别为裸
ext4/FAT32 的设备解析 MBR 主分区。QEMU 使用：

```
/dev/vda       (原始根设备, minor=0)
/dev/vdb       (工具盘, minor=1)
/dev/vda1..N   (vda 的 MBR 主分区，或 vdb 分区兼容别名)
/dev/vdb1..N   (vdb 的 MBR 主分区)
```

2K1000LA 的单块 SATA SSD 使用：

```
/dev/sda       (原始 SATA SSD)
/dev/sda1..N   (MBR 主分区)
/dev/vda       (sda 兼容别名)
/dev/vda1..N   (sdaN 兼容别名)
```

完整测试镜像固定保留以下设备 ABI：

| 分区 | 兼容节点 | 内容 | 自动挂载 |
|------|----------|------|----------|
| P1 | `/dev/vda1` | 4GiB 官方 LA64 测试集 ext4 | `/sdcard` |
| P2 | `/dev/vda2` | 1280MiB FAT32 暂存盘 | 否；由 basic/mount 测试临时挂载 |
| P3 | `/dev/vda3` | 768MiB MangoCore 工具 ext4 | 无第二块盘时挂到 `/tools` |

`PartitionBlockDevice` 保留 MBR 的 512 字节 LBA 语义，并转换到平台
`BLOCK_SZ`。未按 2KiB/4KiB 对齐的分区通过 bounce buffer 访问；自然对齐分区
走整块直接 I/O。文件系统打开前还会按 ext4 原生块大小或 FAT BPB
`BytsPerSec` 包装 `BlockSizeAdapter`，因此文件系统块号不会被误当成平台块号。
用户态 `mount(2)` 打开 ext4/FAT32 时也走同一 `detect_fs_layout()` 和
`BlockSizeAdapter` 路径，不能直接把 `BlockDevInode.inner` 交给文件系统。
当前只解析四个 MBR 主分区，不支持扩展分区和 GPT；包含 protective MBR 的
混合分区表也会整盘拒绝。

2K1000LA 只读验收镜像将 `/dev/sda*` 与 `/dev/vda*` 节点标记为 `0440`，
节点写入直接返回 `EROFS`。挂载使用 `MountFlags::RDONLY`，底层再套一层
`ReadOnlyBlockDevice`，用于拦截 FAT inode drop 等绕过 VFS 的内部回写。

### /dev/cpu_dma_latency

Null 类型的别名。写入丢弃，读返回 EOF。用于需要打开 `/dev/cpu_dma_latency` 的测试程序。

### 随机设备安全边界

随机设备不直接输出硬件寄存器内容。硬件样本只负责启动播种，用户可见字节统一来自 ChaCha20 流，并在每次请求后用隐藏输出重键。该设计已经消除全零实现，但当前尚未实现运行期按字节数或时间阈值重新采集硬件熵；详见 `docs/07_driver/random.md`。

## 初始化流程

```
rust_main
  -> fs::init()
    -> mount_common_filesystems()
      -> DEV_FS.add_dev("null")     // + tty, zero, urandom, random
      -> DEV_FS.add_dev("full")     // /dev/full
      -> DEV_FS.add_dev("ptmx")     // /dev/ptmx
      -> DEV_FS.add_dev("pts")      // /dev/pts 动态目录
      -> DEV_FS.add_dev("rtc")      // /dev/rtc
      -> DEV_FS.add_dir("misc")     // /dev/misc
        -> misc_dir.add_dev("rtc")  // /dev/misc/rtc
      -> DEV_FS.add_dir("shm")      // /dev/shm 覆盖挂载点
    -> mount_boot_block_devices()
      -> detect_fs(raw)             // 优先识别整盘裸 ext4/FAT32
      -> probe_mbr(raw)             // 裸盘未识别时解析主分区
      -> DEV_FS.add_dev("vda1")...  // 或 2K1000 的 sda/sdaN
      -> detect_fs(partition)       // 选择首个可挂载分区
```

## Test Mapping

| 特性 | 设备 | 测试覆盖 | 状态 |
|------|------|---------|------|
| null 读写 | /dev/null | busybox dd, LTP open05 | pass |
| zero 填零读 | /dev/zero | busybox dd, mmap 测试 | pass |
| full 写返回 ENOSPC | /dev/full | LTP fcntl01 | pass |
| urandom 读取 | /dev/urandom | rng_test（getrandom/设备活性、差异性） | pass/QEMU+2K1000LA |
| tty 字符 IO | /dev/tty | login, shell 交互 | pass |
| pipe 环形缓冲 | pipe() syscall | LTP pipe*, libc 测试 | pass |
| pty pair 创建 | /dev/ptmx | busybox, telnetd | pass |
| rtc 时间查询 | /dev/rtc | hwclock, LTP | pass |
| 块设备原始 IO | /dev/vda、/dev/sda | dd, mkfs, mount | pass/board pending |
| MBR 分区节点 | /dev/vdb1、/dev/sda1 | QEMU LBA63 ext4/FAT32 镜像 | pass/board pending |
| 原生块大小转换 | BlockSizeAdapter | ext4 1KiB、FAT32 512B 根目录读取 | pass/board pending |

## Known Issues

1. **随机池只在启动时采集硬件熵**
   - 现状：VirtIO RNG 或 2K1000LA APB RNG 提供 64 字节启动样本，之后由 ChaCha20 CSPRNG 输出并逐请求重键
   - 边界：尚未按输出量或运行时间周期性重新读取硬件熵
   - 影响：已具备启动后安全随机流和前向重键，但长期运行时不能宣称持续硬件重播种
   - 修复方向：持久化平台熵设备句柄，在不持有随机池锁时采样，并按阈值混入且执行连续健康检查

2. **pty 缓冲区大小固定**
   - 现象：master 到 slave 和 slave 到 master 各只有 4KB 环形缓冲区
   - 根因：`PTY_BUF_SIZE` 硬编码为 4096
   - 影响：大量数据写入（如 `git push` 通过 ssh）容易阻塞
   - 修复方向：参考 Linux N_TTY 缓冲策略，支持动态扩展或更大默认值

3. **FIFO 注册表泄漏风险**
   - 现象：系统长时间运行后 FIFO_REGISTRY 可能积累陈旧条目
   - 根因：`compact_fifo_registry` 需要周期性由 reclaim 触发，触发频率不足时会持有已关闭的 PipeRingBuffer
   - 当前缓解：每次 fifo_open 时检查并清理当前条目同 key 的脏数据；compact 在轮询中被调用
   - 修复方向：确保 compact 高频周期化或在 pipe inode close 时主动清理注册表

4. **pipe 匿名管道容量上限**
   - 现象：`F_SETPIPE_SZ` 无法超过 64KB（RING_DEFAULT_BUFFER_SIZE）
   - 根因：PipeRingBuffer 底层为固定大小 Box<[u8; 65536]>
   - 影响：大块数据传输场景受限
   - 修复方向：改为动态分配 Vec，支持按需扩容

5. **tty 仅支持单字节 I/O**
   - 现象：每次 read 最多返回 1 字节
   - 根因：Teletype 的 `last_char` 暂存仅缓存一个字符，没有行缓冲或 raw 模式连续读取
   - 影响：cat 等逐字节读取工作的应用性能差
   - 修复方向：实现 ICANON 模式的行缓冲和 raw 模式的批量读取

6. **分区表范围仅覆盖 MBR 主分区**
   - 现象：protective MBR、GPT 和扩展分区会报告 unsupported
   - 原因：本阶段只为 2K1000LA SSD 的首次只读挂载接入传统 MBR
   - 影响：GPT 格式 SSD 不能挂载
   - 后续方向：实板 MBR 路径稳定后，再独立实现 GPT 校验和分区项解析
