---
title: "2K1000LA P4 持久应用根、P3 私有运行时与 Python 状态分层复盘"
category: debug
status: resolved-with-known-limits
author: MangoCore Team
last_update: 2026-07-15
tags: [postmortem, la64, 2k1000la, p4, chroot, bind-mount, apk, cpython, loader, persistence]
code_paths:
  - "user/src/bin/initproc.rs"
  - "user/tools/cpython/python3-wrapper.sh"
  - "os/initramfs/apk/usr/bin/apk"
  - "os/initramfs/apk/usr/bin/persist-shell"
  - "os/initramfs/apk/usr/libexec/mango/persist-profile"
  - "os/src/fs/fat32/fat_inode.rs"
  - "os/src/fs/vfs/index_node.rs"
related_docs:
  - "docs/09_debug/la64_on_board/260710/development-log.md"
  - "docs/09_debug/la64_on_board/260710/09-ext4-variable-dirent-rename.md"
  - "docs/08_testing/apk-isolated.md"
  - "docs/08_testing/cpython-isolated.md"
  - "docs/08_testing/mangocore-python-guide.md"
  - "docs/03_fs/2k1000-full-test-disk.md"
evidence_commits:
  - "6b628240"
  - "0778a319"
  - "32b93c89"
  - "85314659"
  - "f133ba44"
  - "b62828cf"
evidence_records:
  - "docs/Work_Log.md, 2026-07-14 CPython/APK/P4 entries"
  - "logs/p4-qemu-install.log"
  - "logs/p4-qemu-reuse.log"
  - "logs/ext4-apk-board-final-20260715.log"
---

# 2K1000LA P4 持久应用根、P3 私有运行时与 Python 状态分层复盘

## 0. 一句话结论

2K1000LA 的持久软件环境不是 overlay root。宿主 `/` 始终是易失 RAMFS；P4 上的
`/persist/apk-root` 是一个独立、普通的 ext4 目录树，只有执行 `persist-shell` 时才由
静态 BusyBox `chroot` 作为应用视图的 `/`。P3 的只读 CPython 和 P4 的 Python 可写
状态不会因 chroot 自动出现，必须分别 bind 到应用根的 `/tools` 与
`/var/cache/mango-python`。

这条集成链先后暴露了四个**相互独立**的问题：chroot 看不到 P3、Python 子进程需要
私有库搜索路径、FAT32 不支持 `ensurepip copy2()` 所需时间戳元数据、CA bundle 存在
但 libfetch 默认 `/etc/ssl/cert.pem` 路径缺失。它们共享同一个最终环境，却不是一个
“P4 坏了”的总根因。最终方案按职责分层：P3 存只读运行时，P4 存 APK 根、pyc、
pip user tree 和临时事务，P2 FAT32 只存可重建下载缓存；启动以 commit marker 区分
install/reuse，并用真实 chroot 动态执行闭环。

---

## 1. 问题卡：这是设计链复盘，不是把四个故障揉成一个

| 子问题 | 现象 | 独立根因 | 修正 |
|--------|------|----------|------|
| A. 持久根模型 | 宿主可见 P4，但动态程序不能按完整根运行 | P4 是目录树，不是 overlay/宿主根 | 只在 `persist-shell` 中 chroot |
| B. P3 运行时可见性 | chroot 内找不到 `/tools/tests/cpython` | chroot 改路径解析根，不自动带入宿主挂载 | bind `/tools` 到应用根 |
| C. Python 私有 loader/DSO | wrapper 首次能起，ensurepip/pip 重启 Python 可能找不到 libpython | 子进程直接执行 `sys.executable`，不会重走 wrapper 的命令行 | Python 进程树内导出私有 `LD_LIBRARY_PATH` |
| D. pip 可写状态 | `ensurepip copy2()` 在 P2 返回 `utime ENOSYS` | FAT32 inode 没有完整 POSIX metadata/set_metadata 语义 | `TMPDIR/PYTHONUSERBASE` 移到 P4 ext4 |
| E. HTTPS CA | CA bundle 已复制，`apk update` 仍报证书不信任 | libfetch 查找 `/etc/ssl/cert.pem`，默认入口缺失 | 建相对 symlink 并启动自检 |
| F. 跨重启发布 | 中断安装可能留下半棵树 | 目录存在不等于事务完成 | `committed-v1` / `shell-ready-v1` 分阶段发布 |

