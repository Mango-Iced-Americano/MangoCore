---
title: "ext4_lwext4 融合迁移审计与实施报告"
category: plan
status: runtime-semantics-under-validation-production-blocked
owner: MangoCore Team
last_updated: 2026-07-18
tags: [ext4, lwext4, migration, audit, 2k1000la, ssd, crash-consistency]
entry_points:
  - "os/src/fs/ext4_lwext4/ext4fs.rs"
  - "os/src/fs/ext4_lwext4/layout.rs"
  - "os/src/fs/ext4_lwext4/inode_state.rs"
code_paths:
  - "os/src/fs/ext4_lwext4/"
  - "dependency/lwext4_rust/"
  - "os/src/drivers/block/partition.rs"
  - "os/src/fs/page_cache.rs"
  - "os/src/fs/mod.rs"
  - "os/src/syscall/fs/sys_mount.rs"
  - "os/src/kernel_tests/ext4.rs"
  - "user/src/bin/regression/"
related_docs:
  - "docs/10_plan/board-develop-ext4-migration.md"
  - "docs/10_plan/ext4-perf-gap.md"
  - "docs/10_plan/lwext4-upstream-fixes.md"
  - "docs/03_fs/ext4.md"
---

# ext4_lwext4 融合迁移审计与实施报告

## 1. 结论先行

当前融合方向是正确的：继续在 `board-develop-combined` 上完成迁移，不再把新文件系统
改动堆回 `la64-on-board`。目标分支已经用双父 merge commit `78dd1c8c` 保留了
`la64-on-board@464e24b5` 和 `develop@60800fa2` 的完整历史，适合作为唯一集成与验收线。

但当前只能给出以下分层结论：

1. **运行期 POSIX 语义：显著改善，双架构专项门禁已通过。** 当前补丁已经补上
   2 KiB 平台块字节桥、未对齐分区边界、非递归空目录删除、真实 inode 共享状态、
   open-unlink 延迟回收、rename 后旧 fd 跟随 inode、覆盖目标的运行期回滚锚点，以及
   sparse truncate 从 hole 起步时的 extent 回收。RV64/LA64 regression、双架构 ext4 ktest、
   正常 teardown 和五份离线 fsck 证据均已归档。
2. **旧 onboard 修正：大部分已由新引擎或共享基础设施覆盖，但不能按“代码存在”直接验收。**
   旧 `balloc/ialloc/direntry/meta_cache` 修正不应机械移植到 lwext4；其不变量和回归必须
   迁移，并通过全新 fixture、冷重挂载和离线 `e2fsck -fn` 重新证明。
3. **性能：潜力更高，但没有当前补丁的新旧实测 A/B，不能宣称更快。** 通用 PageCache
   批量参数和 AHCI 64 KiB 命令合并仍在；新字节桥又避免了双重缩放和中间整块逐块
   bounce。但 C FFI、路径 probe、粗粒度锁和 inode-state registry 也会增加固定税。
4. **掉电一致性：未满足。** 当前 P4 和本轮 QEMU fixture 都是
   `mke2fs -O ^has_journal`；运行期 state 虽已使用 inode generation 隔离复用，但 deferred
   unlink 没有带 generation 的 on-disk orphan 链或 mount-time replay。rename 回滚也只在
   内核仍运行时有效。
5. **SSD 生产结论：禁止。** 全盘备份的长度、双 hash、zstd 解压和首 1 MiB 对比已经
   验证；双架构专项门禁和正常关机 fsck 也已完成。但真实 2 KiB 实板只读、journal/orphan
   故障注入、AHCI flush、性能 A/B 和受控 scratch 仍未完成，不得把备份完成解读为“可以
   切换生产 P4”。

因此推荐状态为：**QEMU 迁移继续；实板只读准备可以继续；SSD 写入保持关闭；生产切换
保持阻塞。**

## 2. 审计范围与证据边界

### 2.1 基线

| 角色 | 分支/提交 | 本报告中的定位 |
|---|---|---|
| 实板稳定回退点 | `la64-on-board@464e24b5` | 旧自研 ext4、2K1000LA AHCI/P4/运行时的已知可用基线 |
| 新文件系统输入 | `develop@60800fa2` | lwext4 引擎、VFS 适配、PageCache 和新测试基础设施 |
| 融合起点 | `board-develop-combined@78dd1c8c` | 两条历史的双父 merge，不重写输入分支 |
| 当前工作树 | `78dd1c8c-dirty` | 本轮 2K bridge、namespace/inode lifetime 与回归补丁，尚未形成最终提交 |

### 2.2 当前补丁验证状态

下表只记录本轮编排已实际执行或明确未执行的项目。完整日志、镜像、fsck、容器事件、
mount 映射、命令、配置和新鲜性记录位于
`docs/Work_Log/evidence/2026-07-18/`。

