# ext4 长期缓存 / 写回重构计划

## TL;DR

> **快速摘要**：用 DragonOS 风格的长期正确分层替换当前 ext4 `DirtyBlockDevice` 旁路缓存：VFS 负责通用同步语义，PageCache 负责文件数据脏页，BlockCache/ext4 负责元数据写回，ext4 写路径采用 two-phase cached write，最终用 QEMU + debugfs 证明 clean sync 后真实落盘。
>
> **交付物**：
> - `fsync` / `fdatasync` / `sync` / `umount2` 通过 VFS trait 走通用写回路径，不能硬编码 ext4。
> - ext4 文件数据写入进入 PageCache dirty/writeback 路径，不再 direct write 后 invalidate。
> - ext4 元数据通过显式 BlockCache / FileSystem flush 语义落盘，不再依赖 block-device-wide dirty shim。
> - `DirtyBlockDevice` 从 ext4 正常 I/O 路径中移除或隔离为待删除兼容层。
> - rv64/la64 双架构 QEMU 持久化证据：文件数据、目录项、rename/unlink/truncate、`/bin/bash` preload 等在重启/镜像检查后仍存在。
>
> **预计工作量**：Large
> **并行执行**：YES - 4 个实施波次 + 最终验证波
> **关键路径**：T1 → T2/T3/T4/T5/T6 → T10/T11/T12/T13/T14 → T16/T18/T19 → F1-F4

---

## 背景

### 原始需求
用户发现 ext4 数据似乎没有真正写回，并指出硬编码 ext4 flush 不合理；要求参考 DragonOS 做长期主义、标准化的缓存/写回设计，不接受 `DirtyBlockDevice` 这类 hack 作为最终方案。

### 访谈摘要
**关键决策**：
- syscall/VFS 层不能识别 ext4 后调用 `flush_dirty_blocks()`。
- `DirtyBlockDevice` 是技术债，不是目标架构；它会掩盖“单次启动 read-your-writes 成功但磁盘没落”的问题。
- 目标是 DragonOS 风格：per-inode PageCache、全局 PageCache 写回入口、VFS sync/datasync/on_umount 语义、ext4 two-phase cached write。
- 当前目标不是完整 ext4 journal；目标是 clean `fsync` / `sync` / `umount` / 正常退出后的持久化正确性。

**研究发现**：
- `os/src/fs/ext4/dirty_block_device.rs`：`write_block()` 写入 `BTreeMap<block_id, Vec<u8>>`，`read_block()` 优先读 dirty map，只有显式 `flush_dirty_blocks()` 才落真实块设备。
- `os/src/syscall/fs.rs`：`sys_fsync` 当前等价 no-op；`sys_umount2` 是 fake implementation；`sys_sync` / `sys_fdatasync` 未找到或未接通。
- `os/src/fs/vfs/index_node.rs`：`sync()` / `datasync()` 默认无实际写回；ext4 当前没有完整覆盖。
- `os/src/fs/page_cache.rs`：已有 DragonOS-like PageCache 状态机，但 ext4 `write_at` 当前绕过 PageCache，写完 invalidate；`Ext4PageCacheBackend` 的 writeback 仍会走当前 `fs.block_device`，存在 double-deferral 风险。
- DragonOS：缓存位于 VFS/PageCache 层；有 PageCache registry、后台 page reclaim/writeback、`IndexNode::datasync()` → PageCache manager、`MountFS::umount()` → `on_umount()`、ext4 two-phase write。

### Metis / Oracle 结论
- Metis：计划必须明确长期架构边界，防止“补 flush 继续堆 hack”。
- Oracle Phase 1：`CHECK [5/5] PASS | VERDICT: GO`。

---

## 工作目标

### 核心目标
用 DragonOS 风格的 VFS/PageCache/BlockCache 写回架构替换当前 ext4 `DirtyBlockDevice` 旁路缓存，实现正确的通用 sync/fsync/umount 语义，并用双架构 QEMU 持久化验证证明 clean sync 后数据真实落盘。

### 具体交付物
- VFS trait 级别的 `sync` / `datasync` / `sync_fs` / `on_umount` 合同。
- syscall → fd → File → IndexNode/FileSystem 的通用同步桥接。
- PageCache 全局注册/写回入口。
- ext4 数据写入进入 PageCache dirty page 路径。
- ext4 two-phase cached write：预分配块 → 提交 i_size/必要元数据 → 写 PageCache。
- BlockCache/ext4 元数据 flush 合同。
- `DirtyBlockDevice` 从 ext4 正常路径移除或隔离为未使用兼容层。
- QEMU/debugfs 持久化验证脚本和证据。

### 完成定义
- [ ] `make rv64-kernel-build-only` 通过。
- [ ] `make la64-kernel-build-only` 通过。
- [ ] rv64 QEMU 持久化场景通过：写入 → fsync/sync/umount → 退出/重启/镜像检查 → 数据仍在。
- [ ] la64 QEMU 持久化场景通过。
- [ ] `fsync` / `sync` / `umount2` 不再假成功。
- [ ] ext4 正常数据/元数据路径不依赖 `DirtyBlockDevice` 作为写回架构。

### Must Have
- syscall/VFS 层只调用 trait，不 downcast ext4。
- clean `fsync` / `sync` / `umount` 后必须有磁盘持久化证据。
- ext4 文件数据最终由 PageCache dirty/writeback 管理。
- ext4 cached write 必须处理 extent/i_size 顺序。
- 元数据写回必须是 filesystem/block-cache 语义，不是匿名 block map。
- 每个任务必须保存 `.sisyphus/evidence/` 证据。

### Must NOT Have（护栏）
- 不允许 syscall 层出现 `if ext4 { flush_dirty_blocks() }`。
- 不允许 `DirtyBlockDevice` 作为最终优化方案。
- 不允许只验证单次启动内 read-your-writes。
- 不允许并行编译 rv64/la64。
- 不把完整 ext4 journal 纳入本计划范围。
- 不直接编辑 `lang_items.rs`。

---

## 验证策略（强制）

> **零人工干预**：所有验收必须由 agent 执行命令、QEMU、debugfs 或日志检查完成。

### 测试决策
- **测试基础设施**：YES，项目有 Makefile/QEMU/Docker 集成测试。
- **自动化测试策略**：tests-after，裸机内核不使用 `cargo test` / `cargo clippy`。
- **验证框架**：`make rv64-kernel-build-only`、`make la64-kernel-build-only`、QEMU run、`debugfs` 镜像检查、`os_test.conf` mask。
- **Agent QA**：所有任务必须有 happy path + failure/edge scenario。

### QA 证据规范
- 所有任务证据保存到 `.sisyphus/evidence/task-{N}-{slug}.txt|md|log`。
- QEMU 场景必须保存 QEMU 输出、debugfs 输出、必要时保存写回计数/日志。
- 持久化场景必须使用 fresh image，不能复用不确定状态镜像。

---

## 执行策略

### 并行波次

```text
Wave 1（基础合同 + 基线证据，立即开始）:
├── T1: 基线缓存/写回地图与持久化 harness [quick]
├── T2: VFS sync trait 合同与 File 桥接设计 [unspecified-high]
├── T3: PageCache registry / writeback API 基础 [unspecified-high]
├── T4: BlockCache 元数据 flush 合同 [unspecified-high]
├── T5: ext4 写路径职责地图与 DirtyBlockDevice 删除边界 [deep]
└── T6: fsync/fdatasync/sync/umount syscall 表面审计 [quick]

Wave 2（核心通用写回接线，依赖 Wave 1）:
├── T7: 实现通用 fsync/fdatasync/sync syscall 桥接 [unspecified-high]
├── T8: 实现真实 umount2 → MountFS::umount → on_umount/sync [unspecified-high]
├── T9: 实现 ext4 IndexNode sync/datasync/close 元数据语义 [deep]
├── T10: ext4 数据写入改走 PageCache dirty pages [deep]
├── T11: 添加 ext4 two-phase cached write 协议 [deep]
└── T12: 修正 Ext4PageCacheBackend，避免 DirtyBlockDevice double-deferral [deep]

Wave 3（移除 shim + 完整写回行为，依赖 Wave 2）:
├── T13: BlockCache 元数据 flush 集成到 FileSystem::sync_fs [unspecified-high]
├── T14: 从 ext4 正常路径移除/隔离 DirtyBlockDevice [deep]
├── T15: 添加全局 dirty PageCache 写回 / reclaim 触发 [deep]
├── T16: 添加持久化 QEMU 场景与证据捕获 [unspecified-high]
└── T17: busybox/preload 性能回归护栏，不依赖写回 hack [unspecified-high]

Wave 4（跨架构硬化，依赖 Wave 3）:
├── T18: rv64 集成通过与镜像持久化审计 [unspecified-high]
├── T19: la64 集成通过与镜像持久化审计 [unspecified-high]
├── T20: 更新写回架构文档与限制说明 [writing]
└── T21: 清理 DirtyBlockDevice 过期引用和 stale 注释 [quick]

Wave FINAL（所有任务后，4 个 review 并行）:
├── F1: 计划合规审计 (oracle)
├── F2: 代码质量审查 (unspecified-high)
├── F3: 真实手工 QA 执行 (unspecified-high)
└── F4: 范围忠实度检查 (deep)
```