另有 ext4 目录项/rename 故障曾影响 APK 文件提交，但它属于文件系统事务问题，证据和
当前状态单列在 `09-ext4-variable-dirent-rename.md`。本文不把该问题倒推成上表所有
症状的共同根因。

## 2. 最终存储拓扑

| 介质 | 宿主路径 | 应用根路径 | 职责 | 属性 |
|------|----------|------------|------|------|
| initramfs | `/` | 不整体映射 | 启动、静态工具、包装器 | RAMFS，易失 |
| P1 | `/sdcard` 等 | 不作为应用根 | 竞赛测试黄金源 | ext4，只读 |
| P2 | `/scratch` | `/scratch` bind | `.apk` 下载缓存、可删除工作区 | FAT32，可写 |
| P3 | `/tools` | `/tools` bind | CPython、私有 musl loader、标准库、工具 | ext4，只读 |
| P4 | `/persist/apk-root` | `/`（chroot 视图） | APK 数据库、程序、动态库、配置 | ext4，持久可写 |
| P4 | `/persist/python` | `/var/cache/mango-python` bind | pyc、pip user、Python tmp | ext4，持久可写 |
| 宿主伪 FS | `/dev`、`/proc` | 同名 bind | 设备、进程、DNS/网络状态 | 易失运行时 |
| 宿主临时 FS | `/tmp`、`/run` | 同名 bind | 临时文件、运行态 | 易失 |

核心数据流是：

```text
P3 read-only CPython + loader + stdlib
             │ bind /tools
             v
P4 /persist/apk-root --chroot--> MangoPersist:/
             ^
             │ bind /persist/python -> /var/cache/mango-python
P4 pyc + pip --user + tmp

P2 /scratch/apk-cache 只保存可重建归档，不承载 Unix 软件根
```

## 3. 底层原理

### 3.1 chroot、bind mount、overlay 是三件不同的事

`chroot(path)` 只改变该进程及后代的路径解析根。它不会：

- 把宿主 `/tools` 自动复制进新根；
- 把宿主 `/dev`、`/proc` 自动重新挂载；
- 合并只读 lower 与可写 upper；
- 提供 overlayfs 的 copy-up、whiteout 或 merged root 语义。

因此 P4 应用根必须自己包含 APK 安装结果，并在 chroot 前创建 mountpoint、逐项 bind
运行时目录。宿主 `/` 没有被替换，退出 `persist-shell` 后仍回到 RAMFS 根。

### 3.2 `PATH` 只能找文件，不能修复 ELF interpreter

动态 ELF 包含绝对 `PT_INTERP`，loader 还要解析它和目标程序的 DSO。把某个二进制
目录加进 `PATH` 只解决“shell 在哪里找到文件”，不解决“内核在哪里找到 interpreter”
和“loader 在哪里找到 libpython/扩展 DSO”。

Python wrapper 因而显式选择 P3 内的架构 loader：

```text
/tools/tests/cpython/lib/ld-musl-loongarch64.so.1
  --library-path <P3 usr/lib:P3 lib>
  /tools/tests/cpython/usr/bin/python3
```

这保证第一次启动不依赖宿主全局 loader 布局。

### 3.3 为什么还需要进程树内 `LD_LIBRARY_PATH`

`ensurepip`/pip 可能通过 `sys.executable` 直接启动新的 Python，而不是再次执行
`/usr/bin/python3` wrapper。命令行 `--library-path` 只属于第一次 loader 进程，不能
自动改写后续 `execve(sys.executable, ...)`。

`b62828cf` 因此在 wrapper 中导出 P3 私有 library path，使 Python 后代能解析
libpython 和扩展 DSO。作用域被刻意限制：

- `persist-shell` 进入 chroot 前先 `unset LD_LIBRARY_PATH`；
- profile 不给整个 shell 设置 P3 库路径；
- 只有启动 Python 的 wrapper 再导出；
- APK、curl 和其他程序仍使用应用根自己的 musl/DSO。

这避免“为了 Python 能跑，把整个 shell 的动态链接环境污染掉”。

### 3.4 下载缓存与安装/临时根要求不同文件系统语义

`.apk` cache 是不透明归档，FAT32 能保存即可。APK 安装根和 Python/pip 工作目录却
需要：

- Unix mode；
- symlink；
- 原子 rename；
- mtime/utime；
- 包数据库和事务中间文件。

