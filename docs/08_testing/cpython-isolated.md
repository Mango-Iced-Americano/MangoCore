---
title: "隔离 CPython 运行时测试"
category: testing
status: draft
author: MangoCore Team
last_update: 2026-07-14
tags: [testing, cpython, qemu, loongarch64, riscv64, 2k1000]
code_paths:
  - "scripts/fetch_cpython_runtime.py"
  - "os/build_initramfs.sh"
  - "scripts/make_2k1000_tools_partition.py"
  - "scripts/write_2k1000_p3.py"
  - "user/tools/cpython/cpython_testcode.sh"
  - "user/tools/cpython/run_cpython.sh"
  - "user/tools/cpython/python3-wrapper.sh"
  - "user/src/bin/initproc.rs"
  - "os/src/fs/vfs/file.rs"
  - "os/src/syscall/fs.rs"
  - "os/src/net/syscall/getsockopt.rs"
  - "os/src/fs/fat32/fat_inode.rs"
  - "os/src/fs/tmpfs/mod.rs"
  - "os/src/hal/arch/loongarch64/trap/context.rs"
  - "os/src/hal/arch/loongarch64/trap/trap.S"
  - "os/src/syscall/process/signal.rs"
  - "user/tools/cpython/L7_filesystem.py"
  - "user/tools/cpython/L9_socket.py"
entry_points:
  - "rv64-cpython-run"
  - "la64-cpython-run"
  - "la64-2k1000-cpython-tests"
  - "la64-2k1000-cpython-tools"
  - "2k1000-cpython-p3-write"
arch:
  rv64: supported
  la64: supported
related_docs:
  - "docs/03_fs/2k1000-full-test-disk.md"
  - "docs/01_architecture/hal-and-platform.md"
  - "docs/05_process/signal.md"
---

# 隔离 CPython 运行时测试

## 1. Overview

该测试链将 Alpine 目标架构的 CPython、musl 加载器、动态库和 CA 证书组装为独立运行时，并在 MangoCore 上执行 L3-L9 分层验收。运行时不安装到内核 initramfs，也不改写官方测试分区；QEMU 从 tools 镜像读取，2K1000LA 从 SSD P3 `/tools/tests/cpython` 读取。

2026-07-14 验证的运行时为 CPython 3.14.5。rv64、la64 QEMU 和 2K1000LA 实板均完成 72/72 个 PASS 记录，覆盖语言核心、标准库、文件系统、signal round-trip、线程、子进程、DNS、TCP、HTTP 和默认 CA 校验的 HTTPS。实板 L7 在真实 FAT32 `/scratch` 执行，除整套测试外还通过 50 轮无 `fsync` 覆盖 rename、空文件覆盖和旧目标 open-fd 专项验证。

## 2. Design Goals

- **隔离性**：测试运行时与竞赛既有 musl/glibc 分组分离；`/usr/bin/python3` 和 `/usr/bin/python` 仅是启动包装器，不把私有动态库并入全局 `/lib`。
- **可复现入口**：通过独立 Make 目标生成运行时、QEMU 镜像、实板 uImage 和 P3 分区 payload。
- **Linux ABI 验收**：使用未为 MangoCore 特制的 CPython/musl 暴露 `getdents64`、`FIONBIO`、`SO_TYPE`、signal、futex、subprocess 与 TLS 路径问题。
- **存储安全**：实板 P1/P3 保持只读，普通持久化文件操作写入 P2 `/scratch/cpython`；FAT32 无法表示的 POSIX symlink 用例在易失 TmpFS `/tmp` 执行。
- **架构一致性**：rv64 和 la64 使用同一组脚本和 judge，只替换目标架构运行时。

## 3. Architecture

```text
Alpine APKINDEX
  -> fetch_cpython_runtime.py
    -> user/tools/<arch>/tests/cpython/{lib,usr,etc}
      + user/tools/cpython/{L3..L9,runner}
        -> tools-rv.img / tools-la.img
        -> 2K1000LA P3 replacement image

MangoCore initproc
  -> install /usr/bin/{python3,python} launcher
    -> private musl loader + library path + PYTHONHOME
  -> /cpython_test volatile marker
    -> cpython-isolated group only
      -> musl loader --library-path ... python3
        -> L3 ... L9
          -> judge_cpython-isolated.py
```

QEMU tools 磁盘可写，因此默认可在 `/tools/tests/cpython` 建立普通临时文件。实板 P3 为只读 ext4，`cpython_testcode.sh` 优先选择 `/scratch/cpython`，其次是可写 tools，最后回退到 `/tmp/cpython`。L7 的普通 I/O 和覆盖 rename 保留在 FAT32 工作区：先验证空文件覆盖，再以相同源/目标名称重复 20 轮、不调用 `fsync`，使簇复用和 inode/PageCache 别名成为稳定门禁。symlink/readlink/stat-lstat 子集始终使用 TmpFS `/tmp`，避免把 FAT32 不支持 POSIX symlink 的格式限制误判为 VFS 缺陷。检测到 `/scratch` 时还会自动设置 `CPYTHON_L9_REQUIRE_NET=1`，使实板 DNS/HTTP/HTTPS 失败成为组失败，而不是 SKIP。

