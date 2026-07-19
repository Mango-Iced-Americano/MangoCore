---
title: "P4 strict-aligned Python 默认运行时固化"
category: debug
status: completed-runtime-routing-with-exposed-smolagent-dependency
author: MangoCore Team
last_update: 2026-07-17
tags: [loongarch64, 2k1000la, python, persist, ext4, strict-align, smolagent]
code_paths:
  - "user/tools/cpython/python3-wrapper-persist.sh"
  - "user/tools/cpython/python-entry-wrapper.sh"
  - "user/src/bin/initproc.rs"
  - "os/build_initramfs.sh"
  - "os/initramfs/apk/usr/libexec/mango/persist-profile"
  - "scripts/build_cpython_runtime_la64_strict.sh"
  - "scripts/deploy_cpython_runtime.py"
  - "scripts/deploy_cpython_bench.py"
  - "scripts/board/verify_persist_python.sh"
  - "Makefile"
related_docs:
  - "docs/09_debug/la64_on_board/260717/05-strict-align-first-experiment.md"
  - "docs/09_debug/la64_on_board/260717/07-strict-runtime-and-anon-unmap-quantification.md"
  - "docs/09_debug/la64_on_board/260717/09-aligned-pillow-and-smolagent-closure.md"
  - "docs/08_testing/mangocore-python-guide.md"
  - "docs/08_testing/cpython-isolated.md"
---

# P4 strict-aligned Python 默认运行时固化

> 本文保留默认路由阶段结束时“只缺 aligned PIL”的历史停止点。该缺口已在后续
> [`09-aligned-pillow-and-smolagent-closure.md`](09-aligned-pillow-and-smolagent-closure.md)
> 中闭合：最终 `43d7bb2ecf21` release 含 100 个 ELF，默认 SmolAgent、Pillow P4 ext4
> 编解码、AgentImage 和 L3-L9 `72/72` 均已通过。下文 b7/PIL fail-exposed 记录不改写，
> 用于审计依赖闭包的发现顺序。

## 1. 任务与完成口径

前一阶段的 strict-aligned CPython 已能在 P4 路径中显式运行，但系统默认 `python`、旧
P3 包装器、pip shebang、SmolAgent console entry、APK chroot 和部署脚本之间仍有多条
可能回到 `/tools` 的间接路径。本阶段目标不是再做一轮性能优化，而是把运行时选择固化为：

```text
2K1000LA 的全部 Python 入口
    -> initramfs policy wrapper
    -> P4 /persist/python-runtime/current
    -> hash 命名的 strict-aligned release
```

P3 `/tools` 只作历史数据备份。任何时候都不把 P3 中的 Python、loader、动态库、标准库、
CA、wheel 或 console script 当作运行依赖。P4 有问题时必须失败并暴露问题，不能 fallback。

完成必须同时满足：

1. 运行时 artifact 是完整 strict-aligned native 闭包，并通过逐 ELF hash；
2. `python/python3/pip/pip3/smolagent/smolagents` 默认命令全部走 P4；
3. pip 产生的其他 console script 也不能按旧 shebang 执行；
4. Python 子进程不能从宿主继承 `/tools` PATH 或动态库路径；
5. `persist-shell` 与宿主 Shell 使用同一 runtime；
6. runtime 部署本身不执行待退役的 P3 Python；
7. 最终在 2K1000LA 的 P4 ext4 上验证，QEMU 只作筛查；
8. P3 全程只读且没有覆盖。

## 2. 修改前的隐式回退面

| 回退面 | 旧行为 | 风险 |
|---|---|---|
| 默认解释器 | `/usr/bin/python3` 最终进入 P3 wrapper | aligned runtime 不是默认 |
| wrapper | `CPYTHON_ROOT` 默认 `/tools/tests/cpython` | P4 缺失时静默换解释器 |
| pip | 规范要求手工 `python3 -m pip` | 直接 `pip3` 可按 shebang 绕过 wrapper |
| SmolAgent | user bin 的脚本带具体 Python shebang | 可能执行旧 ELF或找不到 loader |
| 子进程 | 宿主 `PATH/LD_LIBRARY_PATH` 含 `/tools` | aligned Python 间接加载旧程序/库 |
| chroot | bind `/tools` 并用兼容用户目录 | 宿主与应用根运行身份可能不同 |
| 部署 | 曾用旧 Python `tarfile` 解包新 Python | 慢、会卡死，而且依赖退役 runtime |
| 状态目录 | P4 不可用时回退 scratch/tmpfs | mount/ext4 故障被掩盖 |