### 依赖矩阵

- **1**: 无依赖；阻塞 7, 8, 16, 18, 19
- **2**: 无依赖；阻塞 7, 8, 9, 13
- **3**: 无依赖；阻塞 10, 12, 15
- **4**: 无依赖；阻塞 9, 13, 14
- **5**: 无依赖；阻塞 10, 11, 12, 14
- **6**: 无依赖；阻塞 7, 8
- **7**: 依赖 2, 6；阻塞 16, 18, 19
- **8**: 依赖 2, 6；阻塞 16, 18, 19
- **9**: 依赖 2, 4；阻塞 13, 16
- **10**: 依赖 3, 5；阻塞 11, 12, 15, 16
- **11**: 依赖 5, 10；阻塞 12, 16
- **12**: 依赖 3, 5, 10, 11；阻塞 14, 16
- **13**: 依赖 4, 9；阻塞 14, 16
- **14**: 依赖 5, 12, 13；阻塞 17, 18, 19, 21
- **15**: 依赖 3, 10；阻塞 18, 19
- **16**: 依赖 1, 7, 8, 9, 10, 11, 12, 13；阻塞 18, 19
- **17**: 依赖 14, 16；阻塞 18, 19
- **18**: 依赖 14, 15, 16, 17；阻塞 F1-F4
- **19**: 依赖 14, 15, 16, 17；阻塞 F1-F4
- **20**: 依赖 14, 15, 16；阻塞 F1-F4
- **21**: 依赖 14；阻塞 F1-F4

### Agent 分派摘要
- Wave 1：6 个任务 — T1/T6 `quick`，T2/T3/T4 `unspecified-high`，T5 `deep`
- Wave 2：6 个任务 — T7/T8 `unspecified-high`，T9-T12 `deep`
- Wave 3：5 个任务 — T13/T16/T17 `unspecified-high`，T14/T15 `deep`
- Wave 4：4 个任务 — T18/T19 `unspecified-high`，T20 `writing`，T21 `quick`
- FINAL：F1 `oracle`，F2/F3 `unspecified-high`，F4 `deep`

---

## TODOs

> 实现 + 测试 = 一个任务，不能拆开。每个任务必须有 Agent Profile、并行信息、引用、验收标准和 QA 场景。

- [x] 1. 基线缓存/写回地图与持久化 harness

  **要做什么**：
  - 生成 `.sisyphus/evidence/task-1-baseline.md`，记录当前 DirtyBlockDevice、PageCache、BlockCache、syscall sync 空洞的调用链。
  - 建立 fresh image QEMU + debugfs 的 rv64/la64 持久化检查流程。
  - 明确区分“单次启动内可读”和“真实磁盘落盘”。

  **不能做**：不改内核行为；不把一次启动内读回当作持久化证明。

  **推荐 Agent Profile**：
  - **Category**: `quick` — 证据采集和基线报告。
  - **Skills**: [] — 无匹配技能。
  - **Skills Evaluated but Omitted**: `playwright` — 无浏览器 UI。

  **并行信息**：Wave 1；可并行；阻塞 T7/T8/T16/T18/T19；无前置依赖。

  **引用**：
  - `os/src/fs/ext4/dirty_block_device.rs:21-78` — 当前 dirty map block wrapper。
  - `os/src/fs/ext4/ext4fs.rs:23-66` — ext4 block device / dirty_bd 字段。
  - `os/src/fs/mod.rs:382-517` — preload 阶段当前同步点。
  - `os/src/syscall/fs.rs:1205-1214` — 当前 `sys_fsync` no-op。
  - `os/src/syscall/fs.rs:1425-1441` — 当前 `sys_umount2` fake。
  - `AGENTS.md` — Docker/双架构串行验证规则。

  **验收标准**：
  - [ ] baseline 报告包含 DirtyBlockDevice、PageCache、BlockCache、fsync、umount2、debugfs/QEMU 流程。
  - [ ] rv64 和 la64 fresh image 检查命令明确。
  - [ ] 报告说明 read-your-writes 假阳性风险。

  **QA 场景**：
  ```text
  Scenario: 基线报告存在并覆盖写回空洞
    Tool: Bash
    Preconditions: baseline 报告已生成。
    Steps:
      1. test -s .sisyphus/evidence/task-1-baseline.md
      2. grep -E 'DirtyBlockDevice|sys_fsync|sys_umount2|debugfs|QEMU' .sisyphus/evidence/task-1-baseline.md
    Expected Result: 两条命令均 exit 0。
    Evidence: .sisyphus/evidence/task-1-baseline-check.txt

  Scenario: 拒绝单次启动读回作为持久化证明
    Tool: Bash
    Preconditions: baseline 报告已生成。
    Steps:
      1. grep -E 'read-your-writes|single boot|reboot|remount|debugfs' .sisyphus/evidence/task-1-baseline.md
      2. 确认报告要求 reboot/remount/debugfs。
    Expected Result: 报告明确标记假阳性风险。
    Evidence: .sisyphus/evidence/task-1-false-positive-guard.txt
  ```

  **证据**：`task-1-baseline.md`、`task-1-baseline-check.txt`、`task-1-false-positive-guard.txt`

  **Commit**: NO

- [x] 2. VFS sync trait 合同与 File 桥接设计

  **要做什么**：
  - 明确/修正 `IndexNode::sync`、`IndexNode::datasync`、`FileSystem::sync_fs`、`FileSystem::on_umount` 的通用合同。
  - 如果缺失，添加 `File` 层 sync/datasync 桥接，使 syscall 能通过 fd 调用通用同步。
  - persistent inode/filesystem 必须显式覆盖；无持久化对象才允许 no-op。

  **不能做**：不能在 syscall/VFS 中 downcast ext4；不能让 persistent dirty data 静默 no-op。

  **推荐 Agent Profile**：
  - **Category**: `unspecified-high` — trait 变更影响多文件系统。
  - **Skills**: []。
  - **Skills Evaluated but Omitted**: `playwright` — 无浏览器 UI。

  **并行信息**：Wave 1；可并行；阻塞 T7/T8/T9/T13；无前置依赖。

  **引用**：
  - `os/src/fs/vfs/index_node.rs:31-349` — inode trait 默认 sync/datasync。
  - `os/src/fs/vfs/file_system.rs:85-125` — filesystem sync/on_umount 位置。
  - `os/src/fs/vfs/file.rs:1187-1191` — File drop/close 路径。
  - `os/src/fs/vfs/mount.rs:480-494` — MountFS umount hook。
  - DragonOS `kernel/src/filesystem/vfs/mod.rs` — `datasync()` → PageCache manager 模式。

  **验收标准**：
  - [ ] syscall 可通过 fd 调用通用 File/IndexNode sync。
  - [ ] persistent filesystem 可通过 `sync_fs` / `on_umount` 写回。
  - [ ] ext4 不需要被 syscall/VFS 特判。
  - [ ] rv64/la64 编译通过。

  **QA 场景**：
  ```text
  Scenario: 通用 sync bridge 不含 ext4 特判
    Tool: Bash
    Preconditions: 实现完成。
    Steps:
      1. 搜索 os/src/syscall 和 os/src/fs/vfs 中的 downcast_ref::<Ext4FileSystem>。
      2. 搜索 syscall fs 路径是否调用 File/IndexNode sync/datasync。
      3. 运行 rv64 kernel build。
    Expected Result: syscall/VFS 无 ext4 downcast；rv64 build 通过。
    Evidence: .sisyphus/evidence/task-2-generic-sync-bridge.txt

  Scenario: ext4 不再依赖默认 no-op sync
    Tool: Bash
    Preconditions: 实现完成。
    Steps:
      1. 搜索 ext4 inode/filesystem 的 sync/datasync/sync_fs 覆盖。
      2. 运行 la64 kernel build。
    Expected Result: ext4 有显式写回 hook；la64 build 通过。
    Evidence: .sisyphus/evidence/task-2-ext4-overrides.txt
  ```

  **证据**：`task-2-generic-sync-bridge.txt`、`task-2-ext4-overrides.txt`

  **Commit**: YES — `refactor(vfs): define generic sync contracts`

