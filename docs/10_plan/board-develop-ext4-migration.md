---
title: "2K1000LA board/develop ext4 融合迁移计划"
category: plan
status: integration-validated-backup-pending
owner: MangoCore Team
last_updated: 2026-07-18
tags: [ext4, lwext4, migration, 2k1000la, ssd, branch]
---

# 2K1000LA board/develop ext4 融合迁移计划

## 1. 目标与冻结基线

融合目标分支为 `board-develop-combined`。迁移以两个不可移动的基线为输入：

| 角色 | 分支/提交 | 用途 |
|---|---|---|
| 实板稳定基线 | `la64-on-board` / `464e24b5` | 2K1000LA、AHCI、P4、严格 CPython、网络与安全策略的回退点 |
| 新文件系统基线 | `develop` / `60800fa2` | `ext4_lwext4`、新 VFS 生命周期、DMA pool、回归框架与后续 FS 语义修复 |
| 融合目标 | `board-develop-combined` | 只接收经过双架构和分层测试门禁的融合提交 |

融合期间不改写上述两个输入分支。冲突解决、编译和测试先在独立 integration
worktree 完成，最终只以 fast-forward 方式推进目标分支。

## 2. 数据安全先决条件

任何会向实板 SSD 写数据的测试都必须晚于全盘镜像完成。全盘备份要求：

1. 板卡从 RAM 救援内核启动，P1/P2/P3/P4 均不得以可写方式挂载；
2. `/dev/sda` 及分区节点由内核以只读节点注册，并在块设备层套
   `ReadOnlyBlockDevice`；
3. 从 `/dev/sda` 起始字节到设备报告末尾顺序读取，宿主保存压缩镜像；
4. 同时记录 SSD 型号、容量、MBR、原始流 SHA-256 和压缩文件 SHA-256；
5. 对完成文件执行压缩流完整性检查，并解压到管道复算原始长度和 SHA-256；
6. 只有 `.part` 文件通过全部门禁后才原子改名为正式备份。

备份失败或中断不影响源盘；失败的 `.part` 只能作为断点诊断材料，不能作为可恢复镜像。

## 3. 新旧实现对比

| 维度 | legacy `fs/ext4` | `fs/ext4_lwext4` | 融合结论 |
|---|---|---|---|
| 核心实现 | MangoCore 自研 Rust ext4 | 上游 lwext4 C + Rust 适配层 | 新实现默认，旧实现暂留作回退和 A/B 参考 |
| VFS 生命周期 | 文件系统对象直接挂入 MountFS | `BackendLifecycle` 统一 sync/umount | 采用 develop 生命周期模型 |
| ext4 特性面 | extent、稀疏文件等竞赛所需子集 | journal、目录索引、flex_bg、64-bit、metadata checksum 等更完整 | 新实现长期维护成本更低，但不能把“支持”直接等同于已在本项目验证 |
| I/O 路径 | 直接解析磁盘结构，Mango PageCache 后端 | 路径型 lwext4 API + Mango PageCache 后端 + C FFI | 保留 develop 的批量 PageCache 与 DMA pool，避免退回逐扇区 VirtIO 路径 |
| 并发/锁 | Rust 内部锁图，可细化热点 | 每个 ext4 实例用一把 Mutex 串行化 C 调用，但 lwext4 的设备/挂载表仍是 C 全局数组 | 当前单核且 C 调用不跨等待点；未来 SMP 前必须加跨实例全局锁或去全局化 |
| 多实例挂载 | 原生由对象隔离 | 需要唯一设备名和唯一内部 mount point | 使用 per-instance ID 和路径前缀，必须测试 P1/P3/P4 同时存在 |
| 错误处理 | 多处构造函数直接返回对象或 panic | mount 返回 `Result`，错误映射到 errno | 采用可失败构造；启动失败仅在明确策略处 fallback/panic |
| 只读保证 | board 分支已有 VFS flag + 设备适配 | develop 原始 wrapper 挂载时固定传 `read_only=false` | 同时保留 VFS `RDONLY`、`MangoBlockDev` 写拒绝和物理 `ReadOnlyBlockDevice` 三层屏障 |
| 已知风险 | 功能缺口和自研维护成本高 | FFI 句柄清理、inode 复用、路径缓存、sync/umount、上游补丁维护 | 新后端架构质量更高，但实板质量结论必须由门禁数据给出 |

