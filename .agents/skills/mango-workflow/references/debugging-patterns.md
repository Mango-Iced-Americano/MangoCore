# 调试模式库

> 跨对话可复用的调试技巧和排查方法。

## 文本解析

### awk 匹配多段文本时无意中匹配到统计摘要行

- **根因**: awk 模式 `/^(first-party|maintained|vendor):/ && !/^#/` 同时匹配了数据行（如 `first-party:dead_code:src/file.rs`）和摘要行（如 `first-party: 175`），因为两者都以此前缀开头且摘要行不是 `#` 注释。导致基线比较时多出 175 个"已解决"的假阳性。
- **修复**: 在类别前缀后加 `[^ \t]` 排除摘要行：`/^(first-party|maintained|vendor):[^ \t]/ && !/^#/`。此模式要求冒号后紧跟非空白字符，有效将统计摘要过滤掉。
- **教训**: 解析带多个段（数据 + 统计摘要 + 页眉/页脚）的结构化文本文件时，awk 模式不能仅匹配行前缀就确定身份。必须显式检查分隔符后续的第一个字符，避免摘要/页眉/页脚行被当作数据行处理。这是 grep/awk 解析中的常见陷阱。
- **相关文件**: `scripts/lint-check.sh:272`

## 堆分配器性能退化

### buddy allocator free-list 线性扫描导致渐进退化

- **根因**: `Heap::dealloc()` 在合并 buddy 时线性遍历 size-class 的 free-list（`for block in free_list.iter_mut()`）。heap 碎片化后 free-list 变长，每次 dealloc 扫描步数从 19 爆炸到 114（6x），导致 open/close 退化 2.6x、fork+exit 退化 1.8x。
- **修复**: 加 per-class free-membership bitmap。dealloc 前 O(1) 查 bitmap — buddy 不在 free-list 中就直接跳过扫描。bitmap 内存从 heap region 前端 carve 出来（~4MB / 256MB heap）。
- **教训**: 渐进退化优先怀疑"有状态的数据结构"（free-list、hash table、LRU list）而非纯计算路径。用 per-call scan_steps 计数器可以精准定位。
- **相关文件**: `os/vendor/buddy_system_allocator/src/lib.rs:161`

## 启动/Panic 排查

### QEMU DTB 落入 kernel BSS 时，必须在清零前消费启动信息

- **现象**：QEMU 传入的 DTB 地址落在大型内核 BSS（常见于嵌入 initramfs/测试资产）内；`mem_clear()` 后 FDT 解析静默回退，或预先解析后 DTB carveout 与 kernel exclusion 重叠导致 allocator/map panic。
- **根因**：仅保存 DTB 指针而不在 BSS 清零前读取内容；DTB 页既是 firmware reserved 范围又可能已被 kernel image 覆盖。
- **修复**：在 `mem_clear()` 前执行无分配的内存区域解析并保存结果；对重叠 exclusion 做区间合并，且不重复映射完全包含于 kernel image 的保留范围。
- **教训**：启动协议提供的物理 blob 的存活期不能假设晚于内核 BSS 初始化。测试 FDT 路径时使用带真实 ktest block drive 的 profile；无 drive 的裸 QEMU 不能验证依赖 `block_devices()[0]` 的 ext4 测试。
- **相关文件**：`os/src/main.rs`、`os/src/mm/frame_allocator.rs`、`os/src/mm/kernel_space.rs`

### 固件寄存器参数必须先按启动协议建立信任边界

- **根因**: 将架构入口寄存器（如 `a1`）一律解释为 DTB 指针，只检查非零；`UbootGo` 和 `LoongArchLegacy` 的同一寄存器位置可能是无关的垃圾值，导致早期 volatile 读取或 raw-slice FDT 解析访问错误地址。
- **修复**: 所有 DTB 消费入口先要求 `matches!(boot_info().protocol, BootProtocol::RiscvFdt)`，再检查指针非零、页对齐、FDT magic 和有界 `totalsize`；FDT 成功后仍保留编译期 firmware carveout，并保留 DTB 自身页面。
- **教训**: 原始启动参数不是跨平台 ABI。先按协议缩小信任域，再执行指针解引用或物理地址转换；“非零”从来不是可访问性证明。
- **相关文件**: `os/src/hal/firmware/{mod.rs,fdt.rs}`

### 内核 panic 定位
- 启动时加 `LOG=debug make rv64-run` 查看详细日志
- 使用 GDB 调试：`make rv64-debug` → `b rust_main` → `c`
- panic 输出包含 syscall 上下文、内存状态、任务信息（`panic_diag.rs`）

### 早期启动 MMIO 恒等映射必须包含 post-heap 前使用的编译期外设范围

- **现象**: VF2 实板在早期启动阶段访问 L2CC 缓存控制器 MMIO 地址 `0x02010200` 时触发 `StorePageFault`；该地址在 FDT/PlatformInfo 初始化前使用，但不在编译期 identity MMIO 映射表范围内。
- **根因**: 早期启动代码（FDT 初始化前、堆分配器就绪前）依赖编译期 identity MMIO 映射表访问外设寄存器。L2CC 缓存控制器地址 `0x0201_0000..0x0201_1000` 未被纳入该表，导致 volatile 写操作触发了对未映射物理地址的 StorePageFault。
- **修复**: 在 VF2 板级 identity MMIO 映射表中添加 `0x0201_0000..0x0201_1000` 范围的恒等映射，确保 post-heap FDT/PlatformInfo 初始化前的 L2CC 访问有有效页表项。
- **教训**: 任何在堆分配器就绪前（即 post-heap FDT 枚举前）需要访问的 MMIO 外设，其地址范围必须显式加入编译期 identity MMIO 映射表。仅依赖 FDT/PlatformInfo 动态映射的地址在早期启动阶段不可访问。此类问题表现为早期 boot 路径中的 `StorePageFault` 而非设备探测失败。
- **相关文件**: `os/src/hal/platform/riscv/vf2.rs`、`os/src/mm/kernel_space.rs`、`os/src/drivers/net/gmac_jh7110/mmio.rs`

### 大内存启动早期 OOM：bootstrap heap 必须覆盖按 RAM 线性增长的结构

- **现象**: 提高 QEMU 内存后（如 la64 `-m 36G -smp 12`、rv64 `-m 16G`），内核在启动早期
  `Console initialized.` 之后、`init_runtime_heap` 之前 panic：
  `HEAP ALLOCATION FAILED (FATAL) layout: size=9356593, align=1`，`KERNEL_HEAP_SIZE` 显示的
  仍是 bootstrap heap 大小（如 8MiB 减去 metadata 后的 8335360）。`align=1` 是 `Vec<bool>`/
  `Vec<u8>` 类分配的指纹。
- **根因**: `StackFrameAllocator::init`（`frame_allocator.rs`）为每个 DRAM region 调用
  `FrameRegion::new()`，其 `recycled_flags: Vec<bool>` 按 region 页数 1 字节/页分配，随
  物理内存线性增长（36G ≈ 9.35M 页 → 9.36MiB）；buddy 分配又把它圆整到下一个 2 的幂
  order（9.36MiB → order 12 = 16MiB 连续块）。这些结构在 runtime heap（DRAM 扩展堆）
  建立前就要分配，8MiB bootstrap heap 必然失败。la64 还有 `laflex::IdentityDirtyMap`
  （highest_end/4096/64 个 `AtomicUsize`，36G 约 1.19MiB）同样在预 runtime 期分配。
- **修复**: 调大双架构 `KERNEL_BOOTSTRAP_HEAP_SIZE`（8MiB → 32MiB），把 buddy 圆整后的
  order 12 块 + IdentityDirtyMap + slab 余量都覆盖进去。数值推导写入 config.rs 注释，
  避免后人盲目改小。
- **教训**: ① 启动早期 OOM 先看 `align` 指纹——`align=1` 且 size 接近页数 = per-page
  结构；size 是 2 的幂 order 倍数 = buddy 圆整，别只按"实际字节 < 堆大小"判断。②
  随物理内存线性增长的结构（frame flags、bitmap、dirty map）必须按评测最大内存推导
  bootstrap heap 需求，不能只按默认内存（768MiB/1GiB）验证。③ 修复后必须用评测
  级内存重新 QEMU 验证，且注意 rv64 要用 strip 后的 `Image`/`kernel-rv` raw binary
  启动（ELF PhysAddr 是链接虚拟地址，QEMU 加载不了）。
- **相关文件**: `os/src/mm/frame_allocator.rs`、`os/src/mm/heap_allocator.rs`、
  `os/src/hal/arch/{riscv,loongarch64}/config.rs`、`os/src/hal/arch/loongarch64/laflex.rs`

### PCI host 的 `ranges` 与 `reg` 必须在预堆阶段同时映射

- **现象**: RV64 用 `BLK_MODE=virt_pci` 启动时，PCI config access 或 VirtIO BAR access 触发 page fault；仅把固定 ECAM 地址加到板级 MMIO 常量会使 QEMU 特例可用，但仍无法适配 FDT 描述的不同 host。
- **根因**: PCI host 的 `reg` 描述 ECAM，而可分配的 BAR memory window 在 `ranges`。若 pre-heap parser 只收集节点 `reg`，`PlatformInfo` 虽能在 post-heap 解析出 PCI host，驱动实际访问 BAR 时仍没有内核页表映射。
- **修复**: pre-heap 解析 `pci-host-ecam-generic`/`pci-host-cam-generic` 的 `ranges`，只接受 memory space 的完整固定长度 entry，并验证非零长度、地址转换和加法不溢出后写入 early MMIO buffer；post-heap 再由同一 FDT 数据构造驱动用的 `PciHost`。缺失或畸形 host 才允许显式 warning fallback，不能用固定 QEMU 地址替代有效 FDT。
- **教训**: FDT 资源的“发现”和“可访问”是两个阶段的契约。任何在 post-heap discovery 后立即被驱动解引用的总线资源，都必须在内核页表建立前从 FDT 的全部资源属性（而非只 `reg`）获得映射。
- **相关文件**: `os/src/hal/firmware/fdt.rs`、`os/src/hal/platform/info.rs`、`os/src/drivers/block/virtio_blk_pci.rs`

## 内存问题

### 物理地址异常（如 0xb0000000）
- la64: 检查 `MEMORY_SIZE` 是否匹配 DTB 中 RAM 范围
- rv64: 检查 `device_tree.rs` 中内存区域解析

### 堆耗尽
- 检查是否有 `try_reserve` 防御
- 查看 `heap_trace.rs` 的分配记录（需启用 feature）

### getdents 等 syscall 对 guarded 用户缓冲区返回 EFAULT 而非部分数据

- **根因**: `UserBufferWriter::new` 在 FS 工作之后调用。guard page（未映射页）场景下，`UserBufferWriter` 可能对第一个有效页成功，写入部分数据后返回正数字节数而非 EFAULT。
- **修复**: 将 `UserBufferWriter::new(token, ptr, len)` 移到任何内核工作（FS 操作、内存分配）之前。如果用户缓冲区不可写，`new()` 立即失败返回 EFAULT，避免先做了内核工作再报错。
- **教训**: 所有接受用户缓冲区指针的 syscall，都应尽早创建 `UserBufferWriter`/`UserBuffer` 进行预校验。Linux 内核在 `copy_to_user` 每次写入时都会 fault，所以天然不存在此问题；我们的批量拷贝模型需要显式预校验。
- **相关文件**: `os/src/syscall/fs.rs` — `sys_getdents64`

### bind/umount 后 `/proc/mounts` 仍有 sandbox 残留
- 症状：LTP `fs_bind*` 清理阶段反复提示 `There are still mounts in the sandbox`，`umount` 看似成功但同一路径仍出现在 `/proc/mounts`
- 优先检查：子 `MountFS` 是否还能通过 `self_mountpoint` 找到父 `MountFSInode`，以及父 `mountpoints` 表是否真正删除了该 inode id
- 典型根因：挂载点 backref 只保存弱引用或 overmount 旧挂载未走统一 detach，导致 `detach_from_parent_and_cleanup()` 无法摘除父表项
- 修复模式：保留稳定 parent backref，在 detach 时 `take()` 断开引用；覆盖挂载旧节点也走完整 cleanup，避免 dentry/child mount 缓存继续持有 covered subtree

## Drop 与锁顺序

### 持锁时替换 fd 条目标隐式 drop 旧文件 → 死锁

- **根因**: `FdTable::alloc_fd_at()` 中 `self.fds[fd] = Some(new_file)` 会替换旧值，若旧 `Arc<File>` 引用计数降为 0，其 `Drop` 触发 `File::drop` → `inode.close()`，在 close 非 no-op 时尝试获取其他锁（page cache lock、FS 内部锁），而调用者仍持有 `fd_table` 锁 → 死锁。
- **修复**: (1) 用 `core::mem::replace` 提取旧值而非隐式 drop，通过返回值传出；(2) 调用者先释放 `fd_table` 锁，再 `drop(old_file)`。
- **教训**: Rust 隐式 drop 让你看不见资源释放点。修改 `Vec<Option<Arc<T>>>` 等容器持有重型资源时，`=` 赋值的隐式 drop 可能在持锁路径下触发 `Drop`，导致死锁。安全模式：`let old = core::mem::replace(&mut slot, new_value); ... unlock(); drop(old);`
- **相关文件**: `os/src/fs/vfs/file.rs` — `alloc_fd_at()`, `os/src/syscall/fs.rs` — `sys_dup2()`, `sys_dup3()`

## 信号问题

### 信号处理不生效
- 检查 sigaction 是否正确设置了 `SA_SIGINFO` 等 flags
- la64: 检查 `rt_sigaction` 的 sigsetsize 参数（libc 传 16 字节而非 8）

### 进程停止/继续状态异常
- 检查 `SIGSTOP`/`SIGCONT` 是否正确更新进程状态
- 检查父进程 wait 是否正确消费 stopped/continued 事件

## Errno 返回值问题

### errno 常量双取反导致正"成功"返回值

- **根因**: 项目 errno 常量定义为负 `isize`（`EINVAL = -22`, `EAGAIN = -11`），但 `flock.rs` 返回时又取反：`return -EINVAL` → 实际返回 `22`（正数，syscall 入口视作成功）。`-EAGAIN` 同理返回 `11`。
- **修复**: 直接返回常量 `EINVAL`/`EAGAIN`（已为负值），不再额外取反。
- **教训**:
  - 定义 errno 常量前先确定符号约定 — 代码库中使用负值 vs Linux 正值。本项目使用负值直接返回即可。
  - 新增 syscall 时检查返回模式：`os/src/syscall/fs.rs` 中 `return EINVAL;` 是正确的参考模式。
  - `return -ENOERR` 模式在该项目中都是可疑的 — 如果 errno 常量已经是负值，取反就变成正数。
- **相关文件**: `os/src/syscall/flock.rs`, `os/src/syscall/errno.rs`

## 性能问题

### 新增诊断计数器必须进入 reset 窗口

- **现象**：相邻诊断组的同一计数器看似精确翻倍，但其调用计数已被正确清零，导致按当前组调用次数计算的平均值失真。
- **根因**：新增 `AtomicUsize` 只接入 recorder 与 sysfs 导出，遗漏 `perf::reset_all_counters()`，所以 initproc 的每组 reset 不会清空该原子值。
- **修复**：同一个改动中同时完成“声明 → recorder/no-op → sysfs 导出 → reset”；用连续两个诊断窗口确认第二个窗口不含第一个窗口的累计值。
- **相关文件**：`os/src/task/perf.rs`、`os/src/fs/sysfs/files/diag.rs`

### deferred journal 被 direct metadata barrier 提前提交

- **根因**：另一个 ext4 的普通数据回写若进入 `lock_direct_metadata_mutation()`，该 guard 为了让 direct writer 看到稳定元数据会先 `flush_deferred_journal()`。小 append 或已映射覆盖若落入该路径，会把原本可合并的 deferred JBD2 transaction 在每个 writeback run 提前提交。
- **修复**：将单页 append 也纳入 journal append path；对所有块已映射的覆盖写，在 transactional/inode mutation guards 持有期间写数据块，并复用已映射数据写核心，不进入 direct metadata domain。保留 hole/allocation 路径的 direct barrier，以及 fsync/sync 时的明确 journal commit。
- **教训**：优化 deferred transaction 时不要只检查 `defer_or_commit()` 阈值；还要沿所有 fallback/数据写路径检查是否跨入会强制 flush 的一致性域。数据块在 metadata 提交前必须已完成写入，且 fsync 仍必须显式提交 deferred journal。
- **相关文件**：`dependency/another_ext4/src/ext4/{low_level,data_write,mod}.rs`

### 空 deferred batch 会伪造 DirectMetadataBarrier journal commit

- **根因**：`Transaction::defer_or_commit()` 若没有任何 staged home image，仍会留下一个空 `deferred_transaction`。后续 direct metadata guard 只按 `Some` 判断 pending，于是对零 image 执行完整的四阶段 JBD2 commit。
- **修复**：总 staged image 数为零时直接释放 writer token，不保存 deferred batch；只有非空 batch 才允许 size/timestamp inode image 通过 transactional path 合并。无 pending 的 metadata 保持既有 direct path。
- **验证**：用诊断按 `JournalCommitReason` 和 phase 计数确认 `DirectMetadataBarrier` transaction 归零、每个保留 transaction 仍恰有四个 phase；结合 data-before-metadata 单元测试与 fsync/sync QEMU 回归。
- **相关文件**：`dependency/another_ext4/src/ext4/{journal_transaction,journal,low_level}.rs`

### journal flush 数必须按 commit reason 与 flush phase 分离

- **现象**：随机写诊断看到大量 device flush，若只统计总数会误判为同数量的 journal commit，进而把 cleanup 或 durability I/O 错归因给 PageCache writeback。
- **根因**：一个 `commit_journal` 包含 ActiveLog、CommitRecord、Checkpoint、TailUpdate 四个设备 flush；此外 direct metadata、`fsync`/`sync` 等一致性边界可独立执行 device flush。旧的混合计时无法区分它们。
- **修复**：诊断插桩同时记录 transaction id/reason、四个 phase 和独立 boundary flush；报告先验证 `journal_flush_count = transaction_count × 4`，再将余数归为明确的 boundary flush。`staged_blocks=0` 只表示 deferred journal staging，不能用于否定周边 direct metadata I/O。
- **教训**：聚合优化只能减少可合并 transaction 的数量，不能删除四阶段的 journal 顺序或绕过 `fsync`/`sync` durability boundary。先按 reason 切分写回 commit 与 cleanup commit，再决定优化范围。
- **相关文件**：`dependency/another_ext4/src/ext4/{journal_transaction,journal,mod,low_level}.rs`、`os/src/{task/perf.rs,fs/ext4_another/blockdev.rs,fs/sysfs/files/diag.rs}`

### write syscall 前台成本要按 syscall→uaccess→VFS→PageCache 边界拆分，不能只拆 PageCache

- **现象**：随机 writers 的 `write(2)` 总 ticks 中 PageCache 前台只占 ~24%，剩下 ~76% 在 PageCache 之外；若继续微调 lookup/lease/copy/commit，收益 <1%。
- **根因**：`write` 与 `pwrite` 走不同 syscall 包装，但 `write_from_user`（非定位写）原先没有任何边界计时——只有 `pwrite_from_user`/`pwrite_user` 有 `PWRITE_*` 计数器，且 `mutations.rs` 的 `write_at_user` setup/post 是无条件记录的（两种路径都计）。误以为写路径已被 PWRITE_* 覆盖是常见陷阱。
- **修复**：给 `write` 路径镜像一套 `WRITE_*` 边界计数器（fd prep、uaccess、VFS mode/seals、offset/touch 收尾），PageCache 前台继续用既有 `pc_write_cycles`；在 `sys_write` 分发前记 fd/fsize 准备，在 `write_from_user` 内把 UserBufferReader 构造与 `file.write_user` 分开计时，`File::write_user` 内镜像 `pwrite_user` 的 mode/seals/offset 三段。用 `write_total_count == pc_write_calls == write(2) count` 做路径覆盖自检，并确认 pwrite 窗口的 `WRITE_*` 全零（两条路径不串扰）。
- **教训**：有界拆分一轮后若没有任何子桶稳定达到总成本 ~25%（本案例最大两个为 PageCache ~21–22% 和通用 syscall 残留 ~23%），即按止损门槛停止该方向深挖，转向差距更大的基准（lat-pagefault ~25x 对 random-writers ~3x）。新增计数器必须同步声明 → recorder/no-op stub → sysfs 导出 → reset 四件套，且要用 `pc_read_user_calls` 类已覆盖路径的比值验证 workload 真的命中目标代码段。
- **相关文件**：`os/src/{task/perf.rs,syscall/fs/sys_write.rs,syscall/fs/common.rs,fs/vfs/file.rs,fs/sysfs/files/diag.rs}`

### la64 大量 page fault 慢
- 检查陷阱入口是否有不必要的 `invtlb`
- 检查页帧清零是否用了高效的 64-bit store 而非 byte-wise

### fork/wait 越来越慢
- 检查 TID 分配器是否有 O(n²) 查重
- 检查物理页释放是否有线性扫描 free-list

