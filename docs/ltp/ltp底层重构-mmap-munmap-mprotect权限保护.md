# mmap/munmap/mprotect Panic/UB 路径重构与回归测试报告

## 1. 背景

`LTP_BOTTOM_UP_GUIDE.md` 第三点指向 `mmap`、`munmap`、`mprotect` 相关的底层 VM 稳定性问题。旧实现中，用户态传入非法参数或构造跨 VMA、空洞、file-backed partial split 等边界场景时，内核存在多条用户可触达的 `panic!`、`unwrap()`、`unwrap_unchecked()` 或 VM 状态污染路径。

这类问题的核心不是某个 LTP case 是否完整通过，而是系统调用边界必须先满足 Linux 风格的基本约束：

- 用户态非法参数只能返回稳定 errno。
- 用户态不能触发内核 panic 或 Rust UB。
- `mmap/munmap/mprotect` 失败不能留下半更新的 VMA 状态。
- lazy/COW/file-backed 映射的特殊路径不能因为 split 或页表更新触发崩溃。

本轮修复目标与 `user-copy` 权限保护重构保持一致：先把用户态可达的不安全路径收束成明确 errno，再在这个基础上继续补齐更完整的 Linux 兼容语义。

## 2. 旧实现的问题根因

### 2.1 syscall 入口参数未显式校验

旧 `sys_mmap` 直接依赖 bitflags 解析：

- `MapPermission::from_bits(...)` 后续 `unwrap()`。
- `MapFlags::from_bits(...)` 后续 `unwrap()`。
- 未知 `prot` bit、未知 `flags` bit 可能走到 panic。
- `MAP_SHARED/MAP_PRIVATE` 类型组合没有统一按 Linux 语义返回 `EINVAL`。

同时，旧实现把 `mmap len=0` 向上取整成一页，这与 Linux 行为不一致。Linux 下 `mmap(len=0)` 应返回 `EINVAL`。

### 2.2 地址区间加法可能溢出

旧代码多处直接计算：

```rust
start + len
start_vpn + count
```

用户态可以传入接近 `usize::MAX` 的地址和长度。如果加法溢出，后续 range 判断、VMA 查找、页表操作都会建立在错误区间上。

本轮将用户可控的 `start + len` 统一改成 `checked_add`，溢出返回 `EINVAL`。

### 2.3 `MAP_FIXED` 删除旧映射时存在 `unwrap_unchecked`

旧 `MemorySet::mmap` 在处理 `MAP_FIXED` 时，会先尝试 `munmap(start, len)` 删除重叠区域，然后使用：

```rust
unsafe { self.munmap(start, len).unwrap_unchecked() };
```

这条路径的问题很直接：

- Linux 语义允许 `MAP_FIXED` 覆盖空洞区域。
- 旧 `munmap` 遇到未映射空洞可能返回错误。
- 错误被 `unwrap_unchecked()` 当作不可能分支处理，形成用户可触达 UB。

修复后，`MAP_FIXED` 使用内部 range unmap helper，只删除实际重叠 VMA，空洞按成功处理。

### 2.4 `munmap/mprotect` split 假设过强

旧 `munmap/mprotect` 主要面向“目标区间完全落在单个 VMA 内”的情况，内部调用 `into_two/into_three` 时直接 `unwrap()`。这会在以下场景出错：

- 删除 VMA 头部。
- 删除 VMA 尾部。
- 删除 VMA 中间页。
- 删除区间跨多个 VMA。
- 删除区间中间包含空洞。
- `mprotect` 区间跨多个 VMA。

这些都是用户态可以直接构造的合法或半合法系统调用输入，不能让 split 失败冒泡成 panic。

### 2.5 file-backed 映射存在用户可达 panic

file-backed VMA split 需要维护文件偏移。旧代码中存在：

- `lseek(...).unwrap()`。
- file-backed partial `munmap/mprotect` split 时，偏移计算失败可能 panic。
- 页错误路径中 `find_map_area(...).unwrap()` 在并发或 stale PTE 场景下不够稳。

修复后，file-backed split 仍沿用 `deep_clone + offset`，但所有可失败文件操作都转成错误返回。

### 2.6 `PROT_WRITE` 页表权限补充问题

