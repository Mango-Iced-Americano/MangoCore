---
title: "SmolAgents WebSearch 超时：TCP/epoll 边沿事件与 DDGS 重定向修复"
category: debug
status: current
author: MangoCore Team
last_update: 2026-07-18
tags: [loongarch64, 2k1000la, smolagents, ddgs, primp, tls, epoll, tcp, chroot]
code_paths:
  - "os/src/fs/eventpoll.rs"
  - "os/src/net/socket/inet/stream/mod.rs"
  - "os/src/syscall/fs.rs"
  - "scripts/board/patch_ddgs_redirect.py"
  - "scripts/build_cpython_runtime_la64_strict.sh"
  - "scripts/board/verify_persist_python.sh"
  - "os/build_initramfs.sh"
  - "user/src/bin/initproc.rs"
related_docs:
  - "docs/09_debug/la64_on_board/260717/10-tty-smolagent-interactive-fix.md"
  - "docs/09_debug/la64_on_board/260717/11-smolagents-toolkit-dependency-closure.md"
  - "docs/06_net/dhcp.md"
---

# SmolAgents WebSearch 超时：TCP/epoll 边沿事件与 DDGS 重定向修复

## 1. 现象与结论

P4 strict-aligned runtime 已完整安装 `ddgs 9.0.0` 和 `primp 0.15.0`，默认 `curl`
也已能通过 DHCP DNS 访问公网，但 SmolAgents 的 `web_search` 仍在 Bing HTTPS 请求上报：

```text
DDGSException: https://www.bing.com/search RuntimeError:
error sending request for url (...): operation timed out
```

最终确认这不是一个单层问题，而是三个顺序暴露的问题：

1. MangoCore TCP producer 在一次真实 `EPOLLIN` 边沿上向 epoll 发送整个候选掩码
   `EPOLLIN | EPOLLRDHUP`，伪造了对端半关闭；Tokio/Mio/BoringSSL 随后把连接永久视为
   read-closed，并在实际 socket 已返回 `EAGAIN` 后继续错误重试直到超时；
2. 内核事件修复后，DDGS 9.0.0 能收到 Bing 的 HTTP 302，但其 primp client 显式配置
   `follow_redirects=False`，`_get_url()` 又只接受 200，因此把正常的
   `www.bing.com -> cn.bing.com` 地区跳转处理为 `None`；
3. 精确 SmolAgents 入口首次复测又暴露 `getcwd(2)` 在 chroot 中泄漏全局路径
   `/persist/apk-root`。该路径在 chroot 内不可访问，导致 `python-dotenv` 报
   `OSError: Starting path not found`。这是独立的 chroot/getcwd 语义错误，不是网络回退。

三层均修复后，最终 2K1000LA 实板的真实
`TOOL_MAPPING["web_search"]().forward(query="Luogu P1003 problem")` 在 2.774 秒完成，
返回 1420 字符搜索结果；原 `operation timed out` 不再出现。

## 2. 为什么 curl 成功不能排除内核问题

`curl` 与 primp 走不同用户态 reactor：当前 primp 是 Rust/rquest + Tokio 1.44.2 +
Mio 1.0.3 + BoringSSL。Tokio/Mio 对非阻塞 socket 使用 `EPOLLET`，依赖内核只在状态变化
时报告真实事件，并要求用户态一直读到 `EAGAIN`。curl 成功只能证明 DNS、路由、TCP 和
TLS 的一条客户端实现可用，不能证明另一套 epoll edge-triggered reactor 语义正确。

分层 A/B 的关键结果为：

| 层 | 修复前 | 修复后/最终实板 |
|----|--------|-----------------|
| curl + 默认 DNS | HTTP/2 200 | HTTP/2 200 |
| primp，本地主机 raw HTTP | 15.126 s 才返回 | 0.196 s；hostname 0.608 s |
| Python `ssl` + epoll ET，本地 TLS | 1.803 s 成功 | 保持成功 |
| primp，本地 TLS | timeout | 0.244 s；最终镜像 0.107 s，HTTP 200 |
| primp，`https://example.com` | timeout | 3.410 s；最终镜像 2.007 s，HTTP 200 |
| DDGS 默认 Bing | primp timeout；内核修复后变为 302/None | 1.348 s 返回 3 条；最终由 SmolAgents 封装通过 |
| SmolAgents WebSearchTool | `operation timed out` | 2.774 s，1420 字符 |