### lmbench 污染后 pipe/context switch/open/stat 同步变慢
- 先用 counters 区分旧目录线性扫描和 scheduler-loop 后台维护：如果 `dir_full_scan_entries=0`、dentry miss rate 稳定，但 pipe/context switch 也变慢，优先查调度循环里的 reclaim/stale prune。
- 对 scheduler-loop maintenance 做 rdcycle 分阶段计时，而不是只看总耗时。典型阶段包括 FIFO registry、ext4 registry、`prune_inode_objects`、`prune_children_stale_entries`、page cache metric、clean shrink。
- 如果 stale weak cleanup 每固定 tick 全量扫描 inode/children registry，即使 `stale_weak=0` 也会随缓存规模增长而拖慢非 FS microbench。修复优先级通常是降频、压力触发、dirty flag 或 incremental prune，而不是继续扩大 dentry cache。
- 相关文件：`os/src/fs/reclaim.rs`, `os/src/fs/ext4/ext4fs.rs`, `os/src/task/processor.rs`

### reclaim 平均成本下降但污染态 lmbench 仍退化
- 先同时看 `cycles_total/avg` 和 `cycles_max`。固定周期 batching 可能让平均值变好，却把原本分散的 stale weak cleanup 攒成单次长尾尖刺。
- 如果 S0 变好但 S1/S2b 的 `open/stat/pipe/read/write` 仍同步恶化，重点看 `prune_kids cycles_max`、`kids_removed` 和 budget hit，而不是只看 `prune_kids` 占比。
- 修复模式：把全量 prune 或固定间隔 batching 改成 cursor/budget 增量回收；每次 reclaim 限制 parent inode 和 child entry 扫描量，并输出 `scanned/budget_hit` 证明工作被分摊。
- 相关文件：`os/src/fs/reclaim.rs`, `os/src/fs/ext4/ext4fs.rs`

### budgeted reclaim 长尾下降但 total cycles 仍高
- `budget_hit=100%` 不一定表示真实 backlog 没扫完；如果 parent 预算按 raw registry entry 计数，budget hit 也可能只是 cursor 每轮正好扫满固定父项。
- 判断 S2/S2b 这类重污染场景时，要同时看 `kids_removed`、`kids_entries_scanned`、`kids_skipped` 和 `prune_kids cycles_total`。如果 `kids_removed` 很低但 cycles_total 很高，优先怀疑反复空扫/近空扫，而不是盲目扩大 budget。
- 修复模式：在 cursor/budget 基础上增加 dirty/event-driven generation；normal reclaim 在 generation 追平后跳过，heap pressure/critical 再 force scan，避免 stale Weak 自然过期没有回调导致永不清理。
- 相关文件：`os/src/fs/reclaim.rs`, `os/src/fs/ext4/ext4fs.rs`

### 对象数量 budget 无法约束 reclaim 单次长尾
- **现象**: shrink/reclaim 已有 entry budget，但 `cycles_max` 仍出现数量级尖刺；同时 `removed` 大幅增加，说明不是空扫，而是一次调用内连续处理大量可回收对象。
- **定位方法**: 把 `removed/scanned/budget_hit/skipped` 与分阶段 `cycles_max` 同看。若 entry 数很小但 max 很高，说明单个对象操作或连续 stale 段成本不可用“条目数”近似；需要增加 `time_budget_hit`/cycle slice 之类的直接时间片计数。
- **修复模式**: 在 cursor/budget 的基础上加 cycle 时间片；时间片命中时保存游标并返回 scheduler loop，未完成工作留到下轮。不要在 scheduler reclaim 路径里调用真正的 task yield，因为此时可能没有当前用户任务可安全挂回 ready queue。
- **相关文件**: `os/src/fs/reclaim.rs`, `os/src/fs/ext4/ext4fs.rs`

### heap_trace live 不回落但 PCB/TCB 正常
- 先区分真实生命周期泄漏和缓存型常驻：同时看 `zpcb/stale/tcb`、heap used、free frames、对象 owner。
- la64 需要额外检查架构特定 cache，例如 kernel stack 以 `Vec<u8>` 从 kernel heap 分配并可能被全局 cache 保留；1000 fork/futex 压力可把缓存打满，看起来像 heap leak。
- 资源报告也要一起查：`/proc/meminfo` 的 `MemAvailable` 如果只看 free frames，可能把静态预留但空闲的 kernel heap 漏掉，导致 LTP 大内存用例误判 `TCONF`。
- 修复模式：给大对象 cache 设置字节上限，保留小规模复用；对用户可见资源报告区分 `MemFree` 和估算型 `MemAvailable`。

### 用户 buffer 大小与实际 copy 长度不一致
- 症状：同一 syscall 在 glibc/musl 或不同架构下偶发 `EFAULT`，但实际输出内容很短，例如 `getcwd()` 只复制当前路径却按用户传入的 `PATH_MAX` 校验整段 buffer。
- 优先检查：syscall 是否用"用户声明容量"做 VMA 可访问性校验，而真实 `copy_to_user` 只会访问更短的 `write_len`。
- 修复模式：保留 Linux 语义需要的容量判断（如 `ERANGE`），但地址可访问性和 `UserBufferWriter` 长度按实际读写字节数校验。

### UserBufferWriter::write_from 总是返回 Ok — 调用者必须检查实际写入长度
- **症状**：`write_from().is_err()` 永远为 `false`（因为 `write_from` 实现为 `Ok(self.buffer.write(src))`），所以当 `UserBuffer::write` 返回少于 `src.len()` 的字节数时，调用者误以为复制完全成功，返回部分字节数而非 `EFAULT`。
- **根因**：`UserBufferWriter::write_from` 的 `Result<usize, isize>` 签名暗示可能返回 Err，但其内部 `UserBuffer::write` 永远不返回错误——它只返回实际写入字节数，可能少于请求长度。包装层丢失了部分写入的信号。
- **修复**：不要依赖 `.is_err()` 检查 `write_from`。必须检查返回值 `copied != src.len()`，或者用 `unwrap()` 获取实际写入数后再比较。
- **检查清单**：所有调用 `write_from` 的地方都必须检查返回值（当前约 12 处调用，包括 `os/src/syscall/fs.rs`、`os/src/net/syscall/getsockopt.rs` 等）。
- **相关文件**：`os/src/mm/uaccess.rs:363`, `os/src/syscall/fs.rs`（`sys_getdents64` 等），`os/src/net/syscall/getsockopt.rs`

## QEMU / 测试

### SMP 早期 IPI 正常但首次远程任务后整颗 AP 静默失联

- **现象**: online、idle、BSP→AP PING 和 AP→BSP round-trip 全部通过；一旦 AP 首次
  context switch 到动态 kernel stack，远程任务超时，后续 PING/STOP 对全部 AP 级联失败，
  且没有普通 kernel panic 文本。
- **首个多米诺骨牌**: 先按测试顺序找第一个失败，不要把后续 IPI、STOP 超时拆成多个 bug。
  若首次 task dispatch 是分界点，优先核对切换前后的 `sp` 地址域、页表根和 trap 栈。
- **典型根因**: BSP 构造并激活 kernel page table 只写了本 CPU 的 SATP/PGDH；AP 仍靠
  恒等映射或 DMW 执行 text/data/idle stack，所以早期 IPI 会制造“MMU 已可用”的假象。
  `__switch` 换到高虚拟地址 stack 后首个压栈失败，kernel trap 又沿坏 `sp` 保存现场，可能
  形成二次故障循环而无法打印 panic。
- **修复模式**: 把“共享页表对象已构造”和“每 CPU 页表根已安装”分开建模。AP 在
  scheduler-ready Acquire 之后、scheduler-entered Release 之前激活本 CPU kernel page
  table。AP 激活后新增的动态 kernel-global 映射仍需先做目标 TLB flush/ack，再发布依赖
  该映射的 runnable，不能把冷 TLB 当作正确性前提。
- **验证**: 除 scheduler mask 和 remote task exactly-once 外，必须继续运行后续 PING 与
  terminal STOP；否则只能证明任务计数变化，不能证明 AP 已返回稳定 idle 调度循环。
- **相关文件**: `os/src/mm/mod.rs`, `os/src/smp.rs`, `os/src/task/processor.rs`,
  `os/src/hal/arch/{riscv,loongarch64}`

### 远程 runnable 已发布但 idle CPU 仍可能睡眠

- **症状**: 状态已经是 `Queued(target)`、目标 runqueue 也能在诊断中看到任务，但 AP 偶发
  停在 `wfi/idle`；或者为了避免该问题，在持有 runqueue/全局调度锁时直接发送 IPI，
  使并发锁序和失败回滚变得不可证明。
- **根因模式**: “容器所有权提交”和“硬件 doorbell”是两个不同阶段。先发 IPI 再入队会让
  AP 被唤醒、看到空队列后重新睡眠；锁内发 IPI 又会把硬件失败、IPI handler 和调度锁耦合。
- **固定协议**: 在唯一 owner 锁域内完成状态 CAS 与目标 runqueue 插入；释放目标队列及
  全局 registry 锁；最后按聚合 CPU mask 发送 `RESCHEDULE`。IPI handler 只写 per-CPU
  原子 pending/need-resched，真正 fetch 在 idle 安全点发生。批量 wake 循环每次必须在
  处理下一个目标前释放前一个 runqueue，不能为聚合 mask 同时持有多把队列锁。
- **idle 对偶条件**: 目标 CPU 必须在关中断窗口检查 runqueue/pending，再执行架构 idle；
  doorbell 在 check→wait 窗口到达时必须保持 pending 并使 idle 返回。
- **验收**: 不只做 IPI ping。让远端任务真实 `Blocking -> Blocked`，确认 current 和 runqueue
  均已释放，再从另一 CPU 经生产 WaitQueue 批量唤醒；验证它回到预期 CPU、只运行一次，
  并在 terminal STOP 前清空 current/runqueue。

### 跨 runqueue 搬运必须显式建模“无容器 owner”窗口

- **危险模式**: 在源队列锁内摘除任务，释放源锁后直接去锁目标队列，但状态仍写成
  `Queued(source)` 或提前写成 `Queued(target)`。前者会让并发 fetch/remove 去错误队列，
  后者会让观察者看到目标 owner 却找不到容器节点；同时持有两把 rq 锁虽能隐藏窗口，却会
  扩大锁序和死锁面。
- **固定协议**: 先在无调度锁区完成所有可能等待的准备（如目标 kernel-stack TLB 同步）；
  source 锁内把 `Queued(source)` 交给一个不携带 CPU 的 `Migrating` owner 并摘除；释放 source；
  由唯一迁移调用方发布 placement 字段；target 锁内完成 `Migrating -> Queued(target)` 和插入。
  doorbell 必须在 target 解锁后发送。
- **观察者规则**: affinity/remove/nice 更新遇到 `Migrating` 只能在不持普通锁时重试；wake 将其
  视为已经 runnable；终态转换必须 fail-stop。若检查方仍持有 CPU A 的 rq 锁，任务不可能在
  同一窗口“迁走又迁回 A”，因为回迁本身必须先取得这把锁；模型报告中的竞态时间线必须逐步
  对照实际 `drop(lock)` 位置，不能只按函数意图裁决。
- **相关文件**: `os/src/task/run_queue.rs`, `os/src/task/task.rs`, `os/src/task/manager.rs`

### TLB sequence ack 必须证明“本轮失效动作发生在本轮请求之后”

- **危险顺序**: handler 先 flush，再读取 request sequence 并写 ack。若新请求恰好在旧 flush
  之后发布，handler 会用旧失效动作确认新 sequence；发送方看到 ack 到齐后释放 frame，
  目标 CPU 却仍可能保留新请求所针对的旧翻译。
- **固定顺序**: 发送方先 Release/AcqRel 发布 request，再发布 mailbox reason；handler 先
  Acquire 快照 request，随后执行架构 TLB invalidate，最后 Release 写 ack。ack 的含义必须是
  “我在观察到至少该 sequence 后完成了失效”，而不仅是“我最近做过一次 flush”。
- **合并 reason bit**: 多个请求可以合并为一次 handler，但 handler 应确认自己快照到的最新
  sequence，发送方用单调比较等待。sequence wrap 必须显式防御，不能静默把 0 当正常轮次。
- **相关文件**: `os/src/smp.rs`, `os/src/hal/arch/{riscv,loongarch64}/mod.rs`

### 精确 MM 驻留测试不能把远端 IPI 次数当成唯一成功条件

- **现象**: 历史 CPU mask 改为 active mask 后，TLB/membarrier 协议本身正确，但原测试偶发
  报告“目标 CPU 没有收到 request”。不同架构通过与否还可能仅由 timer 时序决定。
- **根因模式**: helper 在观察窗口调用调度安全点后可以合法切离 MM。切离侧 full fence、
  active bit 清除和再次进入时的 generation catch-up 已经兑现协议，此时修改方不再向它发
  IPI；继续断言 request 必须增长，实际是在测试旧的历史 mask 行为。
- **验证模式**: 若要专门覆盖远端 IPI 分支，让 helper 保持本地中断开启但不要主动调度，
  使 IPI handler 能运行而 MM residency 保持稳定；若要覆盖切离分支，则显式等待
  `Blocked + active bit 清除`，在零目标窗口修改 PTE，并验证无 IPI、generation 落后及
  wake 后本地补刷。不要放宽 timeout 或接受任意一个结果掩盖竞态。
- **相关文件**: `os/src/kernel_tests/smp.rs`, `os/src/mm/tlb.rs`,
  `os/src/task/processor.rs`

### 测试不能用线程级终态推断进程级收尾已经完成

- **现象**: 重复并发测试中，`live_threads == 0` 且所有 TCB 已是 `Zombie`，但紧接着断言
  PCB `Zombie` 偶发失败；双架构在同一个 repeat 位置出现相同现象。
- **根因模式**: 最后线程先释放 live token、发布 TCB 终态，随后才由 owner 调用进程级
  `finish_exit()`。这些是有意分层的发布点，中间窗口不是生产退出失败。
- **验证模式**: 完成等待必须包含测试真正要断言的最终条件，例如 PCB `is_zombie()`；不要
  只增加轮询时间，也不要在尚未等待最终发布点时修改生产退出顺序。
- **相关文件**: `os/src/task/task.rs`, `os/src/task/process.rs`,
  `os/src/kernel_tests/smp.rs`

### 不要在 `Drop` 中等待跨核 ack：析构只提交退休，安全点完成回收

- **危险模式**: 资源析构时直接获取页表锁、发送 IPI、等待远端 ack，再释放 frame。Rust 的
  隐式 `Drop` 可能发生在任意容器替换或锁保护区内，调用者很难证明全局锁序；若资源还是
  当前 kernel stack，还会形成栈自毁。
- **固定模式**: `Drop` 只把资源标识提交到固定容量、无堆、短临界区的退休队列；CPU 已切回
  idle 栈且未持普通锁的安全点执行“清 PTE 并保留 frame → 释放 MM 锁 → 全核 flush/ack →
  释放 frame → 归还虚拟 slot”。队列锁不得跨 MM 锁或 ack 等待。
- **地址类型陷阱**: kernel stack allocator 常返回字节地址，而撤映射接口接收 VPN newtype；
  必须显式 `VirtAddr::from(byte_addr).floor()`，不要依赖 `usize.into()` 猜测单位。
- **相关文件**: `os/src/hal/mod.rs`, `os/src/hal/arch/*/kern_stack.rs`,
  `os/src/mm/kernel_space.rs`

### 临时开 IRQ 的同步等待可能截获 one-shot，回调仍必须留在原安全点

- **现象**: 为避免双向 shootdown 死锁，等待 ack 时临时开放本地 IRQ；focused 协议用例通过，
  但紧随其后的 timer 用例超时。硬件 timer 已在等待窗口触发，handler 只发布 deferred pending，
  而特殊 ktest runner 没有经过普通 trap-return 去消费它。
- **错误修复**: 在 MM/TLB 同步层直接运行 timer callback 或调度器工作。这样会让一个底层
  内存一致性原语获得跨子系统副作用，也可能在调用者未声明的锁/生命周期上下文切换任务。
- **正确修复**: 保持 IRQ handler 只发布 pending；生产调用链返回原有 trap/scheduler 安全点。
  若测试 harness 绕过该路径，测试闭环应显式调用同一个生产安全点入口，而不是复制回调逻辑。
- **相关文件**: `os/src/smp.rs`, `os/src/task/processor.rs`, `os/src/kernel_tests/smp.rs`

### 委托只读审查必须冻结受测源码

- **现象**: 模型报告本身完整、进程 exit 0，但包装器发现审查前后 source fingerprint 不同。
- **规则**: 这种报告只能作为风险线索，不能作为当前 patch 的独立验收证据。设计探索可以与
  实现并行；声称“最终只读审查”时必须冻结源码，记录 HEAD、status hash 和 tracked diff hash，
  并由主 Agent 对模型结论逐条裁决，不能只转发 `PASS`。

### RISC-V MTTCG 下不能把 OpenSBI Boot HART 写死为 hart0

- **现象**: `-smp 2` 时内核正常，扩到 4/8 核后只看到 OpenSBI banner，
  内核无输出或所有 CPU 都在等待 AP release；日志中的 `Boot HART ID`
  可能是 1、2、6 等非零值。
- **根因**: 省略 `-accel` 并不等于单线程。QEMU TCG 在前后端支持且没有
  icount/replay 等冲突功能时默认启用 multi-thread；OpenSBI cold-boot
  lottery 的获胜者不属于 OS ABI 保证。宿主调度恰好长期选中同一 hart
  也不能当作固件契约。
- **修复**: 将实际启动 hart 登记为逻辑 CPU0，建立硬件 hart ID 与连续
  逻辑 CPU ID 的可逆映射；调用 SBI HSM 时必须把逻辑目标反向映射回真实
  hart ID。只有控制并验证了定制 OpenSBI cold-boot policy 时，才能删除
  映射并固定物理 hart0。
- **验证**: 同时覆盖显式 `-accel tcg,thread=multi` 和比赛式省略
  `-accel`、`-bios default -smp 8`；判定必须检查 Boot HART、online
  mask、测试 PASS 和无 panic，不能只看 QEMU 退出码。
- **相关文件**: `os/src/smp.rs`, `os/src/hal/arch/riscv/sbi.rs`,
  `os/make/rv64.mk`

### LoongArch QEMU 直启的 AP 在 slave boot ROM 等 mailbox + IPI

- **现象**: CPU0 可以完整启动，但 `CORE_NUM=2` 时 online mask 始终只有
  bit0；把 `start_secondary_cpu()` 写成 no-op 会稳定等待超时。
- **根因**: QEMU 9.2 direct-kernel boot 只让第一个 CPU 进入 kernel
  entry；其余 CPU 从 pflash 的 `slave_boot_code` 启动，打开 IPI 后
  `idle`，直到 mailbox 含入口且收到 IPI vector 0。
- **修复**: CPU0 先向 `IOCSR_MAIL_SEND(0x1048)` 写目标 CPU 和入口低
  32 位，执行 `dbar` 保证 mailbox 先于 doorbell，再向
  `IOCSR_IPI_SEND(0x1040)` 发送 vector 0。AP 跳到 `_start` 后仍需独立的
  Release/Acquire 启动阶段门，不能把“硬件已唤醒”等同于“共享内存可用”。
- **官方依据**: QEMU v9.2.1 `hw/loongarch/boot.c`、
  `hw/intc/loongson_ipi_common.c` 和
  `include/hw/intc/loongson_ipi_common.h`。
- **相关文件**: `os/src/hal/arch/loongarch64/mod.rs`, `os/src/smp.rs`

### syscall trap 中不能同步 poll 会等待 VirtIO completion 的网络栈

- **现象**：network-enabled RV64 QEMU 在用户态第一次创建 UDP socket 后停在启动同步逻辑；GDB 显示 CPU 在 `VirtQueue::add_notify_wait_pop()` 的 `while !can_pop()` 自旋。
- **根因**：`UdpSocket::new()` 在 `sys_socket` trap 路径内调用完整 `NET_INTERFACE.poll()`。smoltcp 的 DHCP egress 进入 `VirtIONetRaw::send()`，该路径等待 VirtIO used-ring completion；而 trap 上下文的 `sstatus.SIE` 已关闭，completion interrupt 无法运行，因此同一 hart 永远等不到 `can_pop()`。
- **根治**：发送路径必须感知中断状态。`NetTxToken::consume()` 检查 `hal::irq_enabled()`（riscv `sstatus.sie()` / la64 `CrMd.is_interrupt_enabled()`）：SIE=1 走原阻塞发送；SIE=0（syscall 上下文）把包放入**独立的全局延迟队列**（`static DEFERRED_TX_QUEUE: Mutex<Vec>`），由下次调度器 poll（中断开启）在 `poll_once` 中 drain 真正发送。同时删除 socket 构造函数里多余的 `poll()`。
- **⚠️ 锁重入陷阱**：延迟队列**不能**放进 `NetInterfaceInner`——`push_deferred_tx` 被 smoltcp poll 的 `inner_handler` 内调用，此时 `NET_INTERFACE.inner` 已被持有，二次 `inner.lock()` 造成 `spin::Mutex` 重入死锁。必须用独立于 `inner` 锁的全局队列。
- **正确性**：DHCP/ARP/TCP 有协议级重传计时器兜底，延迟发送不破坏语义（smoltcp 认为 consume 成功即推进状态机）。
- **定位**：GDB 确认 `sstatus.SIE=0`、`sie` 缺少 external interrupt 位，调用链 `sys_socket → UdpSocket::new → NetInterface::poll_once → VirtIONetRaw::send`。
- **相关文件**：`os/src/net/adapter.rs`、`os/src/net/config.rs`、`os/src/net/socket/inet/datagram/udp.rs`、`os/src/hal/{mod.rs,arch/riscv/sbi.rs,arch/loongarch64/sbi.rs}`