| 项目 | 当前结果 | 证据等级 | 能证明什么 |
|---|---|---|---|
| RV64 / LA64 `kernel-build-only` | 均退出 0，严格串行 | 持久 Docker 日志 + container event | 当前 Rust/C/FFI 组合在两工具链均可编译 |
| RV64 L4 regression | 顶层 6/6；namespace 9/9；sparse Phase 6 PASS | 完整 QEMU 日志、status、fixture | 运行期 rmdir/hardlink/rename/unlink-open/truncate 语义由 RED 转 GREEN |
| LA64 L4 regression | 顶层 6/6；namespace 9/9；sparse Phase 6 PASS | 完整 QEMU 日志、status、fixture | 同一补丁没有只在 RV64 偶然成立 |
| RV64 all ktest | 21/21，其中 ext4 7/7 | 完整 QEMU 日志、fixture | 块桥、分区、partial writeback 与其他内核专项共存 |
| LA64 ext4 ktest | 7/7 | 完整 QEMU 日志、fixture | 2K bridge、partition、partial writeback 在 LA64 通过 |
| LA64 all ktest | 20/21；ext4 全过，`timer::tick_advances` 在 1 ms 内 `t1 == t0` | 完整 QEMU 日志、fixture | ext4 无失败；全量门禁仍如实记为非全绿 |
| workload 后离线 fsck | 五份 fixture 均完成 Pass 1-5，无修复项 | 五份 `*.img.fsck.log` | 正常关机后的冷磁盘元数据在这些 workload 下自洽 |
| 新旧性能 A/B | 未执行 | 无 | 不得声称 lwext4 更快或更慢 |
| 2K1000LA 当前补丁实板验收 | 未执行 | 无 | 参数化 2K ktest 不等于真实 AHCI/SSD 结果 |
| SSD 写入 | 未执行 | 无 | 本轮代码与测试没有触碰实板 SSD |
| SSD 全盘备份 | 32,017,047,552 B；raw/zstd hash、完整解压、首 1 MiB 对比通过 | 独立备份产物与校验记录 | 具备回滚副本；不等于授权生产写入 |

融合提交 `78dd1c8c` 之前已有双架构 4/4 ext4 ktest 和 5/5 L4 回归记录；它们是
**融合基线**，不能替代本轮 dirty patch 的重新验证。

## 3. 分支与提交历史

### 3.1 onboard 旧 ext4 的关键修正

`la64-on-board` 相对 develop 的旧 ext4 重点不是单一补丁，而是三组经验：

- `b6c5c973 fix(fs): stabilize ext4 persistent writes`
  - lazy block/inode bitmap 初始化；
  - 超级块和 block-group 计数的累计更新；
  - 可变长目录项首记录删除、目录 checksum 身份；
  - 释放块的 metadata-cache invalidate；
  - inode snapshot 同步、延迟回收和 cache owner；
  - 文件系统实例 identity；
  - 双架构用户入口 16 字节栈对齐；
  - clean fixture、63/63 `fs_test` 和离线 fsck 工作流。
- `b62828cf fix(board): stabilize persistent Python and ext4 rename`
  - 同目录覆盖 rename 的顺序与失败回滚；
  - 源/目标同 inode no-op；
  - 覆盖目标链接计数延迟到发布成功后更新；
  - APK/pip 的 `temporary -> final` 持久发布场景。
- `3bbe678c`、`a59ad04d`、`549a5426`、`eab09356`、`9b215fe7`、
  `ed9fda29`、`e40cc4e4`
  - extent-range cache、批量页 I/O、byte-based allocator、128 KiB 预分配；
  - 32 KiB VirtIO 请求；
  - PageCache 128 页预读、256 页 writeback、8192/16384 dirty 水位。

这些提交是审计输入和回退证据，不应继续在原分支上追加新 lwext4 代码。

### 3.2 develop 的 lwext4 演进

develop 已按阶段引入并迭代新后端：

- `13d073ff` 至 `20412ea6`：vendor/build、BlockDevice bridge、VFS 读写、PageCache、
  POSIX gap 和默认切换；
- `ea34683d` 至 `62751192`：锁、句柄清理、seek/dir-open errno、多实例设备与挂载点；
- `2a944001` 至 `11831af3`：lazy PageCache、写合并、metadata probe/cache、readahead；
- `2f5b9ee1` 至 `d0f76b3f`：多实例、rename/PageCache、write ordering、strong cache
  registry、rmdir child 验证、close flush 回退；
- `add37522`、`4e3d75dd`：rename 安全检查和 mount lifecycle；
- `14464d77`、`dcc5bbc5`、`015805bc`、`c0ca73d7`：稀疏文件、stale backend、
  batched hole read 和 inode-incarnation cache isolation。