本轮融合对 develop 原始 lwext4 路径额外做了以下正确性加固：

- wrapper 显式跟踪“设备已注册→文件系统已挂载→journal 已启动→writeback
  已启用”四阶段状态，失败时按逆序回滚；卸载未完全成功时优先保留对象，
  不释放仍被 C 全局表引用的内存，避免 UAF。
- C 层 `ext4_mount` 中途失败会撤销 block cache、block device 和 mountpoint
  slot；设备/挂载上限从 2 提到 8，覆盖 P1/P3/P4 等多 ext4 实例。
- 只读挂载不启动 journal/writeback，不在卸载时 flush；若超级块带
  `RECOVER` 位则返回 `EROFS`，而不在声称只读的路径中偷做 journal replay。
- `MS_REMOUNT` 仍可修改 noexec/nosuid/atime 等单挂载策略；只读位发生变化时
  统一返回 `EOPNOTSUPP`，因为当前 `FileSystem`/`BackendLifecycle` 没有可原子切换
  backend、journal、writeback 和物理块设备的 remount 契约。

## 4. 性能判断

迁移不能预设“换成 lwext4 就一定更快”。两套实现的瓶颈形态不同：

- legacy 在 2K1000LA P4 上的 5,000 小文件生命周期约为 `9.290 ms/file`；
  100 文件缩放实验表明它是高线性固定税，不是 O(N²)。SATA read/write/flush
  只解释约 23% 的 sys 时间，其余主要在 VFS、ext4、PageCache 和路径/元数据软件路径。
- `ext4_lwext4` 带来更成熟的磁盘格式处理和 develop 的批量 I/O/DMA pool，但路径型
  FFI、重复 metadata probe、C 句柄 open/close 和每实例 Mutex 可能增加小文件固定成本。
- 大块顺序 I/O 更可能从 batch PageCache、连续 DMA 和减少 512B fallback 中受益；
  小文件/目录项性能必须单独测量，不能由顺序吞吐外推。

必须在同一 SSD、同一镜像、同一 workload 参数下记录以下 A/B：

| 场景 | 核心指标 | 通过要求 |
|---|---|---|
| 10 MiB 顺序读写 + fsync | MB/s、sys、SATA req/bytes/flush | 不出现数量级退化，内容和重启后 hash 一致 |
| 100/5,000 小文件 | ms/file、sys、metadata/FFI 次数 | 斜率近线性；明确报告相对 legacy 的变化 |
| sparse/truncate/reopen | 空洞零填充、inode size、cold reopen | `gf14/gf18/gf27/gf28` 全通过 |
| mmap/shared writeback | fault、dirty/writeback、cold reopen | 不丢页、不继承复用 inode 的旧 PageCache |
| 多 ext4 分区 | mount/unmount、路径隔离 | P1/P3/P4 不串盘，不共享错误的内部 mount point |

首轮性能数据只用于发现回归，不作为删除 legacy 后端的依据。至少需要三轮稳定样本，
并同时报告 median、离散度、user/sys 和块层计数，避免把缓存冷热或网络噪声当成改进。

## 5. 代码融合顺序

1. **基础设施**：合入 workspace、lwext4 vendor/build、双架构工具链和回归 initramfs；
2. **VFS 契约**：合入 `BackendLifecycle`、MountFS identity、持久 mount flags、PageCache API；
3. **默认后端**：根挂载、启动分区挂载和 `mount(2)` 统一走 `ext4_lwext4`；
4. **板端策略**：补回 native block size adapter、只读设备屏障、P4 UUID/label/recovery
   门禁和 `/sdcard`、`/tools`、`/scratch`、`/persist` 拓扑；