定向测试中还暴露出一个隐藏问题：`mprotect(PROT_WRITE)` 后如果只设置 `W|U`，在部分架构页表语义下这是非法或不可用组合，会导致用户态写访问持续 fault，进而让测试卡住。

本轮在 `prot` 解析阶段将 `PROT_WRITE` 映射成 `R|W|U`。这与常见硬件约束一致，也符合 Linux 上很多架构对 write-only 用户页的实际行为：写权限通常隐含可读硬件权限。

## 3. 重构目标

本轮目标是修复 no-panic/no-UB 和基础 errno 语义，不顺手扩大到完整 VM 功能重写。

明确目标：

- 非法 `prot/flags` 返回 `EINVAL`。
- `MAP_SHARED/MAP_PRIVATE` 类型组合非法返回 `EINVAL`。
- `mmap len=0` 返回 `EINVAL`。
- `munmap len=0` 返回 `EINVAL`。
- `mprotect len=0` 作为空区间成功返回。
- 用户区间加法溢出返回 `EINVAL`。
- `MAP_FIXED` 覆盖空洞或已有 VMA 都不 panic。
- `MAP_FIXED_NOREPLACE` 遇到重叠返回 `EEXIST`。
- `munmap` 支持跨多个 VMA 的部分删除、完整删除和空洞跳过。
- `mprotect` 支持跨多个连续 VMA；遇到未映射洞返回 `ENOMEM`。
- file-backed partial split 不 panic。
- 保留 lazy/COW 现有保护，不为了改权限强制分配 lazy 页。

非目标：

- 不实现完整 `mremap`。
- 不重写 OOM、swap、复杂 dirty/writeback 语义。
- 不承诺所有 mmap/mprotect/munmap LTP case 语义完全通过。
- 不向主工作区新增用户态测试源码。

## 4. 核心设计

### 4.1 syscall 入口显式解析

新增 `parse_mmap_prot` 和 `parse_mmap_flags`，将用户态整数先转成内核内部类型。

`prot` 规则：

| 用户输入 | 行为 |
| --- | --- |
| `PROT_NONE` | 只保留用户态 `U` 权限 |
| `PROT_READ` | `R|U` |
| `PROT_WRITE` | `R|W|U` |
| `PROT_EXEC` | `X|U` |
| 未知 bit | `EINVAL` |

`flags` 规则：

| 用户输入 | 行为 |
| --- | --- |
| `MAP_PRIVATE` | 接受 |
| `MAP_SHARED` | 接受 |
| `MAP_SHARED_VALIDATE` | 按 shared-like 类型接受 |
| private/shared 类型缺失 | `EINVAL` |
| private/shared 类型组合冲突 | `EINVAL` |
| 未知 bit | `EINVAL` |

这样 syscall 层不再把非法 bitflags 带进 VM 内部逻辑。

### 4.2 用户 range 统一检查

在 `MemorySet` 内部新增统一 range 检查：

- 页对齐检查。
- `checked_add` 溢出检查。
- 空区间处理。
- 用户 mmap 上界检查。

这让 `mmap`、`munmap`、`mprotect` 对地址区间的判断来源一致，避免每个 syscall 各自复制一套容易漏边界的逻辑。

### 4.3 `MAP_FIXED` 和 `MAP_FIXED_NOREPLACE`

`MAP_FIXED` 新语义：

1. 校验目标地址和长度。
2. 删除目标区间内所有实际重叠 VMA。
3. 空洞不报错。
4. 在指定地址插入新 VMA。

这里保留了一条中文注释：

```rust
// MAP_FIXED 覆盖空洞是成功路径，不能把未映射区间当成错误。
```

这条注释是为了防止后续维护时把空洞当错误重新引入 `unwrap_unchecked` 类风险。

`MAP_FIXED_NOREPLACE` 新语义：

1. 先检查目标区间是否与任何 VMA 重叠。
2. 有重叠返回 `EEXIST`。
3. 无重叠则按 fixed 地址插入。

非 fixed `mmap` 仍保留现有向上分配策略，但插入前补充用户 mmap 上界和 VMA overlap 检查。

### 4.4 VMA split helper

新增内部 split helper，用来把“目标区间与一个 VMA 的交集”切成最多三段：