本地 HTTP/TLS 使用 macOS 受控主机 `192.168.2.1:18080/18443`，不会把公网服务延迟误算
为内核时延。公网样本只作端到端门禁。

## 3. 内核根因证据

### 3.1 事件时间线

在稳定复现后短时启用 `net_perf_diag`，得到同一次 TCP 数据到达的关键状态：

```text
socket current readiness: 0x145
producer notification:    0x2001
post-read result:          EAGAIN
post-read readiness:       0x104
```

`0x145` 表示当前确实存在读/写类就绪位，但不包含 `EPOLLRDHUP(0x2000)`；通知载荷
`0x2001` 却包含 `EPOLLRDHUP | EPOLLIN`。源码原因是旧 `wake_if_ready()` 只用候选组判断
“组内是否至少有一位 ready”，命中后却把整组候选位传给 EventWaitQueue：普通数据到达
因此被扩大为“可读且对端半关闭”。

BoringSSL/Tokio 收到伪造 RDHUP 后，即使目标 socket 已读空并返回 `EAGAIN`，仍反复尝试
读取直至外层 timeout。受控 Python `ssl + epoll` 成功而 primp 失败，进一步将差异收敛到
Tokio/Mio 对事件载荷和 edge re-arm 的使用，而不是 TLS 密码套件或服务端行为。

### 3.2 修复原则

`TcpSocket::wake_if_ready()` 现在先计算：

```text
became_ready = current & !previous
```

每条 wait queue 只收到 `became_ready` 与自身相关 mask 的交集。这样：

- 普通 `EPOLLIN` 不会再携带伪造 `EPOLLRDHUP/EPOLLHUP/EPOLLERR`；
- 持续 writable 不会在每次网络 poll 中退化成 level-like 通知；
- read/write 后从真实 smoltcp 状态刷新 pollee，短读或部分写不再被“syscall 成功”错误地
  当成仍然 ready；
- `shutdown()` 自身是状态转换，释放 socket inner lock 后显式唤醒等待者，避免严格边沿
  语义丢失 shutdown wakeup。

`EventPoll` 又区分两种输入：状态扫描使用 `record_observed_event()`，producer callback 使用
`record_notified_event()`。callback 本身就是新的边沿，不能因上一次 scan 的 `last_ready`
仍含同一 bit 就被抑制；否则 transition 与下一次 `epoll_wait` scan 竞争时会丢真实边沿。

