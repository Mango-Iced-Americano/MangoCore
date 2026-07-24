---
title: "SmolAgent 交互首字符自锁、TTY line discipline 与 P4 源码恢复"
category: debug
status: verified
author: MangoCore Team
last_update: 2026-07-18
tags: [loongarch64, 2k1000la, tty, waitqueue, deadlock, smolagents, python, ext4]
code_paths:
  - "os/src/fs/dev/tty.rs"
  - "os/src/task/manager.rs"
  - "os/src/task/processor.rs"
  - "os/src/trace.rs"
  - "os/build_initramfs.sh"
  - "user/src/bin/initproc.rs"
  - "user/tools/cpython/python3-wrapper-persist.sh"
  - "scripts/board/patch_smolagents_action_type.py"
  - "scripts/board/verify_persist_python.sh"
related_docs:
  - "docs/03_fs/devfs.md"
  - "docs/09_debug/la64_on_board/260717/08-persist-strict-python-default.md"
  - "docs/09_debug/la64_on_board/260717/09-aligned-pillow-and-smolagent-closure.md"
---

# SmolAgent 交互首字符自锁、TTY line discipline 与 P4 源码恢复

## 1. 结论

本轮同时闭合了三个相互独立、但在用户界面上叠加为“SmolAgent 交互卡住”的问题：

1. **已确认的内核自锁**：Python 阻塞 read 被 UART 字符唤醒后，TTY 在消费第一个字符
   时通知自己的 read wait queue；此时 `WaitQueue::wait_event_impl()` 正持有同一非重入
   `spin::Mutex` 做 lost-wakeup 复查，单核因此永久自旋。这个根因精确解释“刚输入第一个
   `c`，还没按 Enter 就整机卡住”。
2. **缺失的 line discipline**：旧 TTY 声明了 `ICRNL` / `ICANON` / `VMIN` / `VTIME`，
   实际读路径却只返回一个原始字节。自锁消除后，如果不补 `CR -> NL` 和规范行缓冲，
   Rich/Python `input()` 仍可能把屏幕换行误表现为未收到行结束。
3. **SmolAgents 1.26.0 上游 CLI 逻辑错误**：选择 action type 后，函数又无条件写回
   `action_type = "code"`；同一版本还在菜单中提供 `OpenAIServerModel`，而 `load_model()`
   只接受 `OpenAIModel` 名称。本轮只对精确官方源码哈希应用两处最小补丁，未知版本
   fail-closed。

此外，测试期间一次人工 RESET 暴露了 P4 ext4 的数据完整性异常：
`smolagents/cli.py` 的内容变成 CPython 3.14 `.pyc` 字节码，而 inode 大小仍等于修补后
源码大小。该现场没有被误记为内核自动重启；P4 同目录官方源码备份保持完整，已加入
自动恢复和禁止新 pyc 写入的安全门禁。

## 2. 现象边界

### 2.1 用户实际观察

- `smolagent <prompt>` 单命令模式正常；
- 进入无参数交互界面，出现 code/tool-calling 选择提示；
- 用户准备输入完整 `code`，但只输入第一个字符 `c`、尚未按 Enter，内核就不再响应；
- 这不是“Prompt 需要整行，所以只按 c 本来就在等 Enter”的普通等待：旧实现会在收到
  第一个 UART 字符后进入内核自旋，后续 Enter、Ctrl-C 和 shell 都不能推进。

### 2.2 SmolAgents/Rich/CPython 输入语义

SmolAgents 1.26.0 使用 Rich `Prompt.ask()`；Rich 最终调用 Python `input()`，CPython 走
整行 `fgets/read`。因此正确语义是输入完整 `code` 或 `tool_calling` 后按 Enter。TTY 修复
后的“只按 c 时继续等行结束”是规范模式正确行为，不能再用“用户态是否立即打印结果”
判断 hang；应同时验证内核仍响应、字符可继续输入、Enter 后整行返回。

## 3. 内核自锁证据链

旧执行链如下：

```text
SmolAgent / Rich / Python input()
  -> read(0, ...)
  -> sys_read
  -> WaitQueue::wait_until_interruptible
  -> wait_event_impl
       lock(read_waiters)
       cond()                 # lost-wakeup 二次复查，仍持 queue lock
         -> File::read
         -> Teletype::read_at
              consume 'c'
              notify_events_at_most(EPOLLIN)
                -> lock(read_waiters)   # 同一 spin::Mutex 再入
```

字符到达前，task 已加入 TTY read queue。LoongArch 调度循环从 UART 取到 `c`，stash 后唤醒
task。task 恢复时进入 `wait_event_impl()` 的持锁条件复查；`Teletype::read_at()` 成功消费
字符后又调用 `notify_events_at_most()`，后者试图再次锁住同一 wait queue。`spin::Mutex`
不可重入，且当前为单核，所以不是短暂竞争，而是永久自旋。

这个设计缺陷并非 `WaitQueue` “不该持锁复查”。持锁复查负责闭合：

```text
无锁检查未就绪 -> 生产者通知 -> 消费者尚未入队
```