早期 wrapper 优先把 `TMPDIR`、`PYTHONUSERBASE` 放在 `/scratch/python`。P2 是 FAT32，
而 MangoCore FAT inode 没有覆盖完整 `set_metadata()`；VFS 默认实现返回 `ENOSYS`。
Python `ensurepip` 的 `shutil.copy2()` 在复制后更新时间戳，于是出现 `utime ENOSYS`。

问题不在 wheel 内容，也不是 pip 不支持 LA64。把目录改到 P4 ext4 后，同一 bundled
wheel 能安装 pip，并继续在线安装 user package。

### 3.5 CA bundle 存在不等于默认入口存在

应用根已经有：

```text
/etc/ssl/certs/ca-certificates.crt
```

实板 `apk update` 仍报 `TLS: server certificate not trusted`。现场检查发现 libfetch
使用的默认入口 `/etc/ssl/cert.pem` 不存在。修复不是关闭证书校验，而是建立：

```text
/etc/ssl/cert.pem -> certs/ca-certificates.crt
```

并在 chroot 启动自检中要求该路径可读。之后 edge 索引和 curl 依赖继续以 HTTPS + CA
校验下载。

### 3.6 commit marker 是发布协议，不是目录存在性测试

首次安装时，P4 可能在任何一步复位。`/persist/apk-root` 存在只说明创建过目录，不能
说明包数据库、文件和 metadata 全部完成。

当前协议：

```text
无 committed-v1:
  删除旧的未提交根
  update + add
  sync package tree
  写 .committed-v1.tmp
  sync
  mv -> committed-v1
  sync

有 committed-v1:
  不重装
  直接校验 package DB、loader、BusyBox
```

随后安装 wrapper、profile、CA、keys 等交互启动文件，再原子发布 `shell-ready-v1`。
两个 marker 分别表示“基础 APK 树完成”和“可进入交互应用根”。

## 4. 调试与演进时间线

### 4.1 `6b628240`：先建立 P3 只读 CPython

P3 保存完整运行时，宿主 `/usr/bin/python{,3}` 指向 wrapper。初版可写状态优先回退
到 `/scratch/python`，且默认禁写 pyc。该阶段证明 LA64 CPython 主体可运行，但尚未
解决 P4/chroot 集成。

### 4.2 `0778a319`：先用 RAMFS 验证 APK 主链

安装根 `/run/apk-root` 易失，P2 只放 cache。QEMU/实板先闭合 HTTPS、签名、fetch、
add、trigger、包数据库和私有 APK loader 执行，避免一开始就把网络、包管理器、
持久化和 ext4 混在一起。

### 4.3 `32b93c89`：P4 install/reuse 门禁

新增固定身份 P4 和 `committed-v1` 协议。同一非 snapshot QEMU 盘先输出 install、再
输出 reuse；真实 SSD 也完成两次启动门禁。此时仍是聚焦测试，不是交互 shell。

### 4.4 `85314659`：独立 P4 chroot 应用根

增加 `apk_persist_shell`：

- P4 继续挂在 `/persist`，不覆盖宿主 `/`；
- 初版 bind `/dev`、`/proc`、`/tmp`、`/run`、`/scratch`；
- `persist-shell` 用静态 BusyBox chroot；
- 宿主 `apk` wrapper 显式带 `--root`；
- 配置 CA 默认 symlink 和 DNS symlink。

QEMU 验证既有树 reuse、空 P4 bootstrap，以及 chroot `/root` 标记跨重启保留。实板
验证 P4 reuse、HTTPS/curl；CA 路径缺口也在这一阶段由真实板暴露并修正。

### 4.5 `f133ba44`：只读 runtime，持久 pyc

运行时仍留 P3，只把 `PYTHONPYCACHEPREFIX` 外置到 P4。实板 33 个 pyc 在 RESET 后
仍存在，重导入中位数从约 18.322 s 降到 4.495 s。这是性能/状态分层验证，不等于
chroot 内 pip 已经完成。

### 4.6 `b62828cf`：补齐 P3/P4 bind 与 pip 状态语义

初版 chroot 只有五个运行时 bind，看不到 P3 CPython。该提交新增：

```text
/tools          -> /persist/apk-root/tools
/persist/python -> /persist/apk-root/var/cache/mango-python
```

