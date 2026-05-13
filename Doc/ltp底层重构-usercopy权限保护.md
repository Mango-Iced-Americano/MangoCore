# User-Copy 权限检查重构与回归测试报告

## 1. 背景

本次重构针对 MangoCore 内核 user-copy 路径的权限语义问题。原实现中，很多用户指针访问最终都会走到 `translated_byte_buffer`、`translated_refmut`、`copy_to_user` 等 helper。这些 helper 的核心行为是把用户虚拟地址翻译成物理地址，然后通过内核可访问的物理页切片或引用完成读写。

问题在于：旧实现只关注“地址是否能翻译到物理页”，没有按 syscall 的真实方向检查 PTE 上的用户权限位和读写权限位。因此，类似 `mprotect(PROT_READ)` 后把该页作为 `read(fd, buf, len)` 输出 buffer 的情况，本应返回 `-EFAULT`，旧内核却可能直接写入这个只读页。

这个问题不会自然进入硬件 trap。因为内核不是用用户态 store 指令访问用户虚拟地址，而是先翻译用户 VA，再拿到物理页对应的内核映射并写物理内存。硬件不会在这个路径上重新检查用户 PTE 的 `W` 权限，所以必须在 user-copy 翻译层显式检查权限。

## 2. 旧实现的问题根因

旧版 `translated_byte_buffer` 的行为可以概括为：

```rust
let page_table = PageTableImpl::from_token(token);
let ppn = match page_table.translate(vpn) {
    Some(pte) => pte,
    None => check_page_fault(vpn.into())?.floor(),
};
v.push(&mut ppn.get_bytes_array()[...]);
```

这里的问题有三个：

1. `page_table.translate(vpn)` 只说明页表项存在并能得到物理页，不能说明该页允许当前 syscall 的访问方向
2. `check_page_fault` 没有区分 `Load/Store/Execute`，lazy/COW 路径无法按真实访问类型处理
3. 返回的是 `&mut [u8]` 或 `&mut T`，后续 `read_user`/`copy_to_user` 可以直接写物理页，绕过用户 PTE 的 `W` 位

所以旧内核中：

```text
mprotect(page, PROT_READ)
read(pipe_fd, page, 1)
```

不会因为用户页只读而 trap。旧内核会直接把 pipe 里的字节写进 `page`，并且 pipe 数据已经被消费。回归测试随后再用栈 buffer 读取 pipe，期望确认数据仍在 pipe 中，此时 pipe 已空，于是阻塞到 initproc 超时。

## 3. 重构目标

本次重构的目标是把 user-copy 路径从“能翻译就能访问”改成“按真实语义显式授权后才能访问”：

1. 用户输入 buffer 只需要 `Read`
2. 用户输出 buffer 只需要 `Write`
3. 同一个对象需要先读后写时使用 `ReadWrite`
4. 不加入 `Executable` 到 `UserAccess`，取指缺页由 trap 路径的 `FaultAccess::Execute` 处理
5. 当前任务 token 可以触发 lazy/COW fault，非当前 token 默认不补 lazy/COW，避免 clone 写 child 地址空间时意外给目标地址空间建页或 COW
6. 保持现有 `UserBuffer` 抽象，不引入 `UserReadBuffer/UserWriteBuffer`

## 4. 核心设计

### 4.1 UserAccess

新增：

```rust
pub enum UserAccess {
    Read,
    Write,
    ReadWrite,
}
```

语义：

| 访问类型 | 语义 | 典型 syscall |
| --- | --- | --- |
| `Read` | 内核从用户地址读数据 | `write/send/sendmsg/setsockopt/pathname/iovec 数组/argv/envp` |
| `Write` | 内核向用户地址写数据 | `read/recv/recvmsg/getcwd/uname/stat/clock_gettime/socketpair/status/oldset/optval` |
| `ReadWrite` | 内核需要读旧值并可能写回 | `translated_refmut`、部分 offset/addrlen 这类输入输出参数 |

没有加入 `Executable`，因为 user-copy 不是取指路径。用户态执行权限由架构 trap 中的 instruction fault 处理。

### 4.2 FaultAccess

新增：

```rust
pub enum FaultAccess {
    Load,
    Store,
    Execute,
}
```

架构 trap 和 user-copy helper 都通过这个类型把缺页原因传给 VM：

| 来源 | FaultAccess |
| --- | --- |
| 用户读缺页 | `Load` |
| 用户写缺页 | `Store` |
| 用户取指缺页 | `Execute` |
| `UserAccess::Read` | `Load` |
| `UserAccess::Write` | `Store` |
| `UserAccess::ReadWrite` | 先 `Load`，再 `Store` |

### 4.3 范围检查

新增 `check_user_range(ptr, len)`：

1. `len == 0` 直接成功，不因为坏指针误报
2. 非零长度使用 `checked_add` 防止地址溢出
3. 检查 `[ptr, ptr + len)` 不超过 `USER_VA_END`

这修复了旧代码里 `start + len` 可能溢出的隐患，也让零长度 copy 的 Linux 语义更明确。

### 4.4 PTE 权限检查

`PageTable` trait 新增：

```rust
fn user_access_ok(&self, vpn: VirtPageNum, access: UserAccess) -> Option<bool>;
```

双架构实现：

| 架构 | 检查项 |
| --- | --- |
| RISC-V SV39 | PTE `V/U/R/W` |
| LoongArch64 LAFlex | PTE `V/PLV3/readable/W` |

读访问要求页有效、用户可访问、可读。写访问要求页有效、用户可访问、可写。`ReadWrite` 同时要求可读和可写。

### 4.5 translate_user_va_checked

新增统一入口：

```rust
translate_user_va_checked(token, va, access) -> Result<PhysAddr, isize>
```

职责边界：

1. 检查单地址范围
2. 如果未映射，按 `access` 触发当前任务的 lazy fault
3. 检查 PTE 用户权限和读写权限
4. 如果写权限不满足，允许当前任务按写访问再触发一次 COW/fault
5. 最终返回物理地址

