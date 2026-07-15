---
title: "Bug: LA64 hole-read mismatch 与用户栈 ABI 对齐"
category: debug
status: resolved-with-known-limits
author: MangoCore Team
last_update: 2026-07-15
tags: [postmortem, mm, execve, stack, abi, loongarch64, llvm, debugging]
code_paths:
  - "os/src/mm/mod.rs"
  - "os/src/mm/address_space.rs"
  - "os/src/syscall/process/exec.rs"
  - "os/src/task/task.rs"
  - "os/src/task/context.rs"
  - "os/src/task/signal/frame.rs"
  - "os/src/hal/arch/loongarch64/trap/context.rs"
  - "os/src/hal/arch/loongarch64/trap/mod.rs"
  - "os/src/hal/arch/loongarch64/trap/trap.S"
  - "user/src/bin/fs_test.rs"
related_docs:
  - "docs/09_debug/la64_on_board/development-log.md"
  - "docs/04_mm/address-space-and-vma.md"
  - "docs/04_mm/architecture.md"
  - "docs/04_mm/debugging.md"
entry_points:
  - "AddressSpace::create_elf_tables"
  - "TaskControlBlock::load_elf"
  - "TrapContext::app_init_context"
  - "trap_return"
  - "signal_frame_layout"
---

# Bug: LA64 hole-read mismatch 与用户栈 ABI 对齐

## 1. 摘要

该故障于 2026-05-19 首次在 LA64 自制 `/fs_test` 的 `lseek_hole_read` 用例中
出现。当时已经通过内核逐层插桩确认 PageCache、VFS、用户拷贝和用户地址回读均
正确，但根因停留在“用户态编译器优化或栈布局”假设，问题未结案。2026-07-15，
在对 ext4 修复执行 QEMU 文件系统回归时再次稳定触发；进一步打印用户缓冲区并
反汇编 LA64 release ELF 后，最终确认是用户栈 ABI 对齐违规。

对 LA64 release 用户程序反汇编后确认，LLVM 按 LoongArch ELF ABI 假设函数入口
栈指针为 16 字节对齐，并将地址加 8 优化为按位 OR 8。MangoCore 的
`AddressSpace::create_elf_tables()` 当时只按 `sizeof(usize) == 8` 对齐最终用户栈，
使函数入口 SP 可能以十六进制 `...8` 结尾。该条件下 OR 8 不再等价于加 8，程序
重复读取 `buf[50..52]`，而不是读取 `buf[58..60]`，最终产生静默比较错误。

该问题属于用户态 ABI 违规，不是 ext4、page cache、`read(2)` 或 `lseek(2)` 的数据
错误。其影响面覆盖 LA64 上所有经 `execve` 启动的优化用户程序，以及由内核构造
入口栈的用户信号处理函数。

| 属性 | 结论 |
|------|------|
| 严重性 | High / P1 |
| 故障类型 | 内核 ABI 违规、潜在静默数据错误 |
| 影响架构 | 已在 LA64 触发；共享栈构造同时按 rv64 ABI 修正 |
| 定位难度 | 很高 |
| 修复难度 | 中等 |
| 直接根因 | 最终用户 SP 只保证 8 字节对齐 |
| 规范要求 | 过程入口 SP 按 128-bit，即 16 字节边界对齐 |
| 首次发现 | 2026-05-19，内核数据链排除完成但根因未定位 |
| 根因确认 | 2026-07-15，LA64 release 反汇编与地址推导闭环 |
| 修复提交 | `b6c5c973`，`fix(fs): stabilize ext4 persistent writes` |
| 集成验证 | rv64/la64 构建；两架构 `fs_test 63/63`；QEMU basic/iozone/libctest；2K1000LA 启动与 P4 回归 |

LoongArch 官方 ELF ABI 的 Stack 章节规定，过程入口的栈指针必须按 128-bit 边界
对齐：<https://loongson.github.io/LoongArch-Documentation/LoongArch-ELF-ABI-CN.html>。

## 2. 发现背景与时间线

### 2.1 2026-05：首次发现与未完成定位

早期版本的 FS suite 在第 35/51 项首次报告：

```text
[35/51] lseek beyond EOF + hole read
  FAIL: hole: data at 50 mismatch
```

