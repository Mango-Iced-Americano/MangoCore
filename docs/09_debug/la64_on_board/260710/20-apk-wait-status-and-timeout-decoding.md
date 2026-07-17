---
title: "2K1000LA APK 原始 wait status 9 与 300 秒外层超时误判复盘"
category: debug
status: resolved
author: MangoCore Team
last_update: 2026-07-15
tags: [postmortem, la64, 2k1000la, apk, waitpid, signal, timeout, test-harness]
code_paths:
  - "user/src/bin/initproc.rs"
  - "os/src/task/process_manager.rs"
  - "os/src/task/signal/mod.rs"
related_docs:
  - "docs/09_debug/la64_on_board/260710/development-log.md"
  - "docs/08_testing/apk-isolated.md"
  - "docs/05_process/exit-wait.md"
evidence_commits:
  - "0778a319"
evidence_records:
  - "docs/Work_Log.md, 2026-07-14 isolated APK gate"
  - "docs/08_testing/apk-isolated.md, sections 3-4"
---

# 2K1000LA APK 原始 wait status 9 与 300 秒外层超时误判复盘

## 0. 一句话结论

实板第一轮 APK 门禁最终拿到的整数 `9` 不是“APK 主动 `exit(9)`”，而是 Unix
`waitpid` 原始状态中的 **signal number 9，即 SIGKILL**。当时 `initproc` 的 300 秒
保护刚好在安装完成附近到期，由测试外壳向被监控 shell 发送 SIGKILL；真正的
`exit(9)` 在原始 wait status 中应编码为 `9 << 8 = 2304`。

定位没有停在状态位推导：安装后的 BusyBox、LoongArch musl loader 和 P2 zlib
缓存均已存在，手工通过私有 loader 执行输出 `APK_BOARD_EXEC_OK`。随后门禁把实板
预算改为 900 秒，增加 `stage=verify`、`stage=exec`，并同时打印 raw status 与 shell
语义 exit code。最终 QEMU 和 2K1000LA 都输出完整阶段链和 `RESULT=PASS`。

---

## 1. 问题卡

| 属性 | 结论 |
|------|------|
| 现象 | 在线安装结束附近，自动门禁返回原始 wait status `9` |
| 初始误读 | “APK 返回了 exit code 9” |
| 实际含义 | 子进程被 signal 9（SIGKILL）终止 |
| SIGKILL 来源 | `run_bash_cmd_timeout(..., 300)` 的外层有限保护 |
| 为什么会撞线 | 2K1000LA 共享网络下载多个 HTTPS 索引耗时可达数分钟 |
| 反证 | 真正 `exit(9)` 的 raw status 是 `0x0900` / 2304 |
| 功能旁证 | 包文件、loader、BusyBox、缓存存在；手工 loader 执行成功 |
| 修复 | timeout 300 -> 900；补 verify/exec 阶段；解码并同时打印 raw/exit |
| 修复提交 | `0778a319`，`feat(apk): add isolated writable package-manager gate` |
| 最终实板结果 | `stage=verify`、`stage=exec`、`PASS ...`、`RESULT=PASS` |

本案是测试框架诊断错误，不是通过忽略 APK 非零返回来“修测试”。最终门禁仍要求
update、fetch、add、包数据库检查和新装动态程序执行全部成功。

## 2. 原始现场

第一版易失 APK 门禁把：

- `apk.static`、repositories 和签名公钥放在 initramfs；
- 安装根放在 RAMFS `/run/apk-root`；
- QEMU cache 放 `/tmp/apk-cache`；
- 实板 cache 放 P2 `/scratch/apk-cache`。

2K1000LA 首轮已经完成：

```text
HTTPS index
zlib fetch
musl + busybox + zlib add
post-install / trigger
```

但外层只得到：

```text
raw wait status = 9
```