并把当前 initramfs wrapper 安装到应用根、增加 `python` 链接，将
`TMPDIR/PYTHONUSERBASE` 从 FAT scratch 优先级移到 P4 ext4，同时让 Python 后代继承
私有 library path。

## 5. 代码证据

### 5.1 bind 列表明确证明它不是 overlay

`bind_apk_persist_runtime()` 当前逐项 mount 七条 source/target；没有 overlay mount，
也没有把 P4 mount 到 `/`。任一 bind 失败都会返回 false，准备函数转成失败 status，
不会静默进入残缺 chroot。

### 5.2 wrapper 的路径选择顺序

Python tmp/user 状态依次选择：

```text
/var/cache/mango-python   # chroot 中的 P4 bind
/persist/python           # 宿主中的 P4
/scratch/python           # 无 P4 时 FAT 回退
/tmp/python               # 最后易失回退
```

pyc 依次选择 P4、scratch、tmpfs。完整持久镜像必须命中 P4；回退路径只保证无 P4
镜像仍可运行，不应被记成“持久化已验证”。

### 5.3 两阶段状态文件

`committed-v1` 在基础包树 sync 后发布；`shell-ready-v1` 在 wrapper/profile/CA/keys
准备后发布。宿主 `apk` 和 `persist-shell` 都先检查 ready marker，避免用户绕过启动
修复直接进入半成品树。

## 6. 修复方案收敛出的不变量

最终实现不是一组互相兜底的临时路径，而是五条可检查的不变量：

1. **根边界**：P4 永远挂在 `/persist`；只有 `persist-shell` 的进程树 chroot，宿主根
   不切换，P1/P3 不放宽写权限。
2. **可见性边界**：应用根使用前必须完成七个 bind；失败立即使准备门禁失败，不能
   进入缺 `/tools`、`/proc` 或 Python 状态目录的半环境。
3. **动态链接边界**：Python wrapper 显式选择 P3 loader，并只向 Python 后代传递 P3
   library path；外层 shell 和 APK 程序不继承该路径。
4. **文件系统语义边界**：APK/Python 需要 POSIX metadata 的可变状态进入 P4 ext4；
   P2 FAT32 只保存可丢弃、可重建的归档缓存。
5. **发布边界**：基础包树通过 `committed-v1` 发布，交互启动文件通过
   `shell-ready-v1` 发布；reuse 路径必须实际校验数据库、loader 和程序执行。

这五条分别修复“根模型、路径可见性、loader 作用域、状态介质、跨重启发布”，没有
用其中一条掩盖另外四条。

## 7. 验证矩阵：QEMU 与实板不能混写

| 能力 | LA64 QEMU | 2K1000LA 实板 |
|------|-----------|---------------|
| P4 首次 install + 第二次 reuse | 同一非 snapshot 盘通过 | 真实 SSD 两次启动门禁通过 |
| 空 P4 bootstrap 到交互根 | 通过，edge/main 5920 packages | 已有 P4 reuse 路径通过 |
| chroot 五个基础 bind | 通过 | `85314659` 镜像通过 |
| CA symlink 可读 | 修复后 QEMU 通过 | HTTPS 索引和 curl 下载通过 |
| `/tools` + `/persist/python` 两个新增 bind | 完整 P3 临时四分区盘通过 | `b62828cf` 当时新镜像尚未 TFTP 验收 |
| chroot `python3 -S` | 输出 `PERSIST_PYTHON_OK` | 当轮未形成同口径串口证据 |
| bundled pip + online user package | pip 26.1.1、six 1.17.0 通过 | 当轮未形成同口径串口证据 |
| 重启后 pip/six/idna reuse | 同一非 snapshot P4 通过 | 当轮未形成同口径串口证据 |
| P4 pyc 跨物理 RESET | 不作为主要门禁 | 33 个 pyc 跨 RESET 保留 |

后续 `logs/ext4-apk-board-final-20260715.log` 明确记录实板：

```text
CPython launchers installed: /usr/bin/python3, /usr/bin/python
[apk-persist] stage=reuse
[apk-persist] RESULT=PASS
```

它证明最终板上镜像能看到 CPython launcher 且 P4 gate reuse；该日志没有出现
`PERSIST_PYTHON_OK` 或 pip 安装输出，所以本文不把它提升为“实板完整 chroot pip 门禁”。

### QEMU 具体闭环

完整 P3 + P4 QEMU 证据包括：