- 左侧保留段。
- 中间目标段。
- 右侧保留段。

`munmap` 使用它删除中间目标段，保留左右两段。

`mprotect` 使用它更新中间目标段权限，保留左右两段。

这里也保留了一条中文注释：

```rust
// 跨 VMA 操作先切出目标段，避免 split 失败变成 panic。
```

这条逻辑是本轮 no-panic 修复的关键点：split 失败只能转 errno，不能再 `unwrap()`。

### 4.5 `munmap` 跨 VMA 删除

新 `munmap` 允许目标区间覆盖多个 VMA，并支持中间存在空洞。

处理方式：

1. 扫描所有与目标区间重叠的 VMA。
2. 对每个重叠 VMA 计算交集。
3. 从页表中 unmap 已实际映射的页。
4. 未触发 lazy 分配的页不强行 unmap 成错误。
5. 删除原 VMA。
6. 插回左右保留段。

用户态 `munmap` 对空洞区间的处理与 Linux 一致：只要参数本身合法，未映射页不构成错误。

### 4.6 `mprotect` 跨 VMA 改权限

新 `mprotect` 支持目标区间跨多个 VMA，但要求目标区间内不能出现未映射洞。

处理方式：

1. 校验 range。
2. 确认目标页范围被 VMA 连续覆盖。
3. 遇到洞返回 `ENOMEM`。
4. 对每个覆盖 VMA split 出目标段。
5. 更新目标段权限。
6. 对已存在 PTE 更新 flags。
7. 对未分配 lazy 页不强行分配。
8. stale PTE 继续按现有路径清理。

保留中文注释：

```rust
// lazy 页还没分配物理页，只改 VMA 权限，不强行建 PTE。
```

这避免把 `mprotect` 变成隐式 fault-in 操作。

### 4.7 file-backed partial split

file-backed VMA 的 partial split 仍使用原有思路：

- `deep_clone` 文件对象。
- 根据目标页偏移修正新 VMA 的 file offset。
- split 后左右段各自保留正确偏移。

变化点是错误处理：

- `lseek` 失败返回错误。
- offset 加法溢出返回错误。
- split 失败返回 errno。

这样 file-backed 映射的复杂语义还没有完全补齐，但用户态不能再通过 partial `munmap/mprotect` 把内核打崩。

## 5. 关键代码迁移

### 5.1 syscall/process.rs

| 位置 | 改动 |
| --- | --- |
| `parse_mmap_prot` | 新增 `prot` 显式解析，未知 bit 返回 `EINVAL` |
| `parse_mmap_flags` | 新增 `flags` 显式解析，非法类型组合返回 `EINVAL` |
| `sys_mmap` | `len==0` 返回 `EINVAL`，解析失败不进入 VM |
| `sys_mprotect` | `len==0` 成功返回，解析失败返回 `EINVAL` |

### 5.2 mm/memory_set.rs

| 位置 | 改动 |
| --- | --- |
| `checked_user_range` | 统一地址区间校验和溢出检查 |
| `split_area_for_range` | 将 VMA 与目标区间交集安全切分 |
| `unmap_range` | 内部 range unmap helper，空洞成功 |
| `protect_area` | mprotect 单 VMA 目标段改权限 |
| `mmap` | 重写 fixed/noreplace/普通 mmap 插入逻辑 |
| `munmap` | 支持跨多个 VMA 和空洞 |
| `mprotect` | 支持跨连续 VMA，洞返回 `ENOMEM` |

### 5.3 mm/map_area.rs

| 位置 | 改动 |
| --- | --- |
| file-backed split | 去除 `lseek(...).unwrap()` |
| `into_two` | file offset 失败转错误 |
| `into_three` 调用链 | 不再要求调用方 `unwrap()` |

## 6. 新旧代码行为对比