关键点是：只有当前任务 token 允许补 lazy/COW。非当前 token 访问如果页不存在或权限不满足，直接 `EFAULT`。

### 4.6 translate_user_buffer_checked

新增：

```rust
translate_user_buffer_checked(token, ptr, len, access) -> Result<Vec<&'static mut [u8]>, isize>
```

它按页拆分用户 buffer，并且每一页都通过 `translate_user_va_checked` 检查真实访问方向。`translated_byte_buffer` 和 `translated_byte_buffer_append_to_existing_vec` 都增加了 `access: UserAccess` 参数，不再保留旧签名，强制所有调用点显式选择方向。

### 4.7 translated_ref / translated_refmut / translated_ref_write

本次整理后的语义：

| helper | 权限 | 用途 |
| --- | --- | --- |
| `translated_ref<T>` | `Read` | 用户对象只读 |
| `translated_refmut<T>` | `ReadWrite` | 需要读旧值并写回的用户对象 |
| `translated_ref_write<T>` | `Write` | 纯输出对象 |

`translated_ref<T>` 和 `translated_refmut<T>` 会检查完整 `T` 的范围。当前实现对跨页 `T` 直接返回 `EFAULT`，跨页批量数据使用 `translated_byte_buffer`/`copy_from_user`/`copy_to_user` 处理。

### 4.8 copy_to_user 改为纯写

旧版 `copy_to_user` 在单页对象上会走 `translated_refmut`。这会把纯输出误表达成“读写”。本次改为内部统一走 `translated_byte_buffer(..., UserAccess::Write)`，因此只要求用户页可写，不再要求可读。

同理：

1. `copy_to_user_array` 使用 `Write`
2. `copy_to_user_string` 使用 `Write`
3. `copy_from_user` / `copy_from_user_array` 使用 `Read`
4. 数组长度乘法使用 `checked_mul`

## 5. 关键调用点迁移

### 5.1 文件 I/O

| syscall | 方向 | 修改后权限 |
| --- | --- | --- |
| `read` | 内核写用户 buffer | `Write` |
| `pread` | 内核写用户 buffer | `Write` |
| `write` | 内核读用户 buffer | `Read` |
| `pwrite` | 内核读用户 buffer | `Read` |
| `readv` 的 iovec 数组 | 内核读 iovec 描述符 | `Read` |
| `readv` 的 iov_base | 内核写用户 buffer | `Write` |
| `writev` 的 iovec 数组 | 内核读 iovec 描述符 | `Read` |
| `writev` 的 iov_base | 内核读用户 buffer | `Read` |
| `getcwd` | 内核写路径字符串 | `Write` |
| `stat/fstat/statx/statfs` | 内核写结构体 | `Write` |
| `select/poll` 输出 fd 集合 | 内核写结果 | `Write` |

`readv/writev` 额外增加总长度校验，`iov_len` 累加超过 `isize::MAX` 返回 `EINVAL`。

### 5.2 进程与时间

| syscall/helper | 方向 | 修改后权限 |
| --- | --- | --- |
| `uname` | 内核写 `utsname` | `Write` |
| `clock_gettime` | 内核写 `timespec` | `Write` |
| `nanosleep` rem | 内核写剩余时间 | `Write` |
| `wait4` status | 内核写退出状态 | `Write` |
| `rt_sigpending` set | 内核写 signal set | `Write` |
| `getrlimit` old limit | 内核写 rlimit | `Write` |
| `get_robust_list` | 内核写 head/len | `Write` |
| `clone` parent tid | 当前地址空间纯写 | `Write` |
| `clone` child tid | 子地址空间纯写，但不补 lazy/COW | `Write` |

`CLONE_CHILD_SETTID` 是本次特别关注的路径。旧思路如果直接对 child token 调用会触发 lazy/COW，就可能在非当前地址空间写入本不该由当前内核路径创建或复制的页。修改后非当前 token 不补 fault，失败只记录 warn，不影响 clone 成功。

### 5.3 futex

`FUTEX_WAIT` 的用户 word 检查本质是读旧值，改为 `translated_ref`。`WAKE` 主要按地址/key 找等待队列，不应该为了“可变引用”要求写权限。只有真正需要读写同一 word 的操作才应使用 `ReadWrite`。

### 5.4 网络

| syscall | 方向 | 修改后权限 |
| --- | --- | --- |
| `send/sendto/sendmsg` 数据 buffer | 内核读用户数据 | `Read` |
| `recv/recvfrom/recvmsg` 数据 buffer | 内核写用户数据 | `Write` |
| `setsockopt` optval | 内核读用户 optval | `Read` |
| `getsockopt` optval | 内核写用户 optval | `Write` |
| `socketpair` sv | 内核写 fd 数组 | `Write` |
| `getsockname/getpeername` addr | 内核写地址 | `Write` |
| `addrlen` 这类输入输出参数 | 读旧长度并写回新长度 | `ReadWrite` 或按 helper 拆成读/写 |

## 6. 新旧代码行为对比

### 6.1 修改后内核

使用同一份临时用户态测试程序分别注入 rv64/la64 临时镜像，basic 组只运行 `/usercopy_access`。

修改后结果：

| 架构 | musl | glibc | 结论 |
| --- | --- | --- | --- |
| rv64 | `USERCOPY_ACCESS PASS`, `exit_code=0` | `USERCOPY_ACCESS PASS`, `exit_code=0` | 通过 |
| la64 | `USERCOPY_ACCESS PASS`, `exit_code=0` | `USERCOPY_ACCESS PASS`, `exit_code=0` | 通过 |

rv64 日志中两套 libc 都出现：