这些路径中任意一条存在，都只能证明“aligned 能运行”，不能证明“所有 Python 完全走
aligned”。因此本次以闭包而不是单一符号链接作为验收对象。

## 3. 默认 wrapper

### 3.1 运行时身份

`python3-wrapper-persist.sh` 只接受：

```text
/persist/python-runtime/current
/persist/python-runtime/releases/*
```

`readlink -f` 后必须落到 `/persist/python-runtime/releases/<id>`。随后校验：

- `strict-runtime-manifest.json`；
- `.mango-strict-runtime` 激活标记；
- marker schema 与 policy `mangocore-la64-strict-align-v1`；
- marker 中 manifest SHA 与现场文件一致；
- artifact SHA 的前 12 位与 release 目录名一致；
- manifest target 为 `loongarch64-linux-musl`；
- strict flags 精确包含 `-march=loongarch64 -mabi=lp64d -mstrict-align`；
- loader 与 Python ELF 可执行。

身份不满足分别以 126/127 失败。wrapper 没有 `/tools`、`/scratch`、tmpfs 或其他解释器
fallback。

### 3.2 P4 状态闭合

mutable state 固定为：

```text
PYTHONUSERBASE=/persist/python/user
PYTHONPYCACHEPREFIX=/persist/python/pycache
TMPDIR=/persist/python/tmp
```

目录不能建立时直接失败。这样第三方包、pyc、临时 wheel 解包和 benchmark 临时文件也都
验证 P4 ext4，不允许只把 runtime ELF 放 P4、其余 I/O 悄悄转到 FAT32/tmpfs。

### 3.3 Python 进程树隔离

wrapper 覆盖：

```text
PATH=/bin:/sbin:/usr/bin:/usr/sbin
LD_LIBRARY_PATH=<release>/usr/lib:<release>/lib
PYTHONHOME=<release>/usr
MANGO_PYTHON_POLICY=p4-strict-align-v1
```

宿主 Shell 为其他测试保留的 `/tools/bin` 和 `/tools/lib` 不会进入 Python 进程树。使用
显式 musl loader 启动 Python。最终 runtime 还把 Python ELF 的 `PT_INTERP` 固定为：

```text
/persist/python-runtime/current/lib/ld-musl-loongarch64.so.1
```

因此 `sys.executable`、pip build isolation 和 multiprocessing 直接 exec Python 时也不会
绕回根目录 `/lib`。manifest、host installer、deploy、runtime wrapper 和实板门禁都要求该
字段精确匹配；实板 `self_exec` 已验证子进程仍是同一 P4 release。

wrapper 另外清除 `PYTHONPATH/PYTHONSTARTUP/PYTHONINSPECT/PYTHONBREAKPOINT`、
`LD_PRELOAD/LD_AUDIT` 等继承注入面，OpenSSL config/provider、pip cache 与 user install
也固定到 P4。人工注入 `PYTHONPATH=/tools/forbidden` 的实板测试确认它不会进入 Python。

## 4. console entry 闭包

新增统一 `python-entry-wrapper.sh`：

- `pip/pip3` 固定转换成 strict Python `-m pip`；
- 其他命令只从 `/persist/python/user/bin/<entry>` 查找；
- chroot 兼容视图只允许 `/var/cache/mango-python/user/bin/<entry>`，它 bind 到同一个 P4
  user tree；
- 由 strict Python 直接执行 script 文件，不采用首行 shebang；
- 若旧 chroot 安装留下 shell shim 和 `<entry>.real`，只选择解析后仍位于 P4 user bin 的
  `.real` Python 源文件，忽略 shim 及其旧 `/tools` shebang；
- script 不存在时返回 127，不查 `/tools`。

LA64 initproc 每次启动都建立：

```text
/usr/bin/python{,3} -> /rescue/python3-wrapper
/usr/bin/pip{,3} -> /rescue/python-entry
/usr/bin/smolagent -> /rescue/python-entry
/usr/bin/smolagents -> /rescue/python-entry
```