### LA64 首次用户态恢复跳入 kernel trap stub

- **现象**: competition 启动在 PID1 已入 ready queue、`trap_return()` 已执行后静默空转，始终没有 `[initd]` 首行。
- **根因**: `restore_va` 用 `strampoline` 对 `__restore` 做重定位。LA64 static link 中该 extern 函数符号可解析为 `skern_trap`，使 restore 跳入错误的 kernel trap 区域，而不是 `.text.trampoline` 中的 `__restore`。
- **修复**: LA64 `trap_return()` 直接以链接后的 `__restore as usize` 作为跳转目标；不要通过 `strampoline` 重新计算该地址。
- **教训**: bare-metal assembly entry symbols经 Rust FFI 取地址时，必须核对最终 ELF 符号和反汇编。若首个用户任务已被 scheduler 选中却没有用户输出，优先比较计算出的跳转地址与 `llvm-nm`/`llvm-objdump` 中的 `__restore`。
- **相关文件**: `os/src/hal/arch/loongarch64/trap/mod.rs`, `os/src/hal/arch/loongarch64/trap/trap.S`

### 内联汇编用泛型输入搬运 ABI 参数时发生寄存器自覆盖

- **现象**: Rust 源码按顺序把多个 `in(reg)` 输入 `move` 到 `$a0/$a1/$a2`，debug
  阅读看似正确，优化后二进制却把 trap context 当成 ASID 传入恢复入口；用户态随即出现
  极低坏地址的 store/fetch fault，而不经过该返回路径的内核 focused 测试仍可全部通过。
- **根因**: 编译器只知道所有泛型输入在汇编开始时可读，不知道模板内部前一条 `move`
  会覆盖后续输入。它可以让某个后续输入复用 `$a0/$a1/$a2`，于是模板尚未读取该输入，
  其物理寄存器就已被前一条指令改写。
- **修复**: 固定 ABI 参数直接使用 `in("$a0")`、`in("$a1")`、`in("$a2")` 等显式
  寄存器约束；跳转目标使用独立泛型输入。构建 release ELF 后必须检查最终反汇编，确认
  参数准备顺序和物理寄存器，而不能只检查 Rust/汇编模板。
- **教训**: `asm!` 的数据流边界由 operand constraint 表达，不会自动分析模板内的
  指令依赖。凡是“多个泛型输入 → 固定 ABI 寄存器 → noreturn 跳转”的桥接代码，都应
  优先直接绑定 ABI 寄存器，并把优化后二进制反汇编纳入验证。
- **相关文件**: `os/src/hal/arch/loongarch64/trap/mod.rs`,
  `os/src/hal/arch/loongarch64/trap/trap.S`

### 候选 LoongArch toolchain 在 GNU ld 遇到 `R_LARCH_CALL36` (`0x6e`)

- **现象**: 容器 GNU ld 2.41 链接包含 `R_LARCH_CALL36` 的 LoongArch 对象时报告 `unsupported relocation type 0x6e`；Cargo 的 `linker = "loongarch64-linux-gnu-gcc"` 会把该限制带入 OS 与 user 两条构建路径。
- **修复**: 使用一个受版本控制、将 PATH 限制为镜像系统路径且无条件 `exec /usr/bin/clang --target=loongarch64-linux-gnu -fuse-ld=lld "$@"` 的 wrapper，并同时更新 canonical 与已消费的 Cargo 配置。不要在 wrapper 中改变/过滤 Cargo 参数。
- **验证**: 先以 Cargo verbose 日志固定原 linker、target 与 `-T`/`-nostdlib`/`-static` 参数；以 `clang -###` 确认 image Clang 实际选择 `ld.lld -m elf64loongarch`；在临时 Rustup/Cargo homes 和干净容器副本中离线完成 LA64 build，再检查 ELF `Machine: LoongArch` 与日志中不存在 `0x6e`。
- **相关文件**: `scripts/loongarch64-clang-lld.sh`, `cargo-config/{os,user}/config.toml`, `{os,user}/.cargo/config.toml`

### `make docker` 拉镜像超时但 Docker CE 源已换国内镜像

- **现象**: `apt update`/`apt install docker-compose-plugin` 已走清华等 Docker CE 软件源，但 `make docker` 仍在拉 `os-dev` 镜像时 timeout。
- **根因**: Docker CE APT 源只影响 Docker 软件包安装；`docker compose up` 拉取镜像走容器 registry（Docker Hub 或显式 registry 前缀），由 `/etc/docker/daemon.json` 的 `registry-mirrors` 或 compose 中的镜像地址决定。
- **修复**: 先用 `docker compose config` 确认实际 image，再用国内 registry 前缀或可用 daemon mirror 拉取；项目入口应支持 `DOCKER_IMAGE=...` 覆盖。
- **相关文件**: `docker-compose.yml`, `Makefile`, `scripts/run_test_docker_parallel.sh`

### 对照实验必须同时确认 kernel 和 sdcard 用户态产物

- **现象**: 在旧 commit 上应用用户态 probe 后直接跑 `make rv64-run`，日志可能仍显示候选版本行为；或者重新跑了 `*-kernel-build-only` 后，QEMU 日志仍没有新增 probe 输出。
- **根因**: `make *-run` 的 `comp` 目标会直接使用已有 `../kernel-rv`/`../kernel-la`；`*-kernel-build-only` 会重建 user/initramfs/kernel，但不会自动更新测试盘 `sdcard-*.img` 里的 `/initproc`。如果 stage-1 实际执行的是测试盘旧 `/initproc`，用户态 probe 不会生效。
- **修复**: 做旧版/新版对照时必须先重建对应 kernel，再显式确认或注入测试盘上的 `/initproc`；可用配置行新增字段、输出字符串、二进制大小或 `debugfs stat /initproc` 确认实际运行产物。
- **教训**: 对照实验不能只看源码 HEAD；必须把内核产物、initramfs 产物、sdcard 用户态二进制和 `/os_test.conf` 四者同时纳入控制变量，否则容易把旧产物误当成原版或候选行为。
- **相关文件**: `os/make/rv64.mk`, `os/make/la64.mk`, `os/Makefile`, `user/src/bin/init.rs`, `user/src/bin/initproc.rs`

### 性能探针自身污染 scheduler/pipe 指标

- **现象**: 加了 rdcycle/atomic counters 后，scheduler loop_avg、pipe EAGAIN rate 或 lmbench group time 出现新的放大；报告把“未拆分的剩余开销”直接归因到架构路径。
- **根因**: 高频调度循环和 pipe read/write 每秒可能执行数十万到数百万次；即使单个 atomic/rdcycle 很轻，累计也会改变 QEMU 时序。跨架构绝对 cycles 还可能来自不同计数源，不能直接比较倍率。
- **修复**: profile counters 默认关闭，只在 profile_before reset 时打开，profile_dump 后关闭；同时保留一组无 profile baseline，用来量化探针自身税。跨架构先看同架构 S1/S0，再看 wall time 和标准化事件数。
- **教训**: “non-reclaim delta” 只是未解释桶，不等于 SBI/trap/TLB。必须先拆 console、timer、wake、futex、fetch、switch_prep 等 scheduler stage，再做核心修复。
- **相关文件**: `os/src/task/processor.rs`, `os/src/fs/dev/pipe.rs`, `user/src/bin/initproc.rs`

### trap/interrupt 计时跨 context switch 导致虚高

- **现象**: timer trap/handler cycles 总量远大于同一 profile 的 scheduler loop cycles，且单次 max 接近整段任务延迟；报告看起来像 trap 本体比其它架构慢几十倍。
- **根因**: trap handler 内部可能调用 `suspend_current_and_run_next()`。如果在调用前读 cycle、调用返回后才记录，计时会跨过“当前任务被调度走直到再次调回”的整段时间，而不是纯 trap handler 成本。
- **修复**: handler 本体计时必须在可能 context switch 前完成记录；trap 入口成本和 handler 成本分开记录。若需要观察“被调度走多久”，另加独立的 off-CPU/latency counter，不要混入 trap cycles。
- **相关文件**: `os/src/hal/arch/riscv/trap/mod.rs`, `os/src/hal/arch/loongarch64/trap/mod.rs`, `os/src/task/manager.rs`

### Docker Compose 进入了别人的容器

- **现象**: 在自己的工作区执行 `docker compose exec os-dev ...`，但容器内 `/app` 实际挂载到队友目录，后续 `docker cp` 或容器内写入会改到别人的源码。
- **根因**: Compose 默认从 `.env` 读取 `COMPOSE_PROJECT_NAME`。如果多个工作区使用了同一个 project name，`docker compose exec` 会连接到已存在的同名 service 容器，即使当前 shell 位于另一个源码目录。
- **修复**: 每个开发者/实验任务必须使用独立 project name，并在任何编译或 QEMU 前确认挂载：
  `docker compose ps`、`docker inspect <container> --format '{{range .Mounts}}{{println .Source "->" .Destination}}{{end}}'`。期望 `/home/<user>/projects/MangoCore -> /app`。
- **教训**: 结果报告的 manifest 必须记录 `COMPOSE_PROJECT_NAME`、container id、git HEAD、host cwd 与 mount 映射；若这些字段缺失，性能数据不能作为提交依据。
- **相关文件**: `.env`, `docker-compose.yml`, `cc-codex/results-*/manifest.json`

### la64 编译失败（61+ errors）— 缺少 initramfs 特性

- **现象**: `rv64-kernel-build-only` 成功，但同样的 initramfs 代码在 la64 上报大量编译错误
- **根因**: la64 的 `make/la64o.mk` 使用 `--no-default-features`，而 rv64 使用默认 features（含 `initramfs`）。la64 内核中部分代码路径只在 `initramfs` 特性 gate 下才编译通过
- **修复**: la64 构建必须显式传递 `initramfs`：
  ```
  cargo build --no-default-features --release --features "comp board_laqemu block_virt_pci log_off initramfs"
  ```
  或通过 `make -f make/la64o.mk build EXTRA_FEATURES="initramfs"`
- **注意**: 根 Makefile 没有 `la64-kernel-build-only` 目标；`rv64_all`/`la64_all` 通过不同的 Makefile 目标处理特性
- **相关文件**: `os/make/la64o.mk`, `os/Makefile`

### la64 clone09 停在 CLONE_NEWNET 后无 timeout

- **现象**: `ltp_runner=inline` 单跑 `clone09`，日志停在 `create clone in a new netns with 'CLONE_NEWNET' flag`，超过 LTP 标称 30s timeout 仍无 `TPASS/TFAIL/TBROK`，且没有 BTreeMap/heap panic。
- **根因**: la64 64KiB kernel stack 对 netns clone 路径栈深度不足；guard page 版本会避免静默 heap corruption，但容量仍可能导致该路径无法正常返回。
- **修复**: 将 la64 `KERNEL_STACK_SIZE` 提升到 128KiB，同时保留每 slot 的 guard page；重新编译并注入 focused LTP 配置验证。
- **教训**: clone/netns 路径挂住时不要只看用户态 LTP timeout；对比 kernel stack 容量和 guard 命中情况，若扩大栈后用例恢复，说明是栈深度问题而非测试 harness 或工具盘问题。
- **相关文件**: `os/src/hal/arch/loongarch64/config.rs`, `os/src/hal/arch/loongarch64/kern_stack.rs`, `os/src/hal/arch/loongarch64/trap/mod.rs`

### la64 全量压力触发 kernel stack slot 上限

- **现象**: la64 全量 LTP 跑到 syscalls 尾段的 `futex_cmp_requeue01` 后，大量 waiter 打印 `wasn't woken up: ETIMEDOUT`，随后出现 `[task_quota] SOFT LIMIT reached: used=921/1024`，最终在 `clone` 路径 panic：`la64 kernel stack slot 1024 exceeds max 1024`。
- **根因**: la64 kernel stack 改为固定 VM slot 后，`KERNEL_STACK_MAX_SLOTS` 是硬容量边界；压力用例留下大量任务时，stack slot 分配可能先于普通 clone 失败路径触发边界 panic。
- **修复**: 后续应让 kernel stack 分配成为 fallible，并在 clone 中把 slot 耗尽转成 `EAGAIN/ENOMEM`；同时复核 task quota 与 stack slot 容量是否完全一致，以及超时 waiter 是否及时回收。
- **教训**: 全量回归要区分三类问题：guard 命中的真实栈溢出、BTreeMap/heap 这类随机破坏、slot/quota 这类确定性容量上限。看到 slot panic 时优先检查 quota、allocator 和压力用例残留任务，而不是继续调大单个栈大小。
- **相关文件**: `os/src/hal/arch/loongarch64/config.rs`, `os/src/hal/arch/loongarch64/kern_stack.rs`, `os/src/task/quota.rs`, `os/src/task/task.rs`

### rv64 musl LTP retry helper 变成 UINT_MAX timeout

- **现象**: 某些本应很快 `TCONF` 的 LTP suite 用例在 rv64/musl 下逐个触发外层 60s per-case timeout；日志里 `tst_test.c` 打印 `Timeout per run is 1193046h 28m 15s`。
- **根因**: suite runner 注入 `LTP_TIMEOUT_MUL=2` 后，当前 rv64 musl LTP 镜像的 `strtod()`/浮点解析路径会把 timeout multiplier 算坏，LTP retry helper 变成 `UINT_MAX` 秒级重试。
- **修复**: 对 `rv64 + musl` 不导出 `LTP_TIMEOUT_MUL`，让 LTP 使用默认 multiplier；其它架构/libc 保持原超时放大。
- **教训**: 当 syscall 已返回预期 errno 但 LTP 没有进入 `TCONF/TBROK`，先查 `Timeout per run` 和 `TST_RETRY_FUNC`，不要直接把问题归到 syscall 阻塞。
- **相关文件**: `user/src/bin/ltprunner.rs`

### LTP timer test 固定小幅 oversleep

- **现象**: `clock_nanosleep02`/`nanosleep01` 每组样本都比请求时间稳定多睡约 0.5ms，`tst_timer_test.c` 报 `slept for too long`，但没有大幅长尾或随机卡顿。
- **根因**: syscall sleep 直接等全局 timeout 队列的真实 deadline，任务被唤醒并重新调度后存在固定尾部延迟；LTP 的截断均值阈值约 450us，尾部延迟会让短 sleep 全组失败。
- **修复**: timeout 队列提前一个很小的 guard 窗口唤醒，最后一段用短 `spin_loop()` 等到真实 deadline，避免早醒又降低调度尾部误差。
- **教训**: 看到所有样本都平移式 oversleep 时，优先看 deadline 唤醒后的调度尾延迟；若是少量异常大值，才优先查 timer interrupt、抢占或 QEMU 抖动。
- **相关文件**: `os/src/task/sleep.rs`

### LTP futex_wait timeout 固定小幅 oversleep

- **现象**: `futex_wait05` 中 `FUTEX_WAIT` timeout 样本稳定多睡约 0.5ms 到 0.8ms，`tst_timer_test.c` 报 `futex_wait() slept for too long`；基础 `futex_wait01-04` 和 `futex_wake01-03` 仍正常。
- **根因**: futex timeout 直接阻塞到真实 deadline，任务从 timeout queue 唤醒并返回用户态存在固定尾差；la64 QEMU 的短 timeout 出口尾差更大，10ms/25ms/100ms 档会超过 LTP 约 450us 阈值。
- **修复**: futex wait 在 deadline 前预留 guard 窗口，尾部仍保持在 futex wait queue 中自旋，期间继续检查 futex word、信号和是否被 `FUTEX_WAKE` 移出队列；la64 对相对 `FUTEX_WAIT` 的中短 timeout 额外补偿固定出口尾差。
- **教训**: futex timeout 精度不能直接套用 sleep 的“先出队再自旋”，否则可能丢掉 deadline 前的真实 wake；尾部自旋必须保持 wait queue 可观察或显式处理被 wake 移除的状态。
- **相关文件**: `os/src/task/threads.rs`

### libcbench pthread 超时但 futex 计数正常

- **现象**: libcbench `b_pthread_create_serial1` 卡到 120s timeout；clone/exit 计数已经接近完成，`fut_wait == fut_ready` 且无 futex timeout/intr，最后 syscall 长时间表现为 `read()` 返回 1024。
- **根因**: libcbench 的 `print_stats()` 会以 1KiB buffer 读取 `/proc/self/smaps`。如果 procfs 每个 chunk read 都重新生成完整 smaps，或者每次从第一个 VMA 扫到目标 offset，高 VMA/线程 churn 后会形成 O(N²) 级开销，看起来像 pthread/futex 卡住。
- **修复**: 对大文本 proc 文件采用 Linux `seq_file` 思路的 per-open 快照缓存：打开文件后生成一次文本，后续 offset 读取只切片复制；高 VMA 情况下 smaps 还应压缩非必要字段。
- **教训**: 性能测试超时要先用轻量计数排除同步原语本身；若 `last_sys=read` 且返回固定小块长度，优先查测试程序的统计读取路径和 procfs 生成策略。
- **相关文件**: `os/src/fs/procfs/mod.rs`, `os/src/fs/procfs/pid/smaps.rs`, `os/src/mm/address_space.rs`

### lmbench pipe 慢但 syscall 空转不慢

- **现象**: `Simple syscall` 已在微秒级，但 `Pipe latency`/`Pipe bandwidth` 仍明显偏慢。
- **根因**: pipe 走的是 VFS stream fd 路径；如果仍沿用普通文件的 offset 原子更新、append/seal/mtime 检查，或者每次 notify 前重新锁共享 ring 查询 peer，单次小包 ping-pong 会放大这些固定开销。
- **修复**: `FMODE_STREAM` 在 `File::read/write` 中直接调用底层 inode，不推进 offset；pipe 成功读写后复用 ring 锁内取得的 peer，并在无 fasync 监听者时跳过 `SIGIO` 空列表分发。
- **教训**: lmbench pipe 指标优先拆分 VFS stream 包装层、pipe ring、wait queue 三段看固定成本；不要只盯调度器或 copy 本身。
- **相关文件**: `os/src/fs/vfs/file.rs`, `os/src/fs/dev/pipe.rs`, `os/src/fs/vfs/fasync.rs`

### 页级 TLB 刷新疑似失效时用 full flush 做对照

- **现象**: PTE 权限或 PPN 已按预期更新，但同一用户 VA 仍反复触发相同类型 page fault；临时把页级 flush 替换为 full TLB flush 后问题消失。
- **定位方法**: 先保留 fault VA/PC/ASID/PPN 的窄范围日志，确认不是新的地址或新的权限错误；再做 page flush vs full flush 的最小对照。如果 full flush 有效而 page flush 无效，应检查架构指令的 ASID/global 参数、VPN 对齐和当前地址空间切换时机。
- **教训**: full flush 只能作为定位实验，不应作为最终 workaround。最终修复应让页级 invalidate 精确命中目标 ASID 或 global 映射，避免掩盖地址空间隔离 bug 和性能退化。
- **相关文件**: `os/src/hal/arch/loongarch64/tlb.rs`, `os/src/hal/arch/loongarch64/laflex.rs`

### getcwd 失败排查：区分 syscall 路径 vs libc manual walk 路径

- **现象**: musl `getcwd()` 报 "cannot access parent directories: Invalid argument"，但 glibc 的 `getcwd()` 正常。内核日志中看不到 `sys_getcwd` 调用。
- **定位方法**:
  1. 确认 libc 是否调用了 syscall — grep qemu.log 的 `syscall getcwd(17)`，如果只有 glibc 程序有、musl 程序没有 → libc 走了 manual walk 回退
  2. musl manual walk 使用 `fstatat("/")` 获取根 inode，再通过 `openat("..")`/`getdents` 逐级往上走，比较 inode 判断是否到根
  3. 比较 `fstatat("/")` 的 `st_ino` 和 `fstatat("..")` 的 `st_ino` — 如果不同，inode 不一致导致 musl 永远检测不到根
  4. 如果 inode 一致但 getdents 找不到匹配条目 → 检查 `d_ino` 是否与 `st_ino` 一致（bind mount 场景常见）
- **教训**: `sys_getcwd` 走的是 `FsStatus::working_path` 缓存（`cb9053a4` 引入），与 VFS 路径解析是两条独立链路。修复 `sys_getcwd` 不等于修复 VFS 层 ".." 语义。任何依赖 `find("..")` 的调用链都可能是下一个受害者。
- **相关文件**: `os/src/syscall/fs.rs` (sys_getcwd), `os/src/fs/vfs/mount.rs` (do_find, lookup_dotdot)

### chroot 后 bindgen/libclang 头文件缺失先查 `/proc/<pid>/fd` 路径

- **现象**: 进程已经能在 chroot 内打开源码，Cargo 也能打印正确的 workspace 根目录，但 bindgen/libclang 报相对头文件（例如 `ctypes.h`）不存在；同一镜像中的普通 Rust 文件读取可能正常。
- **根因**: VFS 的 `absolute_path()` 是全局挂载树路径，`/proc/<pid>/fd/*` 若直接返回它，会把 chroot 外的挂载前缀（例如 `/sdcard/...`）泄漏给 `realpath`/fd 重开路径。`FsStatus.working_path` 也可能在 `chdir`/`fchdir` 时缓存该全局前缀。
- **修复**: 统一按进程 `root_inode` 将 inode 路径转换为 root-relative；`getcwd`、`/proc/<pid>/fd/*` 和 cwd 缓存必须共用该转换。不要只修 `sys_getcwd`，也不要硬编码某个挂载点名称。
- **验证**: 在临时镜像中先确认 `current_dir`、`canonicalize("ctypes.h")` 和 `readlink("/proc/self/fd/<fd>")` 的路径；随后以 8 核/8G profile=buildstorm 重跑，区分路径修复与镜像 ext4 journal replay 的启动延迟。
- **相关文件**: `os/src/fs/mod.rs`, `os/src/fs/procfs/pid/fd.rs`, `os/src/syscall/fs/sys_getcwd.rs`, `os/src/syscall/fs/sys_chdir.rs`, `os/src/syscall/fs/sys_fchdir.rs`