```text
ok basic_mprotect_read
ok lazy_ro_output
ok lazy_ro_input_zero
ok lazy_rw_output_success
ok cow_legal_write
ok cow_mprotect_before_fork
ok cow_mprotect_after_fork
ok shared_mprotect
ok cross_page
ok iov
ok output_syscalls_ro
ok zero_and_overflow
ok child_settid_lazy
ok child_settid_cow
ok final_marker
USERCOPY_ACCESS PASS
```

la64 结果与 rv64 一致。

### 6.2 原始 HEAD 旧内核

为了避免污染当前工作区，没有对主仓库执行 `git stash`。测试方式是把 `HEAD=9d88371` 导出到临时干净目录：

```text
/tmp/mangocore-usercopy-baseline.DfqVh7/repo
```

然后注入同一份 `/usercopy_access` 到临时镜像运行。

旧内核结果：

| 架构 | musl | glibc | 结论 |
| --- | --- | --- | --- |
| rv64 | 打印 `USERCOPY_ACCESS START` 后 60 秒超时，`exit_code=9` | 同样超时，`exit_code=9` | 失败 |
| la64 | 打印 `USERCOPY_ACCESS START` 后 60 秒超时，`exit_code=9` | 同样超时，`exit_code=9` | 失败 |

旧内核关键日志：

```text
USERCOPY_ACCESS START
[initproc] TIMEOUT (60s) for basic_testcode.sh in /musl, sending SIGKILL
[initproc] done basic_testcode.sh in /musl exit_code=9
USERCOPY_ACCESS START
[initproc] TIMEOUT (60s) for basic_testcode.sh in /glibc, sending SIGKILL
[initproc] done basic_testcode.sh in /glibc exit_code=9
```

### 6.3 为什么旧内核是超时而不是 not ok

第一个用例 `test_basic_mprotect_read` 的关键逻辑是：

```rust
let ret = sys_read(fds[0], page, 1);
let mut got = [0u8; 1];
let preserved = read_exact_stack(fds[0], &mut got) && got[0] == 88;
```

正确内核：

1. `page` 已被 `mprotect(PROT_READ)` 降成只读
2. `read(pipe, page, 1)` 发现输出 buffer 不可写
3. 返回 `-EFAULT`
4. pipe 中的 `X` 没被消费
5. 后续 `read_exact_stack` 能读到 `X`
6. 用例继续运行并打印 `ok basic_mprotect_read`

旧内核：

1. `page` 已被 `mprotect(PROT_READ)` 降成只读
2. `read(pipe, page, 1)` 没检查 `W` 权限
3. 内核直接通过物理页切片写穿只读页
4. pipe 中的 `X` 已被消费
5. 后续 `read_exact_stack` 在空 pipe 上阻塞
6. initproc 60 秒后杀掉测试进程

所以旧内核没有机会打印 `not ok basic_mprotect_read`，而是卡在第一个用例内部。

## 7. 测试覆盖说明

| 用例 | 覆盖点 | 期望 |
| --- | --- | --- |
| `basic_mprotect_read` | 普通只读页作为输出 buffer | `read` 返回 `-EFAULT`，pipe 数据不被消费；只读页作为 `write` 输入成功 |
| `lazy_ro_output` | lazy 页先降权再作为输出 | 返回 `-EFAULT`，不触发错误写入 |
| `lazy_ro_input_zero` | lazy 只读页作为输入 | `write` 成功读到零页内容 |
| `lazy_rw_output_success` | lazy 可写页作为输出 | 合法写 fault 成功分配并写入 |
| `cow_legal_write` | fork 后子进程合法写 COW 页 | 子进程看到新值，父进程保持旧值 |
| `cow_mprotect_before_fork` | 先 `mprotect(PROT_READ)` 再 fork | 子进程输出写返回 `-EFAULT` |
| `cow_mprotect_after_fork` | fork 后子进程再降权 | 子进程输出写返回 `-EFAULT` |
| `shared_mprotect` | `MAP_SHARED` 降权后输出写 | 返回 `-EFAULT`，不能用旧 map_perm 恢复 W |
| `cross_page` | 跨页输出和跨页输入 | 输出跨到只读页返回 `-EFAULT`，输入跨两个只读页成功 |
| `iov` | `readv/writev` 权限方向和溢出 | iovec 数组只读可读，输出 base 只读失败，输入只读成功，长度溢出 `-EINVAL` |
| `output_syscalls_ro` | `clock_gettime/uname` 纯输出 | 只读页返回 `-EFAULT` |
| `zero_and_overflow` | 零长度与地址溢出 | 零长度坏指针成功，非零溢出返回 `-EFAULT` |
| `child_settid_lazy` | 非当前 child token lazy 页 | 内核不补 lazy fault 写 child tid |
| `child_settid_cow` | 非当前 child token COW 页 | 内核不触发 COW 写 child tid |

## 8. 临时测试执行方式

测试遵循“不污染主仓库、不污染真实镜像”的原则：

1. 临时复制仓库到 `/tmp/mangocore-usercopy-regress/repo`
2. 只在临时副本新增 `user/src/bin/usercopy_access.rs`
3. 复制真实镜像为临时镜像：
   - `/tmp/mangocore-usercopy-regress/sdcard-rv-usercopy.img`
   - `/tmp/mangocore-usercopy-regress/sdcard-la-usercopy.img`
4. 只向临时镜像写入：
   - `/usercopy_access`
   - `/os_test.conf`
   - `/musl/basic_testcode.sh`
   - `/glibc/basic_testcode.sh`
5. QEMU 直接使用临时 kernel 和临时 image 启动

真实镜像在测试前后 checksum 不变：

```text
5910732e32bd0d13c9efa671a5adee70efba6dd423e95bc28fbcf9b59bcbe575  /app/sdcard-rv.img
173585b9966cd42a044d71f9ab4b3dde5826508ff77370bead8041d653f56bed  /app/sdcard-la.img
```

## 9. la64 COW/Lazy Panic 补丁补充

### 9.1 问题现象