当时使用 `LOG=info` 在 ext4、PageCache、syscall copy-out 和用户页回读路径逐层
插桩，取得了以下结果：

| 检查项 | 结果 |
|--------|------|
| 两次 write 和一次 read 的 PageCache 实例 | 相同，诊断地址为 `pc=0x805a1410` |
| PageCache 写入后 `snap[50..60]` | `DATA_AT_50` |
| metadata flush 后 `snap2[50..60]` | `DATA_AT_50` |
| read_at page 与 cache 状态 | `page=0 cached=true` |
| copy-out 前 kernel buffer | `kbuf[50..60] = DATA_AT_50` |
| UserBufferWriter 长度 | `src_len=60 buf_total_len=60 n_bufs=1` |
| copy-out 后从用户地址回读 | `READBACK[50..60] = DATA_AT_50, match=true` |

这一轮已经排除 ext4 write/read、PageCache 实例分裂、metadata flush、文件大小、
UserBufferWriter 长度、用户页翻译及 copy-out。剩余假设包括用户态 Rust 优化、栈
布局和 syscall wrapper，但没有继续反汇编 release ELF，因此问题以“根因未定位”
状态保留。

### 2.2 2026-07：ext4 回归中再次触发

#### 原始 ext4 问题

本轮工作的起点是 Alpine APK 安装 Python 依赖时出现大量提交失败：

```text
ERROR: python3-3.14.5-r2: failed to commit usr/bin/python3: No such file or directory
ERROR: python3-3.14.5-r2: failed to commit usr/lib/python3.14/socket.py: No such file or directory
ERROR: python3-3.14.5-r2: failed to commit usr/lib/python3.14/ssl.py: No such file or directory
```

围绕 ext4 已检查和修复的主题包括：

- `EXT4_BG_BLOCK_UNINIT` 和 `EXT4_BG_INODE_UNINIT` lazy-init 语义；
- block group 与 superblock 空闲计数一致性；
- 目录项删除、文件类型和 checksum；
- create、unlink、rename、rmdir 后的 parent inode snapshot；
- inode/block 释放和重用后的 metadata cache 失效；
- negative dentry 与目录 version 的一致性。

修复后的 P4 APK 测试已经完成包安装、Python import、rename、symlink、64 MiB 写入，
对应干净测试镜像的离线 `e2fsck -fn` 返回 0。因此继续扩大 QEMU 回归范围，检查
修复是否破坏其他文件系统路径。

#### QEMU 回归扩大

依次执行了以下检查：

1. RV64 `basic + libctest + iozone` 完成，测试组退出码均为 0；
2. LA64 强制重建 QEMU 内核后，相同测试组完成，退出码均为 0；
3. 运行项目自制的 63 项 `/fs_test`；
4. RV64 为 63/63，LA64 初始为 62/63；
5. LA64 唯一失败项为 `lseek_hole_read`。

这时测试路径仍是根文件系统下的 `/tmp*`，主要用于判断通用 VFS/ramfs 行为。它
不是后续使用 `chroot /sdcard` 强制命中 ext4 的那一轮测试。因此，本节描述的 ABI
故障与 ext4 磁盘格式本身无关。

#### 关键反常现象

首次失败输出为：

```text
[36/63] lseek beyond EOF + hole read
  FAIL: hole: data at 50 mismatch
[FAIL] lseek_hole_read
```

加入读长度和缓冲区窗口诊断后得到：

```text
read=60
bytes[45..65]=[
  0, 0, 0, 0, 0,
  68, 65, 84, 65, 95, 65, 84, 95, 53, 48,
  0, 0, 0, 0, 0
]
```

十进制字节 `68,65,84,65,95,65,84,95,53,48` 对应：

```text
D A T A _ A T _ 5 0
```

即预期的 `DATA_AT_50`。内核返回值、空洞补零和目标数据全部正确，但用户态比较
仍失败。这一矛盾将问题边界从文件系统移动到了用户程序的比较代码及其运行 ABI。

证据日志：

- `logs/fs-test-la64-hole-diag-20260715.log`
- `logs/fs-test-la64-buffer-diag-20260715.log`
- `logs/fs-test-la64-stack-align-fixed-20260715.log`

## 3. 触发用例

触发函数位于 `user/src/bin/fs_test.rs`：