### init stage-1 后无输出先查首个外部等待点

- **现象**: QEMU 日志完成 net/block/mount 后停在 `[init] MangoCore stage-1 boot (initramfs mode)`，没有后续 bind mount 或 initproc 输出。
- **根因**: stage-1 init 打印该行后立即执行 NTP 同步；如果 `ntpd`、DNS、guest 网络或 timeout wake 路径卡住，父进程 `waitpid` 会让日志看起来像挂载阶段之后卡死。
- **修复**: init 阶段的外部依赖必须 bounded best-effort；NTP 子进程用 `waitpid_wnohang` 轮询加硬超时，超时后 `SIGKILL` 并 fallback。调度器中 legacy timeout sweep 的移除必须先通过 early boot、网络等待、timerfd/nanosleep/futex timeout 的有效验证。
- **教训**: 看到“最后一行”不要直接把责任归给上一行的模块。先沿源码确认下一条将执行的用户态/内核路径，再对比旧成功日志中 stage-1 后的第一条输出。
- **相关文件**: `user/src/bin/init.rs`, `os/src/task/processor.rs`, `os/src/task/manager.rs`

### 保留兜底语义，用 next-deadline gate 降低轮询税

- **现象**: scheduler loop 中的 legacy timeout/timer sweep 能保证正确性，但污染态下 pending flag 长期为 true，导致每轮调度都锁 queue/heap、读时间和扫描，`sched_stage_wake_expired` 成为主要 cycles 来源。
- **根因**: pending 只表示“队列非空”，不表示“当前已有 deadline 到期”。把非空队列当作每轮都需要处理，会把少量未来 timer 放大成每个 scheduler loop 的固定税。
- **修复**: 不直接删除 sweep；为 timer queue 和 timeout waitqueue 维护 cached earliest deadline。热路径先读 pending + next deadline，未到期直接返回；只有到期或 next deadline 不可信时才加锁处理。这个模式类似 Linux timer/hrtimer 使用 cached next expiry 决定下一次处理/重编程。
- **教训**: 对早期启动、NTP、nanosleep、futex timeout、timerfd 这类等待路径，正确性兜底往往比性能优化更早暴露风险。优化顺序应是“让兜底便宜”，不是先删除兜底。
- **相关文件**: `os/src/task/manager.rs`, `os/src/task/processor.rs`

## lwext4 VFS 适配器常见陷阱

### spin::Mutex 不可重入 — 持有外层锁时调用内层加锁函数会死锁

- **现象**: `metadata()` 调用 `probe_type()`，两者都尝试获取 `self.fs.lw.lock()`（`spin::Mutex`）→ 死锁。同理 `list_dirents()` 调用 `get_inode_id()`。
- **根因**: `spin::Mutex` 不是可重入锁（不同于 Linux 内核的 `mutex_lock`），同一上下文不能重复加锁。
- **修复**: 将内层函数的逻辑内联到外层锁作用域内，或在进入外层锁前释放锁并通过其他方式获取信息（如 `hash_path()` 伪 inode ID）。
- **教训**: 审计所有方法中"持有锁 → 调用另一方法"的链式调用，特别是在同一个 struct 的方法之间。`probe_type()`、`get_inode_id()` 这类帮助函数内部都持有 `fs.lw.lock()`。
- **相关文件**: `os/src/fs/ext4_lwext4/layout.rs`, `os/src/fs/ext4_lwext4/ext4fs.rs`

### 文件句柄泄漏 — `?` 提前返回时 file_close 未调用

- **现象**: PageCache 后端的 `read_page`/`write_page`/`read_pages`/`write_pages` 在 `file_seek`/`file_read`/`file_write` 失败时，`?` 提前返回但 `file_open` 已打开的句柄未被关闭。
- **根因**: lwext4 的 `Ext4File` 使用 `file_open`/`file_close` 手动管理（无 RAII），`?` 运算符绕过了底部的 `file_close().ok()`。
- **修复**: 用闭包包裹所有 I/O 操作，闭包返回 `Result`，然后在外层调用 `file_close().ok()` 并返回闭包结果：
  ```rust
  f.file_open(path, flags)?;
  let result = (|| -> Result<usize, SyscallErr> {
      // ... I/O operations that may fail with ?
  })();
  f.file_close().ok();
  result
  ```
- **教训**: 在 C 风格 API（手动 open/close）上使用 Rust 的 `?` 运算符时，必须确保 close 在所有路径上执行。闭包 + 外层 close 是一种轻量级 RAII 模拟。
- **相关文件**: `os/src/fs/ext4_lwext4/page_cache.rs`

### file_seek EOF clamp 破坏 POSIX 语义

- **现象**: `file_seek()` 在 `offset > file_size` 时将 offset clamp 到文件大小。这导致 `pwrite(fd, data, 4096, offset=8192)` 实际写入 offset=4096。
- **根因**: 看似防御性的 EOF 检查，但 POSIX 明确允许 seek 超出 EOF（创建稀疏文件/空洞）。
- **修复**: 移除 clamp，直接将原始 offset 传递给 `ext4_fseek`。
- **教训**: 不要对 POSIX 行为做"防御性"修正，尤其当底层 C 库（lwext4 ext4_fseek）已经实现了 POSIX 语义时。mmap 脏页回写、pwrite 等场景依赖 seek-beyond-EOF。
- **相关文件**: `dependency/lwext4_rust/src/file.rs`

## 纯逻辑 Bug

### TimeSpec::AddAssign 不归一化导致时间计算错误

- **现象**: 链式 `+=` 操作后，`TimeSpec.tv_nsec >= NSEC_PER_SEC (1_000_000_000)`，导致 `to_ns()` 溢出和比较运算符产生错误结果。
- **根因**: `AddAssign` 仅做分量加法 `self.tv_sec += rhs.tv_sec; self.tv_nsec += rhs.tv_nsec;`，未做进位处理。而 `Add` trait 实现中正确进行了归一化，两个 trait 实现不一致。
- **修复**: `AddAssign` 末尾添加 `self.tv_sec += self.tv_nsec / NSEC_PER_SEC; self.tv_nsec %= NSEC_PER_SEC;`
- **教训**:
  - 实现 `AddAssign` 时必须保证与 `Add` 等价：`a += b` 应等于 `a = a + b`。
  - 需要单元测试覆盖链式 `+=` 场景（至少 3 次累加带进位）。
  - 任何含有多个分量且分量之间存在进位关系的类型（钟表 / 日历 / 坐标加法），`AddAssign` 必须做归一化。
- **相关文件**: `os/src/timer.rs:138`, `libs/mango-kernel-core/src/time.rs`

### `1u8 << N` 在 N == 8 时 debug panic

- **现象**: 当 `VALID_SEG_COUNT == 8` 且 `(seg_end - seg_start) == 8` 时，`(1u8 << 8) - 1` 在 debug 模式下 panic（shift-width-equal-to-bit-width）。
- **根因**: Rust 规定 `1u8 << 8` 是未定义行为，debug 模式会 panic。当所有 8 个 512B segment 都要标记为 valid 时，计算 `(1 << 8) - 1` 即触发此 panic。
- **修复**: 安全写法 `if count == 8 { u8::MAX } else { (1u8 << count) - 1 }` 或 `u8::MAX >> (8 - count)`。
- **教训**:
  - 任何 `1uN << M` 表达式都必须保证 `M < N`（移位宽度严格小于位数）。
  - 边界条件 `M == N` 发生在 bitmap full-set 场景（全掩码），需要用 `MAX` 常量代替。
  - 此模式在 bitmask 计算中极常见，编写时主动加断言或安全分支。
- **相关文件**: `os/src/fs/page_cache.rs:95`, `libs/mango-kernel-core/src/page_cache.rs`

## UserBufferWriter::new 提前 fault-in 导致 stateful 操作无限循环

- **现象**: `getdents64` 在用户缓冲区访问越界时陷入无限循环，日志中持续出现相同偏移量的 EFAULT。
- **根因**: `sys_getdents64` 在调用 `get_dirent64()` 前先用 `UserBufferWriter::new(token, dirp, count)` fault-in 全部 [dirp, dirp+count) 页面。若某个页面不可访问 → 返回 EFAULT，但 `get_dirent64` 从未被调用 → 文件 offset 未前移 → 用户态重试相同 offset → 相同 EFAULT → 死循环。
- **修复**:
  1. 用 `check_user_range(ptr, len)`（纯地址范围检查，不 fault 页面）替代 `UserBufferWriter::new` 做前置验证。
  2. 在调用 stateful 操作前保存状态（`old_offset = file.offset()`），在任意后续失败路径回滚（`file.set_offset(old_offset)`）。
  3. 用实际写入字节数（`written`）而非缓冲区大小（`count`）创建 Writer，避免 fault-in 未使用的页面。
- **教训**:
  - `UserBufferWriter::new` 会 fault-in 整个 [ptr, ptr+len) 区间，不可用于前置验证。
  - 所有 stateful 操作（offset 前移、inode 修改等）必须在故障路径中回滚，否则调用者重试时状态不一致。
  - `check_user_range` 是纯地址范围检查（无页表访问），安全用于前置验证。
  - 此模式适用于所有类似场景：`readdir`、`seek` + `read`、批量 `write` 等。
- **相关文件**: `os/src/syscall/fs.rs`, `os/src/fs/vfs/file.rs`

## `*at` syscall 对绝对路径无条件解析 dirfd → EBADF

- **现象**: LTP `openat02` 等测试用例失败：对绝对路径（如 `/etc/passwd`）传入无效 dirfd（如 -1），预期成功但实际返回 `EBADF`。
- **根因**: 所有 `*at` syscall（`openat`, `unlinkat`, `mkdirat`, `mknodat`, `renameat2`, `symlinkat`, `readlinkat`, `fstatat`, `statx`）在检查路径是否绝对之前就调用 `resolve_start_inode(dirfd)`，无效 dirfd 在此处立即返回 `EBADF`，后续代码根本无法执行到路径判断。
- **修复**: 在每个 `resolve_start_inode(dirfd)` 调用前添加 `if path.starts_with('/') { crate::fs::current_root_inode() } else { resolve_start_inode(dirfd) }`。`check_parent_search_access` 内部已有绝对路径处理（common.rs:2082-2086），但此前从未被执行到。
- **教训**: 实现 `*at` 系列 syscall 时，**dirfd 解析必须是条件性的**——只有相对路径才需要 dirfd。绝对路径场景 dirfd 被 Linux 语义忽略。新增 `*at` syscall 时应在第一步就加这个检查，避免后期批量修复。
- **相关文件**: `os/src/syscall/fs/common.rs`, `os/src/syscall/fs/sys_*.rs`

## 文件系统多路径操作（renameat2）中的验证镜像缺失

## 目录项发布的存在性检查必须与插入共享命名空间锁

- **现象**：VFS 适配层在调用文件系统后端前用 `lookup()` 返回 `EEXIST`，但两个并发创建/硬链接任务都可能在检查时看到名称不存在，随后分别插入相同名称的目录项。
- **根因**：检查与 `dir_add_entry()` 发布点不在同一个后端 `namespace_lock` 临界区；桥接层的检查只能优化常见失败路径，不能建立后端命名空间不变式。
- **修复**：在所有后端命名空间操作持有 `namespace_lock` 后、调用 `dir_add_entry()`/`link_inode()` 前使用 `dir_find_entry()` 检查同名项并返回 `EEXIST`。新 inode 路径须将检查放在既有自动释放包装内，确保失败仍释放未发布 inode。
- **相关文件**：`dependency/another_ext4/src/ext4/low_level.rs`

### 路径搜索权限检查遗漏（renameat2）

- **现象**: `renameat2` 对 oldpath 做了路径遍历搜索权限检查，但对 newpath 同样路径却没有做，导致非特权进程能通过 newpath 遍历非本用户目录。
- **根因**: `renameat2` 需要操作两条路径（oldpath 和 newpath），但代码只对 oldpath 做了 `check_parent_search_access`，newpath 路径完全未验证。双向路径操作必须在两条路径上都执行权限验证。
- **修复**: 在 `vfs_lookup_parent_for_start` 调用前，对 old_start 和 new_start 分别调用 `check_parent_search_access`。
- **教训**: 任何涉及**两条路径**的系统调用（renameat2、linkat、symlinkat 等），必须在两条路径的**遍历之前**分别做搜索权限检查。不要假设一条路径通过后另一条就自动安全。
- **相关文件**: `os/src/syscall/fs/sys_renameat2.rs`

### sticky bit 检查遗漏 target parent

- **现象**: 当 target parent 目录设置 sticky bit 时，非文件所有者仍可通过 renameat2 将文件移入/移出该目录。
- **根因**: renameat2 仅对 old parent（源父目录）做了 sticky bit 检查，完全遗漏了 new parent（目标父目录）的检查。Linux 语义要求 renameat2 对**两个父目录**都做 sticky bit 验证。
- **修复**: 在 old parent sticky bit 检查后，对 new parent 执行相同逻辑的 sticky bit 检查。
- **教训**: 多路径操作的权限检查必须在每条路径上**镜像**。实现时先列出需要检查的完整清单（两条路径 × 三种检查：search、write、sticky），逐项实现，避免遗漏。
- **相关文件**: `os/src/syscall/fs/sys_renameat2.rs`

### 不变式检查被存在性检查条件门控（ext4 rename）

- **现象**: ext4 的 `rename()` 中，子树检测（防止重命名目录为其子目录）仅当 `target_exists` 为 true 时才执行。若目标不存在，循环目录可以成功创建。
- **根因**: 子树检测是一种**全局不变式**（目录不能成为自己的后代），不应与目标是否存在相关。将不变式检查放在 `if target_exists { }` 块内意味着当目标不存在时该检查完全跳过。
- **修复**: 将子树检测代码从 `if target_exists { }` 块内移出到块外，使其**无条件执行**。
- **教训**: 检视文件系统 `rename()` 时，区分三类检查：(1) 只能在目标存在时做的（类型冲突、ENOTEMPTY）；(2) 与目标无关的全局不变式（子树检测、循环检测）；(3) 权限检查。只有第 (1) 类可以放在 target_exists 块内。第 (2)(3) 类必须无条件执行，**绝不**被存在性检查条件门控。
- **相关文件**: `os/src/fs/ext4/ext4fs.rs`

## Errno 对齐

### fd-based vs path-based xattr 使用不同的 errno

- **现象**: fgetxattr 对 pipe/socket fd 返回 EOPNOTSUPP，但 LTP open13 期望 EBADF。
- **根因**: Linux 语义不同：(1) fd-based xattr（fgetxattr/fsetxattr/fremovexattr）对错误 fd 类型（pipe、socket）返回 **EBADF**；(2) path-based xattr（getxattr/lgetxattr/setxattr/lsetxattr）对非 file/dir 目标返回 **EOPNOTSUPP**。项目代码在 fd_to_inode() 中使用了 EOPNOTSUPP，与 fd-based 语义不匹配。
- **修复**: `fd_to_inode()` 中将 `EOPNOTSUPP` 改为 `EBADF`，仅改 fd-based 路径。
- **教训**: 修改 errno 时，查 Linux 源码确认 syscall 的具体语义，不要仅凭直觉推断。fd-based 和 path-based 变体可能使用不同的 errno。
- **相关文件**: `os/src/syscall/fs/common.rs`

### fd-based xattr syscall 的 errno 优先级：fd 验证必须在参数验证之前

- **现象**: fgetxattr/fsetxattr 对 O_PATH fd 或 pipe/socket fd 返回 EOPNOTSUPP，但 Linux 期望 EBADF。根因是 `validate_xattr_name()`（检查非 user.* 前缀 √ 返回 EOPNOTSUPP）比 `fd_to_inode()` 先调用，EOPNOTSUPP 抢在 EBADF 之前返回。
- **根因**: Linux syscall 的 errno 优先级规则：fd 有效性检查（EBADF）比参数语义检查（EOPNOTSUPP/EINVAL）优先级更高。当调用顺序为 `validate_xattr_name → fd_to_inode` 时，参数检查先于 fd 检查执行，导致错误的 errno 被返回。
- **修复**: 将 `fd_to_inode()` 移到 `user_cstring()`/`validate_xattr_name()` 之前，确保 fd 相关的错误先被返回。
- **教训**: 实现 fd-based syscall 时，始终将 fd 有效性检查排在最前面，再执行参数/缓冲区校验。这是 Linux 全局惯例，不仅限于 xattr 类 syscall。同样问题也存在于 `sys_fsetxattr.rs` 和 `sys_fremovexattr.rs`。
- **相关文件**: `os/src/syscall/fs/sys_fgetxattr.rs`

## I/O 转发 syscall 的数据保全

### 文件源显式 offset 在目标写入确认前被推进 → 数据丢失

- **现象**: splice(file→pipe) 中，若目标管道写入失败（EAGAIN, EPIPE），文件偏移量 `*off_in` 已被推进，下一次 splice 调用会跳过已读但未传输的数据，导致静默数据丢失。
- **根因**: 传输循环中 `*off_val += n` 在读取阶段执行（`inode.read_at()` 之后立即推进），但写入阶段（write to pipe）可能在推进 offset 之后失败。offset 反映的是"读取量"而非"实际传输量"。
- **修复**: 将 offset 推进推迟到写入成功后执行。读取阶段仅使用 offset 定位，不修改它；写入阶段成功后 `*off += wrote`（其中 `wrote ≤ n`），确保 offset 精确反映已确认写入目标的字节数。
- **教训**: 任何跨越两个独立 I/O 对象的 syscall（splice、sendfile、copy_file_range）都必须遵循"状态推进在输出确认之后"的原则。对于文件源的显式 offset 参数，推进发生在写入成功之后而非读取成功之后。管道源是破坏性读取（无可回滚机制），需通过容量探测或最小化读取窗口来限制损失。
- **相关文件**: `os/src/syscall/fs/sys_splice.rs`

## Trap 返回半恢复窗口：不要在写回状态寄存器时提前开中断

- **现象**: 用户程序在动态加载器第一条栈保存指令偶发 fault；用户 PC 完全正确，但
  `sp` 精确等于 trap-context VA。单核或另一次多核运行可能不复现，容易被误判为
  boot CPU 映射、ELF 或文件系统波动。
- **根因**: syscall 内核窗口允许本地中断后，新建 trap context 可能复制 live
  `sstatus.SIE=1`。返回汇编若先写 `sstatus`、再恢复 `sepc/GPR`，pending timer 会在
  半恢复的 S-mode 现场上立即嵌套进入；当 `sscratch` 与 `sp` 都指向 trap context 时，
  嵌套入口会把 trap-context VA 保存进用户 `sp` 槽。
- **定位方法**:
  1. 不凭高地址猜测固件/内核归属，先结合 PIE/interpreter 映射基址反汇编真实 ELF。
  2. 由 fault 指令的栈偏移反推函数入口 `sp`，再与 trap-context/用户栈槽位公式逐值比较。
  3. 按“CSR 写回 → 硬件中断判定 → 嵌套入口寄存器交换 → 嵌套返回”逐指令模拟，解释
     为什么 PC 可以正确而 GPR 已被污染。
- **修复**: 所有用户返回统一设置 `SPP=User、SIE=0、SPIE=1`；恢复 GPR 期间保持
  S-mode 中断关闭，只由最终 `sret` 执行 `SIE <- SPIE`。状态规范化必须放在统一
  `trap_return()`，不能只覆盖初始任务或 exec。
- **附带审计**: 从 Rust 跳入 `asm!(options(noreturn))` 前，所有局部 owned `Arc` 必须
  显式 `drop`；noreturn 路径不会展开调用者 Rust 栈帧，不能依赖作用域自动析构。
- **相关文件**: `os/src/hal/arch/riscv/trap/context.rs`,
  `os/src/hal/arch/riscv/trap/mod.rs`, `os/src/hal/arch/riscv/trap/trap.S`

## Judge 物理行失分：先还原用户 syscall 分片与内核安全点

- **现象**: 功能测试打印了成功标记且完整退出，但父子/多线程的同一条逻辑输出被拼成
  `prefix: 64prefix: 0` 或数字落到下一物理行，严格按行 judge 因而失分；不同 libc、架构
  或重复运行可能换一组触发。
- **定位方法**:
  1. 先保留 START/END 之间的原始字节块，不用 judge 解析结果替代串口事实。
  2. 从实际测试镜像提取带符号二进制，用 `nm/objdump` 核对一个 `printf/puts` 最终发起
     几次 `write/writev`；不要假定一个 libc 逻辑调用就是一个 syscall。
  3. 沿 trap fast path、syscall 中断窗口和 scheduler 调用链确认真实 context-switch 点。
     timer 能打断 syscall 不等于 timer handler 会在 syscall 中途调度。
  4. 若分片只可能在独立 syscall 返回之间交错，分别验证功能 token、退出码、panic/timeout
     和测试 END，再判断是功能回归还是物理行解析问题。