- [x] 3. PageCache registry / writeback API 基础

  **要做什么**：
  - 建立 DragonOS-like 的 active PageCache 注册/枚举能力，或等价的全局 dirty PageCache flush 入口。
  - 暴露 `writeback_all` / `writeback_range` / global dirty flush API。
  - 规定锁顺序：全局 registry 锁不能跨块 I/O。

  **不能做**：不能在持有全局/VFS/inode 锁时执行长时间块 I/O；PageCache 不得依赖 DirtyBlockDevice。

  **推荐 Agent Profile**：
  - **Category**: `unspecified-high` — 缓存基础设施和并发敏感。
  - **Skills**: []。
  - **Skills Evaluated but Omitted**: `playwright`。

  **并行信息**：Wave 1；可并行；阻塞 T10/T12/T15；无前置依赖。

  **引用**：
  - `os/src/fs/page_cache.rs:149-509` — PageCache 状态机和写回方法。
  - `os/src/fs/page_cache.rs:712-791` — Ext4PageCacheBackend。
  - `os/src/fs/ext4/layout.rs:53-117` — ext4 per-inode lazy PageCache。
  - DragonOS `kernel/src/filesystem/page_cache.rs` — PageCache registry。
  - DragonOS `kernel/src/mm/page.rs` — 写回/回收线程锁顺序。

  **验收标准**：
  - [ ] 有通用 active PageCache 枚举或全局 flush 入口。
  - [ ] 写回错误可返回/记录，不静默丢弃。
  - [ ] 证据说明不持 registry 锁执行 I/O。
  - [ ] rv64/la64 编译通过。

  **QA 场景**：
  ```text
  Scenario: 全局 PageCache flush 不依赖 DirtyBlockDevice
    Tool: Bash
    Preconditions: 实现完成。
    Steps:
      1. 搜索 page_cache.rs 中 registry/flush 入口。
      2. 搜索 generic PageCache 是否 import/use dirty_block_device。
      3. 运行 rv64 kernel build。
    Expected Result: registry/flush 存在；无 DirtyBlockDevice 依赖；build 通过。
    Evidence: .sisyphus/evidence/task-3-pagecache-registry.txt

  Scenario: 写回失败可观测
    Tool: Bash
    Preconditions: 实现完成。
    Steps:
      1. 检查 writeback API 是否返回 Result 或有错误传播。
      2. 搜索 `let _ = pc.writeback_all()` 并分类剩余忽略点。
    Expected Result: 关键写回路径不静默吞错。
    Evidence: .sisyphus/evidence/task-3-writeback-errors.txt
  ```

  **证据**：`task-3-pagecache-registry.txt`、`task-3-writeback-errors.txt`

  **Commit**: YES — `refactor(cache): add global pagecache writeback entrypoints`

- [x] 4. BlockCache 元数据 flush 合同

  **要做什么**：
  - 定义现有 `BlockCacheManager` 如何 flush dirty metadata blocks 到真实块设备。
  - 给 ext4 `sync_fs` / `on_umount` 提供显式 metadata flush API。
  - 避免跨等待/块 I/O 持锁。

  **不能做**：不能把文件数据和元数据都塞进匿名 dirty BTreeMap；不能把 BufferCache eviction 等同于 filesystem sync，除非证明确实写到真实设备。

  **推荐 Agent Profile**：
  - **Category**: `unspecified-high` — 元数据缓存语义影响文件系统正确性。
  - **Skills**: []。
  - **Skills Evaluated but Omitted**: `playwright`。

  **并行信息**：Wave 1；可并行；阻塞 T9/T13/T14；无前置依赖。

  **引用**：
  - `os/src/fs/cache.rs:46-191` — BufferCache/BlockCacheManager dirty/evict。
  - `os/src/fs/ext4/ext4_inode.rs:642-671` — inode metadata writeback。
  - `os/src/fs/ext4/block_group.rs:502-522` — block sync-to-disk wrapper。
  - `os/src/drivers/block/block_dev.rs:8-40` — BlockDevice 真实边界。

  **验收标准**：
  - [ ] BlockCache metadata flush API 存在并写真实设备。
  - [ ] ext4 可在 filesystem sync 中调用 metadata flush。
  - [ ] 证据说明 metadata 与 file data 的抽象边界。
  - [ ] rv64/la64 编译通过。

  **QA 场景**：
  ```text
  Scenario: metadata flush 是 filesystem-owned
    Tool: Bash
    Preconditions: 实现完成。
    Steps:
      1. 搜索 cache.rs/ext4 中 metadata flush API。
      2. 搜索新 metadata sync 路径是否依赖 DirtyBlockDevice。
      3. 运行 rv64 kernel build。
    Expected Result: metadata flush 显式存在；无 DirtyBlockDevice 依赖；build 通过。
    Evidence: .sisyphus/evidence/task-4-metadata-flush.txt

  Scenario: 创建目录后的元数据持久化
    Tool: QEMU + debugfs
    Preconditions: fresh ext4 image。
    Steps:
      1. Boot QEMU，创建目录和文件。
      2. 调用 sync/fsync。
      3. 退出后用 debugfs stat/ls 目录和文件。
    Expected Result: 目录项和 inode 元数据在 debugfs 中存在。
    Evidence: .sisyphus/evidence/task-4-dir-metadata-persistence.txt
  ```

  **证据**：`task-4-metadata-flush.txt`、`task-4-dir-metadata-persistence.txt`

  **Commit**: YES — `refactor(cache): expose metadata flush contract`

- [x] 5. ext4 写路径职责地图与 DirtyBlockDevice 删除边界

  **要做什么**：
  - 枚举 ext4 当前所有数据/元数据写路径，并为每条路径指定新 owner：文件数据归 PageCache，元数据归 ext4/BlockCache flush，最终 I/O 归真实 BlockDevice。
  - 定义 `DirtyBlockDevice` 从正常路径移除的精确边界和顺序。

  **不能做**：不能留下“双缓存都能写”的模糊职责；不能在替代路径完成前破坏 ext4 可启动性。

  **推荐 Agent Profile**：
  - **Category**: `deep` — 需要完整 ext4 调用图推理。
  - **Skills**: []。
  - **Skills Evaluated but Omitted**: `playwright`。

  **并行信息**：Wave 1；可并行；阻塞 T10/T11/T12/T14；无前置依赖。

  **引用**：
  - `os/src/fs/ext4/ext4fs.rs:23-66` — 当前 block_device/dirty_bd 字段。
  - `os/src/fs/ext4/file.rs:385-617` — ext4 direct file I/O。
  - `os/src/fs/ext4/ext4fs.rs:382-434` — read 走 PageCache、write 绕过 PageCache。
  - `os/src/fs/page_cache.rs:712-791` — Ext4PageCacheBackend。
  - `os/src/fs/ext4/dirty_block_device.rs:21-78` — 待删除 shim。

  **验收标准**：
  - [ ] `.sisyphus/evidence/task-5-ext4-write-ownership.md` 列出所有 ext4 写路径和新 owner。
  - [ ] 每个 DirtyBlockDevice 使用点都被分类为删除、隔离或临时依赖。
  - [ ] 后续任务不能实现未分类写路径。

  **QA 场景**：
  ```text
  Scenario: DirtyBlockDevice 使用点全部分类
    Tool: Bash
    Preconditions: 职责地图已生成。
    Steps:
      1. 搜索 DirtyBlockDevice 和 flush_dirty_blocks 的所有引用。
      2. 与 task-5-ext4-write-ownership.md 对照。
    Expected Result: 每个引用都有删除/隔离/临时依赖分类。
    Evidence: .sisyphus/evidence/task-5-dirtybd-classification.txt

  Scenario: 数据和元数据 owner 分离
    Tool: Bash
    Preconditions: 职责地图已生成。
    Steps:
      1. grep '文件数据\|元数据\|真实 BlockDevice' task-5-ext4-write-ownership.md。
      2. 确认每节都有具体文件/函数。
    Expected Result: 三类职责边界清晰。
    Evidence: .sisyphus/evidence/task-5-ownership-boundaries.txt
  ```

  **证据**：`task-5-ext4-write-ownership.md`、`task-5-dirtybd-classification.txt`、`task-5-ownership-boundaries.txt`

  **Commit**: NO