develop 的质量已经明显高于最初 Phase 2/3 适配器，但本轮审计证明两个核心差距仍存在：
一是底层平台字节边界，二是 VFS open-file identity 不能继续依赖 path。

## 4. 新旧架构差异

| 维度 | 旧 `fs/ext4` | 新 `fs/ext4_lwext4` | 质量判断 |
|---|---|---|---|
| ext4 引擎 | 项目内纯 Rust，自行维护磁盘结构 | vendored lwext4 C，引擎外加 Rust VFS/块桥 | 新引擎格式能力更成熟；本地 FFI 审计成本更高 |
| 分配/目录元数据 | MangoCore 自己实现并经多轮修正 | lwext4 自己的 allocator/direntry/block cache | 旧修正不能直接 cherry-pick；需迁移不变量和测试 |
| inode 身份 | Rust inode ref 与本地 snapshot | lwext4 path API + `ext4_file` inode handle | 本轮共享 `Ext4InodeState` 后，运行期 identity 明显改善 |
| PageCache | 直接绑定自研 inode/extent | Mango PageCache + lwext4 backend + C block cache | 双层缓存更复杂；批量能力仍保留 |
| 锁 | Rust 内部多把锁，可局部优化 | 每实例 `lw` Mutex + C mount/global state | 单核可接受；SMP/多实例重入仍需专项审计 |
| 错误边界 | Rust `Result`，但历史上有 metadata snapshot 覆盖 | Rust/C errno 映射，部分路径当前归一为 `EIO` | 新后端要继续收紧 errno 和 FFI 回滚 |
| crash consistency | 无 journal | 库有 JBD 能力，但当前 P4/fixture 无 journal；无 orphan 集成 | 当前两者都不满足可靠掉电恢复 |
| 实板成熟度 | 已有 P4/APK/Python 压力和性能数据 | 尚无当前补丁的真实 2K SSD 数据 | 旧实现目前仍是实板回退质量更高的一方 |

综合判断不是“新代码全面高于旧代码”，而是：**新引擎的长期维护和格式覆盖更好，
当前适配层的运行期语义接近或优于旧实现，但掉电安全、真实 2K 板证据和性能证据仍低于旧实现。**

## 5. onboard 修正覆盖审计

状态定义：

- **已覆盖**：相同不变量在新路径有明确实现和 focused 测试；
- **机制替代**：旧代码不适用，由 lwext4 自身承担，仍需冷一致性验证；
- **部分覆盖**：正常运行路径改善，但错误/掉电/目录等边界未闭环；
- **未覆盖**：必须新增设计或证据，不能进入生产门禁。

