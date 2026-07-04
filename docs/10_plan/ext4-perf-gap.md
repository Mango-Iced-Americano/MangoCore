# ext4 性能差距分析及迁移方案

**状态：待调研** | **日期：2026-07-04** | **分支：perf/fs R12**

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

## 相关文档

- [perf/fs 分支优化记录](../../docs/Work_Log.md)
- [VFS 迁移计划](vfs-migration-plan.md)（如存在）
- [DragonOS 架构参考](https://github.com/DragonOS-Community/DragonOS)