在 user-copy 权限重构之后，双架构全量测试中发现 la64 `libcbench` 会触发一个新的内核 panic。关键日志如下：

```text
[mprotect] addr: 1FFFFFD000, len: 40000, prot: R | W | U
[mprotect] Some pages are not mapped, is it caused by lazy alloc?
[copy_on_write] mapped COW page has no resident frame: vpn=VPN(0x200003C), state=Unallocated, area=MapArea { interval: LinearMap { vpn_range: SimpleRange { l: VPN(0x1FFFFFD), r: VPN(0x200003D) }, active: 1, compressed: 0, swapped: 0 }, map_type: Framed, map_perm: R | W | U, map_file: "no" }
[kernel] panicked at 'internal error: entered unreachable code', src/mm/memory_set.rs:1385:14
```

这不是普通 user-copy 权限检查失败，而是 VM 内部不变量被破坏后，`check_page_fault` 又把这种状态当成“不可能”处理，最终升级成 kernel panic。

### 9.2 被破坏的不变量

对匿名 private `MapArea`，页表和 `MapArea.inner.frames` 应该满足以下关系：

| frame 状态 | 页表状态 | 后续 fault 行为 |
| --- | --- | --- |
| `Frame::Unallocated` | 不应有有效 leaf PTE | 走 lazy zero allocation |
| `Frame::InMemory` | 可以有有效 PTE | Store fault 可走 COW |
| `Compressed/SwappedOut` | PTE 应能配合恢复路径 | 走 decompress/swap-in |

出问题时的状态是：

```text
page_table.is_mapped(vpn) == true
area.inner.frames[vpn] == Frame::Unallocated
```

旧 `do_page_fault(Store)` 只看 `page_table.is_mapped(vpn)`。只要页表里有有效 PTE，它就认为这是“已经有 resident frame 的 private 页”，然后进入 `copy_on_write()`。但当前 `MapArea` 里没有 COW 源 frame，于是 `copy_on_write()` 返回 `MemoryError::NotMapped`，再被 `check_page_fault` 的 `_ => unreachable!()` 放大成 panic。

### 9.3 触发链路

这次 la64 更容易触发，与 la64 用户地址布局和 libcbench/glibc pthread 栈路径有关：

```text
la64:
USR_MMAP_BASE = 0x1c00000000
USR_MMAP_END  = 0x1fffffd000
TRAP_CONTEXT_BASE = 0x1fffffe000
trap_cx_bottom_from_tid(1) = 0x1fffffd000
```

旧日志里的异常区域是：

```text
mprotect addr=0x1fffffd000 len=0x40000
fault vpn=0x200003c
area range=[0x1fffffd, 0x200003d)
```

也就是说，异常区域从 la64 mmap 顶部附近开始，贴近甚至越过 trap context/signal trampoline 相邻区域。`libcbench` 的 glibc pthread 用例会反复走线程栈、`mprotect` 和 clone 相关路径，因此更容易把这个布局问题放大出来。

rv64 当前没有触发同样 panic，并不代表没有同类风险。rv64 `libcbench` 中对应 pthread/clone 参数主要落在 `0x6010xxxx/0x6014xxxx`，处于普通 `MMAP_BASE=0x60000000..0x80000000` 区间，没有走到 la64 高端 mmap/trap context 交界路径。

### 9.4 本次补丁内容

本次补丁主要是补强 VM 的防线，避免同类不一致状态继续进入 COW 和 kernel panic：

1. `check_page_fault`
   - `MemoryError::NotMapped` 不再进入 `unreachable!`
   - syscall/user-copy fault 路径统一返回 `-EFAULT`
   - 其他非预期 `MemoryError` 打 warn 后返回 `-EFAULT`

2. `MapArea` / `LinearMap`
   - 新增 `is_unallocated`
   - 新增 `frame_is_unallocated`
   - 新增 `clear_stale_pte`
   - 用于显式识别和清理 “lazy 元数据仍是 `Unallocated`，但页表残留有效 PTE” 的状态

3. `mprotect`
   - 对 `Unallocated` lazy 页不再强行修改 PTE flags
   - 如果发现 lazy 页残留有效 PTE，则清掉 stale PTE
   - 避免 `mprotect(PROT_WRITE)` 后把 lazy 页伪装成可 COW 的 resident 页

4. `do_page_fault(Store)`
   - private 匿名页如果出现 `PTE valid + Frame::Unallocated`
   - 先清掉 stale PTE
   - 对匿名页重新走 lazy zero allocation
   - 对 file-backed 页不粗暴补零，返回 `NotMapped`，因为文件映射需要按 offset、EOF 和 page cache 语义处理

这段逻辑的目的不是让所有异常状态都“静默成功”，而是把匿名 lazy 页恢复到可解释的状态：没有 resident frame 就不能走 COW，只能重新按 lazy 页分配。

### 9.5 补丁验证

补丁后重新验证了双架构构建：

```text
make -C /app/os rv64-kernel-build-only MODE=release LOG=off
make -C /app/os la64-kernel-build-only MODE=release LOG=off BLK_MODE=virt_pci
```

结果：

| 项目 | 结果 |
| --- | --- |
| rv64 release kernel build | 通过 |
| la64 release kernel build | 通过 |
| rv64 libcbench | `PASS=1 FAIL=0` |
| la64 libcbench | 不再出现 `copy_on_write ... Unallocated` 后接 kernel panic |
| rv64 user-copy 临时回归 | musl/glibc 两次 `USERCOPY_ACCESS PASS` |
| la64 user-copy 临时回归 | musl/glibc 两次 `USERCOPY_ACCESS PASS` |

la64 `libcbench` 仍被 `run_test` 判 fail，但失败点已经不是这次的内核 panic，而是既有的 musl 60 秒 timeout 和若干用户态异常日志。这个问题需要单独分析，不应和本次 COW/lazy panic 混在一起。

### 9.6 仍然存在的不足