```rust
fn test_lseek_hole_read() -> bool {
    let fd = sys_open("/tmp26/hole\0", O_CREAT | O_RDWR);
    sys_write(fd as usize, b"0123456789");
    sys_lseek(fd as usize, 50, SEEK_SET);
    sys_write(fd as usize, b"DATA_AT_50");
    sys_lseek(fd as usize, 0, SEEK_SET);

    let mut buf = [0u8; 70];
    let n = sys_read(fd as usize, &mut buf);

    if n < 60 {
        return false;
    }
    if &buf[10..50] != &[0u8; 40] {
        return false;
    }
    if &buf[50..60] != b"DATA_AT_50" {
        return false;
    }
    true
}
```

该用例构造如下文件：

```text
offset 0..10   : "0123456789"
offset 10..50  : 40 bytes hole, read as zero
offset 50..60  : "DATA_AT_50"
```

触发点不是空洞语义，而是最后一个长度为 10 的切片比较。LA64 release LLVM 将其
内联优化为一次 8 字节比较和一次 2 字节比较。

## 4. 用户程序反汇编证据

### 4.1 分析对象

分析的构建产物为：

```text
user/target/loongarch64-unknown-linux-gnu/release/fs_test
```

文件属性：

```text
ELF 64-bit LSB executable, LoongArch, statically linked, not stripped
SHA-256 e4048aef6e16efe05aca1f2dba616f895b0fc603944a65814929638c5b584a92
rustc    nightly-2024-05-01 / 1.80.0-nightly
LLVM     18.1.4
```

本次构建中，目标函数符号和地址范围为：

```text
0x120005620 .. 0x120005937
fs_test::test_lseek_hole_read
```

这些 PC 是该次具体 ELF 构建的地址，不属于稳定 ABI，重新编译后允许变化。

该指纹锚定本文逐条核对的 ELF，但不能补回修复前/后的两个中间 Git tree id。复核应
使用 Rust nightly 随附的 LLVM tools（或项目 Docker 中支持 LoongArch 的
`llvm-objdump`）；macOS 系统自带工具不保证包含 LoongArch 反汇编后端：

```bash
SYSROOT="$(rustup run nightly-2024-05-01 rustc --print sysroot)"
OBJDUMP="$(find "$SYSROOT/lib/rustlib" -path '*/bin/llvm-objdump' -type f | head -n 1)"

"$OBJDUMP" --demangle --syms \
  user/target/loongarch64-unknown-linux-gnu/release/fs_test \
  | grep test_lseek_hole_read

"$OBJDUMP" --demangle --no-show-raw-insn \
  --start-address=0x120005620 \
  --stop-address=0x120005938 \
  -d user/target/loongarch64-unknown-linux-gnu/release/fs_test
```

### 4.2 函数栈帧

函数入口指令：

```asm
0x120005620  addi.d  $sp, $sp, -224
0x120005624  st.d    $ra, $sp, 216
0x120005628  st.d    $fp, $sp, 208
0x120005638  addi.d  $fp, $sp, 224
```

设函数入口栈指针为 `S`：

```text
sp = S - 224
fp = sp + 224 = S
```

`224 == 0xe0`，是 16 的整数倍，因此该函数不会改变入口栈指针的低四位。`$fp`
直接继承 `S` 的对齐状态。

### 4.3 缓冲区地址

缓冲区初始化和 `read` 参数：

```asm
0x1200056cc  addi.d  $s1, $fp, -208
0x1200056d0  ori     $s2, $zero, 70
0x1200056e8  move    $a1, $s1
0x1200056f0  bl      user_lib::usr_call::read
```

因此：

```text
buf      = fp - 208
buf[50]  = fp - 158
buf[58]  = fp - 150
buf[60]  = fp - 148
```

### 4.4 LLVM 生成的 8+2 字节比较

目标字符串后两个字节是 ASCII `"50"`：

```text
'5' = 0x35
'0' = 0x30
little-endian u16 = 0x3035
```

比较代码的关键指令为：