并扫描 P4 user bin，为其他已有 console entry 建立同类 `/usr/bin` 路由。即使 P4 runtime
当前无效，这些链接仍保留为 fail-closed 入口，shell 不会因 `/usr/bin/python3` 消失而继续
搜索后面的 `/tools/usr/bin/python3`。

rv64 没有对应 strict P4 artifact，因此保留原 tools QEMU 测试路径；没有把 LA64 policy
错误应用到 rv64。

## 5. persist-shell 视图

APK 应用根取消 `/tools` bind，改为只 bind：

```text
/persist/python-runtime -> <root>/persist/python-runtime
/persist/python         -> <root>/persist/python
/persist/python         -> <root>/var/cache/mango-python
```

initramfs wrapper、console wrapper 与 profile 会复制进 chroot。profile 的 PATH 只有系统
基础目录，CPYTHON_ROOT 指向 `/persist/python-runtime/current`。应用根启动门禁实际执行
`/usr/bin/python3 -S`，验证 policy、release 路径、userbase 和无 `/tools` 身份后，才打印
`[apk-persist-shell] RESULT=PASS`。

这使“直接运行 Python”和“进入 persist-shell 再运行 Python”不再是两套 runtime。

## 6. artifact 重建与发布身份

打包缓存新增 `package_input_digest`，覆盖 builder 和 `user/tools/cpython/` 全部脚本。wrapper
策略或 runner 改变后必须重新打包，不能沿用旧 artifact。

第一轮实板发布并完成全部功能门禁的 artifact：

| 字段 | 值 |
|---|---|
| 文件 | `cpython-la64-strict-3.14.5-a420d79ddb07.tar.xz` |
| 大小 | 81,628,064 B |
| artifact SHA-256 | `a420d79ddb07c561066dfec1b4af4c46ce4413e5d79807b4610adef9aa8261a9` |
| manifest SHA-256 | `196c4fbe8705ba523eca556c309aa792ba82b8d3ad38f9c2eaeaa96a11d4955f` |
| target | `loongarch64-linux-musl` |
| native ELF | 94 |
| optimization | PGO=true, LTO=true |
| `PT_INTERP` | `/persist/python-runtime/current/lib/ld-musl-loongarch64.so.1` |
| release | `/persist/python-runtime/releases/a420d79ddb07` |

Docker 中复用已完成编译缓存重新打包，archive 通过完整安装器 `--verify-only`，逐成员安全、
target、strict flags、PGO/LTO、PT_INTERP 与 94 个 ELF hash 均通过。旧 `5298fb0fa485`
保留为部署链中间 release，不再是最终 `current`。

### 6.1 重打包幂等性复核

实板验收完成后再次刷新 `package_input_digest` 时，发现原打包步骤会无条件对已处理 ELF 重复
执行 `strip --strip-unneeded` 和 `patchelf --set-interpreter`。后者对已经指向同一 P4 loader
的 ELF 不是字节幂等操作，曾把 `usr/bin/python3.14` 从 69,856 B 扩大到 73,792 B；这不会
改变 strict-align 语义，却会让“未改源码的重复打包”产生不同 artifact hash。

builder 现改为：只 strip 仍有 `.symtab`/`.debug_*` 的 ELF；先读取 `PT_INTERP`，仅在现值
不同时调用 patchelf，然后无条件复核最终 interpreter。清除 package stamp 后连续完整打包两次，
两次均得到：

| 字段 | 值 |
|---|---|
| host canonical 文件 | `cpython-la64-strict-3.14.5-b7f361382399.tar.xz` |
| 大小 | 81,630,652 B |
| SHA-256 | `b7f361382399bb592afb65bf0b8598cb0a8a96c3afcc66cc24e7a680e23c244c` |
| manifest SHA-256 | `3617070c7b187543bd23945ba60b451d653af89ba557199c1e1e9e6f224a1dd0` |

对 a420 与 b7 两个 archive 的 8,810 个成员逐文件解压并计算 SHA-256，只有
`strict-runtime-manifest.json` 不同；94 个 native ELF、Python 脚本、证书和 wrapper 均逐字节
一致。manifest 的差异来自 builder 自身 hash 更新。为满足“以实板为最终标准”，随后仍将 b7
完整下载、解包并发布到 P4，而不是只根据 archive 等价作推断：板端 SHA、identity、94 ELF
现场 hash 和 strict smoke 全通过，workload 705.708 s，最终
`current=/persist/python-runtime/releases/b7f361382399`。默认入口、chroot 和 L3-L9 又在 b7
上重新执行；项目 judge 为 `72/72`。a420 保留为第一轮完整门禁的历史 release。