| 场景 | 旧行为 | 新行为 |
| --- | --- | --- |
| `mmap` 未知 `prot` bit | 可能 panic | `-EINVAL` |
| `mmap` 未知 `flags` bit | 可能 panic | `-EINVAL` |
| `mmap len=0` | 可能映射一页 | `-EINVAL` |
| `munmap len=0` | 行为不稳定 | `-EINVAL` |
| `mprotect len=0` | 行为不稳定 | 成功返回 0 |
| `start + len` 溢出 | range 判断污染 | `-EINVAL` |
| `MAP_FIXED` 覆盖空洞 | 可能 UB | 成功 |
| `MAP_FIXED` 覆盖已有 VMA 中间页 | 可能 panic/状态污染 | 两侧 VMA 保留 |
| `MAP_FIXED_NOREPLACE` 重叠 | 行为不完整 | `-EEXIST` |
| `munmap` 跨多个 VMA | 可能 panic | 稳定删除重叠段 |
| `munmap` 中间空洞 | 可能错误或 panic | 空洞跳过 |
| `mprotect` 跨多个连续 VMA | 可能 panic | 稳定改权限 |
| `mprotect` 遇到洞 | 可能 panic/状态污染 | `-ENOMEM` |
| file-backed partial split | 可能 panic | 稳定返回或成功 |
| `PROT_WRITE` only | 可能持续 fault | 映射为 `R|W|U` |

## 7. 测试覆盖说明

### 7.1 编译验证

双架构编译均在 Docker 环境内执行：

```bash
make -C os rv64-kernel-build-only MODE=release LOG=off
make -C os la64-kernel-build-only MODE=release LOG=off BLK_MODE=virt_pci
```

结果：

| 架构 | 结果 |
| --- | --- |
| rv64 | 通过 |
| la64 | 通过 |

说明：la64 构建会切换生成文件，最后重新执行 rv64 build，使架构相关生成文件回到 rv64 状态。

### 7.2 临时用户态单元测试

用户态单元测试没有写入主工作区。

临时资产：

| 类型 | 路径 |
| --- | --- |
| 临时仓库副本 | `/tmp/mangocore-vm-regress/repo` |
| 临时测试源码 | `/tmp/mangocore-vm-regress/repo/user/src/bin/vm_mmap_regress.rs` |
| rv64 临时镜像 | `/tmp/mangocore-vm-regress/sdcard-rv-vm.img` |
| la64 临时镜像 | `/tmp/mangocore-vm-regress/sdcard-la-vm.img` |
| 临时配置 | 只写入临时镜像内的 `/os_test.conf` |

测试程序特点：

- 直接在临时源码里写 6 参数 syscall wrapper。
- 不修改主工作区 `user/src/syscall.rs`。
- basic 脚本只运行 `/vm_mmap_regress`。
- musl/glibc 都期望打印 `VM_MMAP_REGRESS PASS` 且 exit code 为 0。

覆盖用例：

| 用例 | 期望 |
| --- | --- |
| 非法 `prot` | `-EINVAL` |
| 非法 `flags` | `-EINVAL` |
| `mmap len=0` | `-EINVAL` |
| `munmap len=0` | `-EINVAL` |
| 地址溢出 | `-EINVAL` |
| `MAP_FIXED` 映射空洞 | 成功 |
| `MAP_FIXED` 覆盖已有 VMA 中间页 | 中间页替换，两侧页仍可访问 |
| `MAP_FIXED_NOREPLACE` 重叠 | `-EEXIST` |
| `MAP_FIXED_NOREPLACE` 空洞 | 成功 |
| `mprotect` 头部 split | 权限更新稳定 |
| `mprotect` 尾部 split | 权限更新稳定 |
| `mprotect` 中间页 split | 权限更新稳定 |
| `munmap` 头部删除 | 不 panic |
| `munmap` 尾部删除 | 不 panic |
| `munmap` 中间洞删除 | 不 panic |
| `munmap` 跨多个 VMA | 不 panic |
| file-backed private partial `mprotect` | 稳定返回，不 panic |
| file-backed private partial `munmap` | 稳定返回，不 panic |

结果：

| 架构 | libc | 结果 |
| --- | --- | --- |
| rv64 | musl | `VM_MMAP_REGRESS PASS` |
| rv64 | glibc | `VM_MMAP_REGRESS PASS` |
| la64 | musl | `VM_MMAP_REGRESS PASS` |
| la64 | glibc | `VM_MMAP_REGRESS PASS` |

### 7.3 LTP 定向回归