- **避免的 workaround**: 不为迎合 judge 跨 syscall 持 console/TTY 锁，也不在内核静默
  缓存到换行；前者会跨调度点持锁，后者会延迟 shell prompt、进度条和无换行诊断。终端
  不保证把多个独立 write 合并为原子逻辑行。
- **门禁模式**: raw judge 分数必须原样保留；只有针对已反汇编证明的单个测试定义严格、
  可审计的块级语义解析，才可另算 semantic score。归一化规则必须同时验证完整 token、
  成功标记、END、无错误，并对旧基线和候选版本一致应用，不能通过重复运行挑高分过门禁。

## 用户 MM shootdown：登记、快照与等待必须分成两个同步层次

- **典型漏失竞态**: CPU A 返回用户态时若先读 generation、后加入 cached/active mask，
  CPU B 可在两步之间推进 generation 并快照旧 mask。A 不在 IPI 目标内，又认为自己已经
  观察到旧 generation，最终带着 stale TLB 返回用户态。
- **典型死锁**: PTE 修改方在进程 VM/PTE 锁内等待远端 ack；目标 CPU 已关闭本地 IRQ并在
  page fault 中等待同一锁。发送方等目标 handler，目标等发送方放锁，开放发送方 IRQ也
  无法打破这条环。
- **测试等待者陷阱**: 即使发送方已经释放全部 VM 锁，曾缓存该 MM 的 CPU 若在 kernel
  test/诊断代码中关中断自旋，也无法处理 shootdown IPI。测试等待区应使用既有受控中断
  窗口或周期进入 scheduler 安全点，不能靠延长 timeout、缩小 target mask 或跳过回收过关。
- **日志判读**: timeout 中的 `cpu_id/missing` 通常是未 ack 的目标，不是发起者。若实现先从
  targets 排除 current，再等待 remote，可由 missing CPU 反推出发起 CPU；先核对这条集合
  运算，再判断失败发生在迁移前还是迁移后，避免把退出期 shootdown 误报成构造期失败。
- **正确分层**:
  1. 激活侧与修改侧用同一个 VM 锁串行化“加入 mask”与“推进 generation + 快照目标”；
  2. 激活侧固定先 join、再读 generation，落后时本地 flush 并重查；
  3. 修改侧在锁内只改 PTE、推进代际、快照目标并转移 deferred frame 所有权；
  4. 释放所有普通锁后才发送 IPI、等待 ack，最后释放 frame。
- **Atomic 误区**: cached mask 与 generation 是不同原子。分别使用 Acquire/Release，甚至只把
  generation 的 RMW 升级为 AcqRel，都不自动替代共同锁的 join-vs-snapshot 顺序。若改成
  lockless，必须逐一证明“修改先发生”和“激活先发生”两种次序，并实现相应重试/fence。
- **测试边界**: 只看 request/ack 增长只能证明 mailbox/IPI 闭环；完整证据还需真实 PTE
  降权或撤映射、victim 无偶然 trap 全刷窗口、generation race，以及 ack 前 frame 不复用。
- **相关文件**: `os/src/mm/address_space.rs`, `os/src/mm/tlb.rs`,
  `os/src/mm/page_table.rs`, `os/src/smp.rs`

## 幂等 IPI reason 携带不可合并 payload：使用每发起 CPU 固定槽

- **风险**: per-CPU mailbox 通常用 `fetch_or(reason)` 合并 IPI reason。它只能表达“至少有
  一项工作”，不能携带事件次数；若所有发起者共用一个 ASID/VA 或 ASID/range payload，
  后发布者会覆盖前一请求，目标 CPU 即使处理了 reason 也可能失效错误地址。
- **适用模式**: 当每个 CPU 同时最多同步等待一个请求时，为每个发起 CPU分配一个固定
  原子槽。发起者先 CAS claim，写入 payload/ack 初值，最后用 Release store 发布 targets；
  handler 收到一个合并 reason 后扫描全部槽，以 Acquire 读取 targets，完成硬件操作后再
  Release 写 ack。这样 reason 可以合并，payload 不会合并或覆盖。
- **有界 payload**: hard-IRQ 中处理连续区间时，只发布固定宽度的
  `ASID + start + page_count`，并在发布前校验非空和最大页数。跨度超过上限直接回退全刷，
  不把动态 VPN 列表、堆分配或无界循环带入 handler。稀疏修改可合并为包围区间，多刷少量页
  通常比再引入多段 slot 状态更容易证明；阈值必须作为 IRQ 工作量边界记录并覆盖临界测试。
- **复用边界**: 只有全部 live target ack 后才能清 targets 并释放槽。timeout 后若系统将
  fail-stop，不要提前复用槽；保留旧 payload 可防止迟到 doorbell 把旧事件错配到新请求。
  若未来允许从 timeout 恢复，则必须引入 sequence/epoch，而不能只清一个 claimed bit。
- **测试**: 让全部 CPU 同时发布不同 payload，并断言每项都完成且没有退回全量 fallback；
  分别覆盖“小于等于上限的区间”和“上限加一页的全刷”，同时保留真实 PTE 撤映射与 ack 前
  frame 不复用测试。仅观察 IPI reason 次数不能证明 payload 隔离或硬件 stale entry 已消失。
- **相关文件**: `os/src/smp.rs`, `os/src/mm/mmu_gather.rs`, `os/src/mm/tlb.rs`

## ASID rollover：flush ack 之后禁止重新安装旧地址空间快照

- **隐蔽竞态**: rollover leader 向所有 CPU 发出全量 TLB 失效并收到 ack 后，才能换代和
  复用 ASID。但如果目标 CPU 在 ack 前已经读取旧页表根/ASID，ack 后又从 trap-return
  安装这份旧快照，那么“TLB 已清空”仍不足以保证旧 ASID 不再被使用。
- **闭环条件**: ack 不只要证明硬件失效完成，还要划定地址空间切换的交接边界。用户返回
  必须从取得 MM context 到最终安装寄存器期间保持本地中断关闭；这样目标 CPU 要么先返回
  旧用户态、随后被 pending IPI 拉回内核并 ack，要么先处理 IPI，之后重新取得当前 epoch
  的 context，不能形成“先 ack、后安装旧 context”。
- **审查方法**: 按 `读取 context -> IPI handler -> ack -> 写页表根/ASID -> 用户访问`
  枚举时序，而不是只检查 allocator 的锁和原子序。反汇编确认返回汇编没有提前开中断，
  并用断言固定 Rust 到汇编交接点的 IRQ-off 前置条件。
- **相关文件**: `os/src/mm/address_space.rs`, `os/src/hal/arch/*/trap/`,
  `os/src/hal/arch/*/{sv39,tlb}.rs`

## 跨 CPU 线程组终止：停止请求、owner 自清理与最后 ack

- **危险做法**: 发起 `exit_group` 的 CPU 从远端 runqueue/current 槽移除 sibling 后，直接替它
  释放用户映射、内核栈或最后一个 TCB 引用。远端 CPU 可能仍在这些资源上执行，形成确定性的
  use-after-free；“已经发出 reschedule IPI”不等于目标已经离开资源。
- **永久退出协议**: 线程组成员表和“是否允许发布新线程”的 gate 必须由同一把锁保护。
  第一个退出者在该锁内发布最终退出码并快照成员；锁外再给 sibling 排队不可忽略的终止信号，
  唤醒 Blocked 线程并向 Running/Queued owner 发送 reschedule。目标线程只在自己的安全点进入
  本地退出路径，发起者不远程执行资源清理。
- **阻塞交界竞态**: 仅按一次 `Running/Blocked` 快照发送 wake 会漏掉
  `看到 Running -> 目标登记 Blocking -> 目标真正睡眠`。阻塞侧必须在完成 wait registry 和
  `Running -> Blocking` 登记、释放 manager 锁后，重新读取永久退出状态；命中时把自己重新
  唤醒。这样停止请求无论在线性化点前后发生，都有一侧负责推进。
- **ack 的含义**: live-thread 计数不能在线程刚被标记退出时递减。它必须位于
  `clear_child_tid/futex -> 用户资源与 PTE 清理 -> TLB shootdown` 之后，以 release 语义发布；
  观察到最后一个 ack 的线程才有权收尾进程共享资源。否则“计数为零”只证明收到请求，不证明
  远端已经停止使用 MM/PCB。
- **临时 exec 协议**: 多线程 `exec` 是“暂时停住其他线程、owner 继续运行”，必须有独立
  `ExecSession + Completion`。它可以复用 owner 安全点和 live token，但不能把永久退出码
  当作临时 gate；安装新映像后还要重新开放线程发布。永久 group exit 到来时应覆盖临时 exec。
- **不要把快照或 Arc 计数当成 ack**: 开始时 sibling 快照为空，不代表另一个线程已经完成
  live-token 递减；同一 PCB 的线程也不会各自长期持有 VM `Arc`，所以
  `Arc::strong_count()` 不能证明独占旧地址空间。只有位于用户映射/TLB 清理之后的权威
  live count 降为 1，才能唤醒 exec owner 替换 MM。
- **等待点必须响应生命周期停止**: 普通“不可中断”等待可忽略用户信号，但不能永远阻塞
  group exit/exec。WaitQueue 应在等待协议内识别生命周期请求，先摘除 waiter，再返回调用层
  释放 syscall 栈上的 `Arc` 并进入安全点。vfork child 已 publish 后，父线程被中止不能走
  unpublished cleanup，应返回显式 `StopCaller`。
- **最终退出码要在 live-zero 后复读**: 普通 exit 在线程清理前读取的 group-exit 码仍可能
  与另一 sibling 随后发布永久退出竞争。最后一个 live token 消费者应在计数归零后再次
  Acquire 读取；此时已没有 live 成员能首次发布，才可决定 wait 可见的进程退出码。
- **边界**: 任何路径都不得跨 IPI/TLB ack、context switch、Completion 或其他等待点持普通锁。
- **相关文件**: `os/src/task/process.rs`, `os/src/task/manager.rs`,
  `os/src/task/task.rs`, `os/src/task/mod.rs`

## 精准 TLB shootdown：必须拒绝 trap-return 全刷造成的假阳性

- **计数器证据的边界**: request/ack 只能证明 mailbox、doorbell 和等待闭环完成，不能证明
  handler 真的失效了目标 ASID/VPN。验证 stale translation 时，victim 必须先用真实用户
  load/store 填充旧翻译，再让修改方经过生产 PTE 主链改变 PPN、权限或有效位。
- **常见假阳性**: IPI handler 若只做精准失效和 ack、却不推进该 MM 的 observed generation，
  目标从中断返回用户态时会进入 `activate_cpu()` generation catch-up，再做一次本地全刷。
  即使精准硬件指令完全无效，用户也会看到新映射，测试因此错误 PASS，生产路径还多付一次
  全刷成本。
- **正确发布顺序**: 对同步 fixed slot，发起者保证 MM 在 ack 前存活；handler 应执行
  `precise invalidate -> mark observed generation -> ack`。payload 先写，最后以 Release
  发布 targets；handler Acquire targets，observed 的发布必须先于 ack。timeout 若采用
  fail-stop，旧槽不得复用；若要恢复，必须加入 sequence/epoch 和可证明的所有权回收。
- **排除其它补刷来源**: 在 victim 窗口静默本地 timer，只保留 shootdown IPI；把 timer
  restore helper 排在用户 probe 后，并让 helper 在结果出现前被调度时主动判失败。同时检查
  full-flush request 未增长和 observed 已由 handler 推进，不能只靠低概率 KREPEAT。
- **物理页复用陷阱**: 验证 PPN 替换时额外持有旧 frame，并在 shootdown 返回后才写新 frame
  canary。这样坏 handler 会持续读取确定的 OLD，而不是因为旧 frame 被分配器复用而偶然读到
  NEW。与用户硬件访存并行的内核直映访问使用瞬时 raw/volatile pointer，避免构造重叠的
  Rust `&mut` 引用。
- **相关文件**: `os/src/smp.rs`, `os/src/mm/tlb.rs`, `os/src/kernel_tests/smp.rs`

## 收口单核 unsafe 假设：区分运行期串行化与类型授权

- **不要按关键字机械改写**: `static mut` 可能表示真正的并发共享状态，也可能只是启动期唯一
  移交的后备区或按 CPU 切分的静态栈。先写出“谁在何时取得所有权、运行期还有谁访问”，
  再决定改成 atomic、锁内对象、Per-CPU 槽，还是保留并补全证明。
- **布尔 side table 的并发模式**: 多 CPU 只需独立 set/clear/read bit、且对象生命周期另有锁
  保护时，可用 `AtomicUsize` bitset。set/clear 必须用 `fetch_or/fetch_and`，不能 load-modify-store，
  否则同 word 的相邻 bit 会丢更新；Relaxed 只承担位操作原子性，不应被描述成发布映射内容。
- **Send 与 Sync 不是“有锁”的同义词**: `Mutex<T>` 通常要求 `T: Send`，并不要求内部每一层
  raw-pointer 容器都实现 `Send + Sync`。把最窄的顶层 owner 声明为 Send，并证明移动 owner
  不移动其指向对象、所有解引用仍在同一锁下；没有跨 CPU 共享 `&T` 的需求就不要声明 Sync。
- **审查输出**: 每个保留的 unsafe impl 都应说明真实 owner、保护锁、可移动性和禁止的并发访问；
  每个删除项则说明为何不再需要，而不是用“现在多核了”替代类型推理。
- **相关文件**: `os/src/mm/slab.rs`, `os/src/mm/heap_allocator.rs`,
  `os/src/hal/arch/loongarch64/laflex.rs`

## 多核 console：正常 irq-save 叶子锁与 panic 单向逃生路径

- **本地 irq-off 不等于跨 CPU 串行化**: 关中断只能防止同一 CPU 被 IRQ 打断后再次进入
  console；多个 CPU 仍会并发写设备。正常输出需要一把全局叶子锁，固定顺序为
  `local IRQ-off -> console lock -> 可选的底层 UART lock`，释放顺序相反。
- **panic 不能等待普通 owner**: 崩溃可能发生在 console/UART 锁持有区，或者 panic CPU
  已经停止持锁的其它 CPU。用单向原子状态先发布 panic，再让锁等待循环主动放弃；raw writer
  必须绕过所有 Rust console/UART 锁。它可以等待硬件 ready，不能把“无锁”误写成“非阻塞”。
- **格式化临界区**: 颜色前缀、正文和 reset 属于同一日志记录时，应在一次 `print` 中输出，
  避免三次独立加锁仍被别核插入。格式参数尽量在进入叶子锁前求值，writer 内不得反向取得
  task、MM、VFS 或其它业务锁。
- **不要用 TTY workaround 修 judge 物理行**: 用户程序若用多个 write syscall 拼一行，
  不同进程在 syscall 安全点合法交错不属于单次 console 临界区破坏。先按既有 raw/semantic
  规则复核完整块，不能跨 syscall 持锁或缓存到换行来伪造终端原子性。
- **相关文件**: `os/src/console.rs`, `os/src/lang_items.rs`,
  `os/src/hal/arch/{riscv,loongarch64}/sbi.rs`, `docs/01_architecture/lock-order.md`

## Panic 诊断中的“只读统计”也必须做传递锁审计

- **根因**: 诊断函数表面只读取 heap/free-frame 数量，但下层 `heap_stats()` 和
  `unallocated_frames()` 分别取得 spin mutex 与 rwlock。panic 若发生在同一锁内，或其它 CPU
  持锁后停止，诊断会在打印根因前永久自旋。
- **修复**: 保留普通调用者的阻塞统计接口，另提供 `try_*` 入口。锁忙时输出原子 charge 或
  `<locked>`；逐 CPU 快照只读现有原子 hint，确需读取 active MM 时仅 `try_lock()` 后复制
  稳定 ID，失败明确标记 unknown。
- **教训**: panic-safe 必须审计完整调用链，不能把“只读”和“无锁”等同。输出字段也不能反向
  驱动生产正确性；跨字段快照应标为 best-effort，不为补齐表格临时增加热路径原子状态。
- **相关文件**: `os/src/panic_diag.rs`, `os/src/mm/{heap_allocator,frame_allocator}.rs`,
  `os/src/{smp,task/processor}.rs`

## SMP uaccess：frame 存活不能替代用户映射同步

- **危险模式**: 先在 VM 锁内把用户 VA 翻译成 PA 或 `&'static mut [u8]`，释放锁后再复制。
  另一 CPU 可在间隙执行 fork/CoW、`mprotect` 或 `munmap`；即使持有 frame `Arc` 防止物理页
  释放，原 VA 的 PPN、权限和 CoW 归属也可能已经改变。
- **修复边界**: fixed-size copy 应按页取得 VM 锁，在同一临界区执行 fault、PTE 权限后验检查、
  取得 direct-map raw pointer 和实际 copy。raw pointer 不得逃逸 closure；锁外再执行
  `MmuGather` 产生的 TLB flush/ack，避免跨等待点持 VM 锁。
- **Rust 引用不是同步原语**: 不要把内核直映地址转成可逃逸的 `&'static mut T`。延长 lifetime
  既不能固定映射，也可能违反 `&mut` 独占 alias 规则；需要瞬时访问时使用带完整 Safety 证明的
  raw pointer，并由真正的 VM/对象锁提供排他关系。
- **跨页语义**: 为避免长时间持有 VM 锁，大 copy 可以逐页串行化；后续页失败时此前 chunk
  可能已经完成。调用方和文档必须显式接受部分完成，不能把 `Err(EFAULT)` 解释为全量回滚。
- **预 fault 不是 pin**: `fault_in_user_range()` 可以在分配 fd、生成随机数等外部副作用前
  提前报错，但另一 CPU 可在预检查后立即改映射。真正读写仍必须走锁内 copy；
  不得把预检查结果转换为裸 slice 或引用。
- **解析器只消费内核快照**: pathname/sockaddr/变长结构的 parser 不应接收用户物理页
  slice。先在 VM 锁内复制到有上限的内核 buffer，释放锁后再扫描、分配和解析；
  这同时避免把 heap allocator 带入 VM 临界区。
- **不跨等待点保存用户视图**: 阻塞 I/O 先把数据收入内核所有 buffer，或在唤醒后才
  执行用户 copy。WaitQueue、socket poll 或磁盘 I/O 期间不保存 VA 翻译得到的 PA/slice。
- **buffer 描述应保存 VA，不保存翻译结果**: 连续 buffer 保存一个 `{start, len}`，scatter
  I/O 保存每个 iovec 的逻辑 VA 区间。构造阶段可以预 fault，但对象中不得保留 PA、frame、
  direct-map pointer 或 Rust slice；实际 copy 每页重新取得 VM 锁并解析当前 PTE。
- **partial iterator 与 exact wrapper 分层**: read/write/pipe/socket 等流式接口遵循“首字节
  失败返回 errno，已有进度返回完成前缀”；固定 ABI 结构再由 `read_exact`/`write_all` 把短
  copy 转为 `EFAULT`。PageCache 有效范围和文件 offset 必须使用实际完成字节数更新。
- **自旋锁内只能 nofault**: 若 ring/index 状态要求复制期间持 `spin::Mutex`，先在锁外
  fault-in，锁内只接受仍满足权限的现有 PTE。并发 remap 时立即失败或部分完成，不能在
  自旋锁内触发 CoW、分配、文件缺页或等待。nofault helper 应保持最小可见性，避免扩散。
- **软件 uaccess 要先 resolve**: `fault_in_user_va()` 不能假设每次调用都对应真实硬件 fault。
  PTE 已映射且权限满足时应直接返回；否则每次 copy-to-user 都可能误走 Cow/SharedWrite，
  重复修改 PTE 并制造无意义的 TLB shootdown。
- **锁序审计**: faultable uaccess 前先释放 fd table、task inner、file-private、socket 等普通
  业务锁。若旧调用链要求持锁复制，先快照/克隆稳定 owner，再释放锁进入 uaccess。
- **写侧用两阶段校验，不要提前碰用户指针**: 若 errno 顺序要求先判断对象身份、权限或固定
  长度，第一段在 registry 锁内只验证并快照；解锁后分配和 copy；第二段重锁确认对象仍是
  同一身份、权限和长度，再一次提交。对象若依赖“不复用 ID”排除 ABA，分配器耗尽时必须
  明确失败，不能用饱和计数反复返回同一 ID。名称创建还要在第二段锁内重新处理 `O_EXCL`
  和容量，使发布点对并发创建保持原子。
- **破坏性读取必须在 owner 锁内完成唯一领取**: 若读取会消费队列元素，不能先在锁内 clone、
  解锁 copy、再重锁按 serial 删除。两个 CPU 可同时 clone 同一元素并都返回成功。应在 owner
  锁内完成选择与 move/remove，把内核所有权唯一交给一个调用者，再锁外做 faultable uaccess；
  非破坏的 peek/`MSG_COPY` 才允许只复制快照。用户 copy 失败是否回滚必须服从 ABI，不能为
  “避免丢数据”擅自改变 Linux 的消费语义。
- **可等待对象的数值 ID 必须排除 ABA**: syscall 若会释放 registry 锁并按 ID 等待，删除后
  立即复用空洞会让旧 waiter 命中新对象。简单内核可以在本次启动期间永久记录所有发布 ID；
  requested ID 与自动 cursor 要分离，自动路径也必须跳过 requested 留下的稀疏历史。历史
  容量必须在对象插入前预留并登记，避免对象已发布而身份记录失败；删除路径只移除并唤醒，
  不得临时分配 tombstone。长期高 churn 再迁移 index+generation，但不能只扩大整数或依赖
  当前对象表查重。若私有等待 helper 的唯一入口已经在同一把锁下证明对象存在，并且 ID 从不
  复用，则无需另存发布历史：后续缺失只能来自删除，可直接返回 `EIDRM`。这个简化必须同时
  证明“唯一入口、初次错误仍为 `EINVAL`、单调 ID”三项，不能仅凭 helper 私有就推断。
