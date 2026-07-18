---
title: "隔离 CPython 运行时测试"
category: testing
status: current
author: MangoCore Team
last_update: 2026-07-18
tags: [testing, cpython, qemu, loongarch64, riscv64, 2k1000, strict-align]
code_paths:
  - "scripts/fetch_cpython_runtime.py"
  - "scripts/build_cpython_runtime_la64_strict.sh"
  - "scripts/install_cpython_runtime_la64_strict.py"
  - "scripts/deploy_cpython_runtime.py"
  - "scripts/board/verify_persist_python.sh"
  - "user/tools/cpython/cpython_testcode.sh"
  - "user/tools/cpython/run_cpython.sh"
  - "user/tools/cpython/python3-wrapper-persist.sh"
  - "user/tools/cpython/python-entry-wrapper.sh"
  - "user/src/bin/initproc.rs"
  - "os/src/fs/vfs/file.rs"
  - "os/src/syscall/fs.rs"
  - "os/src/net/syscall/getsockopt.rs"
  - "os/src/hal/arch/loongarch64/trap/context.rs"
entry_points:
  - "rv64-cpython-run"
  - "la64-cpython-run"
  - "cpython-la64-runtime-strict"
  - "cpython-la64-runtime-verify"
  - "2k1000-python-runtime-deploy"
arch:
  rv64: supported-legacy-runtime
  la64: supported-p4-strict-runtime
related_docs:
  - "docs/08_testing/mangocore-python-guide.md"
  - "docs/09_debug/la64_on_board/260717/05-strict-align-first-experiment.md"
  - "docs/09_debug/la64_on_board/260717/08-persist-strict-python-default.md"
  - "docs/09_debug/la64_on_board/260717/09-aligned-pillow-and-smolagent-closure.md"
  - "docs/03_fs/2k1000-full-test-disk.md"
---

# 隔离 CPython 运行时测试

面向使用者的部署、pip 和 SmolAgent 命令见
[`mangocore-python-guide.md`](mangocore-python-guide.md)。本文描述运行时供应链、隔离
边界和自动验收。

## 1. 当前架构

项目现在有两条明确分开的测试路径：

| 平台 | runtime | 用途 |
|---|---|---|
| rv64 QEMU | `/tools/tests/cpython` 的 Alpine 隔离运行时 | 功能和跨架构 ABI 筛查 |
| la64 QEMU 历史门禁 | `/tools/tests/cpython` | 保留旧 72 项复现，不作为实板默认 Python |
| 2K1000LA 实板 | `/persist/python-runtime/current` | 唯一正式 Python；完整 strict-aligned 闭包 |

2K1000LA 不再从 P3 `/tools` 启动 CPython。P3 只保留旧数据备份；即使 P4 runtime
损坏，启动包装器也必须报错，而不能回退 P3。运行时、pyc、临时目录、pip 用户树和
benchmark 工作区的最终结论均落在 P4 ext4 上。

2026-07-14 的旧 Alpine runtime 曾在 rv64、la64 QEMU 与实板完成 L3-L9 `72/72`。
2026-07-17 又将 musl、CPython 3.14.5 和全部 92 个其他 ELF 依赖按 strict-align 重建，
在 2K1000LA P4 ext4 完成 `72/72`、18/18 benchmark，并使所测 benchmark body 的
非对齐 trap 归零。随后把 Pillow、libjpeg 和 MarkupSafe 的原生代码纳入同一供应链，
PyYAML 固定为纯 Python，最终 manifest 含 100 个 ELF。新默认运行时不再复用旧 P3 ELF。

## 2. 设计目标

- **完整 native 闭包对齐**：不能只重编 CPython 主 ELF；loader、libc、OpenSSL、SQLite、
  zlib、扩展模块等所有 ELF 都必须在 manifest 中并通过 hash/flags 审计。
- **运行入口唯一**：`python/python3/pip/smolagent` 和 pip console entry 全部经过 P4
  wrapper；不相信 console script 的绝对 shebang。
- **失败可见**：P4 mount、runtime identity、扩展模块、包依赖或内核 ABI 问题必须直接
  失败，禁止 `/tools` fallback。
- **部署不自举依赖旧 Python**：主机检查 archive，板端下载、校验和解包只使用 BusyBox；
  smoke 只执行刚解出的 strict runtime。
- **持久状态同介质**：代码、user site、pyc、tmp 和测试输出均在 P4 ext4，不能以
  FAT32/tmpfs 结果替代最终判断。
- **跨架构继续筛查**：rv64/la64 QEMU 保留功能冒烟，但正式性能与实板默认运行时身份只
  由 2K1000LA 决定。

## 3. 构建供应链