该实现遵循 Linux epoll ET 的基本契约：只在受监视 fd 状态变化时交付事件，非阻塞 I/O
持续到 `EAGAIN` 后再等待下一次边沿。参考
[epoll(7)](https://man7.org/linux/man-pages/man7/epoll.7.html)。

## 4. DDGS 9.0.0 的独立 302 问题

内核修复后，直接观察 DDGS client 得到：

```text
status=302
location=https://cn.bing.com/search?...
```

固定 runtime 中 `ddgs.py` 构造 primp client 时设置 `follow_redirects=False`；同一请求把该项
改为 `True` 后 0.847 秒返回 3 条，证明剩余问题是用户态重定向策略，不是第二个内核
timeout。

修复采用精确版本和整文件哈希门禁：

- 只接受 `ddgs==9.0.0`；
- 原文件 SHA-256 必须为
  `eb9a3cc9bcd06f2d711d2a736e7758bd68ebcb46458883d6c183eeb62c383db2`；
- 只允许唯一一处 `follow_redirects=False -> True`；
- 修改后 SHA-256 必须为
  `3c321b9445ec57db0bd1d06899c6a10eeeea2817fa7ecbc1b2e08f37878bed24`；
- 当前 immutable release 不原地修改，而是在 P4 user site 原子发布 pure-Python overlay；
  未来 strict runtime 构建则在打包期应用相同哈希门禁；
- initproc 每次启动安装/复核，`verify_persist_python.sh` 使用 `--check` fail closed。

这不是通用地忽略 3xx/证书错误，而是让 HTTP client 执行标准重定向，同时保留最终响应
状态检查。DDGS 后续版本的 backend 策略可能变化，升级时必须重新审阅源码，不能复用
9.0.0 哈希。项目上游参考：[DDGS](https://github.com/deedy5/ddgs)。

## 5. chroot `getcwd` 修复

旧 `sys_getcwd()` 直接返回 `cwd_inode.absolute_path()`。该方法从全局 VFS root 重建路径，
因此进程执行 `chroot("/persist/apk-root")` 后，即使 cwd 就是新根，Python 仍看到：

```text
GETCWD /persist/apk-root
```

从新 root 查找这个字符串会落到不存在的
`/persist/apk-root/persist/apk-root`。修复后 `sys_getcwd()` 同时读取进程 `root_inode`，把
全局 cwd path 按目录组件边界转换为 root-relative path；cwd 与 root inode 相同时直接返回
`/`，无法证明 cwd 位于 root 内则返回 `ENOENT`，不泄漏或伪造路径。

最终实板：

```text
MangoPersist:/# python3 -c 'import os;print("GETCWD",os.getcwd())'
GETCWD /
```

随后同一正常 site 环境成功导入 `smolagents.cli` 和 `python-dotenv`，证明最初的
`Starting path not found` 已由内核语义修复，而不是通过禁用 dotenv 绕过。

## 6. 构建与最终实板验收

双架构严格串行构建均退出 0：

```text
make rv64-kernel-build-only
make la64-kernel-build-only
```

canonical 实板目标 `make la64-2k1000-apk-persist-shell` 退出 0，最终 uImage：

| 字段 | 值 |
|------|----|
| 文件 | `kernel-2k1000-persist-shell.ui` |
| 大小 | 16,788,240 B |
| SHA-256 | `28c2d836d63171af361a09a00f05fad2f6d6872160a19190e37785b0c3624391` |
| TFTP bytes | 16,788,240 |
| U-Boot CRC-32 | `ab660503` |
| uImage | LoongArch，load/entry `0x90000000`，checksum OK |

启动后依次通过：DHCP `192.168.2.2/24`、P4 rw ext4 `stage=reuse`、scratch smoke、strict
Python launcher、DDGS overlay、SmolAgents CLI patch 和 `[apk-persist-shell] RESULT=PASS`。

最终板端矩阵：

| 测试 | 结果 |
|------|------|
| chroot `os.getcwd()` | `/` |
| primp 本地 TLS | HTTP 200，3675 B，0.107 s |
| primp 公网 HTTPS | HTTP 200，559 B，2.007 s |
| DDGS 补丁幂等门禁 | `overlay-verified` |
| SmolAgents `WebSearchTool.forward` | 1420 字符，2.774 s |

LA64 QEMU 的 P4 shell kernel 也完成构建并启动到 initproc；旧 QEMU P4 fixture 不含当前
`28f61fb764f3` runtime，按设计在 Python current/bind 门禁 fail closed，因此不把该旧 fixture
记作应用 PASS。正式功能与性能结论以 2K1000LA 实板为准。

## 7. 剩余边界

- 搜索结果中的中文在串口抓取里出现乱码/控制字符，但工具返回、长度和 URL 均成功；这是
  高速串口字节完整性的独立问题，不能回写为 DDGS/TLS 失败。
- 本轮验证了受控 TLS、公网 HTTPS、DDGS 和 SmolAgents WebSearchTool；没有再次发送真实
  LLM API 请求，避免把模型服务排队混入网络栈结论。
- 当前 P4 overlay 只含 pure Python 文件，不引入未审计 ELF；未来升级 DDGS 后必须删除旧
  版本假设并重新建立源码哈希、redirect 行为和搜索 backend 实板门禁。
- 原始诊断数据保存在未跟踪目录
  `target/perf-runs/20260718T-smolagents-web-search-diag/`，包含受控 HTTP/TLS probe、临时
  diagnostic build log 和分层测试脚本；报告中的正式数字来自最终 production uImage。
