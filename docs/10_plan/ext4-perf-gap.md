# ext4 性能差距分析及迁移方案

**状态：lwext4 集成方案已调研** | **日期：2026-07-04** | **分支：perf/fs R12**

---

## 现状

rv64 QEMU 同配置下 iozone 4MB/1KB 单进程对比：

| 指标 | MangoCore (R12) | Linux 6.6 | 差距 |
|------|----------------|-----------|------|
| Write | 7,227 KB/s | 22,460 KB/s | **3.1x** |
| Read | 9,028 KB/s | 55,165 KB/s | **6.1x** |

已经完成的优化（27 commits on perf/fs）：
- ext4 extent-range cache (95% 命中)
- 512B batch read/write pages
- byte-based mballoc (256KB 目标)
- 128KB 预分配 + 首块跳过 ensure_blocks_allocated
- DMA 池 4×64KB + VirtIO 批量提交
- 脏页阈值 8192、预读 128 页
- writeback 批量 256 页

结果：**VirtIO 传输层和前台分配路径都已不是瓶颈**。DMA 池将 wb_cycles 降了 96%，write_at_user 预分配将 ensure_blocks_allocated 调用降了 768x，但吞吐量提升只有 +29%（Write）/+2%（Read）。

---

## 根因分析

Oracle 分析结论：**差距在手搓 ext4 实现本身**，不是配置参数能调的。

Linux ext4 拥有而我们缺失的关键特性：

| 特性 | 说明 | 影响 |
|------|------|------|
| **delayed allocation** | 推迟块分配到 writeback 时，合并临近小写 | Write 吞吐 2-5x |
| **extent status tree** | 内存中跟踪已分配/未写入 extent，避免重复磁盘查找 | 读写延迟 |
| **multi-block allocator** | 一次分配多个连续块，而非逐块分配 | 碎片化、extent 大小 |
| **flex_bg** | 将多个块组合并为一个大的 block group | 元数据局部性 |
| **journal checksumming / async commit** | 日志优化 | 元数据写入延迟 |
| **bio 提交层** | 块层合并/排序/调度 I/O 请求 | I/O 吞吐 |
| **readahead 自适应** | 根据访问模式动态调整预读窗口 | Read 吞吐 |

这些是 Linux 内核社区二十多年的积累，手搓补齐不现实。

---

## 可行方案