- [x] 6. fsync/fdatasync/sync/umount syscall 表面审计

  **要做什么**：
  - 审计 syscall ID、dispatch、name mapping 和实现，覆盖 `fsync`、`fdatasync`、`sync`、`syncfs`（如有）、`umount2`。
  - 明确哪些 syscall 必须新增/接线，哪些按项目范围返回合理错误。

  **不能做**：不能把 persistence syscall 静默假成功；不能写 ext4 特判。

  **推荐 Agent Profile**：
  - **Category**: `quick` — syscall 表审计明确且局部。
  - **Skills**: []。
  - **Skills Evaluated but Omitted**: `playwright`。

  **并行信息**：Wave 1；可并行；阻塞 T7/T8；无前置依赖。

  **引用**：
  - `os/src/syscall/fs.rs:1205-1214` — `sys_fsync` no-op。
  - `os/src/syscall/fs.rs:1425-1441` — `sys_umount2` fake。
  - `os/src/syscall/mod.rs` — syscall dispatch。
  - `os/src/syscall/syscall_id.rs` — syscall 编号。

  **验收标准**：
  - [ ] `.sisyphus/evidence/task-6-sync-syscall-audit.md` 记录所有 sync-like syscall 状态。
  - [ ] invalid fd / unsupported target 的 errno 行为被指定。
  - [ ] 审计不认可 silent success。

  **QA 场景**：
  ```text
  Scenario: syscall 审计覆盖 dispatch 和 ID
    Tool: Bash
    Preconditions: 审计报告已生成。
    Steps:
      1. grep 'fsync\|fdatasync\|sync\|umount2\|syscall_id\|dispatch' task-6-sync-syscall-audit.md。
      2. 保存源码搜索结果。
    Expected Result: 报告与源码搜索一致。
    Evidence: .sisyphus/evidence/task-6-syscall-audit-check.txt

  Scenario: 审计不允许 silent persistence success
    Tool: Bash
    Preconditions: 审计报告已生成。
    Steps:
      1. grep 'no-op\|fake\|silent' task-6-sync-syscall-audit.md。
      2. 确认每项被标为待修复或非持久化目标。
    Expected Result: 无“假成功可接受”结论。
    Evidence: .sisyphus/evidence/task-6-no-silent-success.txt
  ```

  **证据**：`task-6-sync-syscall-audit.md`、`task-6-syscall-audit-check.txt`、`task-6-no-silent-success.txt`

  **Commit**: NO

- [x] 7. 实现通用 fsync/fdatasync/sync syscall 桥接

  **要做什么**：
  - 将 `sys_fsync` 从 no-op 改为 fd → File → IndexNode sync/datasync → PageCache/BlockCache/FileSystem writeback。
  - 按审计结果新增/接通 `fdatasync` 和 `sync`。
  - 写回失败必须传播为负 errno。

  **不能做**：不能在 syscall 调 `DirtyBlockDevice::flush_dirty_blocks()`；不能 downcast ext4。

  **推荐 Agent Profile**：
  - **Category**: `unspecified-high` — syscall 和 VFS 语义影响面广。
  - **Skills**: []。
  - **Skills Evaluated but Omitted**: `playwright`。

  **并行信息**：Wave 2；可与 T8-T12 并行；依赖 T2/T6；阻塞 T16/T18/T19。

  **引用**：
  - `os/src/syscall/fs.rs:1205-1214` — 替换 no-op。
  - `os/src/fs/vfs/file.rs` — File bridge。
  - `os/src/fs/vfs/index_node.rs` — sync/datasync target。
  - `os/src/fs/vfs/file_system.rs` — whole-filesystem sync target。

  **验收标准**：
  - [ ] `fsync(valid_fd)` 触发通用写回，成功才返回 0。
  - [ ] `fsync(invalid_fd)` 返回负 errno。
  - [ ] `sync()` 如实现，则通过 trait flush 所有相关 filesystem。
  - [ ] rv64/la64 编译通过。

  **QA 场景**：
  ```text
  Scenario: fsync 后文件数据可被 debugfs 看到
    Tool: QEMU + debugfs
    Preconditions: fresh ext4 image。
    Steps:
      1. Boot rv64 QEMU，创建 /tmp/fsync-proof.txt，内容 fsync-proof-2026。
      2. 对 fd 调用 fsync。
      3. 退出 QEMU，用 debugfs dump/stat 文件。
    Expected Result: debugfs 看到文件和精确内容。
    Evidence: .sisyphus/evidence/task-7-fsync-persistence-rv64.txt

  Scenario: invalid fd 不触发假成功
    Tool: QEMU
    Preconditions: syscall bridge 已实现。
    Steps:
      1. 调用 fsync(-1) 或不存在 fd。
      2. 捕获返回值和 kernel log。
    Expected Result: 返回负 errno，无 panic，无全局误 flush。
    Evidence: .sisyphus/evidence/task-7-fsync-invalid-fd.txt
  ```

  **证据**：`task-7-fsync-persistence-rv64.txt`、`task-7-fsync-invalid-fd.txt`

  **Commit**: YES — `fix(fs): route fsync through vfs writeback`

- [x] 8. 实现真实 umount2 → MountFS::umount → on_umount/sync

  **要做什么**：
  - 将 fake `sys_umount2` 接到真实 VFS unmount 路径。
  - `MountFS::umount()` 成功时触发 `on_umount()` / `sync_fs()`。
  - busy/invalid target 返回错误，不能假成功。

  **不能做**：不能 skip ext4 sync；不能 unsupported 也 success。

  **推荐 Agent Profile**：
  - **Category**: `unspecified-high` — mount 生命周期影响所有 filesystem。
  - **Skills**: []。
  - **Skills Evaluated but Omitted**: `playwright`。

  **并行信息**：Wave 2；可与 T7/T9-T12 并行；依赖 T2/T6；阻塞 T16/T18/T19。

  **引用**：
  - `os/src/syscall/fs.rs:1425-1441` — fake 实现。
  - `os/src/fs/vfs/mount.rs:422-559` — MountFS/MountList。
  - `os/src/fs/vfs/mount.rs:480-494` — umount hook。
  - `os/src/fs/vfs/file_system.rs:85-125` — filesystem hook。

  **验收标准**：
  - [ ] `sys_umount2` 不再输出 fake implementation 后返回成功。
  - [ ] 成功 umount 会触发通用 filesystem sync。
  - [ ] invalid/busy target 返回负 errno。
  - [ ] rv64/la64 编译通过。

  **QA 场景**：
  ```text
  Scenario: umount 触发 generic filesystem sync
    Tool: QEMU + debugfs
    Preconditions: kernel 支持相应 mount/unmount 场景。
    Steps:
      1. Boot QEMU，创建 /tmp/umount-proof.txt。
      2. 调用 umount2 对应挂载点。
      3. 退出后 debugfs 检查文件。
    Expected Result: umount 成功后文件真实落盘。
    Evidence: .sisyphus/evidence/task-8-umount-sync.txt

  Scenario: invalid umount target 显式失败
    Tool: QEMU
    Preconditions: 实现完成。
    Steps:
      1. 调用 umount2('/definitely-not-mounted', 0)。
      2. 捕获返回值和日志。
    Expected Result: 负 errno，无 fake success，无 panic。
    Evidence: .sisyphus/evidence/task-8-invalid-umount.txt
  ```

  **证据**：`task-8-umount-sync.txt`、`task-8-invalid-umount.txt`

  **Commit**: YES — `fix(vfs): sync filesystems during umount`

- [x] 9. 实现 ext4 IndexNode sync/datasync/close 元数据语义

  **要做什么**：
  - 为 ext4 inode 明确 `datasync()`：至少写回文件数据 PageCache。
  - 为 ext4 inode 明确 `sync()`：在 datasync 基础上写回必要 inode/目录/大小/mtime 等元数据。
  - close/drop 路径不能只 `let _ = writeback_all()` 后吞错；需要清晰的错误处理或日志策略。

  **不能做**：不能把 close 当作唯一持久化机制；不能继续依赖 DirtyBlockDevice 兜底。

  **推荐 Agent Profile**：
  - **Category**: `deep` — ext4 inode 元数据和 VFS 语义耦合复杂。
  - **Skills**: []。
  - **Skills Evaluated but Omitted**: `playwright`。

  **并行信息**：Wave 2；可与 T7/T8/T10-T12 并行；依赖 T2/T4；阻塞 T13/T16。

  **引用**：
  - `os/src/fs/ext4/ext4fs.rs:382-434` — Ext4OSInode read/write。
  - `os/src/fs/ext4/layout.rs:82-88` — drop writeback 现状。
  - `os/src/fs/ext4/ext4_inode.rs:642-671` — inode metadata writeback。
  - DragonOS `kernel/src/filesystem/ext4/inode.rs` — metadata_dirty / close-time metadata update 模式。

  **验收标准**：
  - [ ] ext4 inode 显式实现 sync/datasync 或等价 trait hook。
  - [ ] sync 写回数据和必要元数据；datasync 不遗漏文件内容。
  - [ ] close/drop 不作为唯一保证，且不静默吞关键错误。
  - [ ] rv64/la64 编译通过。

  **QA 场景**：
  ```text
  Scenario: ext4 fsync 更新 size 和数据
    Tool: QEMU + debugfs
    Preconditions: fresh ext4 image。
    Steps:
      1. 创建 /tmp/sync-size.txt 并写入 8193 字节固定模式。
      2. 调用 fsync(fd)，退出 QEMU。
      3. debugfs stat 文件大小并 dump 内容 hash。
    Expected Result: size=8193，内容 hash 与写入模式一致。
    Evidence: .sisyphus/evidence/task-9-ext4-sync-size-data.txt

  Scenario: datasync 不依赖 close 才可见
    Tool: QEMU + debugfs
    Preconditions: datasync/fdatasync 路径可触发。
    Steps:
      1. 写入文件后调用 fdatasync 或 datasync 等价路径。
      2. 不依赖正常 close 成功，触发 clean exit/image check。
      3. debugfs 检查内容。
    Expected Result: 数据已落盘；无仅 close 触发的假通过。
    Evidence: .sisyphus/evidence/task-9-datasync-before-close.txt
  ```

  **证据**：`task-9-ext4-sync-size-data.txt`、`task-9-datasync-before-close.txt`

  **Commit**: YES — `fix(ext4): implement inode sync semantics`