如果把这个整数直接当进程退出码，就会得出“APK exit 9”的错误结论。问题在于
`waitpid` 的 status 不是 shell 的 `$?`，也不是 `exit()` 参数的原样返回。

## 3. 底层原理：Unix wait status 如何编码

### 3.1 正常退出

子进程调用 `exit(N)` 时，传统 Unix wait status 把低 8 位退出码放在高字节：

```text
raw_status = (N & 0xff) << 8
```

因此：

```text
exit(0) -> raw 0x0000 -> 0
exit(1) -> raw 0x0100 -> 256
exit(9) -> raw 0x0900 -> 2304
```

判断条件等价于 `raw & 0x7f == 0`，正常退出码为 `(raw >> 8) & 0xff`。

### 3.2 被信号终止

若进程因 signal `S` 终止，低 7 位保存 signal number：

```text
raw_status & 0x7f = S
```

所以 SIGKILL 的 raw status 正好是：

```text
SIGKILL = 9
raw      = 9
```

shell 为方便交互，通常把 signal termination 显示为 `128 + S`：SIGKILL 因而对应
`137`。这是二次解码结果，不是 waitpid 原始状态。

### 3.3 对照表

| 子进程结局 | raw wait status | shell 语义 exit |
|------------|-----------------|-----------------|
| `exit(0)` | `0` / `0x0000` | 0 |
| `exit(9)` | `2304` / `0x0900` | 9 |
| SIGKILL | `9` / `0x0009` | 137 |
| SIGSEGV，无 core bit 时 | `11` / `0x000b` | 139 |

因此仅靠数值 9 就能排除“正常 exit 9”，但还不能仅靠这个数值证明是谁发的
SIGKILL。要确定发送者，还必须把 status 与 timeout 分支、时间点和日志阶段关联起来。

## 4. 测试框架中的实际控制流

`user/src/bin/initproc.rs` 的 `run_bash_cmd_timeout()` 做了以下事情：

```text
fork
  child: exec /bin/bash -c <APK gate>
  parent:
    waitpid_wnohang(child)
    if elapsed >= timeout:
        kill(child, SIGKILL)
        waitpid_wnohang until reaped
    return raw wait status
```

第一版实板保护为 300 秒。也就是说，只要 shell 在 300 秒时仍未被 `waitpid` 观察为
退出，父进程就会主动发送 SIGKILL，最后返回的 raw status 正是 9。

这里存在两层“外层”：

```text
initproc timeout wrapper
  -> /bin/bash -c gate
       -> apk.static update/fetch/add
```

wait status 描述的是被监控的 shell，不是 APK 子命令某一次 `exit()` 的裸值。若 shell
正在等待 APK 或刚从 APK 返回、尚未来得及执行后续检查，都可能在边界时被外层杀死。

## 5. 调试追溯

### 5.1 先检查编码，否定“exit 9”

第一步不是去 APK 源码搜索错误码 9，而是把 raw status 写成十六进制：

```text
9 = 0x0009
```

低 7 位非零，说明是 signal termination；若为正常 `exit(9)`，应是 `0x0900`。这一
位级事实把排查方向从“APK 内部返回码”转到“谁发送了 SIGKILL”。

### 5.2 对齐 300 秒保护分支

`run_bash_cmd_timeout()` 明确在 `elapsed >= timeout_ms` 时执行：

```rust
let _ = kill(pid as usize, SIGKILL);
```

首轮异常正发生在 300 秒预算附近，且下载/安装日志已经到安装结束附近。这给出
“外层 guard 是 SIGKILL 来源”的代码证据和时间证据。

### 5.3 检查安装产物，判断 APK 主链是否已经完成

实板现场继续检查，而不是只改 timeout：

- 安装根中的 BusyBox 存在；
- LoongArch musl loader 存在；
- P2 中两个 zlib cache 文件均约 54.3 KiB；
- 手工执行 loader + BusyBox，输出 `APK_BOARD_EXEC_OK`。