比赛官方（[ref-info.md](https://github.com/oscomp/os-competition-info/blob/main/ref-info.md)）列出了 4 个 ext4 参考实现：

| 库 | 语言 | 使用者 | 特点 |
|----|------|--------|------|
| [lwext4](https://github.com/gkostka/lwext4) + [lwext4_rust](https://github.com/rcore-os/lwext4_rust) | C + Rust FFI | StarryOS, ByteOS, RT-Thread | 最成熟（2014-），功能最全 |
| [ext4_rs](https://github.com/yuoo655/ext4_rs) | 纯 Rust | StarryOS | 中等 |
| [another_ext4](https://github.com/PKTH-Jx/another_ext4) | 纯 Rust | StarryOS | 较新，功能有限 |

**Rust 生态现状：没有一个类似 `smoltcp` 公认的 ext4 库。** StarryOS（2024 年一等奖）甚至同时尝试了三个，说明哪个都不完全可靠。

### 方案 A：`lwext4` + `lwext4_rust`（推荐）

C 库 lwext4 起源于 HelenOS，历史最久（2014 年开始），功能最完整：
- delayed allocation
- extent tree + extent status tree
- journal checksumming
- RT-Thread 也在用，经过一定生产验证

缺点：需要 FFI 集成（Rust ↔ C），`lwext4_rust` 是薄 wrapper，可能需要自己补适配层。

工作量：适配 VFS `IndexNode` trait + FFI 构建，预计 1-2 周。

### 方案 B：纯 Rust 库

`ext4_rs` 或 `another_ext4` 都是纯 Rust，集成更容易，但功能和稳定性可能不及 lwext4。DragonOS 用的是自己手搓的 ext4（非上述库）。

### 方案 C：保持现状

当前 7227/9028 KB/s 的读写吞吐在单核 QEMU 环境下可用。如果赛事性能要求不严格，可以搁置。

---

## lwext4 集成详细评估（方案 A）

### 代码规模

| 组件 | 规模 | 说明 |
|------|------|------|
| lwext4 C 库 | ~20K 行 C + ~5K 行头文件 | ext4 核心逻辑 |
| lwext4_rust wrapper | ~800 LOC | 薄 FFI 封装（含 ~4500 行自动生成的 bindings.rs） |
| StarryOS VFS 适配器 | ~370 LOC | 参考实现 |
| **MangoCore 需新增** | **~360 LOC** | 块设备适配 70 行 + VFS 适配 250 行 + 构建 30 行 |

### ext4 特性覆盖

lwext4 已支持（CONFIG_SET_FULL_EXT4）：

| 特性 | 支持 | MangoCore 手搓 |
|------|------|---------------|
| extent 树 | ✅ | ✅ |
| journal (JBD) | ✅ | ❌ |
| 目录索引 (dir_index) | ✅ | ❌ |
| flex_bg | ✅ | ❌ |
| 64bit | ✅ | ❌ |
| 元数据校验和 (metadata_csum) | ✅ | ❌ |
| 扩展属性 (xattr) | ✅ | ❌ |
| delayed allocation | N/A（块层特性） | N/A |
| 内联数据 (inline_data) | ❌ | ❌ |

### 架构分层

```
┌─────────────────────────────────────────┐
│  MangoCore VFS 适配器  (~250 LOC)       │  ← 需实现 IndexNode trait
├─────────────────────────────────────────┤
│  lwext4_rust                            │
│  ├─ Ext4File (open/read/write/seek)      │
│  ├─ Ext4BlockWrapper (mount/umount)      │
│  └─ bindings.rs (FFI)                    │
├─────────────────────────────────────────┤
│  liblwext4-riscv64.a  (~51KB .text)     │  ← musl-gcc 交叉编译
├─────────────────────────────────────────┤
│  KernelDevOp (~70 LOC)                  │  ← 包装现有 VirtIOBlock
└─────────────────────────────────────────┘
```

### 集成步骤

| 步骤 | 内容 | 估时 |
|------|------|------|
| 1. 构建链 | 用 `riscv64-linux-musl-gcc` 编译 lwext4 → `.a`，`build.rs` 链接 | 半天 |
| 2. 块设备适配 | 实现 `KernelDevOp` trait，包装 `VirtIOBlock`（read/write/seek/flush） | 1 小时 |
| 3. VFS 适配 | 仿照 StarryOS `ext4fs.rs`，实现 MangoCore 的 `IndexNode` trait | 半天 |
| 4. 测试 | 构造 ext4 镜像，mount，跑 basic/iozone/lmbench | 半天 |
| **总计** | | **2-3 天** |

### 注意事项

1. **构建工具链**：需要 `riscv64-linux-musl-gcc`。lwext4 已为 cortex-m 等裸机目标配置了交叉编译，但 riscv64-none-elf 没有现成工具链文件。musl 工作流最省事。
2. **双重缓存**：lwext4 有内部块缓存（默认 16 块）。建议设为 1-2 块或禁用，避免与 MangoCore PageCache 冲突。
3. **许可兼容**：`ext4_extent.c` 和 `ext4_xattr.c` 是 GPLv2，其余 BSD-3。MangoCore 是 GPLv3，兼容。
4. **la64 交叉编译**：需要 `loongarch64-linux-musl-gcc`，逻辑同 rv64。

---

## 相关文档

- [perf/fs 分支优化记录](../../docs/Work_Log.md)
- [VFS 迁移计划](vfs-migration-plan.md)（如存在）
- [DragonOS 架构参考](https://github.com/DragonOS-Community/DragonOS)