| onboard 修正/优化 | 新实现中的对应机制 | 状态 | 是否迁移旧代码 | 后续动作 |
|---|---|---:|---:|---|
| `BLOCK_UNINIT/INODE_UNINIT` lazy bitmap 重建 | lwext4 `balloc/ialloc` 与 metadata checksum 路径 | 机制替代 | 否 | 用现代 mkfs fixture 做大量 create/delete，关机后 `e2fsck -fn` |
| block-group 边界、尾部无效位、保留元数据块 | lwext4 group descriptor/bitmap allocator | 机制替代 | 否 | 加跨组、近满盘和重复释放 fixture；以 fsck 判定 |
| superblock/bg free counter 累计更新 | lwext4 transaction 内更新 | 机制替代 | 否 | `statfs + allocate + sync + delete + sync + fsck` 对照 |
| 新 inode slot 清零、重复 free 防护 | lwext4 inode allocation/free | 机制替代 | 否 | inode reuse 压力和旧数据不可见回归 |
| extent-range cache、byte mballoc、128 KiB 预分配 | lwext4 extent/allocator；Mango PageCache batch | 部分覆盖 | 否 | 不假设等价性能；以 trace 和 A/B 决定是否补优化 |
| 512 B 页 I/O 合并、32 KiB VirtIO 请求 | 通用 PageCache 保留；本轮 bridge 只 bounce head/tail、整块 middle 一次转发 | 已覆盖 | 已在共享层保留 | 真实 2K AHCI 计数验证 request/bytes/flush |
| 128 页 readahead、256 页 writeback、dirty 8192/16384 | 当前 `os/src/fs/page_cache.rs` 仍保留这些值 | 已覆盖 | 已随 merge 保留 | 避免未经 A/B 继续放大参数 |
| AHCI 64 KiB 常驻 DMA/命令合并 | board 驱动仍在块层，后端之下 | 已覆盖 | 已随 merge 保留 | 验证 lwext4 middle request 没在上层重新碎片化 |
| 分区 LBA/平台块单位隔离 | 本轮分区保存 byte offset，未对齐只 bounce 首尾，尾块按 MBR 精确边界截断 | 已覆盖 | 是，迁移“不变量” | 双架构 ext4 ktest 7/7；仍需真实 2K 只读探针 |
| 首条可变长目录项删除 framing | 由 lwext4 C direntry 引擎承担；本轮 `rmdir` 不再由 Rust 枚举后递归删除 | 机制替代 | 否 | 增加 block-first/non-first dirent + 重挂载 + fsck 回归 |
| 目录 checksum 使用目录 inode/generation | lwext4 checksum 路径 | 机制替代 | 否 | metadata_csum fixture 与 fsck 门禁 |
| 释放元数据块立即 invalidate cache | lwext4 block cache/allocator 内部负责 | 机制替代 | 否 | 释放块复用压力；不能仅靠同启动 cache hit 验证 |
| 父 inode snapshot 刷新 | 新后端 probe + shared inode state，不复用旧 Rust snapshot | 部分覆盖 | 否 | 目录 rename 后缓存子孙路径仍需专项修复/验证 |
| inode number 复用隔离 PageCache | create 时解除旧 registry key；真实 inode keyed cache/state | 已覆盖 | 已由 develop + 本轮状态共享覆盖 | 保持 `gf14/gf18/gf27/gf28` 和 name-reuse 回归 |
| open-unlink 延迟回收 | `Ext4InodeState` open count + persistent handle + deferred finalize | 已覆盖（运行期） | 是，重建于新 API | 补 on-disk orphan 后才能宣称掉电安全 |
| rename 同 inode no-op | probe source/target real inode 后直接成功 | 已覆盖 | 是 | 当前 namespace regression 已覆盖 |
| rename 失败保留两侧 | regular target 先持 handle、detach，publish 失败用 handle relink | 部分覆盖 | 是 | closed unlink 排序、目录目标 rollback 和掉电窗口仍未闭环 |
| 覆盖 rename 保留已打开旧目标 | target inode state/handle 延迟 final reclaim | 已覆盖（运行期） | 是 | 保留旧 fd 读写、fsync、最后 close 的 focused 回归 |
| 非空 `rmdir` 不破坏内容 | 新 C `ext4_dir_rm_empty` 在同一 mount lock 内检查并只删空目录 | 已覆盖（运行期） | 是 | 错误注入和 fsck 尚需补齐 |
| 文件系统实例 identity 避免跨 FS inode 碰撞 | VFS `identity_key` + lwext4 unique device/mount point | 已覆盖 | 已随 merge 保留 | 多 ext4 P1/P3/P4 并存测试 |
| 16 字节 exec/signal 用户栈对齐 | 架构/进程共享代码，和 ext4 后端无关 | 已覆盖 | 已随 merge 保留 | 双架构用户态比较/信号回归继续保留 |

审计结论：旧 Rust ext4 的 allocator/direntry 实现不需要移植；真正需要移植的四类内容是
**平台块/分区不变量、inode lifetime、namespace 失败保全、冷重挂载 + fsck 测试纪律**。
前三类已经落地双架构专项门禁；第四类已完成正常 teardown 后离线 fsck，但独立冷重挂载、
故障注入和 journal replay 仍是主要证据缺口。

## 6. 本轮已经完成的实现

### 6.1 2 KiB/4 KiB byte bridge 与分区边界

- `adapt_filesystem_device()` 现在接收完整 `DetectedFs`；`BlockSizeAdapter` 只用于 FAT，
  lwext4 的 byte-level bridge 不再被二次缩放。
- `MangoKernelDevOp` 将任意 byte range 拆为“首部 bounce + 对齐 middle 批量 + 尾部
  bounce”；middle 直接作为一个 multi-block 请求下传。
- `PartitionBlockDevice` 使用 byte 起点；非平台块对齐分区仍正确映射父设备。
- 分区长度不是 `BLOCK_SZ` 整数倍时，对外暴露一个零填充的最后逻辑块；写入只落到
  MBR 声明的有效字节，不能覆盖相邻分区或尾部数据。
- ktest 通过 const-generic 2 KiB mock 在 4 KiB QEMU 内核中验证真实 board contract，
  同时断言调用序列和相邻字节不变。

### 6.2 非递归 `rmdir`

- 新增 C API `ext4_dir_rm_empty()`；目录打开、类型检查、child iterator、link count、
  unlink/truncate/free 都在同一 C mount lock/transaction 作用域内。
- Rust `rmdir()` 不再“先列举、再调用递归 `ext4_dir_rm`”。iterator 错误不再被当作 EOF，
  非空目录返回 `ENOTEMPTY` 且不删除孩子。

### 6.3 真实 inode 生命周期

新增 `Ext4InodeState`，以真实 ext4 inode number + generation 区分 incarnation，并共享：

- 当前已知 live path；
- logical size；
- link count；
- VFS open count；
- pending-delete 状态；
- 持久 `ext4_file` inode handle。