- **相关文件**: `os/src/mm/uaccess.rs`, `os/src/mm/address_space.rs`,
  `os/src/syscall/process/ipc.rs`, `docs/01_architecture/lock-order.md`

## 可迁移等待项：不能用最初队列 membership 推断 wake

- **危险模式**: 通用等待队列只保存 task，正常 wake 通过“恢复后已不在 source 队列”识别。
  一旦业务支持 requeue，同一个现象也可能表示 waiter 只是搬到了 target。timeout/signal
  若仍清理 source，就会把迁移误报为成功，并在 target 留下活动成员。
- **稳定身份**: 每次注册需要独立 identity，并记录 current location 与 actual-wake 状态。
  同一任务可以有多个 waitv/poll 注册，清理必须匹配注册对象而不是只匹配 TCB；队列到任务
  使用 Weak 可以避免生命周期环，但不能替代 registration identity。
- **唯一线性化锁**: enqueue、wake、requeue 和 cancel 应由同一对象锁裁决。requeue 先更新
  current location 再发布到 target；wake 先发布实际结果再让任务 runnable；timeout/signal
  在同锁下按 current location 精确撤销。若拆成多个 bucket 锁，必须另外证明锁序和迁移协议。
- **恢复语义**: 注册完成后的唤醒判定只能读取 registration 的权威结果，不能重读最初业务
  word，也不能把 source 缺席当事件。多等待 API 返回哪个 index 要逐字核对上游实现；Linux
  当前 `futex_unqueue_multiple()` 保留最后一个已 wake 下标，而不是直觉上的第一个。
- **验证账本**: 普通 wait/wake LTP 不能证明 requeue→timeout、requeue→signal 或 waitv 多 key
  同时 wake。未构造精确交错时标为 NOT RUN，用锁内时序做静态证明但不冒充动态覆盖。
- **相关文件**: `os/src/task/threads.rs`, `os/src/task/manager.rs`,
  `docs/05_process/futex.md`, `docs/01_architecture/lock-order.md`

## 共享等待键：可复用物理编号不能充当长期对象身份

- **危险模式**: process-shared futex 或其它跨进程等待表把 raw PPN + offset 当成长期 key。
  原 backing 释放后，同一 PPN 可分配给无关页；旧 waiter 会错误命中新对象，形成 ABA
  false-positive。扩大 PPN 字段或在 wake 时再次翻译都不能证明对象连续性。
- **稳定身份与 pin**: key 应引用共享映射真正共用的分配对象身份；等待表只需为每个非空 key
  保留一份 owner pin，不必每个 waiter 都持有。空队列从 map 删除时释放 pin，requeue 则必须
  先固定并验证目标 backing，再更新 waiter current key，最后发布到目标队列。
- **逐映射类证明**: file-backed `MAP_SHARED`、anonymous shared fork 与 SysV SHM 必须分别确认
  两侧 VMA clone 的是同一个 backing `Arc`。相同文件页的多次 mmap 还要核对 PageCache 是否
  返回同一对象，不能只因为 PPN 相同就推断 identity 相同。
- **VM/table 两阶段**: 在同一 VM 锁内定位 VMA、clone resident backing 并校验 PTE 指向同一
  PPN；随后释放 VM 锁，再进入全局等待表。跨阶段传递拥有所有权的 Arc，而不是裸指针或
  PA；不得建立 `AddressSpace -> wait table` 的嵌套锁序。
- **所有回收入口都必须尊重 pin**: 普通 swap/zram 检查引用计数还不够；deep reclaim、OOM
  fallback 或显式 invalidate 若能绕过该检查，仍会把 VMA backing 换成新对象，形成旧 waiter
  不再命中的 false-negative。不能把“强制回收”理解成允许破坏共享对象身份；简单内核应统一
  走所有权安全的回收入口，直到存在像 Linux inode/page-index 那样独立于 resident frame 的键。
- **临时 pin 不应永久移出回收候选**: `SharedPage` 表示本轮不能回收，不表示永远不能回收。
  把候选放回队尾，并把单轮扫描限制为入口时的队列长度；否则直接丢弃会在 waiter 离开后仍
  无法回收，立即重试又会在同一页上死循环。这个模式也适用于 DMA pin、异步 I/O pin 等临时引用。
- **准确描述剩余风险**: backing pin 与统一匿名页回收能排除物理页复用和强制 swap 风险，
  但文件 truncate/page-cache invalidate 若换成新 backing，仍可能形成 false-negative；pin 也会
  临时增加内存压力。没有构造精确交错时应标为 NOT RUN，不把静态证明或普通 LTP 冒充动态覆盖。
- **相关文件**: `os/src/mm/address_space.rs`, `os/src/task/threads.rs`,
  `os/src/syscall/process/futex.rs`, `docs/05_process/futex.md`

## Futex lost-wake：锁外 fault-in，锁内 nofault 比较并发布

- **危险二选一**: 最后一次值比较若在 queue 锁外，waker 可在“比较通过、尚未入队”的窗口
  改值并 wake，等待方随后永久睡下；若直接在自旋锁内调用通用 uaccess，又可能进入 VM 锁、
  CoW/文件缺页、分配或等待，破坏不可睡眠锁约束。
- **固定协议**: 先在锁外做 faultable 用户读取和 key 解析，提前分配 waiter、clone VM owner；
  再取得 queue/table 锁，只用 VM `try_lock` 解析现有 PTE、校验权限并做一个硬件宽度的
  nofault 读取。值匹配时在同一 table 临界区发布 waiter；不匹配返回 `EAGAIN`。
- **条件修改也要共用线性化锁**: `CMP_REQUEUE` 一类操作不能只把队列搬运加锁，而把用户
  条件比较留在锁外。应先在锁外 fault-in/解析 key，再在 table 锁内 nofault 比较，并在
  同一临界区完成 wake/move。`Retry` 只能在零队列副作用时返回；一旦开始修改就不能重放。
  对不读取 source 的 private REQUEUE，不要为了统一流程擅自增加 PTE 前置条件。
- **nofault 不只是不调用 fault handler**: 若 helper 在外层自旋锁内阻塞等待 VM 锁，它仍然
  不是可接受的 nofault 路径。该边只能是条件式非阻塞 `table -> VM try_lock`；锁忙、PTE
  变化或 shared backing 身份不一致都在 waiter 发布前返回内部 Retry，释放 table 后完整
  重做 faultable 读取和 key 解析。
- **重试边界**: Retry 不是 errno。shared key 必须重新解析，不能沿用可能已被 remap 的旧
  backing；waitv 每次 Retry 要重读所有 word 并重建所有 key，而描述符数组只在 syscall 入口
  快照一次。相对 timeout 先固定为绝对 deadline，否则竞争会无限延长等待。
- **不要第三次读取**: 比较与 enqueue 已共享线性化锁后，入队后再读一次既无必要，又重新
  引入 faultable 锁序；requeue 后原 word 也不再是 registration 的权威状态。
- **证据边界**: 普通 futex LTP 能发现功能退化，不能替代“最后比较/并发 wake”、持续 VM
  lock contention 或比较后 remap 的精确交错。没有专项并发 harness 时应记为 NOT RUN。
- **相关文件**: `os/src/mm/address_space.rs`, `os/src/task/threads.rs`,
  `os/src/syscall/process/futex.rs`, `docs/01_architecture/lock-order.md`

## 带旧值输出的状态 syscall：锁内提交，锁外回复

- **危险模式**: syscall 在 owner 锁内向用户写旧值，会把缺页、CoW 和 TLB shootdown 带进
  普通业务锁；若为了避锁先写旧值、再读取或提交新值，又会破坏 Linux 的副作用顺序，并让
  两个 CPU 的权限检查、旧值快照和新值提交彼此穿插。
- **固定协议**: 先把新值完整 copyin 并完成无锁校验；随后在唯一 owner 锁内快照旧状态、
  复核权限并一次提交新状态；释放锁后执行外部注册，再 copyout 旧值。只读 reply 也先在锁内
  复制为内核所有快照，不能把 guard 或内部引用带进 uaccess。
- **成对字段必须一起发布**: soft/hard、value/interval 这类由一个 ABI 对象表达的字段不能拆成
  两次 owner 锁周期。即使底层只有两个 setter，也要在同一 guard 下完成，让所有合法读者只能
  观察某次完整提交。copyin 与目标查找的先后若影响 errno，还要直接对照官方 syscall 实现。
- **错误语义必须查官方 ABI**: Linux 的 `setitimer`、`timer_settime`、`prlimit` 等路径在新
  状态提交后才写旧值；old pointer 的 `EFAULT` 不回滚已经生效的修改。不要凭“事务直觉”
  擅自回滚，也不要因 copyout 失败重锁覆盖并发更新。
- **查询不能污染权威状态**: remaining/deadline 等派生值应在栈上快照中计算。若只为回复
  用户而改写 owner 内保存值，后续刷新路径可能再次应用同一时间差。
- **先证明共享域，再选择 owner**: 进程属性不能因为调用入口拿到的是 TCB 就放在线程私有锁里。
  对 rlimit 这类线程组状态，应明确验证 thread clone 共享、fork 得到锁内一致快照、exec 保留；
  消费路径只复制所需标量并释放 owner 锁，再进入 task、VM、signal 或 FS 的下一层锁。若某项
  限制还依赖尚未实现的组级计数或特殊共享对象，应明确保留为例外，而不是只迁字段伪造语义。
- **高频计数与低频策略分层**: trap/调度热路径使用线程本地批次和 PCB 原子累计，只发布
  monotonic pending；修改 soft/hard pair、推进阈值和生成信号放到安全点慢路径。writer 重设
  阈值时不能无条件清 pending，否则会覆盖并发越线事件；慢路径发布下一阈值后还要复查累计值，
  闭合“处理旧阈值期间跨过新阈值”的窗口。批量降低争用的同时必须明确最大观测滞后，并在
  schedule-out/exit 强制冲刷尾数。
- **ABI 分项与策略总量分工**: user/system 这类分别对外报告的累计量可用独立原子维护，但
  限额或状态机只能依赖一个权威 total，不能跨两次分项读取拼出判定样本。读取当前进程前先
  强制发布调用线程已经结算的本地尾数；其它 CPU 的活跃 sibling 只提供有界近似快照。
- **退出发布顺序决定组快照完整性**: 每个线程必须先把本地尾数发布到进程累计，再以 release
  语义递减 live token；最后线程通过 acquire/release chain 保存进程级 zombie 快照。不能把
  最后退出 TCB 的私有用量误当成整个线程组，也不能在发布 live token 后再补记账。
- **终态事件必须携带完整值快照**: wait/reap 一类路径不能只把 PID/status 带出锁后再查
  registry；PID 可能已释放或复用。应在 child 仍被父列表固定时一次取得 status 与资源值，
  syscall 回复和 parent 累计复用同一份 Copy 快照，再执行 PID/registry/quota 回收。
- **终态 copyout 失败通常不回滚**: status/rusage/siginfo 等 faultable 回复必须位于所有生命周期
  锁外，但 EFAULT 不代表把已消费事件重新入队。具体字段顺序要对照官方实现；WNOWAIT 只观察，
  不能误做 parent 累计或资源回收。
- **相关文件**: `os/src/syscall/process/lifecycle.rs`,
  `os/src/syscall/process/time.rs`, `os/src/syscall/process/ids.rs`,
  `os/src/task/process.rs`, `docs/01_architecture/lock-order.md`

## WaitQueue 条件只领取内核状态，faultable 回复延后

- **危险模式**: 条件闭包会在 waiter 登记前后重复执行。当前普通 WaitQueue 已在登记后释放
  队列锁再调用 condition，但若闭包直接执行 `UserPtrMut`/copyout，重试仍可能重复消费对象，
  用户缺页、CoW 或 TLB shootdown 也会污染底层同步路径。
- **固定协议**: 条件闭包只在底层 owner 锁内检查并唯一领取内核对象，把拥有所有权的结果
  保存到 syscall 栈；WaitQueue 返回并完成 waiter 清理后，再执行 faultable reply。不要把
  owner guard、内部引用或只靠序号定位的结果带出锁。
- **错误语义**: 领取成功后的 copyout `EFAULT` 是否回滚必须对照官方 ABI。Linux
  `rt_sigtimedwait` 先 dequeue signal，再 `copy_siginfo_to_user()`；写回失败不会重新发布信号。
- **测试边界**: 普通功能用例能验证领取、siginfo、timeout 和 EFAULT，不能证明“第二次条件
  检查→调度器睡眠登记”之间的精确信号到达窗口；这类 interleaving 必须单独建模或测试。
- **相关文件**: `os/src/task/signal/wait.rs`, `os/src/task/manager.rs`,
  `docs/01_architecture/lock-order.md`

## WaitQueue lost-wake：睡眠意图登记后复查持久条件

- **危险窗口**: 第二次 condition 返回 false 后，任务仍可能是 `Running`。事件生产者此时发布
  状态并调用 wake，可能得到“任务尚未睡眠”；若消费者随后无条件登记并切走，这次 wake 不会
  自动保存到未来。仅仅“在 queue 锁内多检查一次”不能覆盖 condition 与调度状态转换之间的边。
- **固定协议**: 把事件发布为 owner 锁保护的持久状态，并用可靠的 `WaitEntry` 通知记录本轮
  wake。普通 wait 的 condition 在队列锁外运行；checked block 只复查 token、信号、超时和
  生命周期，不再次调用可能领取对象的通用 condition。locked wait 则在同一业务锁下完成
  条件检查和 waiter 登记，并在 `Running -> Blocking` 桥接处释放该业务锁。
- **边沿通知需要登记级 token**: 并非所有 wake 都有可复查的 owner 状态。通用 WaitQueue 应为
  每轮登记建立独立 token，由第一个生产者 CAS `Waiting -> Notified`；checked block 只需复查
  token 和通用退出原因。token 只保存本轮通知，`TaskStatus` 仍独占 CPU/runqueue 所有权，不能
  为修 lost-wake 给调度状态机增加 `WakePending` 一类重复状态。
- **多队列共享同一登记**: poll/epoll 一轮等待挂到多个 queue 时应共享一个 token，不能每个
  queue 各建一份通知状态。清理先关闭 token，再逐队列分别摘除同一登记；这样既让多个 wake
  源只能有一个赢家，也避免同时持有两把 queue 锁。
- **返回结果不拥有事件**: Interrupted/timeout 只说明等待循环为何停止，不等于消费者已经取得
  事件。退出等待后应在 owner 锁内最后 claim 一次，再决定返回事件、`EINTR` 或 timeout；claim
  成功后才能把所有权带到锁外。
- **清理路径也要尊重声明**: 若消费者用 wait mask 声明正在领取某类 pending 对象，通用
  ignore/discard 清理必须排除该集合，否则最终复查只能看到已被旁路删除的状态。
- **测试边界**: 普通 8 核功能回归只能证明现实路径未退化，不能证明纳秒级窗口一定被命中。
  无精确注入 harness 时，应把状态转换与锁序证明写入证据，并明确动态交错为 NOT RUN。
- **相关文件**: `os/src/task/manager.rs`, `os/src/task/signal/mod.rs`,
  `os/src/task/signal/wait.rs`, `docs/01_architecture/lock-order.md`

## 派生原子 hint：必须与权威队列在同一 writer 临界区发布

- **危险窗口**: 在 mutex 内修改队列并计算 hint，解锁后才 atomic store。旧消费者可携带空
  快照暂停；新生产者随后完整入队并写非空；旧消费者恢复后再写空，最终形成“队列非空、hint
  为空”的永久 false-negative。每个局部操作都持锁和原子，并不代表组合协议正确。
- **根因不是 Relaxed 本身**: mutex 只串行临界区内的 mutation，锁外 store 已逃出 writer
  全序。把 Relaxed 换成 Release 不能阻止旧 store 在新 store 后执行。
- **固定协议**: 在同一个 owner mutex 内完成 mutation、重新计算完整派生值、Release store，
  然后才 unlock；无锁 fast path 用 Acquire load。mutex 决定 writer 顺序，Release/Acquire
  表达发布/消费。真正领取对象仍要回到 owner 锁内重验，hint 不得升级为权威状态。
- **审计方法**: 不只搜索 hint store，还要搜索底层 queue 的所有 enqueue/dequeue/remove/clear
  写点，确保没有旁路 mutation。若 dequeue 后还要进入另一个 owner，先把值对象带出并释放
  第一把锁，不能为了同步 hint 新增双锁嵌套。
- **验收边界**: 普通并发功能回归能发现明显丢信号，但不保证命中“旧 writer 暂停、新 writer
  完整通过、旧 writer 恢复”的精确三方交错。没有注入点时，登记 NOT RUN 并保留完整锁序证明。
- **相关文件**: `os/src/task/process.rs`, `os/src/task/signal/mod.rs`,
  `docs/01_architecture/lock-order.md`

## 进程级异步对象：预留发布、唯一装载序号与锁外唤醒

- **先定共享域**: timer、异步通知等对象若由整个线程组通过整数 ID 访问，owner 应放在 PCB，
  而不是创建者 TCB。thread clone 共享 owner，fork/exec/exit 的继承和清理规则必须显式编码。
- **faultable ID 发布**: “分配 ID 后写回用户指针”不能跨 owner 锁。slot 使用
  `Vacant -> Reserved -> Active`，先保留身份、锁外 copyout、成功后再发布；查询和删除只接受
  Active，copyout 失败撤销 Reserved。这样既不持锁 fault，也不暴露半初始化对象。
- **拒绝 slot ABA**: 只比较可复用 ID 或 slot-local 重置 generation 不够。每次 arm 从 owner
  范围分配不重复的序号，异步 action 同时匹配 owner、ID、arm sequence 和事件 deadline。
  delete/recreate、rearm、exec 和 exit 的旧节点才能稳定失效。
- **生成事件与投递分层**: callback 在 owner 锁内验证身份、提交对象状态，并把完整值事件写入
  预分配或固定栈批次，使 delete/clear 与“本次到期已被领取”有单一线性化顺序；释放 owner
  锁后再进入可能扩容的 pending 队列、扫描线程、操作 runqueue 或重新注册异步节点。为此可把
  signal API 拆成“只入队”和“只唤醒”两个语义清楚的 helper，但两者都不能偷偷跨回 owner 锁。
- **compact 也属于锁图**: 队列清理若在 queue 锁内读取 owner 表，所有注册路径都必须先释放
  owner 锁再进入 queue，明确形成单向 `queue -> owner`，禁止反向嵌套。
- **相关文件**: `os/src/task/process.rs`, `os/src/task/manager.rs`,
  `os/src/task/signal/delivery.rs`, `os/src/syscall/process/time.rs`

## CPU-clock timer：时钟域分离、锁外采样与锁内唯一领取

- **legacy interval timer 也是线程组对象**: `ITIMER_REAL/VIRTUAL/PROF` 由 Linux
  `signal_struct` 持有；thread clone 共享、普通 fork 不继承、exec 保留、最后线程退出清空。
  不要因为 syscall 从 current TCB 进入就把 timer 放回线程私有状态。
- **REAL 不是可调墙钟 timer**: `ITIMER_REAL` 统计 elapsed monotonic 时间；
  `settimeofday/clock_settime` 不应重定位它。VIRTUAL 读取线程组 user CPU，PROF 读取线程组
  user+system CPU，三者不能共享一套“每次 trap 扣 remaining”的实现。
- **不要借 wall heap 冒充 CPU clock**: wall timer 的 deadline 随 monotonic/realtime 推进；CPU
  timer 只随目标线程或线程组实际消耗推进。把后者换算成 `TimeSpec::now()` 会让 sleep/阻塞时间
  错误触发，也无法表达多线程 process clock 的累计。
- **对象身份而非可复用 ID**: thread CPU timer 应保存目标 TCB 的 `Weak`/稳定对象身份；只保存
  TID 会在退出和复用后误命中新线程。process CPU timer 的 owner/clock 都属于 PCB。
- **采样与领取分层**: 先在 owner 锁外采样单调 CPU 计数，再在 timer 表锁内比较 deadline 并
  清除 one-shot 或批量推进 periodic。多 CPU 扫描同一表时，表锁只允许一个 CPU 领取同一到期；
  并发记账造成的旧样本只会把投递延迟到下一安全点，不会提前触发。
- **热路径只用 hint**: 可用 Release/Acquire 原子 hint 跳过没有 armed CPU timer 的进程，但 hint
  不能取代表内 slot/deadline。arm/disarm/delete/clear/scanner 必须在同一 owner 临界区同步 hint。
- **验收边界**: 参数和 busy-loop LTP 能证明相对/绝对/周期基础语义；跨 sibling 唯一领取、owner
  退出、sleep 不推进 clock 和 per-timer pending/overrun 身份必须有专门交错探针，否则记为
  NOT RUN。
- **相关文件**: `os/src/task/process.rs`, `os/src/task/task.rs`,
  `os/src/task/manager.rs`, `os/src/task/processor.rs`, `os/src/syscall/process/time.rs`

## POSIX timer pending：对象身份、装载身份与交付身份必须分离

