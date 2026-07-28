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

### LA64 首次用户态恢复跳入 kernel trap stub

- **现象**: competition 启动在 PID1 已入 ready queue、`trap_return()` 已执行后静默空转，始终没有 `[initd]` 首行。
- **根因**: `restore_va` 用 `strampoline` 对 `__restore` 做重定位。LA64 static link 中该 extern 函数符号可解析为 `skern_trap`，使 restore 跳入错误的 kernel trap 区域，而不是 `.text.trampoline` 中的 `__restore`。
- **修复**: LA64 `trap_return()` 直接以链接后的 `__restore as usize` 作为跳转目标；不要通过 `strampoline` 重新计算该地址。
- **教训**: bare-metal assembly entry symbols经 Rust FFI 取地址时，必须核对最终 ELF 符号和反汇编。若首个用户任务已被 scheduler 选中却没有用户输出，优先比较计算出的跳转地址与 `llvm-nm`/`llvm-objdump` 中的 `__restore`。
- **相关文件**: `os/src/hal/arch/loongarch64/trap/mod.rs`, `os/src/hal/arch/loongarch64/trap/trap.S`

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
