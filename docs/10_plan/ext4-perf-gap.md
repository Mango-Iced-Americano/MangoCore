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

### 方案 A：迁移到 `another-ext4`（推荐）

[DragonOS](https://github.com/DragonOS-Community/DragonOS) 使用的 Rust ext4 库：
- 完整的 extent 树支持
- delayed allocation
- 已在 DragonOS 上验证可用
- MIT 许可证

工作量：需要适配 VFS 接口（`IndexNode` trait），预计 1-2 周。

### 方案 B：迁移到官方推荐实现

比赛官方可能推荐特定的 ext4 实现或提供参考代码。待赛事方公布后评估。

### 方案 C：保持现状

如果性能指标达到赛事要求，可以搁置。当前 7227/9028 KB/s 的读写吞吐在单核 QEMU 环境下属于可用水平。

---

## 相关文档

- [perf/fs 分支优化记录](../../docs/Work_Log.md)
- [VFS 迁移计划](vfs-migration-plan.md)（如存在）
- [DragonOS 架构参考](https://github.com/DragonOS-Community/DragonOS)