其效果是：

- PageCache 后端优先使用 inode handle，不再用 stale path 的 create fallback；
- source rename 后旧 fd 仍指向原 inode；
- unlink-open 后名字立即消失，但旧 fd 可继续读写，最后 close 才回收；
- 同名新文件获得独立 incarnation，不继承旧 inode 的 PageCache；
- overwrite rename 后，新路径指向 source，已打开的旧 target fd 仍可使用旧 inode。

### 6.4 regular-file rename 的运行期回滚

- 覆盖目标先以 handle 固定 identity，再从 namespace detach；
- source rename 成功后才 finalize 无 open reference 的 zero-link target；
- source publish 失败时用 target handle 重新建立 hard link；
- source/target 同 inode 为 no-op；
- dirty source/target PageCache 在 namespace mutation 前传播 writeback 错误。

这比原 develop 的 path-only rename 安全，但它仍不是 journaled atomic rename，见 §8。

### 6.5 独立回归夹具

- RV64/LA64 regression Make 目标创建 64 MiB、4 KiB、无 journal 的一次性 QEMU ext4
  镜像；测试完成后要求 QEMU status 为 0 且出现唯一 PASS marker，避免 `tee` 掩盖失败。
- `board_2k1000` 编译下 regression mode 明确禁用 block/network init，测试夹具绝不映射
  真实 SSD。
- namespace 子场景扩展为 9 项：非空 rmdir、同 inode rename、hardlink metadata alias、
  `RENAME_NOREPLACE` 失败保全、目录覆盖 fail-closed、open directory fd 跟随 rename、
  target-open overwrite、source-open rename、unlink-open 后同名重建隔离。

### 6.6 sparse/truncate、缓存与错误传播

- C fread 在 EOF 返回 0；seek-beyond-EOF 保持 POSIX 语义。partial-block truncate、shrink
  后 extend、sparse gap 和新分配 partial block 都显式保证零填充。
- `ext4_extent_remove_space()` 不再在 `from` 位于首 extent 前或 extent 间 hole 时直接
  返回成功；它会前进到下一 allocated block、重建 extent path，再删除范围内 extent。
  这修复了“inode 已释放但物理块仍在 bitmap 中”的离线 fsck 泄漏。
- direct write 后 invalidate C block cache；PageCache backend 的 read/writeback/truncate
  通过 per-inode I/O gate 排序，readahead 不再与 truncate 交错发布旧页。
- C transaction、xattr、extent leaf remove 和 Rust backend 的错误尽量原样向上传递，
  不再把多个失败路径静默当作成功。

### 6.7 可失败卸载与正常关机持久边界

- `FileSystem::on_umount()` 返回 `Result`。MountFS backend 进入 Dying 后，只有 teardown
  成功才变为 Dead；失败保持 Dying 并在后续 drain 重试，调用时不持 registry 锁。
- syscall shutdown 与 ktest 统一遵循 PageCache writeback → ext4 metadata flush → backend
  commit/umount → HAL halt。PASS marker 只在测试和 teardown 都成功后发出。
- lwext4 journal stop/umount 的阶段状态只有在实际成功后才清除，失败重试不会假装已经
  脱钩。五份 disposable fixture 的正常关机镜像均离线 fsck clean。

## 7. 性能对比与判断

### 7.1 旧实现已有数据

已有文档中的数据属于旧自研 ext4 或旧 board 栈，不能冒充新后端结果：

- RV64 QEMU 4 MiB/1 KiB iozone：旧实现 write 约 7,227 KiB/s、read 约 9,028 KiB/s；
- 2K1000LA P4 5,000 小文件生命周期约 9.290 ms/file，100/5,000 缩放近线性；
- 旧路径中 SATA read/write/flush 只解释约 23% sys time，主要固定税在
  VFS/ext4/PageCache/路径元数据；
- AHCI 从逐 512 B 命令改为 64 KiB 常驻 DMA 后，实板首次顺序读由约 13.5 MB/s
  提高到 18.6 MB/s；256 KiB 对照没有进一步收益。

这些数据说明“替换 ext4 引擎可能有收益”，但也说明底层 I/O 不是唯一瓶颈。

### 7.2 新实现的预期收益

- lwext4 的磁盘格式、extent、checksum 和目录实现比本地子集成熟，减少继续维护自研
  allocator/direntry 的长期成本。
- 通用 PageCache 仍使用 128 页 readahead、256 页 writeback，并在 lwext4 backend 中
  用 staging buffer 合并连续页写入。
- 连续 PageCache run 只在 batch 边界执行一次 lwext4 cache flush，不为每页单独 flush；
  orderly teardown 也没有退化为“每次 metadata 操作强制同步”。