```asm
# 构造目标后两个字节 0x3035
0x1200057f0  lu12i.w $s0, 3
0x120005888  ori     $a0, $s0, 53

# 基址为 &buf[50]
0x12000588c  addi.d  $a1, $fp, -158

# LLVM 认为它等价于 &buf[50] + 8，也就是 &buf[58]
0x120005890  ori     $a1, $a1, 8
0x120005894  ld.hu   $a1, $a1, 0
0x120005898  xor     $a0, $a1, $a0

# 读取并比较前8字节 "DATA_AT_"
0x12000589c  lu12i.w $a1, 267588
0x1200058a0  ori     $a1, $a1, 324
0x1200058a4  lu32i.d $a1, 278879
0x1200058a8  lu52i.d $a1, $a1, 1525
0x1200058ac  ld.d    $a2, $fp, -158
0x1200058b0  xor     $a1, $a2, $a1

# 合并8字节和2字节比较结果
0x1200058b4  or      $a0, $a1, $a0
0x1200058b8  bnez    $a0, 108
```

当组合结果非零时，`0x1200058b8` 跳转到失败分支 `0x120005924`。

## 5. 地址级根因证明

### 5.1 ABI 前提成立时

LoongArch ABI 要求函数入口：

```text
S mod 16 = 0
fp mod 16 = 0
```

此时：

```text
&buf[50] = fp - 158
(fp - 158) mod 16 = 2

&buf[50] | 8
  = low nibble 0x2 | 0x8
  = low nibble 0xa
  = &buf[50] + 8
  = &buf[58]
```

因此，在 ABI 前提成立时，`ori address, address, 8` 是合法优化。

### 5.2 MangoCore 旧入口 SP

旧实现只保证：

```text
S mod 8 = 0
```

因此允许：

```text
S mod 16 = 0  // 偶然正确
S mod 16 = 8  // 违反ABI
```

故障运行中，由错误指令行为可以反推出：

```text
S mod 16 = 8
fp mod 16 = 8
```

于是：

```text
&buf[50] = fp - 158
(fp - 158) mod 16 = 0xa

&buf[50] | 8
  = low nibble 0xa | 0x8
  = low nibble 0xa
  = &buf[50]
```

`ori` 没有将地址前移，`ld.hu` 实际再次读取：

```text
buf[50..52] = "DA" = little-endian 0x4144
```

而不是：

```text
buf[58..60] = "50" = little-endian 0x3035
```

所以比较必然失败。

### 5.3 实测与推导边界

本次故障日志没有在 `/fs_test` 进入用户态时打印完整 SP，因此报告不虚构完整运行时
地址。当前证据可以严格得出：

| 信息 | 证据类型 | 结论 |
|------|----------|------|
| 用户函数 PC | ELF 符号和反汇编实测 | `0x120005620..0x120005937` |
| 目标比较 PC | 反汇编实测 | `0x12000588c..0x1200058b8` |
| 缓冲区相对地址 | 反汇编实测 | `buf = fp - 208` |
| 完整入口 SP | 未记录 | 不作声明 |
| 入口 SP 低四位 | 由指令与失败结果必然推导 | `S & 0xf == 8` |

## 6. 内核代码链路

### 6.1 普通 execve 路径

普通用户程序的入口栈按以下路径生成并送入 PLV3：

```text
sys_execve()
  -> TaskControlBlock::load_elf()
     -> AddressSpace::from_elf()
     -> alloc_user_res_with_trap_ppn()
     -> AddressSpace::create_elf_tables()
        -> 返回 user_sp
     -> TrapContext::app_init_context(entry, user_sp, ...)
        -> trap_cx.gp.sp = user_sp
  -> scheduler
     -> TaskContext::goto_trap_return()
  -> loongarch64::trap_return()
     -> __restore(trap_cx, user_token, asid)
     -> LOAD_GP 3
     -> ertn
  -> PLV3 用户入口
```

各层职责：

| 层 | 文件 | 作用 |
|----|------|------|
| ELF 与初始栈 | `os/src/mm/address_space.rs` | 映射 ELF，写 argv/envp/auxv，计算最终 SP |
| exec 提交 | `os/src/task/task.rs` | 建立新地址空间和 TrapContext |
| 上下文寄存器 | `os/src/hal/arch/loongarch64/trap/context.rs` | 将 `user_sp` 写入 `$r3/$sp` |
| 首次调度 | `os/src/task/context.rs` | 内核上下文返回到 `trap_return` |
| 返回用户态 | `os/src/hal/arch/loongarch64/trap/mod.rs` | 设置 PLV3、页表、ASID，跳入 `__restore` |
| 汇编恢复 | `os/src/hal/arch/loongarch64/trap/trap.S` | `LOAD_GP 3` 恢复用户 SP，`ertn` 进入用户态 |