- [x] 10. ext4 数据写入改走 PageCache dirty pages

  **要做什么**：
  - 将 ext4 regular file write path 从 direct write + invalidate 改为 PageCache write，标记 dirty page。
  - read path 继续通过 PageCache 保证缓存一致性。
  - direct I/O fallback 仅作为无 PageCache 或特殊对象路径。

  **不能做**：不能写完 PageCache 后再通过 DirtyBlockDevice 才“落盘成功”；不能破坏稀疏文件读 hole 填零语义。

  **推荐 Agent Profile**：
  - **Category**: `deep` — 文件数据路径、PageCache、extent 映射联动。
  - **Skills**: []。
  - **Skills Evaluated but Omitted**: `playwright`。

  **并行信息**：Wave 2；可与 T7-T9/T11/T12 并行；依赖 T3/T5；阻塞 T11/T12/T15/T16。

  **引用**：
  - `os/src/fs/ext4/ext4fs.rs:411-434` — 当前 direct write + invalidate。
  - `os/src/fs/page_cache.rs:149-509` — PageCache write/read/writeback。
  - `os/src/fs/page_cache.rs:712-791` — Ext4PageCacheBackend。
  - DragonOS `kernel/src/filesystem/ext4/inode.rs` — cached write 模式。

  **验收标准**：
  - [ ] ext4 regular file write 使用 PageCache dirty path。
  - [ ] 写后读命中 PageCache 或一致数据，不依赖 DirtyBlockDevice。
  - [ ] range invalidate 只用于明确需要的场景，不是写路径默认行为。
  - [ ] rv64/la64 编译通过。

  **QA 场景**：
  ```text
  Scenario: 写后读一致且经过 PageCache
    Tool: QEMU + 日志/证据
    Preconditions: 可开启必要 cache trace 或统计。
    Steps:
      1. 写入 /tmp/pagecache-write.txt 固定内容。
      2. 立即 read back 并比对内容。
      3. 捕获日志/计数证明走 PageCache write path。
    Expected Result: 内容一致，证据显示写路径进入 PageCache。
    Evidence: .sisyphus/evidence/task-10-pagecache-write-read.txt

  Scenario: 跨页写入后 fsync 持久化
    Tool: QEMU + debugfs
    Preconditions: PageCache write path 已接通。
    Steps:
      1. 写入超过 2 页的数据到 /tmp/cross-page.bin。
      2. fsync 后退出 QEMU。
      3. debugfs dump 并校验大小/hash。
    Expected Result: debugfs 中数据完整。
    Evidence: .sisyphus/evidence/task-10-cross-page-persistence.txt
  ```

  **证据**：`task-10-pagecache-write-read.txt`、`task-10-cross-page-persistence.txt`

  **Commit**: YES — `refactor(ext4): route data writes through pagecache`

- [x] 11. 添加 ext4 two-phase cached write 协议

  **要做什么**：
  - 在 PageCache 写入 dirty data 之前，先确保目标逻辑范围有磁盘块/extent 映射。
  - 扩展文件时先提交 i_size/必要 inode metadata，再允许后台 writeback 查 extent。
  - 处理分配失败时的回滚/错误返回，不能留下 inode size 指向未初始化数据。

  **不能做**：不能先写 dirty page，再让 writeback 发现 extent 不存在；不能 panic 处理 ENOSPC/ENOMEM。

  **推荐 Agent Profile**：
  - **Category**: `deep` — ext4 正确性关键路径。
  - **Skills**: []。
  - **Skills Evaluated but Omitted**: `playwright`。

  **并行信息**：Wave 2；可与 T7-T10/T12 并行但实现依赖 T10；阻塞 T12/T16。

  **引用**：
  - `os/src/fs/ext4/file.rs:385-617` — 当前 another_ext4 write/read 逻辑。
  - `os/src/fs/ext4/ext4fs.rs:411-434` — VFS ext4 write_at。
  - `os/src/fs/ext4/extent.rs` — extent 分配/查找逻辑。
  - DragonOS `kernel/src/filesystem/ext4/inode.rs` — allocate_blocks_for_write → commit_inode_size → PageCache::write。

  **验收标准**：
  - [ ] 扩展写入前完成块/extent 预分配。
  - [ ] i_size 提交顺序保证 writeback 能解析映射。
  - [ ] ENOSPC/ENOMEM 返回错误而非 panic/半提交。
  - [ ] rv64/la64 编译通过。

  **QA 场景**：
  ```text
  Scenario: 扩展文件后后台/显式 writeback 不读 hole
    Tool: QEMU + debugfs
    Preconditions: two-phase write 已实现。
    Steps:
      1. 从空文件写入 3 页以上数据。
      2. 触发 sync/writeback。
      3. debugfs stat/dump 校验 size 和内容。
    Expected Result: 无 EIO/hole 零填充误写，内容完整。
    Evidence: .sisyphus/evidence/task-11-two-phase-extend.txt

  Scenario: 分配失败路径不半提交
    Tool: QEMU
    Preconditions: 可用小镜像或受控 ENOSPC 场景。
    Steps:
      1. 构造接近满盘写入。
      2. 捕获 write/fsync 返回。
      3. 检查文件大小和目录项不出现不一致。
    Expected Result: 返回负 errno；无 panic；无 size 指向未分配数据。
    Evidence: .sisyphus/evidence/task-11-enospc-no-half-commit.txt
  ```

  **证据**：`task-11-two-phase-extend.txt`、`task-11-enospc-no-half-commit.txt`

  **Commit**: YES — `fix(ext4): add two phase cached write protocol`

- [x] 12. 修正 Ext4PageCacheBackend，避免 DirtyBlockDevice double-deferral

  **要做什么**：
  - 让 `Ext4PageCacheBackend::write_page` 最终写到正确的真实 block I/O 路径，而不是再次进入 DirtyBlockDevice。
  - 明确 read_page 对 sparse/hole 的处理：hole 读零，真实映射读块。
  - 写回时必须传播错误。

  **不能做**：不能让 PageCache writeback “成功”但实际只写入另一个内存 dirty map。

  **推荐 Agent Profile**：
  - **Category**: `deep` — PageCache backend 是核心落盘边界。
  - **Skills**: []。
  - **Skills Evaluated but Omitted**: `playwright`。

  **并行信息**：Wave 2；依赖 T3/T5/T10/T11；阻塞 T14/T16。

  **引用**：
  - `os/src/fs/page_cache.rs:712-791` — Ext4PageCacheBackend。
  - `os/src/fs/ext4/ext4fs.rs:23-66` — 当前 block_device 指向。
  - `os/src/fs/ext4/dirty_block_device.rs:21-78` — double-deferral 风险来源。

  **验收标准**：
  - [ ] Ext4PageCacheBackend writeback 不依赖 DirtyBlockDevice。
  - [ ] sparse/hole read 行为有明确实现和测试。
  - [ ] writeback 错误可观测并向上返回。
  - [ ] rv64/la64 编译通过。

  **QA 场景**：
  ```text
  Scenario: PageCache writeback 后 debugfs 可见
    Tool: QEMU + debugfs
    Preconditions: backend 修正完成。
    Steps:
      1. 写入 /tmp/backend-proof.bin 并触发 PageCache writeback。
      2. 退出 QEMU 后 debugfs dump 文件。
    Expected Result: 文件真实存在且内容正确。
    Evidence: .sisyphus/evidence/task-12-backend-real-writeback.txt

  Scenario: sparse/hole 读零不写脏假块
    Tool: QEMU
    Preconditions: 支持 sparse read 场景。
    Steps:
      1. 创建带 hole 的文件。
      2. 读取 hole 区域。
      3. 检查返回全零且未产生不必要脏块写。
    Expected Result: hole 返回零；无异常 dirty writeback。
    Evidence: .sisyphus/evidence/task-12-sparse-hole-read.txt
  ```

  **证据**：`task-12-backend-real-writeback.txt`、`task-12-sparse-hole-read.txt`

  **Commit**: YES — `fix(ext4): make pagecache backend write through real io`