5. **平台能力**：保留 AHCI、多 DRAM region、firmware carveout、GMAC、可信 RNG 和
   strict-align 上下文；
6. **用户态**：融合 regression/LTP 与 P4 strict CPython/APK 入口；
7. **诊断**：保留 legacy、lwext4、PageCache、SATA、网络和 runtime 两侧计数器；
8. **验证后发布**：先 QEMU、后只读实板、最后才允许在备份完成后进行受控写入测试。

## 6. 验证门禁

### G0：静态完整性

- 无未解决 merge entry 或冲突标记；
- `git diff --check` 对自研代码通过（vendored 上游原有格式问题单独记录）；
- shell/Python/Rust 文件可解析；生成型 `lang_items.rs` 不被手工修改。

### G1：双架构编译

严格串行执行：

```text
make rv64-kernel-build-only
make la64-kernel-build-only
```

两个架构使用不同 nightly，禁止并行切换工具链。

### G2：QEMU 功能回归

- normal initramfs 启动；
- basic + busybox；
- lwext4 sparse/truncate/rename/mmap focused regression；
- mount/bind/rbind/remount/read-only 语义；
- CPython L3-L9 至少完成隔离 smoke。

2026-07-18 融合提交前已完成的门禁：

| 门禁 | RV64 | LA64 |
|---|---:|---:|
| `kernel-build-only` | 通过 | 通过 |
| `KTEST=ext4` | 4/4 | 4/4 |
| L4 regression | 5/5 + 正常关机 | 5/5 + 正常关机 |

L4 门禁还暴露并修复了 develop 自带的两个测试设施问题：`mprotect`
用例曾违反页对齐前置条件，以及 PID1 直接 `exec` 后无人输出最终标记并关闭
QEMU。修正后门禁由 Makefile 以退出码和 `L4 REGRESSION RESULT` 双重确认。

### G3：实板只读验收

- IDENTIFY 型号、容量和 MBR 与备份清单一致；
- P1/P3 只读挂载，P4 在首次迁移验收中也先只读；
- 多分区目录可读，所有块节点拒绝用户态写入；
- GMAC、AHCI 和内存 carveout 无 panic/超时。

### G4：实板受控写入

仅在全盘备份完成后启用。先使用一次性 scratch/P4 fixture，完成 write/fsync/reopen/hash、
卸载/重启后复核；再决定是否将融合分支作为日常 P4 可写运行分支。

## 7. 提交与回退结构

本轮使用一个有两个 parent 的 merge commit 保留 `la64-on-board` 与 `develop`
完整历史。所有冲突解决和“不做就无法安全挂载”的只读/生命周期修正与该
merge 原子提交，避免产生一个表面可编译但可写性说谎的中间点。

后续实板验收、性能调优和删除 legacy 后端必须使用可独立回退的小提交，不重写
`la64-on-board`、`develop` 或已发布 merge。任一实板门禁失败时，日常启动继续使用
`la64-on-board@464e24b5`；目标分支保留故障提交供分析，通过 revert 或后续修复前进，
不删除旧后端、不改写历史。

## 8. 完成定义

当前状态是“代码融合 + 双架构 QEMU 已验证”，不是“实板迁移已完成”。
SSD 全盘镜像正在宿主 `/Users/luzimo/dev/ssd-backups/` 流式备份，该流程完成
前禁止融合内核对实板 SSD 做任何受控写入。另有一个独立语义边界待后续实现：
`MS_REMOUNT|MS_BIND` 目前仍先进入 bind 路径，未实现 Linux 的 per-mount bind-remount
策略更新；这与本轮明确拒绝 backend 读写模式切换是两个不同问题。

只有同时满足以下条件，才能宣布迁移完成：

- SSD 全盘备份可解压、长度和双哈希一致；
- 两架构编译通过；
- QEMU focused FS 回归通过；
- 实板只读启动和多分区隔离通过；
- 至少一轮受控 P4 写入跨重启验证通过；
- 新旧性能 A/B 有可复现输入和原始日志；
- `board-develop-combined` 指向通过全部必需门禁的提交，两个输入分支保持不变。
