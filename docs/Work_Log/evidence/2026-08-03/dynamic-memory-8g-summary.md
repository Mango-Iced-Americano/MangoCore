# 双架构动态内存与 8 GiB 适配证据

## 目标与边界

- QEMU RV64/LA64 的内存容量和 bank/address-hole 布局以固件 FDT 为权威，不再由
  编译期 `MEMORY_END` 截断。
- 运行期拓扑贯穿 frame allocator、启动清零、内核映射元数据、`sysinfo(2)`、
  `/proc/meminfo` 与 RamFS `statfs`。
- 2K1000 无合法 EFI/FDT 时继续使用静态双 bank fallback；本次只验证构建，实板 NOT RUN。
- FS/Net/Driver 的 DMA 地址能力由队友模块另行审计，不在本工作包结论内。

## 实现摘要

- `for_each_usable_ram_range()` 在每个 DRAM bank 内扣除第 0 页、固件保留区和调用者排除区；
  exclusion 可无序、重叠且不要求堆分配，迭代绝不跨越 MMIO hole。
- frame allocator 从运行期区间建立多 region fresh 游标；删除按总帧数预留 recycled `Vec`
  的 8 GiB 级启动内存浪费。
- LA64 恒等映射 dirty bitset 在堆就绪后按固件最高 DRAM 地址建立，热路径仍使用原子 word。
- `zero_init` 只清理动态 usable 页，不按最高物理地址线性跨洞写内存。
- `QEMU_MEMORY ?= 1G` 作为统一运行参数；8 GiB focused test 检查最后可用页和 ABI 总量。

## RED 与根因

首轮双架构 8 GiB 中，新 `firmware_memory_reaches_allocator` 均通过，但
`local_mmu_gather_map_protect_unmap` 和 `shared_futex_pin_blocks_reclaim` 均失败。
这不是架构 TLB 差异：启动后 linker payload 的完整页已经登记为 `ReclaimedRegion`，
旧校验却按链接地址把整个 `[skernel, ekernel)` 永久视为不可分配，导致合法用户 PTE 的
物理页后验校验被拒绝。最终恢复该接口原有的无锁拓扑语义：检查整页位于固件可用 DRAM，
实时所有权仍由页表、VMA 和 `FrameTracker` 保证，避免把 allocator 锁引入每页 uaccess。

## 冻结源码与环境

- HEAD：`21ded299fd7be6f7dfa97e3a949c9224c23ce1a4`
- 最终受测 tracked diff SHA-256：
  `3bd1913ba72b0622781a59bb0bb4f6098a3ed385fc0c64184c3aa5d283ff1859`；该快照已包含
  无锁固件拓扑检查及当时的架构/MM 文档。测试完成后只更新本证据中的最终 hash/耗时字段，
  未修改受测 Rust/Make 语义，按文档新鲜性规则无需重跑。
- Docker container：`a99062375fdb`，image
  `sha256:60e9bfa0ecdc6be93d9beb6b1d249f34163b08e32e97f090590a93a92e9357ac`
- mount：`/home/lzm/projects/MangoCore-smp-integration-20260725 => /app`
- QEMU：RISC-V/LoongArch64 均为 10.0.2。
- 本地 DeepSeek job：`dynamic-memory-validation-r2-20260803`（RED 归纳）、
  `dynamic-memory-reclaimed-fix-r3-20260803`（根因修正）、
  `dynamic-memory-lockless-final-r5-20260803`（最终双架构 GREEN）；`cc-codex/` 不进入 Git。

## 验证结果

| 验证 | 结果 | 关键证据 |
|---|---|---|
| RV64 normal kernel build | PASS | exit 0，131s，源码指纹一致 |
| LA64 normal kernel build | PASS | exit 0，132s，源码指纹一致 |
| LA64 2K1000 build | PASS | exit 0，131s；实板运行 NOT RUN |
| RV64 `CORE_NUM=8 QEMU_MEMORY=8G KTEST=mm` | PASS | 6/6，8189 MiB，最高页 `0x27ffff000`，144.303s |
| LA64 `CORE_NUM=8 QEMU_MEMORY=8G KTEST=mm` | PASS | 6/6，8190 MiB，最高页 `0x26ffff000`，133.298s |

两项最终 QEMU 日志都包含 `dynamic_above_static=true`、第 5/6 项 `ok` 和
`[KTEST RESULT: PASS]`；runner 检查均为 exit 0、无源码 mutation、无 timeout、无 forbidden
marker。两项 runner 的 source-before/source-after 均为上述同一指纹。

## 未覆盖边界

- 未通过耗尽低地址内存来强制实际分配最高物理页；永久门禁证明该页已注册且具有内核映射
  元数据，但不等于完成 8 GiB 全容量压力分配。
- 2K1000 实板启动、DMA mask/IOMMU、VirtIO/网卡/文件系统大内存压力均为 NOT RUN。
- 当前 4 KiB 初始映射能够在 30 秒 QEMU 门禁内启动；huge-page direct map 属于后续性能优化，
  不作为本功能正确性的前置条件。