```text
pinned upstream source + pinned patches + GCC 15.2 musl cross toolchain
  -> musl, zlib, bzip2, xz, libffi, expat, mpdecimal
  -> OpenSSL, ncurses, readline, SQLite
  -> CPython 3.14.5 PGO/LTO
  -> libjpeg-turbo 3.1.4.1, Pillow 12.3.0, MarkupSafe 3.0.3
  -> pure-Python PyYAML 6.0.3
  -> 100 ELF dependency/hash manifest
  -> deterministic tar.xz + current.json
  -> host archive validation
  -> board P4 staging + BusyBox SHA/extract
  -> runtime smoke
  -> immutable release + atomic current symlink
```

统一 flags：

```text
-march=loongarch64 -mabi=lp64d -mstrict-align
```

构建和验证：

```bash
make cpython-la64-runtime-strict
make cpython-la64-runtime-verify
```

严格运行时 archive 必须含：

- `lib/ld-musl-loongarch64.so.1`；
- `usr/bin/python3`；
- `strict-runtime-manifest.json`；
- `strict_runtime_smoke.sh`；
- `pillow_strict_smoke.py`；
- P4-only `python3-wrapper.sh`；
- L3-L9、benchmark 和 strict runner。

`current.json` 固定 artifact SHA、manifest SHA、target、policy 和构建模式。只要 wrapper 或
测试脚本变化，package input digest 就会使旧打包缓存失效；不会继续复用包含旧策略的包。

## 4. P4 发布协议

```bash
make 2k1000-python-runtime-deploy \
  PERF_RUN_DIR=target/perf-runs/<UTC-run-id> \
  BOARD_SERIAL=/dev/cu.wchusbserial120
```

部署前置门禁：

1. `/persist` 必须是非 symlink、rw ext4；
2. `/dev/sda4` 必须为预期 4 GiB P4；
3. target parent 固定为 `/persist/python-runtime/releases`；
4. archive path/link 不能逃逸，不能含特殊设备节点；
5. target、strict flags、PGO、LTO、ELF 数量和逐项 SHA 必须匹配；
6. 下载 archive 的 SHA 必须在板端复核。

新版本先解到隐藏 staging。解包完成后写入与 artifact/manifest hash 绑定的激活标记，运行
`strict_runtime_smoke.sh`；该 smoke 会逐个重算 100 个 ELF，并执行 CPython、Pillow、
MarkupSafe 和 PyYAML 检查。只有 smoke 通过才创建 release，并以临时链接 + 原子 rename
发布 `current`。中断不会把半成品暴露给默认 `python`。P3 在整个流程中不写、不读、不执行。

## 5. 默认入口与环境隔离

LA64 initproc 无论 P4 是否健康都会安装 fail-closed 入口：

```text
/usr/bin/python,python3 -> /rescue/python3-wrapper
/usr/bin/pip,pip3,smolagent,smolagents -> /rescue/python-entry
```

所有已有 `/persist/python/user/bin/*` 也映射为同类 `/usr/bin` wrapper。入口 wrapper 查找
P4 user script，并用 strict Python 将脚本作为参数执行，忽略 shebang。新增 console entry
在下次启动自动加入全局短命令。

Python 进程环境只包含 P4 release 的 loader/lib 与系统基础命令路径，不含 `/tools`。
`PYTHONHOME`、`PYTHONUSERBASE`、`PYTHONPYCACHEPREFIX` 和 `TMPDIR` 都在 P4。这样既防止
Python 本体回退，也防止 `subprocess`、pip 或 console entry 间接加载 P3 的 ELF/脚本。

APK `persist-shell` 不再 bind `/tools` 到应用根；它只 bind：

- `/persist/python-runtime -> /persist/apk-root/persist/python-runtime`；
- `/persist/python -> /persist/apk-root/persist/python`；
- 兼容用户目录视图 `/var/cache/mango-python -> /persist/python`。

chroot 中的 Python wrapper、profile 和 console wrapper由 initramfs复制，运行身份和宿主
Shell 相同。

## 6. L3-L9 功能矩阵

| 层级 | 内容 | 主要内核路径 |
|---|---|---|
| L3 | loader、Python、stdlib、CA、动态依赖 | execve、ELF loader、VFS |
| L4 | 两个全局入口、版本、退出码、prefix | wrapper、exec、环境传递 |
| L5 | 算术、字符串、容器、异常、闭包 | 用户态核心、分配/缺页 |
| L6 | stdlib、hash、zlib、sqlite、select、signal | getrandom、DSO、signal |
| L7 | 文件、目录、rename、truncate、fsync、mmap | VFS、ext4、PageCache |
| L8 | thread/futex、queue、subprocess/pipe/wait | clone、futex、pipe、wait4 |
| L9 | socketpair、DNS、TCP、HTTP、TLS/HTTPS | socket、poll、网络、时间 |