- 七个 bind 完成；
- chroot `python3 -S` 输出 `PERSIST_PYTHON_OK`；
- `PYTHONPYCACHEPREFIX=/var/cache/mango-python/pycache`；
- bundled wheel 安装 pip 26.1.1；
- `pip install --user six` 后 `PIP_INSTALL_OK 1.17.0`；
- 重启同一 P4 后 pip/six 仍存在；
- 再安装 idna，输出 `PIP_SIMPLE_OK 3.18`。

这是运行证据，不是仅检查目录或 ELF 文件存在。

## 8. 排除项与独立边界

### 7.1 不是 overlay root

没有 lower/upper/work/merged，也没有 copy-up/whiteout。P1/P3 仍只读，宿主 RAMFS 根
不变。P4 只是一个持久应用目录树。

### 7.2 不是“把 P3 改可写”

CPython、loader、stdlib 留在 P3；所有 pyc、pip user 和 tmp 写入 P4。更新缓存策略只
替换 initramfs wrapper，不需要重写 768 MiB P3。

### 7.3 CA 路径修复不等于关闭 TLS 校验

修复建立默认 trust-store 入口。HTTPS、SNI 和 CA 验证仍执行；DNS 代理兼容与时间
来源及其安全边界属于另外两条链，不能用 symlink 一项概括。这里尤其不能把未认证
NTP 或仅供功能兜底的 build epoch 写成“系统可信时间”。

### 7.4 FAT `ENOSYS` 与 ext4 rename 是两件事

`utime ENOSYS` 来自 P2 FAT metadata 能力不足，移动 tmp/user 状态到 P4 即消失。
APK 原子文件提交中曾出现的目录项问题属于 ext4 实现，必须按独立 postmortem 的源码
与测试结论判断，不能把所有 `ENOENT/ENOSYS` 写成同一根因。

## 9. 已知限制

1. P4 ext4 当前无 journal；marker + sync 协议降低半发布风险，但不是完整掉电事务系统。
2. `committed-v1` 只描述当前 schema/packages；未来升级需要版本迁移，不能永久把 marker
   当万能健康证明。
3. 直接执行 P3 Python ELF 或 wheel 生成的 `pip3` shebang 可能绕过 wrapper；规范入口是
   `python3 -m pip`。
4. P2 FAT32 只能承载可重建 cache；任何需要 mode/symlink/mtime 的新状态都应先审计，
   不能因“P2 可写”就放进去。
5. 完整 chroot Python/pip 的强证据目前是 LA64 QEMU；实板已有 P4、CA、host CPython、
   pyc persistence 证据，但缺同口径 `PERSIST_PYTHON_OK + pip install/reboot` 串口闭环。
6. 这套 app-root 不提供系统级 overlay、配额、快照或通用软件升级回滚。

## 10. 闭合证据链

```text
先在 RAMFS 验证 APK 网络/签名/安装/loader 主链
  -> P4 固定身份 + committed marker 完成 install/reuse
  -> P4 保持 /persist 下的独立目录树，不替换宿主 /
  -> chroot 只改变路径根，必须 bind dev/proc/tmp/run/scratch
  -> 初版 chroot 看不到 P3 CPython
  -> 增加 /tools bind；P3 运行时在应用根可见
  -> Python pyc/tmp/user 需要可写且支持 POSIX metadata
  -> FAT scratch 上 ensurepip copy2 命中 utime ENOSYS
  -> 增加 /persist/python bind，TMPDIR/PYTHONUSERBASE 转 P4 ext4
  -> ensurepip/pip 会直接重启 sys.executable
  -> wrapper 仅在 Python 进程树导出 P3 私有 library path
  -> CA bundle 有文件但缺 libfetch 默认 cert.pem
  -> 补相对 symlink并保持 HTTPS/CA 校验
  -> QEMU 完成 Python 真执行、pip 安装与重启 reuse
  -> 实板完成 P4 reuse、CA/HTTPS、host launcher 与 pyc 跨 RESET
  -> 各问题分别闭环；不把 app-root、loader、FAT、CA、ext4 伪装成一个根因
```

组会汇报可压缩为一句：**P3 提供不可变运行时，P4 提供有 Unix 语义的持久状态，P2
只做可重建缓存；chroot 负责视图，bind 负责可见性，wrapper 负责私有动态链接作用域，
marker 负责 install/reuse 发布边界——它明确不是 overlay。**