- [x] 13. BlockCache 元数据 flush 集成到 FileSystem::sync_fs

  **要做什么**：
  - 将 T4 的 metadata flush API 接入 ext4 `FileSystem::sync_fs` 和 `on_umount`。
  - 确保 inode table、bitmap、block group、directory block 等必要元数据在 clean sync 后落盘。
  - 保守阶段允许全 FS metadata flush，但必须走通用 filesystem hook。

  **不能做**：不能通过 syscall 或外层 mount 直接调用 ext4 私有 flush；不能依赖 DirtyBlockDevice。

  **推荐 Agent Profile**：
  - **Category**: `unspecified-high` — filesystem 级 flush 集成。
  - **Skills**: []。
  - **Skills Evaluated but Omitted**: `playwright`。

  **并行信息**：Wave 3；依赖 T4/T9；阻塞 T14/T16。

  **引用**：
  - `os/src/fs/ext4/ext4fs.rs:706-757` — Ext4FileSystem trait 实现。
  - `os/src/fs/cache.rs:46-191` — metadata cache flush 来源。
  - `os/src/fs/vfs/file_system.rs:85-125` — `sync_fs` / `on_umount` 合同。

  **验收标准**：
  - [ ] ext4 `sync_fs` 调用 metadata flush 和必要 PageCache/global flush。
  - [ ] `on_umount` 覆盖或默认调用 `sync_fs`。
  - [ ] QEMU/debugfs 证明 create/unlink/rename/truncate 后 sync 落盘。

  **QA 场景**：
  ```text
  Scenario: create 后 sync_fs 持久化目录项
    Tool: QEMU + debugfs
    Preconditions: sync_fs 已接 metadata flush。
    Steps:
      1. 创建 /tmp/meta-create.txt。
      2. 调用 sync。
      3. debugfs ls/stat 文件。
    Expected Result: 文件目录项和 inode 均存在。
    Evidence: .sisyphus/evidence/task-13-create-syncfs.txt

  Scenario: unlink 后 sync_fs 持久化删除
    Tool: QEMU + debugfs
    Preconditions: sync_fs 已接 metadata flush。
    Steps:
      1. 创建并 sync /tmp/meta-unlink.txt。
      2. unlink 后再次 sync。
      3. debugfs 确认目录项不存在。
    Expected Result: 删除在镜像中持久化。
    Evidence: .sisyphus/evidence/task-13-unlink-syncfs.txt
  ```

  **证据**：`task-13-create-syncfs.txt`、`task-13-unlink-syncfs.txt`

  **Commit**: YES — `fix(ext4): flush metadata through filesystem sync`

- [x] 14. 从 ext4 正常路径移除/隔离 DirtyBlockDevice

  **要做什么**：
  - 修改 ext4 打开/初始化路径，使正常 data/metadata I/O 不再包一层 DirtyBlockDevice。
  - 删除 DirtyBlockDevice 或隔离到未使用/实验模块，并确保编译路径不依赖它。
  - 清理 `flush_dirty_blocks` 作为正常 writeback 机制的调用链。

  **不能做**：不能留下“仍然启用但没人知道”的 dirty shim；不能用性能回退作为保留 hack 的理由。

  **推荐 Agent Profile**：
  - **Category**: `deep` — 这是架构切换关键点。
  - **Skills**: []。
  - **Skills Evaluated but Omitted**: `playwright`。

  **并行信息**：Wave 3；依赖 T5/T12/T13；阻塞 T17/T18/T19/T21。

  **引用**：
  - `os/src/fs/ext4/ext4fs.rs:41-61` — 当前创建 DirtyBlockDevice 的位置。
  - `os/src/fs/ext4/dirty_block_device.rs` — 待移除/隔离模块。
  - `os/src/fs/ext4/mod.rs` — 模块导出。
  - `os/src/fs/mod.rs` — VFS_ROOT ext4 打开路径。

  **验收标准**：
  - [ ] ext4 正常 mount/open path 不创建 DirtyBlockDevice。
  - [ ] syscall/VFS/FileSystem sync 路径不调用 `flush_dirty_blocks`。
  - [ ] 若文件仍存在，必须标记未启用并无正常引用。
  - [ ] rv64/la64 编译通过，QEMU 启动不 panic。

  **QA 场景**：
  ```text
  Scenario: 正常路径无 DirtyBlockDevice 引用
    Tool: Bash
    Preconditions: 移除/隔离完成。
    Steps:
      1. 搜索 DirtyBlockDevice、flush_dirty_blocks 引用。
      2. 检查 ext4 open_ext4rs/VFS_ROOT 路径。
      3. 运行 rv64 kernel build。
    Expected Result: 正常路径无 DirtyBlockDevice；build 通过。
    Evidence: .sisyphus/evidence/task-14-dirtybd-removed.txt

  Scenario: 移除 shim 后 busybox 基础启动仍通过
    Tool: QEMU
    Preconditions: fresh image，rv64 kernel 已构建。
    Steps:
      1. Boot rv64 QEMU。
      2. 捕获 initproc/busybox/preload 相关日志。
      3. 确认无 ext4 panic、无 /bin/bash 丢失。
    Expected Result: QEMU 正常跑过 basic/preload 阶段。
    Evidence: .sisyphus/evidence/task-14-rv64-boot-no-dirtybd.txt
  ```

  **证据**：`task-14-dirtybd-removed.txt`、`task-14-rv64-boot-no-dirtybd.txt`

  **Commit**: YES — `refactor(ext4): remove dirty block shim from normal path`

- [x] 15. 添加全局 dirty PageCache 写回 / reclaim 触发

  **要做什么**：
  - 基于 T3 的 registry/flush，增加可触发的全局 dirty PageCache 写回入口。
  - 可先由 `sync()` / `sync_fs()` 调用；如添加后台线程/定时器，必须遵守锁顺序和 no_std 调度约束。
  - 写回逻辑不能无限增长内存占用。

  **不能做**：不能引入长时间持锁 I/O；不能让后台写回与 fsync 互相破坏状态机。

  **推荐 Agent Profile**：
  - **Category**: `deep` — 涉及缓存状态机、调度和并发。
  - **Skills**: []。
  - **Skills Evaluated but Omitted**: `playwright`。

  **并行信息**：Wave 3；依赖 T3/T10；阻塞 T18/T19。

  **引用**：
  - `os/src/fs/page_cache.rs:149-509` — PageCache dirty/writeback 状态机。
  - DragonOS `kernel/src/mm/page.rs` — page_reclaim_thread / flush_dirty_pages。
  - `os/src/task` 相关调度文件 — 若实现后台线程需参考任务模型。

  **验收标准**：
  - [ ] `sync()` 或显式全局 flush 可遍历并写回所有 dirty PageCache。
  - [ ] 若实现后台 writeback，周期和锁顺序有证据说明。
  - [ ] 大量写入不会只靠无限内存 dirty page 堆积。
  - [ ] rv64/la64 编译通过。

  **QA 场景**：
  ```text
  Scenario: 全局 flush 写回多个文件
    Tool: QEMU + debugfs
    Preconditions: 全局 dirty PageCache flush 已接通。
    Steps:
      1. 同时写入 /tmp/a.bin、/tmp/b.bin、/tmp/c.bin。
      2. 调用 sync。
      3. 退出后 debugfs 校验三个文件 hash。
    Expected Result: 三个文件均真实落盘。
    Evidence: .sisyphus/evidence/task-15-global-flush-many-files.txt

  Scenario: 重复覆盖同一页不会留下旧数据
    Tool: QEMU + debugfs
    Preconditions: dirty PageCache flush 已接通。
    Steps:
      1. 对同一文件同一 offset 连续写入 old/new 两种模式。
      2. sync 后 debugfs dump。
    Expected Result: 只看到最后一次写入模式。
    Evidence: .sisyphus/evidence/task-15-overwrite-last-wins.txt
  ```

  **证据**：`task-15-global-flush-many-files.txt`、`task-15-overwrite-last-wins.txt`

  **Commit**: YES — `refactor(cache): add global dirty page writeback`

- [x] 16. 添加持久化 QEMU 场景与证据捕获

  **要做什么**：
  - 建立标准持久化测试集合：write/fsync、write/sync、create/unlink/rename/truncate、preload `/bin/bash`。
  - 每个场景都 fresh image、QEMU 执行、clean exit、debugfs 或重启验证。
  - 证据保存到 `.sisyphus/evidence/task-16-*`。

  **不能做**：不能只跑构建；不能只看内核运行时日志说成功。

  **推荐 Agent Profile**：
  - **Category**: `unspecified-high` — 集成测试矩阵复杂。
  - **Skills**: []。
  - **Skills Evaluated but Omitted**: `playwright`。

  **并行信息**：Wave 3；依赖 T1/T7/T8/T9/T10/T11/T12/T13；阻塞 T18/T19。

  **引用**：
  - `AGENTS.md` — QEMU/Makefile/test mask 规则。
  - `os_test.conf` — 测试 mask。
  - `how-to-run.md`（如存在）— QEMU/LTP 本地调试说明。
  - `os/src/fs/mod.rs:382-517` — preload 文件逻辑。

  **验收标准**：
  - [ ] 至少 4 类持久化场景有可重复命令和证据。
  - [ ] 每类场景都包含 rv64 路径；Wave 4 扩展到 la64。
  - [ ] debugfs/reboot/remount 验证被强制执行。

  **QA 场景**：
  ```text
  Scenario: 持久化测试脚本/流程覆盖四类场景
    Tool: Bash
    Preconditions: 测试流程已写入证据或脚本。
    Steps:
      1. 检查 task-16 evidence 是否包含 fsync/sync/metadata/preload 四类。
      2. 检查每类都有 QEMU 和 debugfs/reboot 步骤。
    Expected Result: 四类场景完整。
    Evidence: .sisyphus/evidence/task-16-scenario-coverage.txt

  Scenario: /bin/bash preload 持久化验证
    Tool: QEMU + debugfs
    Preconditions: fresh image。
    Steps:
      1. Boot QEMU 执行 preload/initproc。
      2. clean exit。
      3. debugfs -R 'stat /bin/bash' sdcard image。
    Expected Result: /bin/bash inode 存在且 size 非零。
    Evidence: .sisyphus/evidence/task-16-binbash-persistence.txt
  ```

  **证据**：`task-16-scenario-coverage.txt`、`task-16-binbash-persistence.txt`

  **Commit**: YES — `test(fs): add ext4 writeback persistence scenarios`

