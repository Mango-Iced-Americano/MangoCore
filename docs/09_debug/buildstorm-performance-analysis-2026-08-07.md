---
title: "8 核 8G BuildStorm 性能分析"
category: debug
status: draft
author: MangoCore Team
last_update: 2026-08-07
tags: [perf, buildstorm, ext4, lwext4, pagecache, smp]
code_paths:
  - "os/src/fs/page_cache.rs"
  - "os/src/fs/ext4_lwext4/page_cache.rs"
  - "os/src/fs/ext4_lwext4/inode_state.rs"
  - "os/src/fs/ext4_lwext4/layout.rs"
  - "os/src/fs/ext4_lwext4/global.rs"
  - "os/src/task/run_queue.rs"
  - "os/src/task/processor.rs"
---

# 8 核 8G BuildStorm 性能分析

## 结论

当前阻塞点不是 `rseq`、430 号 syscall、非 0 Boot HART，也不是任务仍被固定在 CPU0。主要瓶颈是 lwext4 读路径将 Cargo/Rust 的并行编译退化成了大量串行 4 KiB I/O：

```text
共享库 mmap/page fault
  → PageCache 单页 miss
  → 临时 ext4_file open
  → 绝对路径遍历
  → 单次 4 KiB 读取
  → close
  → LWEXT4_GLOBAL 全局自旋门
```

B1 的 BuildStorm profile 只将 x0 挂载到 `/sdcard` 并进入 chroot，`/tools` 未覆盖官方环境。因此本次慢路径集中在 x0 的 ext4/lwext4，而不是 x1 工具盘。

## 观测数据

数据来源：`docs/Work_Log/evidence/2026-08-07/`。调试运行使用 RV64、QEMU TCG MTTCG、8 vCPU、8 GiB、`LOG=debug`，600 秒后退出码 124。

| 阶段 | 时间 | 物理读取 | 物理写入 | 结果 |
|---|---:|---:|---:|---|
| ext4 挂载 | 97.63 s | 0.03 MiB | 极少 | 含一次 96.955 s 异常停顿 |
| 动态加载器 | 0.72 s | 0.69 MiB | 0 | 正常 |
| Toolchain 检查 | 62.88 s | 62.40 MiB | 0 | `BUILDSTORM_TOOLCHAIN ok` |
| Minibuild | 174.99 s | 132.18 MiB | 0 | `BUILDSTORM_MINIBUILD ok` |
| tg-xtask 预构建 | 246.31 s | 316.37 MiB | 14.97 MiB | 仍未进入计时编译 |

累计指标：

- lwext4 物理读 133,239 次、511.68 MiB，平均 3.93 KiB/次。
- lwext4 物理写 3,827 次、14.98 MiB。
- 用户文件读取 39,665 次、150.17 MiB。
- 物理读取/有效载荷约 `3.41x`。
- syscall 43,872 次；主要是 `statx=9200`、`lseek=8394`、`read=7610`、`openat=4408`、`futex=3303`。

最大单文件样本是 `librustc_driver.so`：51.45 MiB、12,561 次 4 KiB 读取、12,569 次内部 `file_open`。这说明几乎每个共享库页面都重新打开了一次 ext4 文件。

## 根因一：批量读接口没有接入真实读路径

`PageCacheBackend` 定义了 `read_pages()`，lwext4 后端也实现了“单次打开、读取多页”的版本：

- [PageCacheBackend::read_pages](../../os/src/fs/page_cache.rs:245)
- [LwExt4PageCacheBackend::read_pages](../../os/src/fs/ext4_lwext4/page_cache.rs:150)

但 `sync_batch_read_pages()` 的 Phase 2 仍逐页调用 `backend.read_page()`：[page_cache.rs](../../os/src/fs/page_cache.rs:1710)。普通多页 miss 也在 `fill_miss_runs()` 中逐页读取：[page_cache.rs](../../os/src/fs/page_cache.rs:1274)。

因此批量接口目前基本是“声明存在、实际未使用”。建议按连续页区间调用 `read_pages()`，使用 staging buffer 或有序页面写锁，避免跨页可变引用和数据错位。

## 根因二：文件映射缺页没有 readahead

共享库通过 mmap 加载，用户 FD 往往已经关闭。文件映射 fault 直接调用 `frame_for_filemap_read()`：[filemap.rs](../../os/src/mm/filemap.rs:131)，没有给 VMA 建立 readahead 状态。

当 PageCache miss 时，`with_file()` 没有持久句柄就创建临时描述符：[inode_state.rs](../../os/src/fs/ext4_lwext4/inode_state.rs:241)。所以共享库的每个页面都触发一次：

```text
file_open → file_seek → file_read(4096) → file_close
```

应为文件映射增加受控窗口预取，并让预取真正进入 `read_pages()`。以 32 页窗口估算，`librustc_driver.so` 的内部打开次数可从约 12,569 次降到批次级数量。

## 根因三：路径遍历和缓存抖动

`Ext4OSInode::find()` 会先验证父路径，再探测子路径：[layout.rs](../../os/src/fs/ext4_lwext4/layout.rs:619)。两者最终都会调用 `ext4_raw_inode_fill()`，lwext4 再通过 `ext4_generic_open2()` 从挂载根遍历绝对路径。