`TrapContext`、`trap_return` 和 `__restore` 都只是忠实传播 `user_sp`。直接制造错误
SP 的位置是 `AddressSpace::create_elf_tables()` 的最终布局计算。

### 6.2 初始 init 任务

内核创建第一个 `/init` 时也调用同一个 `create_elf_tables()`：

```text
TaskControlBlock::new
  -> argv = ["/init"]
  -> envp = [PATH, PWD, HOME]
  -> create_elf_tables()
  -> TrapContext::app_init_context()
```

因此修复共享函数可以同时覆盖首个用户任务和普通 `execve`。

### 6.3 用户栈地址区间

当前 LA64 配置：

```text
PAGE_SIZE            = 0x1000
USER_STACK_SIZE      = 0x100000  // 1 MiB
USER_STACK_INIT_SIZE = 0x040000  // 256 KiB
USER_STACK_BASE      = 0x17ffffe000
```

每个地址空间内的用户资源槽位按以下公式取得栈高地址：

```text
slot_top(n) = 0x17ffffe000 - n * (0x100000 + 0x1000)
            = 0x17ffffe000 - n * 0x101000
```

映射起点本身是页对齐的。错误来自把字符串、随机数、auxv 和指针表压入以后，最终
SP 没有重新按 ABI 对齐，而不是用户栈 VMA 的基址不对齐。

## 7. 旧实现为什么会产生 `...8`

旧 `create_elf_tables()` 在压入 argv/envp 字符串后只执行：

```rust
*sp &= !(core::mem::size_of::<usize>() - 1);
```

LA64 上 `size_of::<usize>() == 8`，所以只能保证 `sp mod 8 == 0`。

随后旧实现依次压入：

```text
AT_RANDOM                         16 bytes
固定 padding                       8 bytes
auxv: 17 * sizeof(AuxvEntry)     272 bytes
envp pointers        (envc + 1) * 8 bytes
argv pointers        (argc + 1) * 8 bytes
argc                               8 bytes
```

`argc`、`envc`、参数字符串长度会随程序和 shell 环境变化。固定 8 字节 padding 不能
保证任意参数数量下最终 SP 都满足 16 字节对齐。这解释了为什么问题有时出现、有时
消失：旧实现允许最终 SP 在 `...0` 和 `...8` 之间变化。

## 8. 修复设计

### 8.1 统一 ABI 常量

在 `os/src/mm/mod.rs` 定义：

```rust
pub const USER_STACK_ABI_ALIGN: usize = 16;
```

该常量描述的是“内核进入用户态时”的 ABI 边界，而不是 `usize`、`u64` 等单个数据
类型的自然对齐。

### 8.2 精确计算最终栈布局

修复后的 `create_elf_tables()`：

```text
压入 envp 字符串
-> 压入 argv 字符串
-> SP 向下对齐到16字节
-> 压入16字节 AT_RANDOM
-> 计算 auxv + envp pointers + argv pointers + argc 的总字节数
-> 计算最终 SP 所需的动态 padding，范围为 0..15
-> 压入 padding
-> 压入 auxv/envp/argv/argc
-> debug_assert(final_sp & 0xf == 0)
```

核心公式：

```rust
let final_sp_without_padding = user_sp - table_bytes;
let padding_len = final_sp_without_padding & (USER_STACK_ABI_ALIGN - 1);
user_sp -= padding_len;
```

这样 padding 由实际 `argc/envc/auxv` 布局决定，不再依赖固定 8 字节猜测。

### 8.3 同步 exec 容量预检

`validate_exec_stack_usage()` 必须使用与 `create_elf_tables()` 完全相同的：

- 16 字节对齐；
- auxv 数量；
- argv/envp NULL 终止项数量；
- argc 大小；
- 动态 padding 公式。

否则可能出现预检允许、实际压栈越界，或者合法参数被错误返回 `E2BIG`。

### 8.4 信号处理入口