- [x] 17. busybox/preload 性能回归护栏，不依赖写回 hack

  **要做什么**：
  - 复测 `busybox --install -s /bin` 和 preload 写入在移除 DirtyBlockDevice 后的性能。
  - 如果性能回退，优化目录项查找、metadata flush batching 或 PageCache/BlockCache 策略，而不是恢复 DirtyBlockDevice。
  - 保存 write count / elapsed time / QEMU log 对比。

  **不能做**：不能以性能为由恢复 block-device-wide dirty map。

  **推荐 Agent Profile**：
  - **Category**: `unspecified-high` — 需要性能与正确性共同验证。
  - **Skills**: []。
  - **Skills Evaluated but Omitted**: `playwright`。

  **并行信息**：Wave 3；依赖 T14/T16；阻塞 T18/T19。

  **引用**：
  - `os/src/fs/ext4/direntry.rs` — 目录项搜索/添加优化。
  - `os/src/fs/mod.rs:382-517` — preload 写入。
  - `os/qemu.log` — 可保存 QEMU 写计数/日志。

  **验收标准**：
  - [ ] busybox install 在 rv64 QEMU 180 秒总超时内完成，日志包含 `busybox --install -s /bin -> exit=0`。
  - [ ] busybox install 在 la64 QEMU 180 秒总超时内完成，日志包含 `busybox --install -s /bin -> exit=0`。
  - [ ] `/bin/busybox`、`/bin/bash` 在 debugfs `stat` 中存在且 size > 0。
  - [ ] 相比 DirtyBlockDevice 移除前的基线，metadata write count 不超过基线的 2 倍；若无法获取旧基线，则 rv64 busybox/preload 阶段完整 QEMU 日志中不得出现连续 30 秒无新 initproc/test 输出的卡死段。
  - [ ] 性能证据不依赖 DirtyBlockDevice。
  - [ ] 若出现性能问题，计划内优化点归属明确。

  **QA 场景**：
  ```text
  Scenario: busybox install 完成且持久化
    Tool: QEMU + debugfs
    Preconditions: DirtyBlockDevice 已移除正常路径。
    Steps:
      1. Boot QEMU 捕获 busybox --install 日志。
      2. 退出后 debugfs stat /bin/busybox 和 /bin/bash。
    Expected Result: 180 秒内出现 install exit=0；两文件存在且 size > 0。
    Evidence: .sisyphus/evidence/task-17-busybox-install-persistence.txt

  Scenario: 写放大不靠 DirtyBlockDevice 掩盖
    Tool: Bash/QEMU log
    Preconditions: 有 QEMU write log 或计数。
    Steps:
      1. 运行 preload/busybox 场景保存日志。
      2. 搜索 DirtyBlockDevice 相关日志/引用，确认未启用。
      3. 汇总 write count/耗时。
    Expected Result: metadata write count ≤ 旧基线 2 倍；若无旧基线，则 QEMU 日志不存在连续 30 秒无 initproc/test 输出的卡死段；无 DirtyBlockDevice 参与。
    Evidence: .sisyphus/evidence/task-17-write-amplification-no-hack.txt
  ```

  **证据**：`task-17-busybox-install-persistence.txt`、`task-17-write-amplification-no-hack.txt`

  **Commit**: YES — `perf(ext4): keep preload performance without dirty block shim`

- [x] 18. rv64 集成通过与镜像持久化审计

  **要做什么**：
  - 串行构建 rv64 kernel。
  - 使用 fresh rv64 image 运行 basic/ext4 相关 QEMU 场景。
  - 对输出 image 做 debugfs 审计：文件内容、目录项、truncate、rename/unlink、`/bin/bash`。

  **不能做**：不能复用脏镜像；不能跳过 debugfs。

  **推荐 Agent Profile**：
  - **Category**: `unspecified-high` — 集成验证和证据整理。
  - **Skills**: []。
  - **Skills Evaluated but Omitted**: `playwright`。

  **并行信息**：Wave 4；可与 T19/T20/T21 并行；依赖 T14/T15/T16/T17；阻塞 F1-F4。

  **引用**：
  - `AGENTS.md` — rv64 build/run 命令规则。
  - `os_test.conf` — basic/busybox mask 配置。
  - `.sisyphus/evidence/task-16-*` — 持久化测试流程。

  **验收标准**：
  - [ ] rv64 kernel build 通过。
  - [ ] rv64 QEMU 无 ext4/writeback panic。
  - [ ] debugfs 持久化审计全部通过。

  **QA 场景**：
  ```text
  Scenario: rv64 构建和 QEMU 基础场景通过
    Tool: kernel-dev / QEMU
    Preconditions: Wave 3 完成。
    Steps:
      1. 运行 rv64 kernel build。
      2. fresh image 启动 rv64 QEMU。
      3. 保存完整输出。
    Expected Result: build PASS，QEMU 无 panic，测试流程完成。
    Evidence: .sisyphus/evidence/task-18-rv64-build-qemu.txt

  Scenario: rv64 debugfs 持久化审计通过
    Tool: debugfs
    Preconditions: rv64 QEMU 已 clean exit。
    Steps:
      1. debugfs stat/dump 关键文件。
      2. 检查 rename/unlink/truncate 后状态。
    Expected Result: 所有预期状态与任务 16 场景一致。
    Evidence: .sisyphus/evidence/task-18-rv64-debugfs-audit.txt
  ```

  **证据**：`task-18-rv64-build-qemu.txt`、`task-18-rv64-debugfs-audit.txt`

  **Commit**: YES — `test(fs): verify rv64 ext4 writeback persistence`

- [x] 19. la64 集成通过与镜像持久化审计

  **要做什么**：
  - 串行构建 la64 kernel。
  - 使用 fresh la64 image 运行对应 QEMU 场景。
  - 对 la64 image 做 debugfs 或等价工具审计。

  **不能做**：不能与 rv64 build 并行；不能只用 rv64 结果替代 la64。

  **推荐 Agent Profile**：
  - **Category**: `unspecified-high` — 跨架构集成验证。
  - **Skills**: []。
  - **Skills Evaluated but Omitted**: `playwright`。

  **并行信息**：Wave 4；可与 T18/T20/T21 并行，但 build 命令本身必须和 rv64 串行；依赖 T14/T15/T16/T17；阻塞 F1-F4。

  **引用**：
  - `AGENTS.md` — la64 build/run 命令规则。
  - `os_test.conf` — test mask。
  - `.sisyphus/evidence/task-16-*` — 持久化测试流程。

  **验收标准**：
  - [ ] la64 kernel build 通过。
  - [ ] la64 QEMU 无 ext4/writeback panic。
  - [ ] la64 持久化审计通过。

  **QA 场景**：
  ```text
  Scenario: la64 构建和 QEMU 基础场景通过
    Tool: kernel-dev / QEMU
    Preconditions: Wave 3 完成，rv64 build 不在同时运行。
    Steps:
      1. 运行 la64 kernel build。
      2. fresh image 启动 la64 QEMU。
      3. 保存完整输出。
    Expected Result: build PASS，QEMU 无 panic。
    Evidence: .sisyphus/evidence/task-19-la64-build-qemu.txt

  Scenario: la64 持久化审计通过
    Tool: debugfs 或等价镜像检查
    Preconditions: la64 QEMU 已 clean exit。
    Steps:
      1. 检查关键文件和元数据状态。
      2. 对比 task-16 预期。
    Expected Result: la64 与 rv64 持久化语义一致。
    Evidence: .sisyphus/evidence/task-19-la64-persistence-audit.txt
  ```

  **证据**：`task-19-la64-build-qemu.txt`、`task-19-la64-persistence-audit.txt`

  **Commit**: YES — `test(fs): verify la64 ext4 writeback persistence`