这一 lost-wakeup 窗口。真正违反契约的是 cond/read consumer 在复查期间通知或重取同一个
queue。`WaitQueue` 文档已明确：条件闭包只能查询或消费底层状态，不能通知同队列。

## 4. TTY 修复设计

### 4.1 单向数据流

```text
UART producer
  -> magic-key check
  -> trace stash
  -> Teletype::receive_stashed()
  -> input mapping / line discipline / queue
  -> unlock TTY.inner
  -> notify read waiters + epoll listeners

read_at() -> consume only
poll()    -> query only
```

- 删除消费字符后的 `EPOLLIN` 通知；
- `poll()` 不再读取 UART、不再 stash、不再通知；
- 字符真正进入可读状态后，由生产侧统一 `notify_events_all()`；
- canonical 模式只在完整记录或 EOF 到达时通知；
- noncanonical 模式每个成功入队字节都通知，避免 `VMIN > 1` 只唤醒首字节；
- termios 模式切换若让现有缓冲从不可读变为可读，在释放 `TTY.inner` 后补通知。

### 4.2 固定容量输入缓冲

旧 `last_char` 只能保存一个字节。新 `TtyInputBuffer` 使用固定 1024 字节环形队列，不在
调度器/UART 路径分配内存，并分别维护：

- `len`：队列总字节数；
- `canonical_ready`：队首完整记录的可读字节数；
- `current_line_len`：队尾当前可编辑行；
- `eof_pending`：空行 `VEOF` 产生的一次零长度 read。

规范 read 最多跨出一个记录；用户缓冲小于记录时允许分多次返回。`VERASE` / `VKILL`
只修改当前未完成行，不会破坏队首已完成记录。

### 4.3 输入 flag 与 canonical

输入先按 termios 应用：

- `IGNCR`：丢弃 CR；
- `ICRNL`：CR 转 NL；
- `INLCR`：NL 转 CR。

默认 termios 已设置 `ICRNL`，所以串口工具发送 `CR` 时，Python `input()` 最终读取到 NL。
`ICANON` 下实现 newline、`VEOL`、`VEOL2`、`VEOF`、`VERASE`、`VKILL` 及最小 echo。

### 4.4 noncanonical VMIN/VTIME

当前覆盖四个常见组合：

| VMIN | VTIME | 行为 |
|---:|---:|---|
| >0 | 0 | 等到阈值；read 缓冲更小时阈值取缓冲大小 |
| 0 | 0 | 无数据立即返回 0；有数据立即返回 |
| 0 | >0 | 从 read 开始计总超时；到期可返回 0 |
| >0 | >0 | 首字节后启动/刷新 inter-byte timer；阈值或超时返回 |

超时依赖现有 wait-I/O fallback 定时复查。当前 activity/deadline 位于全局 `TeletypeInner`，
不是 per-open/per-read；多个并发 reader 和 `O_NONBLOCK` 探测的完整 Linux N_TTY 语义仍
需后续拆分。

### 4.5 VINTR 锁序

Ctrl-C 的判定、`NOFLSH` 清输入和信号目标快照在 `TTY.inner` 锁内完成；进程组扫描、
SIGINT 投递、`^C` 回显和日志在解锁后执行。这样不会持 TTY 锁进入 task/process registry。

## 5. SmolAgents 1.26.0 精确修补

### 5.1 两处逻辑错误

最终补丁只改变两个精确锚点：

1. 删除 advanced-options 前无条件 `action_type = "code"`，保留交互开头的用户选择；
2. 让 `load_model()` 对菜单返回的 `OpenAIServerModel` 使用与 `OpenAIModel` 相同分支。

状态机只接受三种审阅状态：

| 状态 | SHA-256 |
|---|---|
| PyPI 官方 1.26.0 `cli.py` | `c7cd04f6312242fbdb16917c48b9b5a672cb5a0652f9553c718b68dd3e2b5d62` |
| 仅 action-type 旧补丁 | `9c3735c6aff445fe01a064f0ab61d4280e36588a1952c8f0220d3ecf8e563a57` |
| 本轮最终双补丁 | `e4052f70bb355b35ec3a9720475a22e898574444d024f4e8a38af41e05de7eba` |

版本不是 `1.26.0`、metadata 模糊/丢失、包目录符号链接逃出 P4、锚点或哈希不符时均拒绝
修改。包确实未安装时 `--allow-missing` 可返回 missing；包目录存在但 metadata 消失不能
伪装成正常缺包。

### 5.2 RESET-safe 发布与入口门禁

- 首次修补前把精确官方源保存在同目录 `cli.py.mango-1.26.0.orig`；
- 同目录 `mkstemp`，完整写入、file fsync、SHA-256 复核后 `os.replace()`；
- replace 后全局 `sync()`；RESET 落在发布前时 active 文件不被 truncate；
- 临时名带随机部分，不会因重启后 PID 重复永久 `EEXIST`；
- active 源缺失或损坏时，只允许从精确官方 `.orig` 重建；
- 同时清理包旁和 `PYTHONPYCACHEPREFIX` 下的 `cli.*.pyc`，并再次 sync；
- `--check` 纯只读，发现缺 backup、残留 pyc 或非最终哈希即失败。