signal handler 也是由内核人为创建的用户函数入口。即使普通 exec 栈已经对齐，内核
在用户栈上压入 `UserContext` 和 `SigInfo` 后仍可能破坏对齐。因此
`signal_frame_layout()` 同步要求：

```text
ucontext_addr 按 max(align_of<UserContext>, 16) 对齐
siginfo_addr / handler SP 按16字节对齐
```

不修信号路径会保留另一个同类入口，使优化后的 signal handler 仍可能产生随机错误。

## 9. 验证结果

### 9.1 修复前

```text
RV64 generic /fs_test: 63/63
LA64 generic /fs_test: 62/63
LA64 failure: lseek_hole_read
```

失败时读取长度和缓冲区内容正确，用户态切片比较错误。

### 9.2 修复后

```text
LA64 generic /fs_test: 63/63
```

在 ext4 整批修复后的定向 ABI 调试阶段，工作记录显示没有继续修改 `read`、`lseek`、
ramfs、ext4 或 PageCache，原用例随后恢复，符合“恢复编译器依赖的 ABI 前提”这一
根因判断。但修复前后两轮没有分别保存 tree hash，最终 `b6c5c973` 又同时包含 ext4
整批改动，因此这段行为证据不能包装成可由两个 Git tree 重建的严格单变量 A/B。

### 9.3 已完成的集成门禁

修复已随 `b6c5c973 fix(fs): stabilize ext4 persistent writes` 提交。该提交同时统一
rv64/la64 用户入口 16 字节栈对齐，并让 exec 容量预检与 signal frame 使用同一约束。
归档验证包括：

- Docker 内严格串行强制构建 rv64、la64，均成功；
- 全新 ext4 fixture 上 LA64、RV64 `/fs_test` 均为 `63/63`；
- 两个镜像关机后 `e2fsck -fn` 五阶段退出 0；
- 两架构 QEMU `basic + iozone + libctest` 外层 group/wrapper 均退出 0，无内核
  panic/I/O error；libctest 内部仍有已知 FAIL/timeout/segfault，不能据此写成 suite 全通过；
- 2K1000LA 启动识别 2 GiB、AHCI/P2/P4、GMAC，P4 reuse、写入和 iozone 回归通过。

其中定向调试记录里的原失败用例由 `62/63` 恢复到 `63/63`，是重要行为回归；更强的
根因证据仍是错误 SP、`ori` 指令和实际地址的逐项闭合，而不是缺 tree id 的 A/B。
此前 ext4 chroot 中的 `ftruncate -ETXTBSY` 后续证明是不同文件系统实例共用占位
`dev_id=0`、inode 号碰撞，已按独立根因修复，不能倒归因到 SP。

### 9.4 仍保留的覆盖边界

当前没有一份单独归档的“所有 argc/envc 组合入口 SP 遥测”日志，也没有只打印 signal
handler 入口 `sp & 0xf` 的专用板端测试。signal frame 已同步修改并经过上述集成回归，
但若未来调整 auxv、signal frame 或线程入口，仍应增加 §13.2 所列的直接 ABI 冒烟门禁。
这一覆盖缺口不影响本次地址级根因和原失败用例闭环，故文档状态为
`resolved-with-known-limits`，而不是继续把已提交修复标为 WIP。

## 10. 为什么旧测试没有发现

该故障需要多个条件同时成立：

1. 最终用户 SP 低四位恰好为 `0x8`；
2. 编译器生成依赖 16 字节入口对齐的机器码；
3. 程序执行到该机器码；
4. 输入数据能够让错误地址读取转化为可见差异。

以下情况都可能掩盖问题：

- 某次 argv/envp 布局让 SP 偶然以 `...0` 结尾；
- debug/release 或不同 LLVM 版本生成普通 `addi.d`，没有生成 `ori`；
- 用户程序只访问自然对齐不超过 8 字节的数据；
- iozone 主要覆盖大块 I/O 和吞吐量，没有相同的 10 字节内联比较；
- LTP 用例的函数栈帧和编译器优化没有命中特定组合；
- 错误被误判为文件系统、测试数据或偶发不稳定。

这类 bug 不一定在压力更大的测试中更容易出现，反而可能由一个很小、很具体的
比较表达式稳定触发。

## 11. 严重性与风险

本问题定级为 High / P1，原因是：