本次补丁是必要的防御性修复，但还不是完整根治。当前内核对 “MapArea 元数据与页表 PTE 一致性” 的维护仍存在结构性漏洞：

1. la64 mmap 顶部和多线程 trap context 区域靠得太近
   - `USR_MMAP_END` 与 `trap_cx_bottom_from_tid(1)` 相邻
   - 多线程场景中 trap context 会向下分配，和高端 mmap 区域存在边界设计风险

2. `mmap` 缺少架构上界检查
   - 非 `MAP_FIXED` 合并已有匿名 private area 时，只检查单个 `MapArea` 不超过 1GB
   - 没有强制 `start_va + len <= USR_MMAP_END`

3. `mmap` / `insert_framed_area` 缺少统一 overlap 校验
   - 代码注释假设没有冲突
   - 新 `MapArea` 只按起始地址插入 `areas`
   - 没有证明新区间不覆盖 trap context、signal trampoline 或已有 `MapArea`

4. `mprotect` 假设目标 range 完全落在单个非重叠 `MapArea`
   - 它只找包含 `start_vpn` 的第一个 area
   - 如果此前已经存在重叠区域，就可能继续放大页表和元数据不一致

5. `do_page_fault` 旧逻辑过度信任 PTE
   - 以前把 `PTE valid` 等同于“当前 `MapArea` 拥有 resident frame”
   - 这在重叠区域或 stale PTE 场景下不成立

后续更彻底的修复方向应该是：

1. 明确 la64 用户 mmap、用户栈、trap context、signal trampoline 的边界和 guard gap
2. `mmap` 对所有非 `MAP_FIXED` 区域做上界检查和全局 overlap 检查
3. `MAP_FIXED` 的 `munmap + insert` 必须确认目标范围完整释放
4. `mprotect` 支持跨多个 `MapArea` 或在遇到跨区间时返回符合 Linux 语义的错误
5. 在 debug/log 模式下增加 VM invariant check，尽早发现 `Frame::Unallocated + valid PTE`

## 10. 完整测试代码

临时测试文件：`user/src/bin/usercopy_access.rs`

