---
title: "MangoCore Python 与 pip 使用指南"
category: testing
status: current
author: MangoCore Team
last_update: 2026-07-18
tags: [python, pip, cpython, loongarch64, 2k1000, persist, strict-align]
code_paths:
  - "user/tools/cpython/python3-wrapper-persist.sh"
  - "user/tools/cpython/python-entry-wrapper.sh"
  - "user/src/bin/initproc.rs"
  - "os/initramfs/apk/usr/libexec/mango/persist-profile"
  - "scripts/build_cpython_runtime_la64_strict.sh"
  - "scripts/deploy_cpython_runtime.py"
  - "scripts/board/verify_persist_python.sh"
entry_points:
  - "cpython-la64-runtime-strict"
  - "2k1000-python-runtime-deploy"
  - "2k1000-boot"
  - "python"
  - "python3"
  - "pip"
  - "smolagent"
arch:
  rv64: legacy-tools-runtime
  la64: supported
related_docs:
  - "docs/08_testing/cpython-isolated.md"
  - "docs/09_debug/la64_on_board/260717/08-persist-strict-python-default.md"
  - "docs/09_debug/la64_on_board/260717/09-aligned-pillow-and-smolagent-closure.md"
  - "docs/03_fs/2k1000-full-test-disk.md"
---

# MangoCore Python 与 pip 使用指南

## 1. 当前策略

2K1000LA 的默认 Python 已收敛为 **P4 `/persist` 上唯一的 strict-aligned 运行时**。
`python`、`python3`、`pip`、`pip3`、`smolagent`、`smolagents` 以及 P4 用户包生成的
console entry 都必须经过 initramfs 中的包装器，最终执行：

```text
/persist/python-runtime/current
  -> /persist/python-runtime/releases/<artifact-sha256 前 12 位>
  -> lib/ld-musl-loongarch64.so.1
  -> usr/bin/python3
```

该运行时的 musl、CPython 和全部原生依赖均使用
`-march=loongarch64 -mabi=lp64d -mstrict-align` 构建。P3 `/tools` 中的旧 CPython 只保留为
历史数据备份，不是候选运行时，不参与启动、回退、pip 引导、console script 或部署。

策略采用 fail-closed：P4 未挂载、`current` 无效、manifest/激活标记不匹配、运行时不是
strict-aligned，或 P4 状态目录不可写时，`python` 会明确失败。它不会继续搜索
`/tools/usr/bin/python*`，从而避免旧运行时把真实问题隐藏掉。

> rv64 尚未切换到这套 P4 strict runtime，仍保留原 QEMU tools 测试链。本文所有
> “唯一运行时”结论仅针对 2K1000LA/LA64。

## 2. P4 布局

| 内容 | 宿主与 `persist-shell` 中的规范路径 | 属性 |
|---|---|---|
| 不可变运行时版本 | `/persist/python-runtime/releases/<12-hex>` | P4 ext4；发布后只读使用 |
| 当前版本入口 | `/persist/python-runtime/current` | 指向单个 release 的原子符号链接 |
| strict manifest | `<release>/strict-runtime-manifest.json` | target、flags、PGO/LTO、100 个 ELF hash |
| 激活标记 | `<release>/.mango-strict-runtime` | artifact/manifest hash 与 policy |
| 用户包与 console script | `/persist/python/user` | P4 ext4，持久可写 |
| pyc | `/persist/python/pycache` | P4 ext4，持久可写 |
| Python 临时文件 | `/persist/python/tmp` | P4 ext4，可清理 |
| APK 应用根 | `/persist/apk-root` | P4 ext4，持久可写 |

没有 `/scratch` 或 tmpfs 回退。Python 的代码、用户包、pyc 和临时工作区全部落到 P4
ext4，P4 的挂载、元数据或持久化缺陷会直接成为测试失败。

P3 仍按只读方式挂载到 `/tools`，可以用于保留历史镜像或数据，但 Python 路径不读取
其中任何 ELF、标准库、CA、wheel 或脚本。不要把 P3 旧 wrapper 手工加入 `PATH`，也不要
直接执行 `/tools/tests/cpython/usr/bin/python3`。

## 3. 构建和部署

构建在项目 Docker 环境中完成：

```bash
make cpython-la64-runtime-build
make cpython-la64-runtime-verify
```