- 影响所有 LA64 `execve` 用户程序的入口 ABI；
- 可能产生静默数据错误，而不是可见 panic；
- 对工具链版本、优化级别和栈布局敏感；
- 可能表现为 Python、musl、APK、shell 或测试程序的随机错误；
- signal handler 若未同步修复，仍保留相同类别风险。

当前没有证据表明它已经导致权限提升、任意代码执行或 SSD 持久数据损坏，因此不
定为 Critical/P0。如果未来证明密码、签名或边界检查可以被该错误绕过，需要重新
评估安全等级。

## 12. 调试方法复盘

本次定位的关键不是最后把常量改成 16，而是按层排除并建立证据链：

```text
测试报告数据错误
-> 打印 read 返回长度
-> 打印用户缓冲区原始字节
-> 确认 VFS 返回数据正确
-> 对比 RV64/LA64 架构差异
-> 反汇编 LA64 release ELF
-> 找到 ori 地址折叠
-> 根据 fp 相对偏移推导入口 SP 低四位
-> 追踪 exec栈 -> TrapContext -> trap_return -> ertn
-> 定位 create_elf_tables 的8字节对齐
-> 按 ABI 设计动态 padding
-> 用原失败用例回归
```

可复用原则：当“内存转储正确、语言级比较错误”时，不要继续盲目修改数据生产者。
应检查：

1. 用户程序反汇编；
2. 函数入口 ABI；
3. 栈和堆地址对齐；
4. 编译器基于 `align`、`nonnull`、`noalias` 等前提做出的优化；
5. 内核向用户态人工构造的所有入口，而不只普通 syscall 返回。

## 13. 预防措施

### 13.1 内核断言

在所有人工用户入口保留或增加调试断言：

```rust
debug_assert_eq!(user_sp & 0xf, 0);
```

至少覆盖：

- 首个 init 任务；
- 普通 `execve`；
- signal handler；
- 新线程用户栈入口；
- `sigreturn` 恢复后的用户 SP 合法性检查，如项目语义要求。

### 13.2 用户态 ABI 冒烟测试

增加 LA64 release 测试，覆盖：

- 函数入口读取 `$sp` 并验证 `sp & 0xf == 0`；
- 不同 argc/envc 奇偶组合；
- 长短参数字符串组合；
- 10、18、26 字节等容易生成分块比较的切片；
- signal handler 入口 SP；
- fork 后 exec；
- 动态加载器入口和静态 ELF 入口。

### 13.3 不使用固定 padding 猜测布局

所有 ABI 布局都应先计算完整表大小，再根据最终地址计算 padding。固定压入一个
word 只对特定字段数量成立，一旦 auxv、argv 或 envp 数量变化就会失效。

### 13.4 保留架构差分

相同用户测试在 RV64 通过而 LA64 失败时，应优先比较：

- ABI 常量；
- 用户入口寄存器；
- trap restore 汇编；
- 编译器目标特有优化；
- 地址规范化和对齐要求。

不要因为测试名称包含 `fs` 就把问题范围限定在文件系统。

## 14. 最终结论

本次故障的直接数据生产链是正确的：文件内容、空洞补零、`read(2)` 返回值和用户
缓冲区都符合预期。真正失败的是 LLVM 依据合法 LoongArch ABI 前提生成的用户代码，
运行在内核提供的非法 8-byte-only 对齐栈上。

完整因果链为：

```text
MangoCore只按8字节对齐最终用户SP
-> 函数入口SP可能以...8结尾
-> LLVM仍按LoongArch ABI假设SP按16字节对齐
-> 将 &buf[50] + 8 折叠为 &buf[50] | 8
-> OR操作在低位已经为8时不前进地址
-> 后2字节比较重复读取buf[50..52]
-> 正确的"DATA_AT_50"被判断为不相等
```

修复必须恢复 ABI 契约，而不是修改测试、禁用优化或把 `ori` 特判为编译器错误。
统一 16 字节用户入口对齐、精确计算 ELF 栈 padding，并同步 signal frame，才是覆盖
完整影响面的根因修复。该修复已进入 `b6c5c973`；仍需长期保留入口 SP 直接断言和
signal/exec ABI 冒烟测试，防止后续布局变更重新破坏契约。