- [x] 20. 更新写回架构文档与限制说明

  **要做什么**：
  - 更新项目文档，说明新的 VFS/PageCache/BlockCache/ext4 writeback 分层。
  - 记录非目标：不实现完整 journal，不保证突然断电 crash consistency。
  - 记录验证方法：双架构 build + QEMU + debugfs。

  **不能做**：不能宣称具备 journal/crash consistency；不能留下 DirtyBlockDevice 作为推荐方案的说明。

  **推荐 Agent Profile**：
  - **Category**: `writing` — 文档任务。
  - **Skills**: []。
  - **Skills Evaluated but Omitted**: `playwright`。

  **并行信息**：Wave 4；可与 T18/T19/T21 并行；依赖 T14/T15/T16；阻塞 F1-F4。

  **引用**：
  - `AGENTS.md` — 架构/经验同步要求。
  - `docs/vfs-migration-plan.md` 或 `Doc/vfs-migration-plan.md`（按实际路径）— VFS 迁移说明。
  - `WORK_LOG.md` / `EXPERIENCE.md`（若存在）— 项目要求的记录位置。

  **验收标准**：
  - [ ] 文档说明新架构路径和 DirtyBlockDevice 移除原因。
  - [ ] 文档说明 clean sync persistence 与 crash consistency 边界。
  - [ ] 文档包含实际验证命令/证据位置。

  **QA 场景**：
  ```text
  Scenario: 文档包含架构和限制
    Tool: Bash
    Preconditions: 文档更新完成。
    Steps:
      1. grep 文档中的 PageCache、BlockCache、sync_fs、fsync、crash consistency。
      2. 检查 DirtyBlockDevice 是否只作为旧问题/移除对象出现。
    Expected Result: 文档描述准确，无夸大保证。
    Evidence: .sisyphus/evidence/task-20-doc-architecture.txt

  Scenario: 文档包含验证命令
    Tool: Bash
    Preconditions: 文档更新完成。
    Steps:
      1. grep rv64-kernel-build-only、la64-kernel-build-only、QEMU、debugfs。
      2. 确认证据目录被引用。
    Expected Result: 验证流程可由 agent 执行。
    Evidence: .sisyphus/evidence/task-20-doc-verification.txt
  ```

  **证据**：`task-20-doc-architecture.txt`、`task-20-doc-verification.txt`

  **Commit**: YES — `docs(fs): document ext4 writeback architecture`

- [x] 21. 清理 DirtyBlockDevice 过期引用和 stale 注释

  **要做什么**：
  - 清理已废弃的 DirtyBlockDevice 模块导出、注释、日志、测试引用。
  - 如果文件保留，必须标注为 disabled/legacy，并确保正常构建路径不引用。
  - 移除会误导后续开发者继续使用 dirty block shim 的说明。

  **不能做**：不能删除仍被正常路径需要的代码而不替换；不能保留“metadata dirty cache 推荐方案”的 stale 注释。

  **推荐 Agent Profile**：
  - **Category**: `quick` — 清理引用和注释。
  - **Skills**: []。
  - **Skills Evaluated but Omitted**: `ai-slop-remover` — 任务不是单文件 AI 风格清理，而是架构引用清理。

  **并行信息**：Wave 4；可与 T18/T19/T20 并行；依赖 T14；阻塞 F1-F4。

  **引用**：
  - `os/src/fs/ext4/dirty_block_device.rs` — 旧模块。
  - `os/src/fs/ext4/mod.rs` — 模块导出。
  - `os/src/fs/ext4/ext4fs.rs` — 旧字段/调用点。
  - `WORK_LOG.md` / `EXPERIENCE.md`（若存在）— 清理记录。

  **验收标准**：
  - [ ] 搜索 `DirtyBlockDevice` 只剩允许的 legacy/disabled 记录，或完全没有。
  - [ ] 搜索 `flush_dirty_blocks` 不在正常 runtime 路径中。
  - [ ] rv64/la64 编译通过。

  **QA 场景**：
  ```text
  Scenario: DirtyBlockDevice stale 引用清理
    Tool: Bash
    Preconditions: 清理完成。
    Steps:
      1. 搜索 DirtyBlockDevice、flush_dirty_blocks、dirty_bd。
      2. 对剩余引用分类。
    Expected Result: 无正常路径引用；剩余引用都有 legacy/disabled 说明。
    Evidence: .sisyphus/evidence/task-21-dirtybd-stale-cleanup.txt

  Scenario: 清理后双架构编译通过
    Tool: kernel-dev / Bash
    Preconditions: 清理完成。
    Steps:
      1. 运行 rv64 kernel build。
      2. 运行 la64 kernel build。
    Expected Result: 双架构 build PASS。
    Evidence: .sisyphus/evidence/task-21-dual-build.txt
  ```

  **证据**：`task-21-dirtybd-stale-cleanup.txt`、`task-21-dual-build.txt`

  **Commit**: YES — `refactor(ext4): remove stale dirty block references`

---

## Final Verification Wave（强制，所有实现任务之后）

> 4 个 review agent 并行运行，全部 APPROVE 后向用户汇总；必须等待用户明确 okay，不能自动完成。

- [x] F1. **计划合规审计** — `oracle`
  逐条核验 Must Have / Must NOT Have，检查实现与证据。重点搜索 syscall 层 ext4 特判、DirtyBlockDevice 正常路径残留、缺失持久化证据。
  **验收标准**：Must Have 全部满足；Must NOT Have 零违反；T1-T21 全部有证据文件；若发现 syscall ext4 特判、DirtyBlockDevice 正常路径残留、或缺少 debugfs/reboot 持久化证据，则必须 `REJECT`。
  输出：`Must Have [N/N] | Must NOT Have [N/N] | Tasks [N/N] | VERDICT: APPROVE/REJECT`

- [x] F2. **代码质量审查** — `unspecified-high`
  串行运行 rv64/la64 build。审查 no_std Rust 正确性、锁顺序、OOM 分配、panic/unwrap、过期注释、重复缓存抽象。
  **验收标准**：rv64 build PASS；la64 build PASS；无新增跨等待点持锁；无关键写回路径 `unwrap`/panic；无关键写回错误静默吞掉；无 stale 注释把 DirtyBlockDevice 描述为推荐方案。
  输出：`rv64 Build [PASS/FAIL] | la64 Build [PASS/FAIL] | Files [N clean/N issues] | VERDICT`

- [x] F3. **真实 QA 执行** — `unspecified-high`
  执行所有任务 QA：fresh image QEMU、fsync/sync/umount 持久化、debugfs 检查、busybox install 回归、跨架构验证。保存到 `.sisyphus/evidence/final-qa/`。
  **验收标准**：T1-T21 所有 QA 场景均执行；rv64 和 la64 持久化场景均 PASS；`/bin/bash`、`/bin/busybox` debugfs `stat` 均存在且 size > 0；rename/unlink/truncate 状态与预期一致；失败任一项则 `REJECT`。
  输出：`Scenarios [N/N pass] | Persistence [N/N] | Integration [N/N] | VERDICT`

- [x] F4. **范围忠实度检查** — `deep`
  对比实际 diff 与计划。若保留 DirtyBlockDevice 作为最终架构、添加 syscall ext4 特判、跳过持久化验证、扩展到完整 journal，则拒绝。
  **验收标准**：所有实际改动均可映射到 T1-T21 或文档/证据；无完整 journal 范围膨胀；无未计划的大型重构；无任务污染其他任务职责；发现未计划关键变更或范围膨胀则 `REJECT`。
  输出：`Tasks [N/N compliant] | Creep [CLEAN/N issues] | Missing [CLEAN/N items] | VERDICT`

---

## Commit 策略

- **Wave 1**: `refactor(vfs): define writeback contracts`
- **Wave 2**: `fix(fs): route sync syscalls through vfs writeback`
- **Wave 3**: `refactor(ext4): remove dirty block shim from writeback path`
- **Wave 4**: `test(fs): verify ext4 writeback persistence across architectures`

---

## 成功标准

### 验证命令
```bash
make docker
cd os && make rv64-kernel-build-only  # Expected: kernel-rv copied
cd os && make la64-kernel-build-only  # Expected: kernel-la copied
cd os && make rv64-run                # Expected: writeback persistence scenario passes
cd os && make la64-run                # Expected: writeback persistence scenario passes
debugfs -R 'stat /bin/bash' sdcard-rv.img  # Expected: inode exists after sync/reboot/image inspection
debugfs -R 'stat /bin/bash' sdcard-la.img  # Expected: inode exists after sync/reboot/image inspection
```

### 最终清单
- [ ] 通用 VFS sync 路径存在，并被 fsync/sync/umount 使用。
- [ ] ext4 文件数据写入使用 PageCache dirty/writeback 语义。
- [ ] ext4 cached write 使用 two-phase allocation/size commit。
- [ ] 元数据 dirty state 通过 filesystem/block-cache flush 语义写回。
- [ ] DirtyBlockDevice 不在 ext4 正常路径中作为写回架构存在。
- [ ] rv64/la64 串行 build 通过。
- [ ] QEMU 持久化证据证明数据不只是内存可读。
- [ ] 文档记录架构、限制和非目标。