```rust
#![no_std]
#![no_main]

use core::arch::asm;
use core::ptr;
use user_lib::println;

const PAGE_SIZE: usize = 4096;
const PROT_READ: usize = 1;
const PROT_WRITE: usize = 2;
const MAP_SHARED: usize = 0x01;
const MAP_PRIVATE: usize = 0x02;
const MAP_ANONYMOUS: usize = 0x20;
const SIGCHLD: usize = 17;
const CLONE_CHILD_SETTID: usize = 0x0100_0000;
const CLOCK_REALTIME: usize = 0;

const EFAULT_RET: isize = -14;
const EINVAL_RET: isize = -22;

const SYS_CLOSE: usize = 57;
const SYS_PIPE2: usize = 59;
const SYS_READ: usize = 63;
const SYS_WRITE: usize = 64;
const SYS_READV: usize = 65;
const SYS_WRITEV: usize = 66;
const SYS_EXIT: usize = 93;
const SYS_CLOCK_GETTIME: usize = 113;
const SYS_CLONE: usize = 220;
const SYS_MMAP: usize = 222;
const SYS_MPROTECT: usize = 226;
const SYS_WAIT4: usize = 260;
const SYS_UNAME: usize = 160;

#[repr(C)]
#[derive(Clone, Copy)]
struct IOVec {
    iov_base: *const u8,
    iov_len: usize,
}

#[cfg(target_arch = "riscv64")]
fn syscall6(id: usize, args: [usize; 6]) -> isize {
    let mut a0 = args[0];
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") a0,
            in("a1") args[1],
            in("a2") args[2],
            in("a3") args[3],
            in("a4") args[4],
            in("a5") args[5],
            in("a7") id,
        );
    }
    a0 as isize
}

#[cfg(target_arch = "loongarch64")]
fn syscall6(id: usize, args: [usize; 6]) -> isize {
    let mut a0 = args[0];
    unsafe {
        asm!(
            "syscall 0",
            inout("$a0") a0,
            in("$a1") args[1],
            in("$a2") args[2],
            in("$a3") args[3],
            in("$a4") args[4],
            in("$a5") args[5],
            in("$a7") id,
        );
    }
    a0 as isize
}

fn syscall3(id: usize, a0: usize, a1: usize, a2: usize) -> isize {
    syscall6(id, [a0, a1, a2, 0, 0, 0])
}

fn syscall4(id: usize, a0: usize, a1: usize, a2: usize, a3: usize) -> isize {
    syscall6(id, [a0, a1, a2, a3, 0, 0])
}

fn sys_exit(code: i32) -> ! {
    let _ = syscall3(SYS_EXIT, code as usize, 0, 0);
    loop {}
}

fn sys_close(fd: i32) {
    let _ = syscall3(SYS_CLOSE, fd as usize, 0, 0);
}

fn sys_pipe2(fds: &mut [i32; 2]) -> isize {
    syscall3(SYS_PIPE2, fds.as_mut_ptr() as usize, 0, 0)
}

fn sys_read(fd: i32, buf: *mut u8, len: usize) -> isize {
    syscall3(SYS_READ, fd as usize, buf as usize, len)
}

fn sys_write(fd: i32, buf: *const u8, len: usize) -> isize {
    syscall3(SYS_WRITE, fd as usize, buf as usize, len)
}

fn sys_readv(fd: i32, iov: *const IOVec, iovcnt: usize) -> isize {
    syscall3(SYS_READV, fd as usize, iov as usize, iovcnt)
}

fn sys_writev(fd: i32, iov: *const IOVec, iovcnt: usize) -> isize {
    syscall3(SYS_WRITEV, fd as usize, iov as usize, iovcnt)
}

fn sys_mmap(len: usize, prot: usize, flags: usize) -> isize {
    syscall6(SYS_MMAP, [0, len, prot, flags, usize::MAX, 0])
}

fn sys_mprotect(ptr: *mut u8, len: usize, prot: usize) -> isize {
    syscall3(SYS_MPROTECT, ptr as usize, len, prot)
}

fn sys_clone(flags: usize, ctid: *mut u32) -> isize {
    syscall6(SYS_CLONE, [flags, 0, 0, 0, ctid as usize, 0])
}

fn sys_wait4(pid: isize, status: *mut u32) -> isize {
    syscall4(SYS_WAIT4, pid as usize, status as usize, 0, 0)
}

fn sys_clock_gettime(ptr: *mut u8) -> isize {
    syscall3(SYS_CLOCK_GETTIME, CLOCK_REALTIME, ptr as usize, 0)
}

fn sys_uname(ptr: *mut u8) -> isize {
    syscall3(SYS_UNAME, ptr as usize, 0, 0)
}

fn map_pages(pages: usize, prot: usize, flags: usize) -> *mut u8 {
    let ret = sys_mmap(pages * PAGE_SIZE, prot, flags | MAP_ANONYMOUS);
    if ret < 0 {
        println!("mmap failed ret={}", ret);
        ptr::null_mut()
    } else {
        ret as usize as *mut u8
    }
}

fn new_pipe() -> Option<[i32; 2]> {
    let mut fds = [0i32; 2];
    let ret = sys_pipe2(&mut fds);
    if ret == 0 {
        Some(fds)
    } else {
        println!("pipe2 failed ret={}", ret);
        None
    }
}

fn pipe_with_bytes(bytes: &[u8]) -> Option<[i32; 2]> {
    let fds = new_pipe()?;
    let ret = sys_write(fds[1], bytes.as_ptr(), bytes.len());
    if ret == bytes.len() as isize {
        Some(fds)
    } else {
        println!("pipe seed write failed ret={} len={}", ret, bytes.len());
        sys_close(fds[0]);
        sys_close(fds[1]);
        None
    }
}

fn close_pipe(fds: [i32; 2]) {
    sys_close(fds[0]);
    sys_close(fds[1]);
}

fn read_exact_stack(fd: i32, out: &mut [u8]) -> bool {
    let ret = sys_read(fd, out.as_mut_ptr(), out.len());
    ret == out.len() as isize
}

fn check(name: &str, ok: bool) -> bool {
    if ok {
        println!("ok {}", name);
    } else {
        println!("not ok {}", name);
    }
    ok
}

fn wait_child_ok(pid: isize) -> bool {
    let mut status = 0u32;
    let ret = sys_wait4(pid, &mut status as *mut u32);
    ret == pid && ((status >> 8) & 0xff) == 0
}

fn test_basic_mprotect_read() -> bool {
    let page = map_pages(1, PROT_READ | PROT_WRITE, MAP_PRIVATE);
    if page.is_null() {
        return false;
    }
    unsafe { *page = 81 };
    if sys_mprotect(page, PAGE_SIZE, PROT_READ) != 0 {
        return false;
    }
    let Some(fds) = pipe_with_bytes(b"X") else { return false; };
    let ret = sys_read(fds[0], page, 1);
    let mut got = [0u8; 1];
    let preserved = read_exact_stack(fds[0], &mut got) && got[0] == 88;
    let wret = sys_write(fds[1], page as *const u8, 1);
    let mut got2 = [0u8; 1];
    let input_ok = wret == 1 && read_exact_stack(fds[0], &mut got2) && got2[0] == 81;
    close_pipe(fds);
    ret == EFAULT_RET && preserved && input_ok
}

fn test_lazy_ro_output() -> bool {
    let page = map_pages(1, PROT_READ | PROT_WRITE, MAP_PRIVATE);
    if page.is_null() {
        return false;
    }
    if sys_mprotect(page, PAGE_SIZE, PROT_READ) != 0 {
        return false;
    }
    let Some(fds) = pipe_with_bytes(b"Y") else { return false; };
    let ret = sys_read(fds[0], page, 1);
    let mut got = [0u8; 1];
    let preserved = read_exact_stack(fds[0], &mut got) && got[0] == 89;
    close_pipe(fds);
    ret == EFAULT_RET && preserved
}

fn test_lazy_ro_input_zero() -> bool {
    let page = map_pages(1, PROT_READ, MAP_PRIVATE);
    if page.is_null() {
        return false;
    }
    let Some(fds) = new_pipe() else { return false; };
    let wret = sys_write(fds[1], page as *const u8, 1);
    let mut got = [1u8; 1];
    let ok = wret == 1 && read_exact_stack(fds[0], &mut got) && got[0] == 0;
    close_pipe(fds);
    ok
}

fn test_lazy_rw_output_success() -> bool {
    let page = map_pages(1, PROT_READ | PROT_WRITE, MAP_PRIVATE);
    if page.is_null() {
        return false;
    }
    let Some(fds) = pipe_with_bytes(b"Z") else { return false; };
    let ret = sys_read(fds[0], page, 1);
    let ok = ret == 1 && unsafe { *page == 90 };
    close_pipe(fds);
    ok
}

fn test_cow_legal_write() -> bool {
    let page = map_pages(1, PROT_READ | PROT_WRITE, MAP_PRIVATE);
    if page.is_null() {
        return false;
    }
    unsafe { *page = 65 };
    let Some(fds) = pipe_with_bytes(b"B") else { return false; };
    let pid = sys_clone(SIGCHLD, ptr::null_mut());
    if pid == 0 {
        let ret = sys_read(fds[0], page, 1);
        let ok = ret == 1 && unsafe { *page == 66 };
        sys_exit(if ok { 0 } else { 1 });
    }
    let ok = pid > 0 && wait_child_ok(pid) && unsafe { *page == 65 };
    close_pipe(fds);
    ok
}

fn test_cow_mprotect_before_fork() -> bool {
    let page = map_pages(1, PROT_READ | PROT_WRITE, MAP_PRIVATE);
    if page.is_null() {
        return false;
    }
    unsafe { *page = 65 };
    if sys_mprotect(page, PAGE_SIZE, PROT_READ) != 0 {
        return false;
    }
    let Some(fds) = pipe_with_bytes(b"C") else { return false; };
    let pid = sys_clone(SIGCHLD, ptr::null_mut());
    if pid == 0 {
        let ret = sys_read(fds[0], page, 1);
        let ok = ret == EFAULT_RET && unsafe { *page == 65 };
        sys_exit(if ok { 0 } else { 1 });
    }
    let mut got = [0u8; 1];
    let ok = pid > 0
        && wait_child_ok(pid)
        && unsafe { *page == 65 }
        && read_exact_stack(fds[0], &mut got)
        && got[0] == 67;
    close_pipe(fds);
    ok
}

fn test_cow_mprotect_after_fork() -> bool {
    let page = map_pages(1, PROT_READ | PROT_WRITE, MAP_PRIVATE);
    if page.is_null() {
        return false;
    }
    unsafe { *page = 65 };
    let Some(fds) = pipe_with_bytes(b"D") else { return false; };
    let pid = sys_clone(SIGCHLD, ptr::null_mut());
    if pid == 0 {
        let pret = sys_mprotect(page, PAGE_SIZE, PROT_READ);
        let rret = sys_read(fds[0], page, 1);
        let ok = pret == 0 && rret == EFAULT_RET && unsafe { *page == 65 };
        sys_exit(if ok { 0 } else { 1 });
    }
    let mut got = [0u8; 1];
    let ok = pid > 0
        && wait_child_ok(pid)
        && unsafe { *page == 65 }
        && read_exact_stack(fds[0], &mut got)
        && got[0] == 68;
    close_pipe(fds);
    ok
}

fn test_shared_mprotect() -> bool {
    let page = map_pages(1, PROT_READ | PROT_WRITE, MAP_SHARED);
    if page.is_null() {
        return false;
    }
    unsafe { *page = 83 };
    if sys_mprotect(page, PAGE_SIZE, PROT_READ) != 0 {
        return false;
    }
    let Some(fds) = pipe_with_bytes(b"E") else { return false; };
    let ret = sys_read(fds[0], page, 1);
    let mut got = [0u8; 1];
    let ok = ret == EFAULT_RET
        && unsafe { *page == 83 }
        && read_exact_stack(fds[0], &mut got)
        && got[0] == 69;
    close_pipe(fds);
    ok
}

fn test_cross_page() -> bool {
    let out = map_pages(2, PROT_READ | PROT_WRITE, MAP_PRIVATE);
    if out.is_null() {
        return false;
    }
    unsafe {
        *out.add(PAGE_SIZE - 1) = 97;
        *out.add(PAGE_SIZE) = 98;
    }
    if sys_mprotect(unsafe { out.add(PAGE_SIZE) }, PAGE_SIZE, PROT_READ) != 0 {
        return false;
    }
    let Some(fds) = pipe_with_bytes(b"HI") else { return false; };
    let ret = sys_read(fds[0], unsafe { out.add(PAGE_SIZE - 1) }, 2);
    let mut got = [0u8; 2];
    let output_ok = ret == EFAULT_RET && read_exact_stack(fds[0], &mut got) && got == *b"HI";
    close_pipe(fds);

    let input = map_pages(2, PROT_READ | PROT_WRITE, MAP_PRIVATE);
    if input.is_null() {
        return false;
    }
    unsafe {
        *input.add(PAGE_SIZE - 1) = 77;
        *input.add(PAGE_SIZE) = 78;
    }
    if sys_mprotect(input, PAGE_SIZE * 2, PROT_READ) != 0 {
        return false;
    }
    let Some(fds2) = new_pipe() else { return false; };
    let wret = sys_write(fds2[1], unsafe { input.add(PAGE_SIZE - 1) }, 2);
    let mut got2 = [0u8; 2];
    let input_ok = wret == 2 && read_exact_stack(fds2[0], &mut got2) && got2 == *b"MN";
    close_pipe(fds2);
    output_ok && input_ok
}

fn test_iov() -> bool {
    let iov_page = map_pages(1, PROT_READ | PROT_WRITE, MAP_PRIVATE);
    let base = map_pages(1, PROT_READ | PROT_WRITE, MAP_PRIVATE);
    if iov_page.is_null() || base.is_null() {
        return false;
    }
    unsafe {
        *(iov_page as *mut IOVec) = IOVec { iov_base: base, iov_len: 1 };
    }
    if sys_mprotect(iov_page, PAGE_SIZE, PROT_READ) != 0 {
        return false;
    }
    let Some(fds) = pipe_with_bytes(b"R") else { return false; };
    let readv_ret = sys_readv(fds[0], iov_page as *const IOVec, 1);
    let readv_iov_ro_ok = readv_ret == 1 && unsafe { *base == 82 };
    close_pipe(fds);

    let ro_base = map_pages(1, PROT_READ | PROT_WRITE, MAP_PRIVATE);
    if ro_base.is_null() {
        return false;
    }
    unsafe { *ro_base = 0 };
    if sys_mprotect(ro_base, PAGE_SIZE, PROT_READ) != 0 {
        return false;
    }
    let iov_stack = IOVec { iov_base: ro_base, iov_len: 1 };
    let Some(fds2) = pipe_with_bytes(b"S") else { return false; };
    let bad_readv_ret = sys_readv(fds2[0], &iov_stack as *const IOVec, 1);
    let mut got = [0u8; 1];
    let readv_ro_base_ok = bad_readv_ret == EFAULT_RET
        && read_exact_stack(fds2[0], &mut got)
        && got[0] == 83;
    close_pipe(fds2);

    let write_iov_page = map_pages(1, PROT_READ | PROT_WRITE, MAP_PRIVATE);
    let write_data = map_pages(1, PROT_READ | PROT_WRITE, MAP_PRIVATE);
    if write_iov_page.is_null() || write_data.is_null() {
        return false;
    }
    unsafe {
        *write_data = 84;
        *(write_iov_page as *mut IOVec) = IOVec { iov_base: write_data, iov_len: 1 };
    }
    if sys_mprotect(write_iov_page, PAGE_SIZE, PROT_READ) != 0
        || sys_mprotect(write_data, PAGE_SIZE, PROT_READ) != 0
    {
        return false;
    }
    let Some(fds3) = new_pipe() else { return false; };
    let writev_ret = sys_writev(fds3[1], write_iov_page as *const IOVec, 1);
    let mut got2 = [0u8; 1];
    let writev_ro_ok = writev_ret == 1 && read_exact_stack(fds3[0], &mut got2) && got2[0] == 84;
    close_pipe(fds3);

    let overflow_iov = IOVec { iov_base: write_data, iov_len: usize::MAX };
    let Some(fds4) = new_pipe() else { return false; };
    let overflow_ret = sys_writev(fds4[1], &overflow_iov as *const IOVec, 1);
    close_pipe(fds4);

    readv_iov_ro_ok && readv_ro_base_ok && writev_ro_ok && overflow_ret == EINVAL_RET
}

fn test_output_syscalls_ro() -> bool {
    let page = map_pages(1, PROT_READ | PROT_WRITE, MAP_PRIVATE);
    if page.is_null() {
        return false;
    }
    unsafe { *page = 0 };
    if sys_mprotect(page, PAGE_SIZE, PROT_READ) != 0 {
        return false;
    }
    let clock_ret = sys_clock_gettime(page);
    let uname_ret = sys_uname(page);
    clock_ret == EFAULT_RET && uname_ret == EFAULT_RET
}

fn test_zero_and_overflow() -> bool {
    let Some(fds) = pipe_with_bytes(b"0") else { return false; };
    let bad = usize::MAX - 1;
    let zero_ret = sys_read(fds[0], bad as *mut u8, 0);
    let overflow_ret = sys_write(fds[1], bad as *const u8, 16);
    let mut got = [0u8; 1];
    let still_has_byte = read_exact_stack(fds[0], &mut got) && got[0] == 48;
    close_pipe(fds);
    zero_ret == 0 && overflow_ret == EFAULT_RET && still_has_byte
}

fn test_child_settid_lazy() -> bool {
    let page = map_pages(1, PROT_READ | PROT_WRITE, MAP_PRIVATE) as *mut u32;
    if page.is_null() {
        return false;
    }
    let pid = sys_clone(SIGCHLD | CLONE_CHILD_SETTID, page);
    if pid == 0 {
        let val = unsafe { *page };
        sys_exit(if val == 0 { 0 } else { 1 });
    }
    pid > 0 && wait_child_ok(pid)
}

fn test_child_settid_cow() -> bool {
    let page = map_pages(1, PROT_READ | PROT_WRITE, MAP_PRIVATE) as *mut u32;
    if page.is_null() {
        return false;
    }
    unsafe { *page = 0x1122_3344 };
    let pid = sys_clone(SIGCHLD | CLONE_CHILD_SETTID, page);
    if pid == 0 {
        let val = unsafe { *page };
        sys_exit(if val == 0x1122_3344 { 0 } else { 1 });
    }
    pid > 0 && wait_child_ok(pid) && unsafe { *page == 0x1122_3344 }
}

#[no_mangle]
fn main(_argc: usize, _argv: &[&str]) -> i32 {
    println!("USERCOPY_ACCESS START");
    let mut failed = 0usize;

    let tests: [(&str, fn() -> bool); 15] = [
        ("basic_mprotect_read", test_basic_mprotect_read),
        ("lazy_ro_output", test_lazy_ro_output),
        ("lazy_ro_input_zero", test_lazy_ro_input_zero),
        ("lazy_rw_output_success", test_lazy_rw_output_success),
        ("cow_legal_write", test_cow_legal_write),
        ("cow_mprotect_before_fork", test_cow_mprotect_before_fork),
        ("cow_mprotect_after_fork", test_cow_mprotect_after_fork),
        ("shared_mprotect", test_shared_mprotect),
        ("cross_page", test_cross_page),
        ("iov", test_iov),
        ("output_syscalls_ro", test_output_syscalls_ro),
        ("zero_and_overflow", test_zero_and_overflow),
        ("child_settid_lazy", test_child_settid_lazy),
        ("child_settid_cow", test_child_settid_cow),
        ("final_marker", || true),
    ];

    for (name, test) in tests.iter() {
        if !check(name, test()) {
            failed += 1;
        }
    }

    if failed == 0 {
        println!("USERCOPY_ACCESS PASS");
        0
    } else {
        println!("USERCOPY_ACCESS FAIL failed={}", failed);
        1
    }
}
```

## 11. 结论

这次重构修复的不是单个 syscall 的局部 bug，而是 user-copy 模型的权限边界问题。旧模型把“地址能翻译”误当成“访问合法”，因此所有通过物理页切片写用户页的路径都可能绕过 `mprotect`、COW 和 PTE 权限保护。新模型把访问方向显式传入翻译层，并在返回物理地址前统一做范围、PTE 用户权限、读写权限和 fault/COW 检查。

同一份回归测试在修改后 rv64/la64、musl/glibc 全部通过；在原始 HEAD 旧内核上双架构都会卡死在第一个只读页输出用例，直接证明了旧实现会写穿只读保护并消费 pipe 数据。

补充的 la64 COW/lazy panic 修复说明了另一个层面的风险：user-copy 权限模型正确之后，底层 VM 仍必须维护 `MapArea` 元数据和页表 PTE 的一致性。本次补丁已经避免 stale PTE 误入 COW 并触发 kernel panic，但 mmap/mprotect 的边界和 overlap 校验仍需要后续继续收紧。