LTP 使用临时配置 `/tmp/os_test_vm.conf`，不修改主工作区 `os_test.conf`。

配置要点：

```text
mask=0x800
ltp_runner=inline
ltp_libc=both
include=mmap05,mmap08,mmap09,mmap19,mprotect01,mprotect02,mprotect03,mprotect04,munmap01,munmap02,msync02
```

执行命令：

```bash
make -C os conf-inject CONF_ARCH=rv64 CONF_BLK_MODE=virt CONF_FILE=/tmp/os_test_vm.conf
make -C os rv64-run
make -C os conf-inject CONF_ARCH=la64 CONF_BLK_MODE=virt_pci CONF_FILE=/tmp/os_test_vm.conf
make -C os la64-run
```

接受标准：

- 无 kernel panic。
- 无 Rust UB 路径触发。
- 无 QEMU 外层 timeout。
- 已支持 case exit code 为 0。
- 暂未完整支持的 case 可以稳定表现为 LTP fail 或 errno，但不能崩内核。

结果摘要：

| 架构 | 结果 |
| --- | --- |
| rv64 | LTP 定向集合跑完，无 panic/Unexpected/timeout；目标 case 稳定 fail/broken 或 glibc 127 |
| la64 | LTP 定向集合跑完，无 panic/Unexpected/timeout；目标 case 稳定 fail |

rv64 观察到的目标 case 状态：

```text
mmap05:2
mmap08:1
mmap09:2
mmap19:2
mprotect01:3
mprotect02:4
mprotect03:4
mprotect04:4
munmap01:4
munmap02:4
msync02:4
glibc target cases: 127
```

la64 观察到的目标 case 状态：

```text
target cases: 32512
```

这些状态说明 LTP 语义仍有未完成项，但本轮目标 no-panic/no-UB 已满足。

## 8. 临时测试执行方式

临时用户态测试的关键约束是“不污染主工作区”。

执行方式：

1. 复制当前工作区到 `/tmp/mangocore-vm-regress/repo`。
2. 只在临时副本新增 `user/src/bin/vm_mmap_regress.rs`。
3. 在临时副本内构建测试 ELF。
4. 复制 `sdcard-rv.img` 和 `sdcard-la.img` 到 `/tmp/mangocore-vm-regress/`。
5. 只向临时镜像写入测试 ELF、`/os_test.conf` 和 basic 脚本。
6. 分别运行 rv64/la64 QEMU。

这样主工作区不会新增用户态测试源码，也不会把临时测试脚本写入主镜像。

## 9. `PROT_WRITE` 隐含读权限补丁补充

在 no-panic 修复完成后，临时回归发现 `mprotect(PROT_WRITE)` 场景仍可能卡住。根因是部分架构页表不接受或无法正常执行 write-only 用户页。

补丁策略：

- 用户态仍允许传入 `PROT_WRITE`。
- 内核内部 PTE 权限设置为 `R|W|U`。
- 注释写在解析处，说明这是硬件页表约束，不是任意放宽。

这不是完整的 Linux VMA 可见权限模型，但能避免用户态构造 write-only 映射后把内核拖入重复 fault 路径。

## 10. 风险边界与后续方向

本轮已经完成的是 syscall 边界稳定性和 VMA 基础 split 能力。

仍未完整解决的方向：

- file-backed `MAP_SHARED` dirty/writeback 语义。
- `msync` 完整行为。
- 更复杂的 MAP flag 组合。
- OOM 后 VM 状态回滚。
- swap/zram 参与时的页表权限同步。
- LTP 中依赖 tmpfs、权限位、文件系统细节的 mmap case。

这些属于后续 Linux 兼容性增强，不影响本轮 no-panic/no-UB 修复结论。

## 11. 涉及文件

| 文件 | 说明 |
| --- | --- |
| `os/src/syscall/process.rs` | syscall 层 mmap/mprotect 参数解析与 errno 返回 |
| `os/src/mm/memory_set.rs` | mmap/munmap/mprotect 核心 VMA 操作重构 |
| `os/src/mm/map_area.rs` | file-backed VMA split 错误处理 |

主工作区未新增用户态测试源码。用户态回归测试仅存在于 `/tmp/mangocore-vm-regress` 临时副本和临时镜像中。