- 本轮 byte bridge 只对 partial head/tail 做 read-modify-write，aligned middle 保持一条
  multi-block 请求；修复前逐平台块 bounce 会直接破坏 AHCI/VirtIO batching。
- 已打开文件复用持久 handle，可减少 rename/unlink 后的重复 path open/probe，并消除
  stale-path 重建风险。

### 7.3 新实现的潜在性能回退

- 每个文件系统的 `lw` Mutex 串行化所有 C 调用；单核下没有并发收益，锁和 FFI 是固定税。
- per-inode I/O gate 会串行化同一 inode 的 read/writeback/truncate；这是正确性所需，但
  metadata-heavy 或同文件并发 workload 可能增加 tail latency。
- lookup/metadata/rename 仍有 path-based probe 和 CString 构造；小文件工作负载会放大。
- generation/path validation 和 inode-state alias 维护增加小文件固定成本；需要用
  probe/open/registry 计数而非只看总 wall time 定位。
- `inode_states`、`page_caches` 使用 BTreeMap registry；若不及时移除 stale weak/strong
  entry，可能引入污染态 lookup/reclaim 成本。
- Mango PageCache 与 lwext4 block cache 是双层缓存；命中率、写回顺序和内存占用需同时看。
- 非对齐分区写的 parent-block read-modify-write 使用全局 guard，避免 sibling 分区丢写，
  代价是少量 partial write 会串行；aligned middle 不走该慢路径。
- 当前集成没有证据证明具备 Linux ext4 的 delayed allocation、bio scheduler 或 async
  commit；不能仅因库名是 lwext4 就假设这些能力存在。
- `fsync`/最终持久性的设备 cache flush 契约仍需在真实 AHCI 上单独验证。

### 7.4 必须执行的 A/B

所有样本使用同一 SSD/同一分区 payload/同一用户态二进制，先冷启动再测 warm cache，
至少 3 轮并报告 median、min/max 或 CV：

| 场景 | 数据与指标 | 目的 |
|---|---|---|
| 16/64 MiB 顺序读写，4/64 KiB record | real/user/sys、MiB/s、block req/bytes/flush | 检查 batching 是否真正到 AHCI |
| 100/5,000 小文件 create/fsync/rename/unlink | ms/file、p50/p95/max、FFI/probe/open 次数 | 衡量路径和锁固定税 |
| APK/pip `temp -> final` | 总时间、失败点、重启后文件 hash | 覆盖真实原子发布模式 |
| sparse/truncate/mmap/writeback | hole zero、logical/on-disk size、cold reopen | 防止缓存快路径掩盖数据错位 |
| P1/P3/P4 并存 | mount/read/write isolation、内部 mount path | 防止多实例串盘 |
| 污染后重复窗口 | registry size、reclaim cycles、同 workload slope | 检查渐进退化 |

初始门禁建议：正确性和 fsck 必须 100% 通过；顺序吞吐或小文件 median 相对旧实现退化
超过 10% 时暂停生产切换并定位，p95/max 出现数量级放大时即使 median 通过也判失败。
阈值可以在首轮无探针基线后调整，但不能事后只选择有利指标。

## 8. 当前 P0：无 journal/orphan 的掉电一致性

### 8.1 事实边界

- vendored lwext4 含有 journal 相关代码，不等于当前卷正在使用 journal。
- P4 生成脚本和本轮 regression fixture 都显式使用 `^has_journal`。
- `ext4_trans_start/stop` 在无 journal 卷上不能提供跨掉电的原子提交。
- 当前 `FsInfo.features` 中的静态 `"journal"` 字样不是卷级能力证明，后续应改为按
  superblock 实际 feature 报告。

### 8.2 deferred unlink 的掉电窗口

当前运行期流程是：

```text
unlink name -> link count becomes 0 -> keep ext4_file open
            -> dirty PageCache can still write inode
            -> last VFS close truncates and frees inode
```

内核持续运行时，该流程满足 open-unlink 语义。但 link count 归零后没有把 inode 写入
on-disk orphan chain。若此时掉电，mount 时没有 orphan replay 可以完成 truncate/free；
可能泄漏 inode/blocks，并依赖离线 fsck 恢复。

另一个必须修正的排序是：`ext4_fremove2(..., defer=false)` 仍可能先 truncate、后 unlink。
如果后续 namespace detach 出错，路径仍存在而数据已被破坏。正确模型应始终先完成
可回滚的 namespace detach，再根据 link/open 状态回收 inode。

### 8.3 rename 的掉电窗口

regular target overwrite 当前采用“detach target -> rename source -> 失败则 relink target”。
该 rollback anchor 只存在于内存 handle 中：