initproc 先安装 strict Python/pip，只有修补器成功且不是 missing 时才发布根环境
`smolagent(s)`；失败会删除旧入口。持久 APK chroot 使用同一 `--check` gate，通用 console
entry 循环显式跳过 SmolAgent，防止从另一条路径绕过 fail-closed。

## 6. 人工 RESET 后的 P4 ext4 现场

用户明确说明测试中发生了一次人工 RESET。重启后 `smolagent` 报：

```text
SyntaxError: source code string cannot contain null bytes
```

板端只读检查：

```text
cli.py size=9709, NUL count=2966
head=2b 0e 0d 0a 00 00 00 00 1b 00 00 00 ed 25 00 00 e3 ...
cli.py.mango-1.26.0.orig size=9658
```

解释：`2b 0e 0d 0a` 是现场 CPython 3.14 bytecode magic；其后为 pyc flags/timestamp，
`ed 25 00 00` 小端值正是 9709，表示编译时源大小；`e3` 开始 marshal code object。即
`cli.py` 数据区出现了 `.pyc` 内容，而文件长度仍是修补后源码长度。这不是缺 pydantic、
不是 aligned ABI，也不是普通 Python 语法错误。

当前证据能确认“pyc 数据覆盖源文件内容”，但仅凭在线现场还不能在 ext4 allocator、
PageCache identity、writeback 或突然复位顺序之间做最终归因，因此根因等级记录为
**ext4/PageCache 高概率，具体子路径待新 ext4 实现或离线 fsck/fault injection 复核**。

在 ext4 迁移完成前，P4 strict wrapper 强制 `PYTHONDONTWRITEBYTECODE=1`。`-B` 仍允许读取
已存在且有效的 pyc，只禁止产生新 cache；因此热 cache 可继续使用，缺 cache 的 import
会退回源码解析。这个安全门禁会影响部分冷启动性能，不能混入原 production Python
性能基线。

## 7. 验证记录

### 7.1 宿主确定性测试

- `python3 -m py_compile scripts/board/patch_smolagents_action_type.py`；
- Shell `sh -n`：strict wrapper、P4 verify；
- 官方 wheel原始态 -> 最终双补丁；
- action-only 旧补丁 -> 双补丁升级；
- 模拟 pyc/NUL 损坏 -> exact `.orig` 自动恢复；
- active `cli.py` 缺失 -> exact `.orig` 自动恢复；
- adjacent/prefix pyc 残留：`--check` 拒绝，normal 模式清理后 `--check` 通过；
- package 存在但 metadata 缺失：即使 `--allow-missing` 仍 fail-closed；
- 最终源码/备份 SHA-256 分别为 `e405...5de7eba` / `c7cd...e2b5d62`。

### 7.2 编译与 QEMU/实板

最终源码已在项目 Docker 镜像内严格串行通过 RV64/LA64 production kernel build，并完成
`la64-qemu-apk-persist-shell` 镜像构建。核心 TTY 修补路径已完成以下 QEMU 与 2K1000LA
功能证明；提交前最后加入的 VINTR 解锁整理和 P4 fail-closed/pyc 门禁只完成双架构编译，
没有伪记为新一轮完整实板重跑：

- la64 QEMU canonical：输入 `code`、Enter 后返回完整行；
- la64 QEMU raw `VMIN=1`：首个 `c` 立即返回 `63`；
- la64 QEMU `VMIN=0,VTIME=1` 空输入返回 0；
- la64 QEMU `VMIN=3,VTIME=1` 首字节后超时返回 1 字节；
- 2K1000LA CPython `input()`：只输入首个 `c` 后等待 3 秒，字符仍可继续输入，Enter 后
  返回 `code`；
- 2K1000LA raw read：首个 `c` 立即返回 `63`；
- 2K1000LA 真实 SmolAgent UI：只输入 `c` 不再整核自旋，完整 `code` + Enter 推进到工具
  选择；Ctrl-C 返回 shell；
- monkeypatch 无网络测试确认 `tool_calling` 不再被覆盖为 `code`。

## 8. 未完成边界

- TTY 是最小 line discipline，不是完整 Linux N_TTY；
- 1024 字节满队列当前丢弃新字节，没有 IMAXBEL/overflow 统计；
- UTF-8 erase 仍按 byte，不实现 IUTF8 codepoint erase；
- `VMIN/VTIME` 计时不是 per-open/per-read；
- `TIOC*` / `TCFLSH` / output `OPOST|ONLCR` 仍不完整；
- fg pgid 未设置的 scheduler VINTR fallback 仍可能向较宽的 interruptible task 集合广播；
- 实板 UART 目前由 scheduler 每轮取一个字节，宿主高速粘贴仍可能丢字符，这与本次
  WaitQueue 自锁不同；自动化命令仍需字符节流和阶段 ACK；
- P4 ext4 的 pyc/source cross-file 覆盖尚未从内核子路径闭环；新 ext4 合入后必须恢复
  bytecode-write A/B、重跑 import 性能，并做 reset/fault-injection 与离线 fsck。