这组证据说明下载、解包和动态执行基础链已经成立。它不能证明被 SIGKILL 时 shell
已经完成所有自动验证，因此后续仍必须补 `verify/exec` 阶段并重跑，不能把手工检查
直接当最终 PASS。

### 5.4 收敛无关网络变量

交互配置可保留 main/community/testing，但自动 smoke 只需要 edge/main 中的
busybox、zlib 及依赖。曾观察到额外 testing 索引可以长时间无进展。将自动门禁限定
为 edge/main 是缩小无关 CDN 变量，不是跳过 HTTPS、签名或依赖解析。

### 5.5 修改可观测性后重跑

最终门禁新增明确阶段：

```text
[apk-test] stage=version
[apk-test] stage=update
[apk-test] stage=fetch
[apk-test] stage=add
[apk-test] stage=verify
[apk-test] stage=exec
[apk-test] PASS root=... cache=...
[apk-test] RESULT=PASS
```

失败日志改为同时打印：

```text
wait_status=<raw> exit=<decoded>
```

`exit_code_from_waitpid_status()` 的终止态解码为：

```rust
if status & 0x7f == 0 {
    (status >> 8) & 0xff
} else {
    128 + (status & 0x7f)
}
```

于是同一个 SIGKILL 会显示 `wait_status=9 exit=137`，不会再冒充 APK exit 9。

## 6. 根因证据矩阵

| 证据 | 支持结论 | 边界 |
|------|----------|------|
| raw status 为 9 | 子进程因 signal 9 终止 | 单独不能证明 signal 发送者 |
| 300 秒分支显式 `kill(pid, SIGKILL)` | wrapper 能产生 raw 9 | 需与时间点对齐 |
| 异常发生在 300 秒预算附近 | 外层保护与现场吻合 | 没有独立 packet trace |
| APK 文件、loader、缓存存在 | 安装主链已走到后段 | 不能替代自动 verify |
| 手工 loader 输出 `APK_BOARD_EXEC_OK` | 新装动态程序可执行 | 是现场手工验证，不是原 shell 的完成状态 |
| 延长预算后出现 verify/exec/PASS | 原功能链在实板可闭环 | 不代表任意网络都能在 900 秒内完成 |
| QEMU 同门禁 PASS | APK/FS/syscall 通用路径可用 | QEMU 网络速度不代表实板 |

最窄根因表述是：

> 测试框架把 waitpid 原始状态直接当退出码展示，同时 300 秒预算低于该次实板 HTTPS
> 工作负载耗时，导致框架主动 SIGKILL 被误记成“APK exit 9”。

## 7. 修复

提交 `0778a319` 同时做了三类修正。

### 7.1 时间预算：300 秒改为 900 秒

900 秒仍是有限保护，不会让失联任务无限运行；它只是按已观察到的共享网络速度给
update/fetch/add 留出合理余量。

### 7.2 业务门禁：不能只看安装命令返回

新增：

- `apk info -e busybox`；
- 安装根 BusyBox 可执行位检查；
- 私有 LoongArch musl loader 存在性检查；
- 通过该 loader 真正执行 BusyBox，并匹配 `APK_EXEC_OK`。

这把“包管理器命令跑完”提升为“安装结果能动态执行”。

### 7.3 诊断输出：raw 与 decoded 并列

保留 raw status 便于精确判断内核 wait ABI，同时打印 shell 语义 exit 便于人读。没有
丢掉原始证据，也没有把 137 反向再误当成 APK 内部返回值。

## 8. 验证

### 8.1 QEMU

在 LA64 QEMU 中，不挂 tools 磁盘，仅依赖 initramfs 的 `apk-tools 3.0.6-r0`：

- edge/main HTTPS 签名索引完成；
- zlib fetch 完成；
- musl/busybox/zlib、post-install、trigger 完成；
- 私有 loader 动态执行完成；
- 输出 `[apk-test] RESULT=PASS`。