## 4. Key Data Structures

| 结构/状态 | 定义位置 | 用途 | 关键字段 |
|-----------|----------|------|----------|
| `Package` | `scripts/fetch_cpython_runtime.py` | APKINDEX 中的软件包节点 | `name`, `version`, `dependencies`, `provides` |
| `File::dirent_snapshot` | `os/src/fs/vfs/file.rs` | 每个 open file description 的稳定目录名快照 | `Mutex<Option<Vec<String>>>` |
| `LsxRegs` | `os/src/hal/arch/loongarch64/trap/context.rs` | 32 个 128-bit LSX 寄存器的 trap/signal 快照 | `v: [[u64; 2]; 32]` |
| `UserContext` offsets | 两架构 `trap/context.rs` | signal 投递与 `sigreturn` 共享 ABI 偏移 | `MCONTEXT_OFFSET`, `LSX_OFFSET` |
| P3 manifest | `scripts/make_2k1000_tools_partition.py` | 限制实板写入边界 | `image_bytes`, `target_start_lba`, `target_sectors`, `sha256` |
| P3 writer | `scripts/write_2k1000_p3.py` | 型号/MBR/清单门禁、三块 TFTP/SCSI 读回、安装文件校验 | 固定 `0xA80800..0xC00800` |

## 5. Execution Flow

### 5.1 QEMU 门禁

```bash
make -C os rv64-cpython-run
make -C os la64-cpython-run
```

两条命令必须串行执行。目标会显式下载或复用对应 Alpine 运行时缓存，重建 tools 磁盘，以 `cpython_test` feature 生成内核，并用 ramfs `/cpython_test` 标记覆盖磁盘中的 `os_test.conf`。

### 5.2 运行时分层

| 层级 | 内容 |
|------|------|
| L3 | Python、musl loader、stdlib、CA 证书和动态库完整性 |
| L4 | 加载器启动、版本、退出码、`sys.prefix` |
| L5 | 算术、字符串、容器、异常、闭包 |
| L6 | 核心 stdlib、随机数、hash、zlib、select、signal、tempfile、sqlite3 |
| L7 | open/read/write、append、空文件及 20 轮无 fsync 覆盖 rename、symlink、stat、getdents、truncate、fsync、unlink |
| L8 | pthread/futex、lock/queue、daemon thread；subprocess/pipe/wait/exit code |
| L9 | socket/socketpair、DNS、TCP/HTTP、TLS/HTTPS |

### 5.3 2K1000LA 构建

```bash
make -C os la64-2k1000-cpython-tests
make -C os la64-2k1000-cpython-tools
```

第一条生成 `kernel-2k1000-cpython-tests.ui`，启用 AHCI/P2 scratch、GMAC DHCP 和 CPython 聚焦标记。第二条生成严格 768 MiB 的 `mango-2k1000la-cpython-tools-p3.img` 及 JSON 清单。P3 写入流程见 `docs/03_fs/2k1000-full-test-disk.md`。

```bash
make 2k1000-cpython-p3-write CONFIRM_P3_START=0xA80800
make 2k1000-boot IMAGE=kernel-2k1000-cpython-tests.ui
```

第一条只允许覆盖已验证 MBR 的 P3，并对三个 256 MiB 块逐块读回；第二条启动聚焦镜像，自动执行 L3-L9。

### 5.4 Shell 全局命令

P3 在 `/tools/tests/cpython` 保存只读运行时，并预置 `/tools/usr/bin/python3` 与 `python` 链接。`initproc` 对保留可写 `/usr` 的 staged 实板路径再次安装同名链接作为兜底。两者最终都进入 Shell 已有的全局 `PATH`，用户可直接执行：

```bash
python3 --version
python3 -c 'print("hello MangoCore")'
python3 /scratch/example.py
```

全局链接优先指向 uImage 随带的 `/rescue/python3-wrapper`，P3 中的同名脚本只作
兼容回退。这样更新启动和缓存策略不需要重新写入 768 MiB P3。包装器按架构选择
P3 内的 musl loader，显式传入私有 library path、`PYTHONHOME` 与 CA bundle，并把
`TMPDIR`、`PYTHONUSERBASE` 优先设置到 `/scratch/python`；不会依赖目标 ELF 中不
存在于根文件系统的绝对解释器，也不会用全局 `LD_LIBRARY_PATH` 污染其他程序。

标准库仍在只读 P3，但包装器默认允许写 pyc，并按 `/persist/python/pycache`、
`/scratch/python/pycache`、`/tmp/python/pycache` 的顺序选择外置
`PYTHONPYCACHEPREFIX`。CPython 自身按源文件时间和大小验证缓存，P3 更新后不会
盲用旧字节码；调用者也可显式设置 `PYTHONDONTWRITEBYTECODE=1` 禁用。实板首次
导入 `json,ssl,hashlib,pathlib` 会创建 33 个 pyc，约 19.1 s；后续稳定约
4.495 s，而原始无缓存中位数为 18.322 s，下降约 75.5%。`python3 -S -c pass`
从约 1.925 s 降到 1.159 s，`python3 -c pass` 从约 2.385 s 降到 1.607 s。
L4 同时执行 `/usr/bin/python3` 和 `/usr/bin/python`，全局入口失效会直接使
CPython 分组失败。