- target detach 后掉电：target 名称可能丢失，zero-link inode 没有 orphan 记录；
- source publish 后、target finalize 前掉电：可能遗留未回收 target；
- directory target 当前在 mutation 前以 `EOPNOTSUPP` fail closed，并保留源/目标；这避免
  了无回滚锚点的破坏，但也表示 POSIX empty-directory overwrite 尚未实现；
- 无 journal 时不能保证 source/target 两个目录项和链接计数作为一个持久事务提交。

### 8.4 生产前需要的设计

1. 使用带 journal 的新 fixture；不要直接在唯一 P4 上试验，先在备份完成后的可丢弃盘验证。
2. namespace detach 返回包含 filesystem identity、inode number、inode generation、剩余
   link count 和稳定 handle 的 reclaim cookie。
3. link count 将变为 0 时，在同一 journal transaction 中把 inode 加入 ext4 orphan chain；
   final close 完成 truncate/free 后再从 orphan chain 移除。
4. mount/recovery 必须 replay journal 并遍历 orphan chain；generation 不匹配时 fail closed，
   防止 inode number 复用后误删新对象。
5. rename 应收敛为单一 C API：同一 mount lock 和 journal transaction 内完成类型/空目录/
   子树检查、目标 orphan、source publish 与链接计数更新；不能由多个 path API 拼事务。
6. 对 unlink、overwrite rename、directory rename 在每个 metadata write/flush 边界做故障注入，
   每个 crash image 都要 mount-replay 并 `e2fsck -fn` clean。

在这组门禁完成前，**不得使用“原子 rename”“崩溃安全 unlink”“可生产上 SSD”等表述。**

## 9. 其他已知限制与质量债

| 限制 | 影响 | 优先级 |
|---|---|---:|
| empty-directory overwrite 当前 fail closed 为 `EOPNOTSUPP` | 数据保全正确，但 POSIX 兼容性不完整 | P1 |
| 目录 rename 未批量更新已缓存子孙 inode 的 path state | rename 后通过旧 child inode I/O 可能命中 stale path | P1 |
| PageCache backend 部分 lwext4 errno 被统一映射为 `EIO` | ABI 诊断精度下降，掩盖 ENOSPC/EROFS 等 | P1 |
| static `FsInfo.features` 宣称 journal | 用户/诊断可能错误判断卷能力 | P1 |
| `jbd_journal_purge_cp_trans()` 可能吞 checkpoint 写失败 | 卸载可能在 checkpoint 未稳定时误报成功 | P1；上 journal 前 |
| `ext4_ext_free_blocks()` 为 `void` 并吞底层 free 错误 | 故障注入时可能继续更新 inode/bitmap 状态 | P1 |
| C 全局注册表 + per-instance lock 的 SMP 语义未闭环 | 多核或跨实例并发可能竞态 | P1/SMP 前 |
| strong PageCache registry 的容量与回收 | 长测可能常驻或渐进退化 | P1 |
| sibling partition 测试尚未真正并发交错 RMW | 已证明边界算术，尚未证明并发调度下无 lost update | P1 |
| block `flush()` 端到端持久性未由真实 AHCI 证明 | `fsync` 返回不等于掉电后稳定 | P0（持久化前） |
| xattr、特殊 inode、hardlink 多 alias 的组合覆盖不足 | 长尾 POSIX/LTP 仍可能回归 | P2 |

## 10. 分支管理与最终整理

### 10.1 现在怎样管理

- `la64-on-board`：冻结，只作为已知可用实板回退点；不再接收新 lwext4 修改。
- `develop`：保持新文件系统主线输入；融合验收期间不为了实板临时问题重写历史。
- `board-develop-combined`：唯一融合目标和验收分支；当前修改继续落在这里。
- 大改动可先放短期 topic branch，经 review 后 fast-forward/merge 回目标分支；禁止在已经
  发布的双父 merge 上 rebase 或 force-push。

建议将当前工作拆成可独立回退的提交：

1. `fix(storage): preserve byte ranges across 2K block bridge`；
2. `fix(lwext4): preserve inode lifetime across namespace mutations`；
3. `test(lwext4): add disposable namespace and 2K boundary gates`；
4. `docs(lwext4): record migration audit and production blockers`。

这样性能回退、namespace 回退或 harness 问题可以分别 revert，不把所有风险藏在一个大提交。

### 10.2 develop 有新提交时

在独立 integration worktree 中显式 merge `origin/develop` 到 `board-develop-combined`，
重跑完整门禁后再推进目标分支。不要只 cherry-pick 目录看似相关的提交；PageCache、VFS、
mount lifecycle 和用户态测试经常跨目录耦合。

### 10.3 最终形态

全部门禁通过后建议：

- 给通过的目标提交打不可变验收 tag，例如 `board-lwext4-validated-YYYYMMDD`；
- 将通用块桥、inode lifetime、测试和文档通过 PR 回流 develop；
- board-specific P4/2K 配置也应最终由 develop 的平台 feature 管理，避免再形成第二套长期
  `on-board` 开发主线；