### 8.2 2K1000LA 实板

最终 uImage：

```text
size       16,719,520 bytes
payload    16,719,456 bytes
SHA-256    459d6a011c0a4b51cf5bc938d881075d950be541c6fee190dc332b11bdb8f1ac
TFTP CRC32 ecca3739
```

U-Boot `iminfo`、load/entry 与 checksum 通过。实板随后输出：

```text
[apk-test] stage=verify
[apk-test] stage=exec
[apk-test] PASS root=/run/apk-root cache=/scratch/apk-cache
[apk-test] RESULT=PASS
```

挂载检查还确认 P1/P3 为只读，只有 RAMFS/TmpFS 和 P2 scratch 可写。这个结果证明
没有为了“修超时”而放开系统盘写权限。

### 8.3 双架构编译

rv64、la64 kernel build 均完成，仅有项目既有 warning。运行时 APK 门禁是 LA64
QEMU/2K1000LA 专项；双架构编译不能被写成 RV64 APK 集成测试通过。

## 9. 排除项

### 9.1 不是 APK 正常返回 9

正常 `exit(9)` 的 raw status 必须是 2304。原值 9 的低 7 位明确表示 SIGKILL。

### 9.2 不是“忽略非零返回后假通过”

最终重跑要求 verify、loader exec 和 `RESULT=PASS` 全部出现。手工检查只是定位证据，
没有替代自动门禁。

### 9.3 不是 P2 FAT32 安装根问题

P2 在该阶段只保存可删除 `.apk` cache；安装根是 RAMFS。FAT32 缺少 Unix 元数据的
问题属于后续 Python/pip 状态布局，不是 raw status 9 的编码根因。

### 9.4 不是把网络卡顿说成内核 APK 失败

额外 testing index 的长停顿与 edge/main 包安装功能是两个变量。自动 smoke 收敛仓库
集合后仍保留真实 HTTPS、签名和依赖安装。

## 10. 已知边界

1. `status=9` 只能证明 SIGKILL；如果没有 timeout 分支日志和时间关联，也可能是其他
   进程或 OOM 路径发送的 SIGKILL，不能机械归因于超时。
2. 当前 decoder 面向“已终止子进程”；它没有实现一个完整的 `WIFSTOPPED/WIFCONTINUED`
   展示器，不应挪去解释任意 wait event。
3. 900 秒只是当前实板环境预算，不是 APK 性能 SLA。网络、DNS、CDN 变化仍可能超时。
4. 该 helper 超时时直接 kill 被监控 pid；若 shell 派生的后代仍存活，后续依赖
   orphan drain。若用于更复杂 workload，应显式建立并 kill 整个进程组。
5. 真正的 APK `exit(9)` 仍然可能发生；此时日志应显示 `wait_status=2304 exit=9`，必须
   按 APK stderr 和阶段继续排查，不能套用本案结论。

## 11. 闭合证据链

```text
实板 APK 在线安装接近完成
  -> 300 秒外层预算到期
  -> initproc timeout 分支发送 SIGKILL(9)
  -> waitpid raw status 低 7 位记录 signal 9
  -> 框架直接打印整数 9，被误读为 APK exit 9
  -> 位级解码证明正常 exit(9) 应为 2304
  -> 现场文件、缓存、loader 与手工执行证明 APK 主链已到后段
  -> timeout 调整为 900 秒，增加 verify/exec 和 raw+decoded 输出
  -> QEMU 完整门禁 PASS
  -> 2K1000LA 出现 verify -> exec -> PASS -> RESULT=PASS
  -> 结论闭合：原“9”来自外层 SIGKILL，不是 APK 正常退出码
```

组会汇报时最直观的一行是：

```text
raw 9 = SIGKILL；exit(9) 的 raw status 是 2304；最终复验不是“延时后不报错”，而是
verify + 私有 loader exec + RESULT=PASS 全链通过。
```