## 7. 部署链去除 P3 Python

`deploy_cpython_runtime.py` 的主机端先验证 archive，再临时启动受控 HTTP server。板端收到的
短控制脚本只执行 BusyBox：

```text
P4/mount/capacity preflight
  -> wget archive
  -> sha256sum -c
  -> tar -xJf to P4 staging
  -> manifest identity
  -> activation marker
  -> 94 ELF 现场 hash
  -> new strict runtime smoke
  -> release publish
  -> current atomic link update
```

控制脚本自身也先下载并验 SHA。串口命令均受 512-byte harness 上限检查。旧 P3 Python
不负责下载、解包、校验或 smoke。

第一次实板部署发现 macOS 上 bundled Python 的 `http.server` 在物理网卡连接后不返回内容，
板端 BusyBox `wget` 长时间等待。该失败发生在 P4 release 下载前，未发布 `current`，没有
写 P3。主机 HTTP server 在 macOS 改用系统 `/usr/bin/ruby -run -e httpd`；Linux 仍使用
Python server。localhost 和 `192.168.9.10` 物理接口均单独验证，Ruby 路径能正常返回文件。

同类 BusyBox download/hash bootstrap 也应用到 benchmark 部署和 P3 备份脚本，避免以后
这些辅助入口再次执行 P3 Python。P3 备份仍只读源数据，目标写 P4 `/persist`。

## 8. 验证门禁

`/rescue/verify-persist-python` 检查：

1. `/persist` 为 P4 ext4 rw、不是 symlink；
2. `current` 精确解析到 12-hex release；
3. activation 与 manifest 存在；
4. `python/python3` 从 `/usr/bin` 解析到 strict wrapper；
5. `pip/pip3/smolagent/smolagents` 解析到 console wrapper；
6. Python 内 policy、root、userbase、pycache 正确；
7. `sys.executable`、`sys.prefix`、`sys.path`、PATH 和 LD_LIBRARY_PATH 无 `/tools`；
8. `python` 和 `python3` 两个命令都通过；
9. normal site、pip 和 `sys.executable` self-exec；
10. P4 user site 不含未纳入 manifest 的 native `.so`；
11. SmolAgent import 的实际状态。

默认模式允许 SmolAgent import 报 `failed-exposed`，表示 Python 路由已经严格切换，而新
runtime 的第三方依赖问题没有被回退隐藏。`--require-smolagents` 把同一问题升级为失败，
用于最终应用门禁。

## 9. 编译、QEMU 和镜像结果

### 9.1 静态与主机脚本

- Shell `bash -n` 通过；
- Python `py_compile` 通过；
- deploy `--dry-run` 通过，四条板端 wrapped command 都低于 512 bytes；
- `git diff --check` 通过；
- strict archive 完整 verify 通过。
- 修正重打包幂等性后，强制连续打包两次均得到 `b7f361382399...`；host installer 对 b7 的
  archive 安全、manifest、P4 PT_INTERP 和 94 ELF 校验通过。a420/b7 除 manifest 外 8,809 个
  成员逐字节相同；b7 随后在实板完整发布并通过默认入口、chroot 和 L3-L9 `72/72`。

项目 Docker Compose 固定映射宿主 `/dev/sdb`，当前 macOS 环境没有该设备，无法直接启动
compose service；实际验证使用相同项目 Docker image 和 `/app` bind，不改变编译环境。

### 9.2 双架构编译

严格串行执行：

```text
make -C os rv64-kernel-build-only  PASS
make -C os la64-kernel-build-only  PASS
```

用户已有生成态差异 `os/src/lang_items.rs`、`user/src/lang_items.rs` 和 LA linker 未作为本轮
手工源码修改清理或覆盖。

### 9.3 QEMU

- rv64 basic 冒烟通过、退出码 0；
- la64 启动到 initproc，但 tools BusyBox 在用户地址 `0x11368` 发生重复
  `InstructionNonDefined`，bad instruction `0x29c9a061`；因此 LA64 QEMU basic 记失败，
  不能写成 PASS；