调试日志中的热点块已映射到：

- ext4 根目录块：重复读取 5,473 次。
- `/root`：4,031 次。
- `/root/.cargo` 及 registry 各级目录：约 1,600～4,030 次。
- `/work`、`/work/tgoskits`：约 1,600 次。

lwext4 块缓存只有 `CONFIG_BLOCK_DEV_CACHE_SIZE=16`，即约 64 KiB：[CMakeLists.txt](../../dependency/lwext4_rust/c/lwext4/CMakeLists.txt:32)。VFS dentry cache 只有 256 项：[dentry_cache.rs](../../os/src/fs/vfs/dentry_cache.rs:25)。Cargo 扫描数千路径时，目录元数据很容易被连续数据页挤出。

建议在读批处理修复后做独立 A/B：

1. lwext4 block cache：16 → 1024。
2. dentry cache：256 → 4096。
3. 记录 block-cache hit/miss 和 dentry eviction。

不能直接删除父路径校验；它承担 rename/unlink 的 ABA 防护。长期方案应改为 inode-relative lookup 或稳定目录句柄。

## 根因四：8 核被全局 lwext4 门串行化

所有 lwext4 C 入口都需要取得进程级 `LWEXT4_GLOBAL`：[global.rs](../../os/src/fs/ext4_lwext4/global.rs:31)。该门覆盖路径探测、临时文件打开、同步文件 I/O 和 VirtIO 块设备访问。

调度器本身已经具备：

- `online_mask=0xff`。
- BuildStorm `sched_setaffinity(all)`。
- 按允许掩码和近似负载选择 CPU：[run_queue.rs](../../os/src/task/run_queue.rs:124)。
- AP 本地调度和 work stealing：[processor.rs](../../os/src/task/processor.rs:635)。

因此“非 0 Boot HART 导致 CPU0 阻塞”不成立。实际情况是多个 CPU 同时进入同一条 lwext4 串行通道；在 QEMU MTTCG 下，其他 vCPU 还会自旋消耗宿主 CPU。

当前证据没有记录全局门等待时间和 per-CPU busy/idle，因此锁竞争的比例仍需用低扰动计数器确认。不能直接移除全局门，因为 lwext4 仍有进程级设备、挂载表和 journal 状态。

## 非主因和环境放大项

### rseq 与 430 号 syscall

- `rseq(293)` 共 108 次，unsupported 为 0，不是当前吞吐瓶颈。
- 当前 rseq 只完成注册和初始 CPU 字段写入，context-switch 更新/abort 仍未实现；这是后续完整语义和正确性任务。
- 430 在 asm-generic ABI 中是 `fsopen`，不是 `set_mempolicy_home_node`。本轮只有 3 次真实探测，日志因重复格式显示为 6 条；工具随后回退旧 mount 路径。

### debug 日志

日志 327,687 行、未压缩约 35.6 MiB。块读、syscall、file read/open/close 等逐事件输出占绝大多数，UART 输出会显著放大 QEMU TCG 时间。因此 `LOG=debug` 数据用于定位调用结构，不用于生产绝对性能基线。

无 debug 的三轮运行仍只有约 5/446、13/446、9/446 crate 进度，说明日志是放大器，不是根因。

### 镜像和 journal

临时 x0 文件是完整分配的 15 GiB，容器 overlay 使用率约 99%。挂载阶段 96.955 s 停顿发生在首次 journal 写入附近，最合理的解释是 guest flush 触发宿主大规模脏页落盘；需要预同步稀疏/reflink 副本后重新测量。

强制终止后的 e2fsck 出现 journal transaction corrupt 和目录 checksum 错误。该现象可能由脏镜像、强制断电和 lwext4 journal/write ordering 共同造成，必须用正常关机和强制断电两组测试分离，不能直接把当前 run 当作干净性能基线。

## 推荐优化和验证顺序

1. PageCache 连续 miss 接入 `backend.read_pages()`。
2. filemap 增加窗口 readahead，优先覆盖动态共享库。
3. A/B 调大 lwext4 block cache 和 dentry cache。
4. 增加 `LWEXT4_GLOBAL` wait/hold cycles、块请求延迟、flush 延迟和 per-CPU busy/idle 计数。
5. 使用预同步稀疏镜像，关闭逐事件 debug，启用 `perf_stats` 的 `memory_io` profile。
6. 在正常关机/强制断电下分别跑 fsck。
7. 最后补齐 rseq context-switch/update/abort 语义。

验收应以 `BUILDSTORM_BEGIN` 和最终 `BUILDSTORM_COMPILE` 为主，同时观察：

- `file_open / file_read page` 是否从约 1 降到批次级。
- 物理读取/有效载荷是否从 3.41x 下降。
- 8 个 CPU 的锁等待和 idle 比例。
- 测试后 fsck 是否干净。

## 证据与复现限制

完整串口、QEMU debug、e2fsck 和配置文件位于 `docs/Work_Log/evidence/2026-08-07/`。证据配置记录的 HEAD 是 `b4f38d48`，当前工作树存在未提交 SMP 改动；下一轮应额外保存 `git diff` 或 dirty-tree hash，避免把不可复现的源码状态当作严格基线。