- **signal bitmap 不是对象身份**: 两个 timer 可以投递同一个非实时 signal。bitmap 只能回答
  “某 signal 是否存在”，不能判断“某 timer 是否已经有事件排队”；用它累计 overrun 会把两个
  timer 相互污染，并让其中一个事件被普通非实时合并吞掉。
- **三个生命期不要混用**: timer 对象从 create 到 delete 用全表单调 `instance_seq`；每次
  `timer_settime()` 的 heap 节点用 `arm_seq`；队列项用 `timer ID + instance_seq`。前者防
  delete/recreate ABA，中者防 stale rearm，后者把 overrun 绑定到实际可交付对象。
- **每对象一项，不是每 signal 一项**: 同一 timer 的周期到期在已有事件上累加 overrun；不同
  timer 即使 signal number 相同，也各自占一个 timer 事件。timer 事件应使用对象预留资格，
  不能被普通 queued-signal 全局配额随机拒绝后永久留下 owner-side pending 标志。
- **交付跨 owner 时拆锁**: signal queue 锁内只 dequeue 精确事件，释放后再取得 timer owner
  核对 instance 并固化 `SigInfo`/last-overrun。delete/exec/exit 反向先在 timer owner 收集事件
  身份，释放后再精确删除 signal queue 项。不要为了“原子”同时持有两把锁；对象序号就是
  跨临界区的重验凭证。
- **重设与旧 pending**: `timer_settime()` 不应为同一 timer 创建第二个队列项。旧项可保留，
  但在新设置真正再次到期前不得更新新一轮 `timer_getoverrun()`；用一个明确布尔状态表达这条
  语义，比复用 heap arm sequence 推断交付归属更清楚。
- **验收边界**: 普通 `timer_settime01/02` 只能证明基础 ABI。两个 timer 共用 signal、周期
  overrun→dequeue→getoverrun 回环、delete/recreate pending ABA 和 signalfd timer 字段需要
  专项探针；未运行时必须登记为 NOT RUN。
- **相关文件**: `os/src/task/process.rs`, `os/src/task/signal/pending.rs`,
  `os/src/task/signal/mod.rs`, `os/src/syscall/process/{time,signal}.rs`

## 启动期常驻 worker 不能先于 PID1 走首次 Blocking

- **现象**: normal 双盘启动在块设备 MBR 分区注册后停滞，zero-drive ktest 仍全部通过；最后一行容易误导为 root-fs mount 或 virtio-block 问题。
- **根因**: CPU0 FIFO runqueue 在 PID1 前先发布一个常驻 worker。worker 的首轮无待处理 generation，立即进入 `wait_event_interruptible_lazy()` 的 `Running -> Blocking` 路径；启动时序因此在 PID1 首次 dispatch 前卡住。
- **修复**: normal boot 先 `add_initproc()`，再发布 worker。这样 PID1 首次 dispatch 先完成 normal 启动；worker 的 generation 条件仍在等待登记前及登记后重查，producer 在任一窗口发布都不会丢 wake。
- **教训**: 含外盘的 normal boot 与 zero-drive ktest 的任务发布顺序不同；ktest 全绿不能证明 early Blocking 对 PID1 启动安全。看到 MBR 后静默时，沿下一条 task publication/首次 dispatch 排查，而不是先假设块 I/O 未完成。
- **相关文件**: `os/src/main.rs`, `os/src/task/run_queue.rs`, `os/src/net/config.rs`, `os/src/task/manager.rs`

## 物理页合法性：静态地址范围不能替代动态 allocator 所有权

- **危险模式**: 初始化时用 `[skernel, ekernel)` 排除内核镜像是正确的，但把同一条件永久
  用作“物理页能否映射给用户”的判定会漏掉后续所有权转交。linker 内嵌 initramfs 等载荷的
  完整页复制完成后仍位于原地址，却已经登记为可复用回收页。
- **固定协议**: 启动区间生成负责排除当时仍有 owner 的内核/固件范围；fault/uaccess 的
  后验检查只验证整页属于固件可用 DRAM，实时所有权由页表、VMA 和 `FrameTracker` 保证。
  若确实需要回答 allocator owner，再使用专门接口；不要把 allocator 锁塞进每页用户复制，
  也不要把“仍位于链接范围”误当作“所有权从未转交”。
- **定位技巧**: 当 alloc/free 测试通过，而 user fault 的 PTE 后验校验在双架构同点失败时，
  先记录实际 PPN 并对照 recycled/reclaimed 来源；不要因为测试涉及 MMU 就先归因于 TLB。
- **验收**: 保留修改前 RED，修正后用同一双架构映射、权限和回收用例转 GREEN；另外探测
  运行期最后一个 usable 页，防止动态大内存仍被旧编译期上界截断。
- **相关文件**: `os/src/mm/frame_allocator.rs`, `os/src/mm/address_space.rs`,
  `os/src/kernel_tests/mm.rs`

## 双架构 syscall 差异：先核对遗漏参数是否被显式初始化

- **现象**: 同一用户程序在 LA64 正常，RV64 的 `wait4` 在 child 已被领取后返回 `EFAULT`，甚至
  改写相邻用户栈数据；表面上很像 fork、调度或用户 copy 的架构竞态。
- **根因**: 用户库把四参数 `wait4(pid, status, options, rusage)` 错接到三参数 wrapper。LA64
  汇编桥会把缺省参数清零，RV64 inline asm 没有约束 `a3`，内核因而把残留寄存器当成 rusage
  用户指针。两架构差异来自 wrapper，不来自内核 wait 语义。
- **定位方法**: 先打印 syscall 返回值和各 copyout 目标；对照内核 dispatch 实际读取的参数数，
  再逐架构检查 inline asm/汇编桥对未传参数的处理。不要因为故障只在 SMP RV64 出现就先归因
  于 TLB 或调度竞态。
- **修复**: 调用者使用与真实 ABI 参数数一致的 wrapper，并显式传零给不用的可选指针。不要
  依赖调用约定、寄存器偶然值或另一架构桥接代码代为清零。
- **相关文件**: `user/src/syscall.rs`, `os/src/syscall/mod.rs`,
  `os/src/syscall/process/lifecycle.rs`

## 共享 fd 的等待 owner 必须在使用点解析

- **危险模式**: fork 共享 open file，但把 signalfd 的 waitqueue 存入 inode。child 的 pending
  已属于新 sighand，却仍睡在父队列；信号正确入队也无法唤醒 child。
- **固定协议**: open file 只保存真正共享的 mask；read/poll 在每次等待时从 current task 动态
  解析 sighand-owned EventWaitQueue。普通 fork 新建队列，`CLONE_SIGHAND` 才共享。
- **锁序**: pending 在 `task.inner`/`process.signal` 中提交，释放 owner 锁后通知事件队列。
  waitqueue 只表示“需要重查”，不能复制或取代 pending 权威状态。
- **审计边界**: 检查 blocking read、poll、epoll 和通用 splice 路径是否都经 File helper；同时
  记录 Linux 的 epoll/fork 限制，继承的 epoll registration 不会自动改绑 child sighand。
- **相关文件**: `os/src/fs/vfs/file.rs`, `os/src/task/signal/action.rs`,
  `os/src/task/process.rs`, `os/src/syscall/process/signal.rs`

## 跨锁域 Weak 注册表必须用强身份快照阻断 ABA

- **危险模式**：注册表用裸地址作 key、保存 `Weak<T>`，walker 升级后立即丢掉强引用，
  释放注册表锁再进入另一个 owner 锁重验。旧对象若在窗口内析构，allocator 可能把同一地址
  分给新对象，仅比较地址或标量字段就会把旧操作误施加到新对象。
- **固定协议**：注册表锁内升级 `Weak`，把 `Arc<T>` 本身放进不可变快照；释放注册表锁后
  进入目标 owner，并以 `Arc::ptr_eq` 对权威表中的当前身份重验。强引用必须覆盖整个跨锁
  窗口，不能只复制地址。完成重验后再执行 mkclean、unmap 或状态提交。
- **适用范围**：file-VMA rmap、route directory、异步 owner 表和任何“弱索引→解锁→另一锁
  下提交”的调用链。数值 generation 可以辅助判断变化，但不能替代对象身份。
- **相关文件**：`os/src/fs/page_cache.rs`、`os/src/mm/vma.rs`、
  `os/src/mm/vma_set.rs`

## 静态单例中的堆对象用 Once 延迟构造，并让事件状态覆盖初始化窗口

- **问题**：`WaitQueue` 等对象内部需要堆分配，不能直接放进普通 `const fn` 静态初始化；
  为此开启全局 const-heap 或在 IRQ 首次触发时构造，都会扩大不必要的初始化与中断风险。
- **做法**：静态控制块保存 `spin::Once<Mutex<WaitQueue>>`，只允许任务上下文首次构造。
  初始化前到达的生产者先把权威 `pending` 置位；若 Once 尚未完成则不唤醒。worker 构造后
  先在 WaitQueue 条件协议中重查 pending，因此早到事件不会丢失。
- **门禁**：IRQ 路径只能写原子 pending/deferred 标志，不能调用 Once、分配或取得 WaitQueue；
  动态测试应通过真实业务结果证明 worker 被唤醒，不要为测试在生产热路径保留 submitted/
  completed 计数或专用 hook。
- **相关文件**：`os/src/net/config.rs`、`os/src/kernel_tests/net_smp.rs`

## FFI 挂载与测试门禁的交易性

### C 全局注册表的失败路径必须逆序回滚

- **现象**: Rust wrapper 在“设备注册成功、mount/journal/writeback 后续步骤失败”时直接
  `Drop`，C 层全局表仍保留指针；后续重试可报重名/无 slot，更严重时访问已释放内存。
- **根因**: 挂载是多步跨语言交易，但 wrapper 只有一个笼统的 mounted bool，C 内部函数也没有
  在每个 error label 撤销 block cache、block device 和 mountpoint slot。
- **修复**: 显式记录 `device_registered → fs_mounted → journal_started →
  writeback_enabled`，每次成功后才推进状态，失败时逆序撤销；只有全部从 C 表
  脱钩后才释放 Rust/C 共享内存。若卸载失败，宁可有界泄漏并报错，也不制造 UAF。
- **教训**: 任何“注册→初始化→启动子系统”的 FFI API 都应当作交易审计；不能仅检查
  happy path 或依赖 Rust `Drop` 自动修复 C 全局状态。
- **相关文件**: `dependency/lwext4_rust/src/blockdev.rs`、
  `dependency/lwext4_rust/c/lwext4/src/ext4.c`

### 回归进程全过不等于门禁完成

- **现象**: TAP 已打印 `N passed, 0 failed`，但 QEMU 一直不退出，Makefile 最终只看到 timeout；
  或测试使用了违反 syscall 前置条件的输入，把正确 errno 误报为内核回归。
- **根因**: PID1 直接 `exec` 测试程序后不再有 supervisor 可以 wait、输出机器可读最终标记并
  关机；同时用例注释的“partial range”没有区分“起点对齐”与“长度可非对齐”。
- **修复**: 保留 PID1 supervisor，fork/exec 子进程、wait 下载状态、打印唯一 PASS/FAIL 标记后
  shutdown；Makefile 同时检查程序退出与标记。syscall 用例先对照 ABI 前置条件，再选边界值。
- **教训**: 门禁必须验证“用例运行→结果聚合→可机器识别的终态→可观测退出”整条链；
  不能将某段日志看似全绿当成门禁通过。
- **相关文件**: `user/src/bin/regression_init.rs`、
  `user/src/bin/regression/regression_mmap_edge_cases.rs`

### U-Boot 串口完整但内核长行确定性缺字时检查 THRE 握手

- **现象**: 同一串口和波特率下，U-Boot 的 TFTP、CRC 和命令输出完整；内核接管 UART 后，短行只剩片段，长行稳定缺少大量字符。重复复位后缺字模式近似一致，使硬件探针实际运行却无法取得可信 PASS 证据。
- **根因**: NS16550A 的 `Write<u8>` 实现未读取 `LSR.THRE` 就直接覆盖 THR，并无条件返回成功；上层 `console_putchar()` 又忽略返回值。CPU 连续 MMIO 写入快于 UART 移出字符时，发送保持寄存器被覆盖。
- **修复**: `Write<u8>` 仅在 THRE 就绪时写 THR，否则返回 `WouldBlock`；上层发送函数循环重试到成功。保留整条 `print` 的 irq-save 序列化，不能用重复打印 marker 或降低日志量掩盖底层发送违规。
- **验收**: 以修改前同一只读实板探针作为 RED，对照修改后原始串口日志必须完整包含型号、容量、重复读取结果和最终 PASS；同时顺序完成双架构编译。U-Boot 输出正常只能证明主机接收链路和波特率正确，不能替代内核 UART 握手验证。
- **相关文件**: `os/src/drivers/serial/ns16550a.rs`, `os/src/hal/arch/loongarch64/sbi.rs`, `os/src/console.rs`

### journal 掉电恢复必须在可证明的持久化窗口外部截断

- **现象**：正常卸载、冷重启和离线 fsck 都通过，但无法证明事务 commit 已落盘、home block
  未 checkpoint 时的恢复正确性；随机关 QEMU 又难以复现，失败镜像也不可比较。
- **方法**：在 journal 内设置默认关闭、单次触发的测试钩子。钩子只能停在 records/commit block
  已写并 flush、journal start pointer 也已写并 flush、home checkpoint 尚未开始的位置；串口先打印
  唯一 marker，再由宿主 timeout/kill QEMU，不能让内核自己正常 shutdown。
- **门禁**：首启制造掉电后保留原镜像；次启必须复用同一镜像，验证 replay 后的语义状态、可写性
  与完整 teardown；关机后再执行只读 `e2fsck -f -n`。正常 remount 或只看 journal start 清零不能替代
  这个两阶段实验。
- **教训**：故障点必须同时证明“恢复记录已经 durable”和“home 状态尚未 durable”；太早只是丢事务，
  太晚只是正常 checkpoint，两者都会产生误导性的绿色结果。日志需记录 fixture feature、block size、
  镜像 hash 与两次启动身份。
- **相关文件**：`dependency/lwext4_rust/c/lwext4/src/ext4_journal.c`、
  `os/src/kernel_tests/ext4.rs`、`os/make/rv64.mk`、`os/make/la64.mk`

### 精确 wait 与通用 reaper 不能竞争同一个子进程

- **现象**：子命令已经明确报错，supervisor 却打印 ready/PASS；随后真正入口因准备未完成而失败。
- **根因**：精确 `waitpid(pid, WNOHANG)` 之前调用 `waitpid(-1, WNOHANG)`，通用 reaper 可能先回收目标 pid 并丢弃状态。精确 wait 随后得到 `ECHILD`，若 status 还以 0 初始化，就会把失败伪装成成功。
- **修复**：目标子进程 outstanding 时只做精确 wait；通用 orphan 回收放到目标状态已取得之后。status 以 `127 << 8` fail closed，任何精确 wait 错误都保留非零状态并记录 errno。
- **教训**：一个 child 的退出状态只能有一个 owner。凡是同时存在 supervisor wait 和全局 reaper，都要明确所有权，不能把 `ret < 0` 与“已成功取得状态”合并处理。
- **相关文件**：`user/src/bin/initproc.rs`

### 成熟 ext4 卷迁移不能只用全新 fixture 验证

- **现象**：全新 QEMU ext4 上嵌套 symlink 创建/删除都通过，真实旧卷却出现同名目录项、dentry type 与 inode mode 冲突、`ln`/`chroot` 失败；应用门禁甚至可能先 PASS，下一次小文件写入却覆盖无关 Python 源码。
- **根因**：旧驱动可能留下重复同名目录项、不可靠 file type，以及“inode/extent 仍引用数据块、块位图却标为空闲”的跨层不一致。全新 fixture 没有历史状态；新驱动按位图合法分配时会复用仍被旧文件引用的块。底层 symlink API 若不是 create-exclusive，还会继续覆盖并隐藏目录问题。
- **定位**：在全盘备份副本上用 `debugfs ls/stat/testi/testb/blocks` 同时审计目录项、inode 和位图，不能只看文件可读。整文件摘要门禁若在一次小写入后变化，要比较异常文件内容与探针 payload；命中就先按跨文件块复用处理，而不是把新摘要加入版本白名单。
- **修复**：路径遍历复用本来就要加载的 child inode，并始终以真实 inode mode 校验类型；适配层补 symlink `EEXIST`，迁移器只删除 readlink 可证明的旧 symlink。更重要的是，成熟旧卷在新后端第一次写入前必须离线 `e2fsck -fy` 到收敛，再以 `e2fsck -fn` 复检 clean；无法安全修复时从逻辑文件备份重建新卷。
- **验收**：至少包含“fresh fixture 语义测试 + 旧卷只读结构/位图审计 + offline-fsck-clean 前置条件 + 同一修复卷首次写入 + 冷启动复用 + 最终离线 fsck + 关键文件摘要”。在线 raw 快照的 fsck 结果不能直接外推为当前实盘状态，但只要该副本能复现 live extent 被标 free 并发生串写，就已经足以否决在该副本上直接迁移；生产卷必须取得自身的离线证据。
- **相关文件**：`dependency/lwext4_rust/c/lwext4/src/ext4.c`、`os/src/fs/ext4_lwext4/layout.rs`、`user/src/bin/initproc.rs`、`scripts/board/patch_ddgs_redirect.py`

### JH7110 DWMAC5 TX 正常但 RX descriptor 零 write-back

- **现象**：VF2 上 TX DMA 能清 OWN、MAC 内部 loopback 可通过，但物理 RX descriptor 始终没有 write-back。
- **根因**：YT8531C 默认门控 MAC 侧 RXC；同时 DWMAC5 增强 RX 描述符的 `RDES3.BUFFER1_VALID_ADDR` 是 bit 1，DMA tail pointer 作为 restart 通知必须晚于 OWN 发布，且通道 RX buffer size 必须与实际 buffer 长度一致。
- **修复**：在 YT8531C extension register 打开 RXC、禁用自动休眠并设置 VF2 RGMII-ID 延迟；用 bit 1 标记 buffer 有效，先写 OWN 和 DMA barrier 再更新 RX tail，使用 2048-byte size field。
- **教训**：TX/内部 loopback 只能证明 MAC 与 TX DMA 路径，不能替代 PHY 到 MAC 的 RXC 时钟和真实 RX descriptor ABI 验证；实板诊断应同时检查 PHY 时钟、RDES3 ownership/valid 位、tail publication 顺序和 DMA size 编码。
- **相关文件**：`os/src/drivers/net/gmac_jh7110/phy.rs`、`os/src/drivers/net/gmac_jh7110/ring.rs`、`os/src/drivers/net/gmac_jh7110/mmio.rs`

## 启动文件系统

### initramfs 占位 `.gitkeep` 阻止目录替换为 sdcard 符号链接

- **根因**: CPIO 中的 `/musl`、`/glibc` 虽然没有运行时库或测试脚本，但保留了 `.gitkeep`，使 `rmdir()` 失败；仅以 `test -e` 判断也会把空目录误当成已正确配置，最终 lmbench 仍在 initramfs 目录找脚本。
- **修复**: 确认 `/sdcard/{musl,glibc}` 存在后，先删除 initramfs 目录的 `.gitkeep`，再 `rmdir` 并创建到 sdcard 的绝对符号链接。
- **教训**: 需要将 CPIO 占位目录替换为挂载盘路径时，先检查打包后的 CPIO 条目，而不是只检查源码树目录是否“空”。
- **相关文件**: `user/src/bin/test_runner/bootstrap/layout.rs`, `scripts/build_initramfs.sh`

## WaitQueue 在 block 前覆盖并发唤醒

- **根因**: 用 `TaskStatus` 作为唯一通知载体时，wake 可在条件复查后把任务标为 `Ready`，随后 block 入口又写回 `Interruptible`，从而吞掉通知。
- **修复**: 每次等待创建共享 `Arc<WaiterState>`；释放保护锁后用 CAS 执行 `Idle → Sleeping`，唤醒方先从队列移除 waiter 再写 `Notified`。signal/timeout 先写 `Closed`，从所有队列删除 waiter，最后复查条件。多队列必须注册同一个 waiter，保证第一个通知获胜。
- **教训**: 调度状态只能描述任务是否可运行，不能承担一次性通知语义；任何“注册 → 解锁 → block”协议都需要独立的原子握手和取消状态。
- **相关文件**: `os/src/task/manager.rs`

## WaitQueue 条件闭包重入

### 网络 poll 从条件闭包重入 EventWaitQueue

- **根因**: `WaitQueue::wait_until_interruptible()` 的条件闭包会在持有 waitqueue 锁时执行；若闭包调用的 TCP `try_*` 方法再执行 `NET_INTERFACE.try_poll()`，poll 回调会通过 `wake_tcp_waiters()`/`wake_raw_waiters()` 通知同一个 `EventWaitQueue`，对不可重入锁再次加锁而死锁。
- **修复**: 在进入 `wait_until_interruptible()` 前执行一次 `NET_INTERFACE.poll()`；条件闭包只调用无 poll 的状态检查变体。通知路径始终使用 `notify_events_all`/`notify_events_at_most`，不能以 `try_lock()` 失败跳过唤醒，否则会丢失就绪通知。
- **教训**: “通知时跳过锁竞争”不是死锁修复；它把确定性死锁替换成数据丢失。对可能触发 wake 回调的操作，应把副作用移出持锁条件闭包，而非削弱回调的交付保证。
- **相关文件**: `os/src/task/manager.rs`, `os/src/net/socket/inet/stream/mod.rs`, `os/src/fs/vfs/event.rs`
## 固件 ABI / Hypercall