- 该异常发生在 QEMU tools 路径，不用于替代 P4 strict 实板验收，也不能据此给实板默认
  runtime 下结论。
- 最新 la64 P4 shell QEMU 启动到 initproc/诊断 shell；QEMU P4 磁盘仍是旧 manifest，
  新 wrapper 明确拒绝后保持 fail-closed，没有回退 `/tools`。该项只验证启动与拒绝路径。

### 9.4 实板镜像

```text
kernel-2k1000-persist-shell.ui
size: 16,769,280 bytes
payload: 16,769,216 bytes
SHA-256: a5e60c0d52b46c0cd36a472e08f2050f1e4f26ac612ceee0748f53851bc95da1
board CRC32: 0ccd9d5d
```

initramfs 构建日志确认包含 `/rescue/python3-wrapper`、`/rescue/python-entry` 和
`/rescue/verify-persist-python`。最终实板 TFTP 字节数、CRC32、`iminfo` 和 payload checksum
全部通过；启动到 `P4 strict-aligned Python launchers installed`，APK chroot 输出
`stage=prepared`、`RESULT=PASS`。完整串口历史归档为
`raw/board_boot_history-through-a5e60c0d.log`。

## 10. 实板执行记录

运行目录：

```text
target/perf-runs/20260717T-p4-strict-python-default/
```

保留了 HTTP bootstrap、中间 `mv -T`、console shim 和 ext4 临时 inode 等失败记录，没有
删除或改写。最终有效结果如下：

| 项目 | 结果 |
|---|---|
| a420 下载、SHA、解包、identity | PASS；77.8 MiB，P4 ext4 |
| b7 最终下载、SHA、解包、identity | PASS；77.8 MiB，workload 705.708 s，P4 ext4 |
| 现场 native 完整性 | PASS；`strict-runtime-integrity-ok elfs=94` |
| runtime smoke/current 发布 | PASS；最终 `current=b7f361382399` |
| 最终 uImage 启动 | PASS；TFTP/CRC/iminfo/payload checksum 全通过 |
| initproc/chroot | PASS；无 `.tmp` EIO，`RESULT=PASS` |
| 默认 Python 门禁 | PASS；解释器、normal site、pip、self-exec 全在 b7 |
| 六个全局入口 | PASS；只解析到 `/rescue` wrappers |
| `/tools` 隔离 | PASS；P3 ext4 ro，Python tree 无 `/tools` |
| 人工污染环境 | PASS；`PYTHONPATH=/tools/...` 被清除 |
| chroot Python/pip | PASS；与宿主共享 b7 runtime/state |
| CPython L3-L9 | PASS；b7 项目 judge `72/72`，workload 49.005 s |
| direct `smolagent` | FAIL-EXPOSED；已越过 `.real`/旧 shebang，只缺 `PIL` |
| `--require-smolagents` | 预期退出 1；同一 `PIL` 缺口 |
| 真实 LLM API | NOT RUN；未在依赖不完整时制造无效端到端结论 |

核心原始日志已复制到
`docs/09_debug/la64_on_board/260717/raw-data/20260717T-p4-strict-python-default/`，共 34 条
structured records、38 份 raw 和 4 份报告（含重打包幂等性与最终 b7 发布审计）。

## 11. 安全和结论边界

- P3 `/tools` 的旧文件没有删除或覆盖，符合“只作数据备份”；
- P3 被保留不等于可执行，LA64 默认和 Python 子进程路径都不含它；
- P4 release 采用内容 hash，不允许就地修改；
- runtime 启用前对 manifest 中 94 个 native ELF 重新计算现场 hash；
- 新 runtime 失败必须修复新 runtime 或内核 ABI，不能用旧 Python 做临时回退；
- QEMU 的 LA64 tools BusyBox 异常必须单独保留，不能用实板通过掩盖，也不能用它否定尚未
  执行的实板 P4 runtime；
- 本阶段固化运行时，不包含匿名页 O(N²) 优化和 ext4 驱动优化。
- 默认 Python 运行时切换已完成；SmolAgent 应用可用性尚未完成。下一项必须自行 strict-aligned
  构建 Pillow/PIL（含 `_imaging.so` 及启用的 libjpeg/freetype/lcms/webp 等 native 闭包）并
  纳入同一 manifest，不能安装普通二进制 wheel，也不能借 P3 Pillow 做 fallback。