验证后的索引位于 `target/cpython-strict/artifacts/current.json`。部署前创建本轮记录目录，
然后只向 P4 发布：

```bash
python3 scripts/kernel_perf.py init \
  --run-dir target/perf-runs/<UTC-run-id> \
  --platform 2k1000la --arch la64 --build-mode production

make 2k1000-python-runtime-deploy \
  PERF_RUN_DIR=target/perf-runs/<UTC-run-id> \
  BOARD_SERIAL=/dev/cu.wchusbserial120
```

部署器先在主机检查 archive 成员安全、strict flags、PGO/LTO、P4 `PT_INTERP` 和所有
ELF hash；板端只用 initramfs BusyBox 下载、验 SHA、解包到 staging。压缩 archive 的
传输临时对象使用 `/tmp`，避免在 P4 同时占用 archive 与解压树；解压后的 release 和全部
Python 状态仍在 P4。staging 通过 runtime smoke 后才改名为 release；板端再次计算 100 个
native ELF hash，通过后原子更新 `current`。部署引导不执行 P3 Python，因此旧解释器即使
损坏也不会影响升级。

当前 2026-07-18 host `current.json` 产物身份：

```text
artifact: cpython-la64-strict-3.14.5-43d7bb2ecf21.tar.xz
artifact SHA-256: 43d7bb2ecf21d662c427959c0b07612d05379d26a8b05bbc3bde84aa6cb4579e
manifest SHA-256: 862aec2368a1eb3480934b6f746732f8bbf23b85ab6104080944edac219870b5
runtime interpreter: /persist/python-runtime/current/lib/ld-musl-loongarch64.so.1
```

当前实板 canonical release 为 `/persist/python-runtime/releases/43d7bb2ecf21`。它在原有
CPython 闭包上新增 strict-aligned Pillow 12.3.0、libjpeg-turbo 3.1.4.1、MarkupSafe 3.0.3，
并以无 ELF 的纯 Python PyYAML 6.0.3 补齐 SmolAgent 当前直接依赖。100 个 ELF 的板端
integrity、四组 runtime smoke、默认入口、SmolAgent command/AgentImage 和 L3-L9 `72/72`
均通过。`e14f2fd9cc6d` 暂留作上一个已发布版本的回退点。

部署完成后必须重新启动包含新 wrapper 的实板镜像，让 initproc 重建 `/usr/bin` 路由：

```bash
make 2k1000-boot \
  IMAGE=kernel-2k1000-persist-shell.ui \
  BOARD_SERIAL=/dev/cu.wchusbserial120
```

## 4. 默认命令路由

启动后无需先进入 `persist-shell`。宿主 Shell 和持久应用根中的以下命令都使用 P4：

```sh
python --version
python3 -c 'import sys; print(sys.executable); print(sys.prefix)'
pip --version
pip3 --version
smolagent --help
smolagents --help
```

当前 43d 实板上全部命令通过；`smolagent` 进入 P4 `.real` 入口，import 的 Pillow、
MarkupSafe 和 PyYAML 都来自当前 aligned release。真实 LLM API 仍需作为独立端到端测试，
命令可用不表示公网与模型服务延迟已经验收。

路由分两类：

| 命令 | `/usr/bin` 入口 | 执行方式 |
|---|---|---|
| `python`, `python3` | `/rescue/python3-wrapper` | 显式 P4 loader + P4 CPython ELF |
| `pip`, `pip3` | `/rescue/python-entry` | strict Python `-m pip` |
| `smolagent`, `smolagents` | `/rescue/python-entry` | strict Python 执行 P4 user bin 中同名脚本，忽略旧 shebang |
| 其他 P4 console entry | `/rescue/python-entry` | 启动时镜像到 `/usr/bin/<name>`，执行 P4 user bin 脚本 |

Python wrapper 会覆盖而不是追加：

```text
PATH=/bin:/sbin:/usr/bin:/usr/sbin
LD_LIBRARY_PATH=<P4 release>/usr/lib:<P4 release>/lib
PYTHONHOME=<P4 release>/usr
PYTHONUSERBASE=/persist/python/user
PYTHONPYCACHEPREFIX=/persist/python/pycache
TMPDIR=/persist/python/tmp
MANGO_PYTHON_POLICY=p4-strict-align-v1
```