### 内联汇编调用约定中未初始化的寄存器被固件解释

- **现象**：VF2 串口输入产生 OpenSBI `ext=0x2 func=0xf4240`。`console_getchar()` 调用 legacy `sbi_call(which=2)`，但 a6 (x16) 寄存器未被显式初始化，包含来自调用者寄存器池的垃圾值 `0xf4240`（= 1_000_000）。
- **根因**：Legacy SBI ABI（EID < 0x10）规范上不使用 a6 传递 FID，但 OpenSBI 实现仍读取 a6 并输出 `func=<a6>` 日志。`sbi_call()` 的 `asm!` 只设置了 a0-a2 和 a7，遗漏了 a6。
- **修复**：在 `sbi_call()` 的 `asm!` 操作数中添加 `in("x16") 0usize`，确保每次 legacy ecall 时 a6 被显式零初始化。
- **教训**：调用固件/超管理器 ABI（ecall/hcall/svc）时，**所有**参数寄存器必须被显式初始化，即使规范将某些寄存器标注为"该调用类型未使用"。固件实现可能读取并记录这些寄存器的值，或将其纳入行为判断（如 OpenSBI 输出日志含 func=）。同一文件中的 `reboot()` 已正确初始化 a6=0（SRST 现代扩展需要 FID），恰好留下了对比。
- **相关文件**: `os/src/hal/arch/riscv/sbi.rs`

## SD/MMC 初始化：reset 后缺延时导致后续命令 INT_RTO

- **现象**: dw-mshc 驱动在实板上 CMD8 已正确应答 `0x1aa`（接口条件握手成功），但紧随其后的 CMD55（ACMD41 探测循环首条命令）触发 INT_RTO（响应超时）。CMD0 → CMD8 → CMD55 无任何延时连续发出。
- **根因**: 卡在上电/复位后需要时间稳定时钟与状态（SD 规范要求约 74+ 时钟周期），且 CMD0 复位后到能可靠应答命令之间需要额外时间。U-Boot `mmc_go_idle()` 在 CMD0 前 `udelay(1000)`、CMD0 后 `udelay(2000)`；本驱动缺失这些延时。
- **修复**: `initialize_card()` 在 CMD0 前 `wait_ms(1)`、CMD0 后 `wait_ms(2)`，并在 CMD8 成功后、首次 CMD55 前 `wait_ms(1)`；新增 `wait_ms(ms)` 帮助函数（`crate::timer::get_time_ms()` 截止时间自旋循环），`wait_10ms()` 委托之。
- **教训**: 硬件初始化命令序列（reset/if-cond/op-cond）之间必须保留电源/复位稳定窗口，不能依赖命令本身往返耗时。参考 U-Boot/Linux 的 `mmc_go_idle`/`mmc_send_op_cond` 时序。另注意本项目 `crate::timer::get_time_ms()` 返回 **`usize`** 而非 `u64`，`saturating_add` 传 `usize` 即可，传 `as u64` 会 E0308。
- **相关文件**: `os/src/drivers/block/dw_mshc/sd.rs`

### GPT 盘分区误判排查：dd if=/dev/mmcblk0p1 头部 'EFI PART' → probe_mbr 误把保护性 MBR 当分区；检查 type 0xEE 与 LBA1 签名

## 按编号/键限流内核日志（首次打印、重复静默）

- **现象**: 实板（VF2）上用户态频繁调用未实现的 syscall（如 `rseq(293)`、`fsopen(430)`），catch-all 分支每次调用都 `println!`+`error!` 打印全部参数，控制台被刷屏、串口吞吐被拖慢，且掩盖真正的错误输出。
- **根因**: 未知 syscall 的 catch-all `_` 分支无状态地去打印每个调用；没有区分"首次见到这个编号"与"重复命中"。
- **修复**: 模块级 `static REPORTED_UNSUPPORTED: spin::Mutex<alloc::collections::BTreeSet<usize>> = Mutex::new(BTreeSet::new());`，在 catch-all 中 `if REPORTED_UNSUPPORTED.lock().insert(syscall_id) { /* 打印一次 */ }`。`BTreeSet::insert` 返回 `true` 表示此前不存在（首次），重复时返回 `false` 静默跳过；始终返回 `ENOSYS`。
- **教训**: no_std 内核里 `spin::Mutex::new` 与 `BTreeSet::new` 都是 const fn，可直接用于 `static` 初始化（无需 lazy_static），已有先例 `os/src/mm/heap_trace.rs`、`os/src/trace.rs`。该模式可复用于任何"每 id/键只报一次"的噪音日志限流，避免在实板上对高频路径无条件打印。
- **相关文件**: `os/src/syscall/mod.rs`

## 性能计时单位

### `rdcycle` 不能用 timer frequency 换算为微秒

- **现象**: `/sys/kernel/stats` 导出 `clock_freq_hz` 并把 `rdcycle` 累计值当作 tick 除以该频率，得到的 handler 平均耗时会比同一 workload 的 lmbench wall time 大一个数量级，或与真实时间完全矛盾。
- **根因**: RV64 的 `cycle` CSR 是 CPU cycle 计数，不保证与 OpenSBI/QEMU 的 timer timebase 同频；而 `clock_freq_hz` 描述的是 `time` CSR / 内核 timer 的频率。两种计数域不能直接相除。
- **修复**: 对外暴露且需要以 `clock_freq_hz` 换算的诊断计时，统一从 `timer::raw_ticks()`（RV64 `time` CSR）取样；保留 `rdcycle` 的计数器必须单独标注为 CPU cycle，不能混用单位。
- **教训**: 首次分析任何 tick 指标前，先用独立 wall-time benchmark 做量纲 sanity check。若 `ticks / clock_freq_hz` 与 workload wall time 矛盾，先验证时钟源，再讨论热点占比；否则所有 µs、百分比和优化优先级都不可信。
- **相关文件**: `os/src/task/perf.rs`, `os/src/timer.rs`, `os/src/hal/arch/riscv/time.rs`

## LA64 临时 ELF 高半区窗口与低地址资源 PTE 别名

- **现象**：FDT/PCI 资源发现完成后，PID1 在 `File::map_to_kernel_space()` 的
  `insert_program_area(...).unwrap()` panic；若保留 `MemoryError`，失败值为
  `AlreadyMapped`，且普通 ktest 可因不加载 PID1 而不触发。
- **根因**：LA64 PGDH 后的三级页表只使用 VA[38:12]。把临时窗口放在
  `MMAP_BASE`（低 39 位为零）会与同一 kernel page table 中低地址恒等资源的 PTE
  别名；FDT 新发现资源会把此前未暴露的冲突变成启动 panic。
- **修复**：在 LA64 config 中定义独立的 `KERNEL_PROGRAM_BASE`，保留覆盖低地址
  固件/PCI 资源的别名保护带，并用编译期 canonical/窗口上界断言保证它仍在
  `KERNEL_PROGRAM_END` 以下；临时映射入口不得退回直接使用 `MMAP_BASE`。
- **验证**：先用 derived normal QEMU 捕获 PID1 的 `AlreadyMapped` RED；修复后必须
  运行实际 PID1 路径的 basic+busybox，ktest 不能替代该验证。
- **相关文件**：`os/src/hal/arch/loongarch64/config.rs`,
  `os/src/mm/kernel_space.rs`, `os/src/fs/vfs/file.rs`

## buddy 大请求取整到整个 heap 造成的"看似渐进泄漏"OOM

- **现象**: 完整 ktest 在某测试报 `HEAP ALLOCATION FAILED (FATAL) size=N`，N 随运行
  漂移（曾见 49152、34588672）。heap 用量统计只显示几 MB used，free 仍有几十 MB，但
  大分配仍失败。
- **根因**: buddy allocator 把 `layout.size().next_power_of_two()` 作为分配块。34 MiB
  的单个 `Vec` 请求取整到 64 MiB（order-24）——正好等于 64 MiB heap 总量，要求整堆
  空闲；任何残留分配都会让该请求失败。`size=N` 只是"最后一个撞墙的分配"，不是泄漏
  的大小；先量 heap used/free，再核对失败 size 取整后的 order 是否接近 heap 总量。
- **修复**: 大 fixture 改为按 `BLOCK_SZ` 分块分配（`Vec<Vec<u8>>`），不再依赖单个
  大连续块；或把 heap 撑大到"失败 size 取整后仍有大量空闲"。
- **教训**: 排查 heap OOM 先算 `next_power_of_two(size)` 与 heap 总量的关系。若
  `size.round_up_to_pow2() >= heap`，这是 fixture 布局问题（即使 free 充足也必然失败），
  不是泄漏；若远小于 heap，才是渐进泄漏。
- **相关文件**: `os/src/kernel_tests/mem_block.rs`,
  `dependency/buddy_system_allocator/src/lib.rs`

## FAT32 512 扇区 vs 4096 设备块：mount 适配层与 FS 内部换算只能二选一

- **现象**: 修好 FAT32 fixture 分配后，`fs_fat_smp` ktest 在 BPB 读（divide-by-zero）、
  `open(O_CREAT)`（ENOSYS）、簇表校验（chain out of range）逐层失败；零盘回归的
  `net_udp`/`net_tcp_accept` 也因无 smoltcp 栈失败。
- **根因**: FAT32 的 `bitmap.rs`（cdc17728 之后）与 `FatPageCacheBackend` 都必须与
  BlockDevice 的 4096 单位编址一致：要么 mount 用 `BlockSizeAdapter` 适配 512 逻辑块
  且 FS 内部不换算，要么 FS 内部 `sector_to_parent` 且设备保持裸 4096。两处叠加换算会
  读到错误 FAT/数据扇区；`FatPageCacheBackend` 漏改是 cdc17728 只改 bitmap 的残留。
  另：`FatInode` 未实现 `create_with_attrs`/`set_metadata`，默认 `set_metadata` 返回
  ENOSYS 使 `open(O_CREAT)` 在 FAT 卷上整体失败。
- **修复**: 统一约定"FAT32 内部自行换算，设备保持 4096 裸编址"：`FatPageCacheBackend`
  加 `sector_to_parent`，`adapt_filesystem_device` 对 FAT32 不再包适配层，ktest 直接传
  裸设备，`verify_cluster_table` 自行换算；`FatInode` 实现 `create_with_attrs` 委托
  `create`（跳过 set_metadata）。
- **教训**: 挂载适配（`BlockSizeAdapter`）与 FS 内部块换算只允许存在一种。改一方时
  用 `grep -n "read_block"` 全 FS 目录核对是否所有访问路径同单位；测试失败逐层暴露时
  按"分配→BPB→create→数据校验"的顺序归因。
- **相关文件**: `os/src/fs/fat32/{bitmap.rs,fat_inode.rs}`,
  `os/src/fs/page_cache.rs`, `os/src/fs/mod.rs`, `os/src/drivers/block/partition.rs`

## syscall 分配 fd 后对用户缓冲区写失败必须回滚

- **现象**: `socketpair(AF_UNIX, SOCK_STREAM, 0, NULL)` 在 Linux 返回 EFAULT 且不泄漏
  fd；本项目原实现在 `sv` 写失败时直接返回 EFAULT，已分配的 fd1/fd2 永久残留 fd 表，
  每次非法调用泄漏 2 个 fd 及各自 socket 的 64 KiB ring buffer。
- **修复**: 在 fd_table 锁内 `drop_fd` 摘除两个 fd，锁外再 drop 返回的 `Arc<File>`
  （避免持锁隐式 drop 触发 socket 析构死锁）。
- **教训**: 任何"先分配 fd / 资源、后写用户缓冲区"的 syscall，写失败都必须回滚已分配
  资源。分配两个及以上 fd 时，第二个 `alloc_fd` 失败也要回滚第一个。
- **相关文件**: `os/src/net/syscall/socketpair.rs`

## 零盘回归缺少 net 初始化导致 net 用例失败/panic

- **现象**: `make test ARCH=rv64 PROFILE=regression`（零盘）在 `net_tcp_accept` 失败、
  `net_udp` 于 `add_routed_socket().unwrap()` panic；同一套 net 用例在 normal/ktest 正常。
- **根因**: Regression 模式跳过 `net::config::init()` 与 net-poll worker。无 smoltcp 栈时
  TCP/UDP socket 创建返回 None；且关闭 socket 的 128 KiB smoltcp 缓冲只有 worker 的
  `drain_pending_socket_removals` 会回收，worker 缺失导致 socket 渐进泄漏。
- **修复**: 零盘回归也调用 `net::config::init()`（无 NIC 时退化为 loopback + null eth，
  不触发 DHCP）并常驻 net-poll worker。
- **教训**: "零盘"不等于"零网络"。loopback TCP/UDP 不需要外部 NIC；跳过 net init 的
  profile 若套件含 net 用例，会得到难定位的失败/panic。
- **相关文件**: `os/src/main.rs`

## PageCache backend 反向强引用会让 dirty retention 自循环

- **现象**: 全部 ktest 用例通过后，收尾 `flush_all_page_caches()` 对同一 another_ext4 inode
  反复返回 `EINVAL`；跨架构一致，且错误只在先后运行独立文件系统 fixture 后出现。
- **根因**: filesystem 强持有 inode lifetime，lifetime 为 dirty 数据强持有 PageCache，而
  PageCache backend 又强持有 filesystem/lifetime，形成 `fs → lifetime → cache → backend → fs`
  环。fixture 的本地 `Arc<Ext4FileSystem>` 已离开作用域，但已删除 inode 的 cache 仍在全局
  PageCache registry 中，于最终 flush 命中失效 inode。
- **修复**: backend 对 filesystem 与 lifetime 只保存 `Weak`，每次 read/write 临界 I/O
  前升级为短生命周期 `Arc`；owner 已销毁时返回 `EIO`，但正常 dirty retention 不再能自行延长
  已卸载 fixture 的寿命。
- **教训**: PageCache 的 dirty-retention owner 可以强持有 cache，但 cache/backend 不能反向
  强持有该 owner 或其 filesystem。审计生命周期时必须画出 owner→cache→backend 的完整图，而
  不能只检查全局 registry 为 Weak。
- **相关文件**: `os/src/fs/ext4_another/{page_cache.rs,lifetime.rs,fs.rs}`

## LoongArch IDLE 的开中断窗口必须与 trap 返回 PC 协同

- **危险模式**：把 RV64 的 IRQ-off `wfi` 协议直接翻译为 IRQ-off `idle 0`。LoongArch
  只有在 `CRMD.IE=1` 时才接受普通 timer/IPI；若先开 IE、再从 Rust 执行 `idle 0`，中断还
  可能恰好落在两者之间，handler 消费唯一 wake event 后返回到 `idle 0`，形成丢失唤醒。
- **固定协议**：把“设置 `CRMD.IE` → `idle 0`”放进有明确 start/exit label 的对齐汇编区域；
  kernel timer/IPI trap 若发现保存 PC 位于该半开区间，返回前改写为 exit。HAL 返回 Rust
  调度器前再次关 IE，从而保持 IRQ-off 的队列重查和锁序契约。
- **测试陷阱**：远程 wake 测试不能只等 target 变成 `Blocked`。sender 可能在 CPU0 清除
  current 后、真正进入 wait 前入队，任务随后被直接取走，测试虽绿却没有覆盖 WFI/IDLE。
  应先记录 idle-wait generation/计数，再等待其增长后发送 RESCHEDULE，并验证 helper 只
  运行一次、IPI 请求被消费且任务 owner/队列计数恢复基线。
- **相关文件**：`os/src/hal/arch/loongarch64/{mod.rs,trap/trap.S,trap/mod.rs}`、
  `os/src/task/processor.rs`、`os/src/kernel_tests/smp.rs`

## 昂贵准备依赖可变候选时，先 claim 所有权再执行准备

- **危险模式**：先在 owner 容器中克隆候选，释放锁执行 TLB 同步、DMA 映射或校验，再重新
  取得 owner 锁确认候选仍在。高竞争下准备结果经常失效，不仅浪费昂贵操作，还会把“竞争
  消失”伪装成后端性能问题。
- **固定协议**：在原 owner 锁内先把对象切换到显式的无容器中间态并摘除，由调用方强引用
  唯一持有；释放 owner 锁后执行只影响本地、失败可 fail-stop 的准备，最后提交到新 owner。
  中间态期间不得释放最后一个强引用、获取可能反向等待原 owner 的全局锁，或执行需要复杂
  回滚的远端协议。
- **计数验收**：候选计数放在 claim 成功之后，并分别记录“无远端工作”“无合格候选”和
  “实际昂贵调用”。正常成功路径应满足 `claimed == prepared == committed`；旧的二次复核失败
  应恒为零。先用 focused 高竞争测试证明对象恰好提交一次，再用长测验证昂贵调用率下降。
- **适用范围**：runqueue steal、跨 owner DMA handoff、异步资源迁移，以及任何
  “弱候选快照→昂贵准备→二次复核”的路径。
- **相关文件**：`os/src/task/run_queue.rs`、`os/src/task/perf.rs`、
  `os/src/kernel_tests/smp.rs`

## 构建网络：cargo libgit2 的 git 依赖代理机制（2026-08-10 实测）

- **根因**：cargo 拉取 git 依赖（如 `os/Cargo.toml` 的 `fdt = { git = ... }`）用内置
  libgit2，**不读取 `GIT_CONFIG_COUNT/GIT_CONFIG_KEY_n/GIT_CONFIG_VALUE_n` 环境变量**；
  只读取 gitconfig **文件**（`~/.gitconfig`、`/etc/gitconfig`）中的 `url.<base>.insteadOf`。
  用环境变量方式配置代理对 cargo 无效（实测：屏蔽 github.com 后 cargo fetch 仍直连失败）。
- **修复**：把 insteadOf 写入 `~/.gitconfig`（先 `mkdir -p $HOME`，fresh HOME 下目录可能不存在，
  `git config --global` 会报 "could not lock config file ... No such file or directory"）。
  git CLI（submodule 等）则两者皆可，用 `git -c "url.<proxy>.insteadOf=..."` 最干净。
- **教训**：验证代理是否真正生效，用 `/etc/hosts` 屏蔽目标域名做对照组，不要依赖
  "fetch 成功"（github 直连时段性可用时会假阳性）。cargo 的 git db 缓存可能部分成功但
  revision 缺失（`revision xxx not found`），预热后必须 `git cat-file -t <rev>` 验证。
- **国内 GitHub 代理 2026-08 实测（容器内 git 协议）**：ghproxy.net 3-4s 稳定可用、
  gh-proxy.com 可用但 12-34s 波动；gitclone.com 502、ghfast.top 403、gh-proxy.com(curl) 403、
  moeyy/ghps/mirror.ghproxy TLS 阻断或超时。rustup 镜像：rsproxy.cn 容器内 ~5MB/s，
  USTC（mirrors.ustc.edu.cn/rust-static）容器内被限速至 ~160B/s（宿主机直连却 2MB/s）。
- **相关文件**：`scripts/rustup-setup.sh`、`Makefile`（prepare-cargo-config）、
  `os/make/common/orchestration.mk`
## 文件后备的懒加载 VMA 必须优先保留 resident 页状态和后备生命周期

- **危险模式**：看到 `VmAreaKind::ElfLazy` 且 PTE 不存在就无条件从文件重建页。PTE 可能因 TLB/`mprotect`、压缩或换出被撤销，但 VMA 内已经保存了进程修改后的私有 frame；重读文件会丢数据。
- **固定协议**：在无 PTE 分支中先查 `VmPageState`：`InMemory -> ResidentWithoutPte`、`Compressed -> Decompress`、`SwappedOut -> SwapIn`，只有 `Unallocated` 才启动文件后备装页。目标 frame 必须先填充再发布 PTE；冷源页以 Retry 移到 VM 锁外读取。
- **生命周期**：后备对象不只要保存 PageCache 配方，还要强持有源 inode 并保持 ETXTBSY。该 `Arc` 必须在 VMA clone/split/fork 中保留；动态解释器有独立 inode，不能只依赖 PCB 的主 `exe` 引用。
- **验收**：构造 ELF 地址空间后 entry/BSS 应无 target frame；分别 fault entry 和不同页 BSS，验证文件字节、零填充和“未触发页仍未 resident”。
- **相关文件**：`os/src/mm/{address_space,filemap,page_fault,vma}.rs`、`os/src/task/process.rs`、`os/src/kernel_tests/mm.rs`

## 状态修复成功路径不能广播异常事件

- **危险模式**：trap/syscall 先尝试修复状态，随后不看结果就统一通知 signal、poll 或其他
  waiter。高频成功路径会为每次 lazy fault、重试或缓存填充支付无关锁和 listener 扫描，
  还把“状态已修复”错误表达成“异常事件已产生”。
- **固定协议**：把结果明确拆成“修复成功”“实际入队事件”“只发布其他状态”。只有事件
  真正入队后才通知相应 wait queue，并先释放 pending owner 锁；仅设置 OOM/退出等其他
  状态时，走其自己的安全点或唤醒协议。
- **验收**：先用计数证明成功次数远高于错误次数；再覆盖成功 hot path、真实错误信号和
  非信号状态三类。错误测试必须证明 waiter 仍被唤醒，成功测试则确认没有虚假通知。
- **相关文件**：`os/src/hal/arch/riscv/trap/mod.rs`、`os/src/task/process.rs`