strict runner 自定位所在 release，不接受隐式 `/tools` 默认。L7 的普通工作区在 P4 ext4；
symlink 等不能被 FAT32 完整表达的语义也不再需要转移到 P2。外网结果仍需区分内核路径和
公网/服务端波动。

## 7. benchmark 与性能证据

完整 18 项 benchmark 使用一次预热 + 一次正式样本，具体基线、strict 对照和数据限制见
`docs/09_debug/la64_on_board/260717/`。关键结论：

- production 旧运行时 18 项 body 累计 `1,928.806 s`；
- strict 后同套 18 项累计 `303.470 s`，但两侧并非 production-to-production 隔离 A/B，
  因而该 6.36 倍只能作为趋势；
- `bm_float` 匹配窗口的非对齐 trap `3,000,039 -> 0`，handler ticks
  `4,767,941,219 -> 0`；
- strict 18 个正式 body 的非对齐计数均为 0；
- `bm_fileio` 仍慢，证明 ext4 小文件固定税与非对齐问题相互独立。

新的默认 P4 runtime 会改变启动/import 缓存路径和包版本，因此部署后的首次启动、热启动、
SmolAgent import 与完整 L3-L9 必须重新留档，不能直接沿用旧 canonical `s-abbc...` 的
实板结果作为新 release 的验收。

## 8. 自动验证

运行时身份和默认路由：

```sh
/rescue/verify-persist-python
```

门禁检查：

- P4 ext4 rw；
- `current` 指向 12-hex release；
- strict activation/manifest 存在；
- `python/python3` 命中 strict wrapper；
- `pip/pip3/smolagent/smolagents` 命中 console wrapper；
- Python 环境、`sys.path`、`PATH`、`LD_LIBRARY_PATH` 无 `/tools`；
- userbase 与 pycache 固定在 P4；
- Pillow PNG/JPEG 原生扩展能加载；
- SmolAgent import 失败时默认记录 `failed-exposed`，`--require-smolagents` 模式则失败；
  当前 43d release 的 require 模式和 `smolagent --help` 均已通过。

L3-L9 日志继续用：

```bash
python3 judge/judge_cpython-isolated.py < logs/cpython-la64-board.log
```

正式门禁目标是 `{"all": 72, "pass": 72}`。judge 只说明测试项通过，不替代 runtime
manifest、默认命令路由和 P3 零依赖验证。

## 9. 已知边界

- strict 的 trap=0 只覆盖已运行 workload，不代表任意第三方 wheel 都已 strict 构建；
  新增 native wheel 必须单独审计 ELF 和实板 trap。
- SmolAgent 的纯路由验证与依赖完整性验证分开。当前已补齐 Pillow `_imaging.so`、JPEG、
  MarkupSafe 和 PyYAML，并在实板验证 AgentImage；但 Pillow 只启用 `jpeg/zlib`。未来开启
  freetype/lcms/webp/tiff 等功能时，仍必须把新增 native 依赖按 strict flags 自行构建并
  加入 manifest，不能安装普通二进制 wheel。
- PyYAML 当前强制为 `py3-none-any`，没有 libyaml accelerator；这是可审计的性能取舍。
- 实板 `statvfs` 的 ext4 free block 当前是挂载快照，不能用作发布容量真值；部署出现
  ENOSPC 时应结合实际分配/清理结果判断，等待 ext4 实现更新后复测。
- LA64 QEMU 当前 basic 冒烟在 tools BusyBox 发生 `InstructionNonDefined`；它证明启动到
  initproc，但不是通过结果，也不能替代实板。
- P4 当前 ext4 无 journal；`sync` 后复位可验证持久性，但不能外推为断电一致性保证。
- rv64 仍使用 legacy tools runtime。未来若统一为 P4 strict，必须先提供独立 rv64 artifact
  和默认路由验证，不能把 LA64 包复用过去。

## 10. 参考

- `mangocore-python-guide.md`：日常部署、pip、console entry 和故障处理。
- `docs/09_debug/la64_on_board/260717/05-strict-align-first-experiment.md`：第一次 strict 实验。
- `docs/09_debug/la64_on_board/260717/07-strict-runtime-and-anon-unmap-quantification.md`：
  runtime 供应链和匿名页量化。
- `docs/09_debug/la64_on_board/260717/09-aligned-pillow-and-smolagent-closure.md`：
  100 ELF 应用闭包、P4 发布异常与最终 SmolAgent/Pillow 实板验收。
- `docs/03_fs/2k1000-full-test-disk.md`：P1-P4 边界与实板写入安全策略。