因此 Python 创建的子进程不会继承宿主 Shell 中面向其他竞赛工具保留的 `/tools` 动态库
路径。普通应用仍可使用 `/tools` 中的非 Python 工具，但它不属于 Python 进程树的默认
搜索路径。

## 5. pip 和 console scripts

规范安装方式仍是用户树：

```sh
python3 -m pip install --user PACKAGE
python3 -m pip check
sync
```

`pip` 和 `pip3` 现在也安全：它们不是 pip wheel 写出的 shebang 脚本，而是固定包装为
`python3 -m pip`。`PIP_BREAK_SYSTEM_PACKAGES=1` 只允许向独立 P4 user base 写入，不应向
runtime release 或 APK 系统目录安装。

安装新包后，如果它新生成了 console entry，可以立即用模块入口测试；要获得全局短命令，
重新启动一次让 initproc 将 `/persist/python/user/bin/*` 镜像到 `/usr/bin`：

```sh
python3 -m some_package --help
sync
# RESET 后
some-package --help
```

包装器执行 P4 中的 script 文件并忽略其 shebang，所以即使 pip 在脚本首行写入某个具体
release 的 Python ELF，后续切换 `current` 也不会绕过策略。

## 6. SmolAgent

SmolAgent 包和依赖必须安装在 `/persist/python/user`。默认命令与 Python 一样走 aligned
runtime：

```sh
command -v smolagent
readlink -f /usr/bin/smolagent
python3 -c 'import smolagents; print(smolagents.__file__)'
smolagent --help
```

前两条应分别显示 `/usr/bin/smolagent` 和 `/rescue/python-entry`；模块文件必须位于 P4
用户树或当前 P4 runtime，不能包含 `/tools`。Pillow `_imaging`、libjpeg 和 MarkupSafe
`_speedups` 已进入 100 ELF manifest，PyYAML 被固定为 pure Python。可用以下命令复核
图像闭包：

```sh
python3 /persist/python-runtime/current/pillow_strict_smoke.py \
  --output-dir /persist/python/tmp/pillow-smoke-manual
```

如果新 CPython 暴露缺失扩展、第三方包或 ABI 问题，命令应
失败并保留 traceback，不允许改回 P3 来让测试“先跑起来”。真实 LLM API 仍需单独区分
公网、服务端排队和本地 Python/内核耗时，串口日志不得记录密钥或请求头。

## 7. 验证与故障判定

完整门禁：

```sh
/rescue/verify-persist-python
/rescue/verify-persist-python --require-smolagents
```

第一条验证 P4 ext4 rw、release identity、六个默认入口、Python 环境、`sys.path`、PATH
和动态库路径。默认情况下，SmolAgent import 失败会记录为 `failed-exposed`，以证明路由
正确且没有回退；第二条把 SmolAgent import 失败升级为整项失败。

手工最小核对：

```sh
mount | grep ' /persist '
readlink -f /persist/python-runtime/current
readlink -f /usr/bin/python3
readlink -f /usr/bin/smolagent
python3 -S -c 'import os,sys; print(os.environ["CPYTHON_ROOT"]); print(sys.path)'
```

常见失败含义：

| 输出 | 含义 |
|---|---|
| `refusing non-P4 runtime` | 调用者试图设置 P3、scratch 或其他自定义根 |
| `P4 runtime is not an active release` | `current` 缺失、悬空或不指向 releases |
| `activation does not match` | artifact、manifest 或发布目录身份不一致 |
| `runtime is not ... strict-aligned` | manifest target/flags 不符合政策 |
| `P4 Python state directories are unavailable` | `/persist` 写入或目录语义有问题 |
| console entry `is not installed` | P4 user base 中没有对应脚本；不会查找 P3 |

这些错误都不应通过建立 `/tools` 链接、导出 P3 `LD_LIBRARY_PATH` 或修改 shebang 规避。
修复 P4 runtime/package 本身并重新运行门禁。

## 8. 安全边界

- 部署和运行 Python 只读写 P4；P3 保持只读备份，不重打包、不覆盖。
- runtime release 以内容 hash 命名；不要就地修改已发布 release。
- `current` 只由验证后的部署器发布，不手工指向 staging。
- P4 写入后执行 `sync`，再做物理复位持久性验证。
- P4 是 ext4 最终运行介质；tmpfs/FAT32 上的通过结果不能替代实板 ext4 结论。
- `current` 不可用时保留失败，禁止临时 fallback。