## 6. Interfaces & APIs

| 接口 | 语义 | CPython 依赖 |
|------|------|--------------|
| `getdents64` | 每个 open file description 使用稳定索引 cookie | `os.listdir`, `os.scandir`, import discovery |
| `ioctl(FIONBIO)` | 设置或清除共享 `O_NONBLOCK` 状态 | socket/pipe 非阻塞包装 |
| `getsockopt(SO_TYPE)` | 返回 4-byte Linux `SOCK_*` 值 | `socket.socket(fileno=...)` 类型识别 |
| `getrandom`/`/dev/urandom` | 平台熵源播种的 CSPRNG | hash randomization、`random`、TLS |
| signal frame | 恢复 machine context、sigmask 和 LA64 LSX | Python signal handler 投递与 libc restorer |
| LSX trap context | syscall/中断/调度时保存完整 128-bit 向量；恢复时不得再用标量 FPR 覆盖低 lane | Alpine LoongArch musl 优化的 `memset` 等路径 |

## 7. Test Mapping

| 特性 | Syscall / API | LTP 用例 | OSCOMP 分组 | 状态 |
|------|---------------|----------|-------------|------|
| 目录 cookie | `getdents64` | `getdents01`, `getdents02` | `cpython-isolated/L6,L7` | partial |
| 非阻塞状态 | `ioctl(FIONBIO)` | `ioctl01` | `cpython-isolated/L8,L9` | partial |
| socket 类型 | `getsockopt(SO_TYPE)` | `getsockopt01`, `getsockopt02` | `cpython-isolated/L9` | partial |
| signal 往返 | `rt_sigaction`, `kill`, `rt_sigreturn` | `sigaction*` | `cpython-isolated/L6` | partial |
| 线程与子进程 | `clone`, `futex`, `pipe2`, `wait4`, `execve` | N/A | `cpython-isolated/L8` | pass |
| DNS/TCP/HTTPS | socket syscall 组、CSPRNG、realtime | N/A | `cpython-isolated/L9` | pass |
| FAT 覆盖 rename | `renameat2`、PageCache writeback、open-file lifetime | `rename*` | `cpython-isolated/L7` | pass（双架构 QEMU + 2K1000LA 完整 L7/专项） |

| 用例 | 跳过原因 | 跟踪 Issue |
|------|----------|------------|
| L9 DNS/HTTP/HTTPS | QEMU 无默认路由或 DNS 时记录 SKIP；实板检测到 `/scratch` 后自动升格为失败 | N/A |
| RISC-V `riscv_hwprobe` | 当前返回 `ENOSYS`，CPython/musl 按 Linux 允许的 fallback 继续运行 | N/A |

QEMU 日志使用下列 judge 验证：

```bash
python3 judge/judge_cpython-isolated.py < logs/cpython-rv64-qemu.log
python3 judge/judge_cpython-isolated.py < logs/cpython-la64-qemu.log
python3 judge/judge_cpython-isolated.py < logs/cpython-la64-board.log
```

预期三者均输出 `{"all": 72, "pass": 72}`。
表中 `partial` 表示 CPython 路径已通过，但本轮没有重跑对应 LTP 全用例，不用 CPython 结果代替 LTP 结论。

## 8. Known Issues

- Alpine `edge/main` 是可变软件源，运行时目录和 APK 缓存不纳入 Git。首次构建需要网络，后续构建复用 `.cpython-runtime.stamp`。释放或长期归档时应额外保存包版本与镜像 SHA-256。
- LoongArch 只发布已保存的 LSX；LASX 和 LBT 仍禁用。增加新 HWCAP 前必须先扩展 trap、任务切换和 signal frame。
- 2K1000LA P3 产物是分区 payload，不是整盘镜像，只能由受限 P3 写入器更新固定 LBA。最新共享脚本已刷新到 P3，实板完整 L3-L9 为 `72/72`、组退出码 0、耗时 125 秒；该结果不替代尚未重跑的相关 LTP 全用例。
- `judge/run_judge.py` 与部分旧 judge 的 JSON 顶层形式存在历史差异；CPython 门禁直接调用 `judge_cpython-isolated.py`。

## 9. References

- `docs/03_fs/2k1000-full-test-disk.md`：SSD 分区布局、P3 写入边界和 U-Boot 验收。
- `docs/01_architecture/hal-and-platform.md`：LoongArch `EUEN`、HWCAP 和 LSX 上下文。
- `docs/05_process/signal.md`：signal frame 构造与 `sigreturn` 恢复。
- `docs/02_syscall/fs-fd-event.md`：`FIONBIO` 和 `getdents64` cookie 语义。
- `docs/02_syscall/network-syscalls.md`：`SO_TYPE` 与 socket 选项语义。