- `board-develop-combined` 保留为本轮集成历史或 release maintenance 分支；日常功能开发
  回到 develop；
- 旧 `fs/ext4` 在 journal/orphan、实板、fsck、性能和长测全部绿之前继续保留为 A/B 与
  emergency fallback。删除旧后端必须是独立提交。

## 11. 分阶段测试门禁

| Gate | 内容 | 当前状态 | 放行条件 |
|---|---|---:|---|
| G0 静态 | 冲突标记、`git diff --check`、生成文件恢复、vendored diff 审查 | 通过 | 全部 clean，`lang_items.rs` 不留生成差异 |
| G1 双架构编译 | RV64、LA64 严格串行 Docker build | 通过 | 持久记录命令、commit、container/mount、exit 0 |
| G2 块边界 ktest | 4K 常规、2K 参数化、未对齐 partition、partial tail | 双架构 ext4 7/7 | LA64 同组通过，证据新鲜 |
| G3 namespace regression | 9 个子项 + 既有 L4，QEMU status + PASS marker | 双架构 9/9；顶层 6/6 | 无 FAIL/panic/timeout |
| G4 冷一致性 | 每轮全新 fixture，workload 后正常关机、重挂载、`e2fsck -fn` | 正常 teardown + 五镜像 fsck 通过；独立重挂载待执行 | 两架构 fixture 均 clean |
| G5 兼容回归 | basic/busybox、fs_test、sparse/mmap、LTP focused、APK/Python publish | 本轮 regression/sparse 通过；其余待执行 | 逐层 GREEN，不直接跳全量 |
| G6 实板只读 | SSD identity/MBR/P1-P4、2K byte mapping、多实例、所有写节点拒绝 | 未执行 | 不发出任何 SSD write，目录和 hash 可读一致 |
| G7 crash consistency | journal/orphan/replay、故障注入、每个 crash image fsck | 未实现 | 全窗口 clean；否则生产阻塞 |
| G8 性能 A/B | §7.4 单变量矩阵 | 未执行 | 无未解释 >10% median 回退或数量级 tail |
| G9 受控 SSD 写 | 仅备份完成后，在可丢弃 scratch/P4 fixture 上 write/fsync/reboot/hash | 备份前提已完成；写入仍禁止 | G0-G8 全通过且另行确认目标 |

注意：双架构使用不同 nightly，G1-G5 的 RV64/LA64 构建必须串行，不能并发切换
`rustup override`。

## 12. SSD 备份与并行迁移边界

代码迁移、QEMU fixture 和 SSD 只读备份可以并行，因为三者不共享写入目标。但必须保持：

- 构建/回归只映射仓库和 `/tmp` disposable image，不映射 host block device；
- 备份已经发布到 `/Users/luzimo/dev/ssd-backups/2k1000la-ssd-20260718T125651Z`：原始长度
  `32,017,047,552` 字节，raw SHA-256
  `815df871d006032eec47c1fd1b44dded43ba4c2618a07bf8a1b49ae1de930b08`，zstd SHA-256
  `ea14dfabb08a9047d671eac0a300c8be8b0f5c7ad75c84b5bff1d38904ff3f95`；zstd 全流解压和
  首 1 MiB 原始数据对比均通过；
- 备份期间和本轮代码测试均未运行 `board_2k1000` 写 feature、未 remount P4 rw、未执行
  设备写探针；
- 备份完成也不是自动写入许可，仍要完成 G6 只读身份、G7 crash safety、G8 性能和受控
  scratch 目标确认。生产 P4 继续只读保护。

## 13. 完成定义

本轮迁移只有同时满足以下条件才可宣称完成：

1. `board-develop-combined` 上形成可审阅、可独立回退的提交序列；
2. 当前提交双架构编译、ktest、namespace/L4、focused FS 回归均有持久证据；
3. 全新 fixture 在 workload 后重挂载且离线 fsck clean；
4. 真实 2K1000LA 完成只读 block/partition/multi-mount 验收；
5. SSD 全盘备份完成长度、双 hash 和压缩流验证；
6. journal-enabled orphan/replay 和 rename/unlink 故障注入闭环；
7. 新旧后端在同一实板完成可重复性能 A/B；
8. 受控 scratch 写入跨 reboot/hash 通过；
9. 生成文件、Work Log、证据目录和相关架构文档同步完成；
10. 最终 tag 指向通过上述全部门禁的 commit。

在第 6 项完成前，最准确的交付语句只能是：

> `ext4_lwext4` 的正常运行 namespace/inode lifetime 已得到加强，正在作为融合候选验证；
> 当前无 journal/orphan 的板盘格式不具备所需掉电原子性，尚不可生产上 SSD。
