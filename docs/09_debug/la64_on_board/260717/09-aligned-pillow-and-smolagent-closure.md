---
title: "Aligned Pillow 与 SmolAgent 原生依赖闭包实板验收"
category: debug
status: completed
author: MangoCore Team
last_update: 2026-07-18
tags: [loongarch64, 2k1000la, python, pillow, smolagents, openai, pydantic, strict-align, ext4, persist]
code_paths:
  - "scripts/build_cpython_runtime_la64_strict.sh"
  - "scripts/deploy_cpython_runtime.py"
  - "scripts/install_cpython_runtime_la64_strict.py"
  - "scripts/board/verify_persist_python.sh"
  - "user/tools/cpython/pillow_strict_smoke.py"
  - "user/tools/cpython/strict_runtime_smoke.sh"
related_docs:
  - "docs/09_debug/la64_on_board/260717/05-strict-align-first-experiment.md"
  - "docs/09_debug/la64_on_board/260717/08-persist-strict-python-default.md"
  - "docs/09_debug/la64_on_board/260717/06-raw-data-index.md"
  - "docs/09_debug/la64_on_board/260717/11-smolagents-toolkit-dependency-closure.md"
  - "docs/08_testing/mangocore-python-guide.md"
---

# Aligned Pillow 与 SmolAgent 原生依赖闭包实板验收

## 1. 任务边界与最终结论

上一阶段已经把 2K1000LA 默认 Python 固定到 P4 `/persist` 的 strict-aligned CPython，
但默认 `smolagent` 在该新运行时中因缺少 `PIL` 失败。此次工作只补齐用户态运行时和
SmolAgent 直接依赖闭包，不修改 LoongArch 非对齐异常模拟器、不优化匿名页释放，也不改
ext4 算法。

最终制品为：

```text
artifact: cpython-la64-strict-3.14.5-43d7bb2ecf21.tar.xz
size: 82,412,900 B
SHA-256: 43d7bb2ecf21d662c427959c0b07612d05379d26a8b05bbc3bde84aa6cb4579e
manifest schema: 3
manifest SHA-256: 862aec2368a1eb3480934b6f746732f8bbf23b85ab6104080944edac219870b5
native ELF count: 100
runtime policy: mangocore-la64-strict-align-v1
PT_INTERP: /persist/python-runtime/current/lib/ld-musl-loongarch64.so.1
```

实板当前状态：

```text
/persist/python-runtime/current
  -> /persist/python-runtime/releases/43d7bb2ecf21
rollback release:
  /persist/python-runtime/releases/e14f2fd9cc6d
```

最终门禁全部通过：默认 `python/python3/pip/pip3/smolagent` 均走 P4 aligned runtime，
Pillow 在 P4 ext4 上完成 PNG/JPEG 写入、`fsync`、重开和解码，SmolAgent `AgentImage`
实际返回 PIL Image，CPython L3-L9 由项目 judge 重算为 `72/72`。P3 `/tools` 在整条构建
部署和默认执行链中没有作为 Python 运行时、库或回退源。

2026-07-18 的后续真实 `OpenAIModel` 调用暴露了此前门禁的范围缺口：`smolagent --help`
和 `AgentImage` 不会创建 OpenAI 客户端，因此不能证明 OpenAI 可选后端依赖完整。后续
实板补装并验证纯 Python `pydantic 1.10.26` 后，默认 `OpenAIModel` 无网络构造通过；
具体版本、安装方法、失败样本和剩余边界见第 12 节。该补充没有改变 100 ELF runtime
artifact，也没有向 user-site 引入新的 `.so`。

同日继续从 `TOOL_MAPPING` 构造路径扫描三个内置工具，补齐 ddgs/markdownify 的传递依赖
后，current 已由本节的 100 ELF `43d` runtime 更新为 schema 4、113 ELF 的 `28f` release。
本节仍是 Pillow/OpenAI 阶段的历史制品记录，内置工具的源码矩阵、lxml/primp 构建和
最终实板证据见 [11-smolagents-toolkit-dependency-closure.md](11-smolagents-toolkit-dependency-closure.md)。

## 2. 为什么不能直接安装普通 Pillow wheel

Pillow 不是纯 Python 包。核心模块 `_imaging.cpython-314.so` 会执行大量 C 代码，并依赖
所启用图像格式的原生库。若直接安装普通 LoongArch wheel，即使 CPython 主程序已经按
`-mstrict-align` 重编，Pillow 自身及其依赖仍可能生成非对齐访问，重新触发内核模拟路径，
使“所有 Python 走 aligned 路径”的结论失效。

因此采用以下闭包原则：

1. 有原生代码的包必须从固定源码构建；
2. 每一个编译单元都必须出现完整 target flags；
3. wheel tag 必须精确匹配 CPython 3.14 和 LoongArch；
4. 产物中的每个 ELF 都进入统一 manifest，记录 hash、SONAME、NEEDED 和 interpreter；
5. 可安全使用纯 Python 实现的依赖，必须显式禁止扩展并要求 `py3-none-any`，不能接受
   “扩展编译失败后恰好回退”的宿主架构 wheel；
6. 主机 QEMU-user smoke 只作交叉构建门禁，最终功能和介质结论以 2K1000LA P4 ext4 为准。

## 3. 固定源码与构建工具

新增闭包的输入全部固定版本和 SHA-256：

| 输入 | 版本/文件 | SHA-256 |
|---|---|---|
| Pillow | `pillow-12.3.0.tar.gz` | `3b8182a766685eaa002637e28b4ec8d6b18819a0c71f579bf0dbaa5830297cce` |
| libjpeg-turbo | `libjpeg-turbo-3.1.4.1.tar.gz` | `ecae8008e2cc9ade2f2c1bb9d5e6d4fb73e7c433866a056bd82980741571a022` |
| MarkupSafe | `markupsafe-3.0.3.tar.gz` | `722695808f4b6457b320fdc131280796bdceb04ab50fe1795cd540799ebe1698` |
| PyYAML | `pyyaml-6.0.3.tar.gz` | `d76623373421df22fb4cf8817020cbb7ef15c725b9d5e45f17e189bfc384190f` |
| setuptools | `80.9.0` wheel | `062d34222ad13e0cc312a4c02d73f059e86a4acbfbdea8f8f76b28c99f306922` |
| pybind11 | `3.0.1` wheel | `aa8f0aa6e0a94d3b64adfc38f560f33f15e589be2175e103c0a33c6bce55ee89` |
| wheel | `0.45.1` wheel | `708e7481cc80179af0e556bbf0cc00b8444c7321e2700b8d8580231d13017248` |

编译器仍为项目 strict runtime 使用的
`loongarch64-unknown-linux-musl-gcc 15.2.0`，统一 flags 为：

```text
-march=loongarch64 -mabi=lp64d -mstrict-align
```

Pillow 12.3.0、MarkupSafe 3.0.3 和 PyYAML 6.0.3 的版本及源码身份同时写入最终
manifest。外部版本选择只用于锁定可复现输入；构建脚本不在实板阶段联网解析“最新版本”。

## 4. 构建实现

### 4.1 libjpeg-turbo

libjpeg-turbo 以共享库形式交叉构建，输出 `libjpeg.so.62`。此次只打开 SmolAgent/Pillow
实际需要的 JPEG 支持，关闭 SIMD 和工具程序；SIMD 的手写汇编不在普通 CFLAGS 的证明
范围内，因此不能一边声明完整 strict-aligned、一边保留未经审计的汇编路径。

构建脚本检查 CMake compile database 中 101 个目标编译单元，要求每条命令都含三个
strict target flags。最终 `libjpeg.so.62` 与 Pillow ELF 一并进入 manifest。

### 4.2 Pillow

交叉构建不能直接相信宿主 Python 的 `sysconfig`。构建环境显式把目标 CPython 3.14 的
头文件、目标 runtime 库和目标 sysconfig 放在宿主路径之前，再用 strict compiler wrapper
追加 target flags。构建结果必须恰好为：

```text
pillow-12.3.0-cp314-cp314-linux_loongarch64.whl
```

脚本审计 Pillow 编译日志中的 80 个 C 编译单元；缺任意 strict flag、出现宿主编译器、
wheel tag 不符或目标头文件未优先使用都会直接失败。

本轮功能选择：

| 类别 | 功能 |
|---|---|
| 启用 | `jpeg`, `zlib` |
| 禁用 | `avif`, `freetype`, `imagequant`, `jpeg2000`, `lcms`, `raqm`, `tiff`, `webp`, `xcb` |

这是刻意的最小闭包，不表示 Pillow 其他格式不重要。后续若业务需要字体、WebP、TIFF 等，
必须对相应库重复同样的源码固定、strict 编译和 manifest 验证，而不是直接开启系统探测。

最终 wheel SHA-256 为
`ceaac4e98c48c665bd432879805cca2eb22ffc7571368eb9267763e12df24467`。

### 4.3 MarkupSafe

第一次 aligned Pillow runtime 发布后，默认 SmolAgent 门禁继续暴露
`ModuleNotFoundError: markupsafe`。这说明只根据最初的 `PIL` 报错补一个包不足以证明应用
闭包，必须从默认 console command 一层层执行到成功。

MarkupSafe 从 3.0.3 sdist 构建，保留 `_speedups.cpython-314.so`，精确 wheel 为：

```text
markupsafe-3.0.3-cp314-cp314-linux_loongarch64.whl
```

其唯一 C 编译单元通过 strict flags 检查，wheel SHA-256 为
`d1ddab68d5066f3d09fab9afbf09d39f37a8171bf8758acb5bbc65bb01f5679e`。

### 4.4 PyYAML

加入 MarkupSafe 后，默认门禁再暴露 `ModuleNotFoundError: yaml`。PyYAML 对 SmolAgent
当前功能不要求 libyaml 加速，因此选择纯 Python 实现，避免再引入一个未经需求证明的
原生库。

第一轮构建不能接受：上游先尝试编译 `yaml._yaml`，失败后生成了
`pyyaml-6.0.3-cp314-cp314-linux_x86_64.whl`。尽管包内容看似可回退到 Python，这个过程
已经执行原生编译且 wheel 带宿主架构 tag。脚本按 fail-closed 规则退出 2，日志完整保留。

第二轮设置 `PYYAML_FORCE_LIBYAML=0` 并传入 `--without-libyaml`，同时要求：

- 编译日志不得出现 C 编译命令；
- wheel 必须恰好为 `pyyaml-6.0.3-py3-none-any.whl`；
- wheel 和安装树不得含 `.so` 或其他 ELF；
- QEMU-user 与实板都执行 `yaml.safe_load` smoke。

最终 wheel SHA-256 为
`1832af5057bfe2ffb6187aa29464f52b3a9c3cb1d05fdda527861b3bc0e4bf66`。
代价是没有 libyaml accelerator；这是明确记录的性能边界，不是缺陷被隐藏。

## 5. 统一 ELF 与运行入口门禁

最终 manifest schema 3 包含 100 个 ELF。native policy 明确为：CPython、musl、Pillow、
MarkupSafe、libjpeg 及所有既有原生依赖均使用 strict flags；PyYAML 为纯 Python。

三层门禁分别验证：

1. 主机构建后：逐 ELF hash、架构、NEEDED、interpreter 和 package metadata；
2. archive 安装前：安全成员路径、manifest hash、artifact hash、100 个 ELF 实物 hash；
3. 实板 staging 激活前：新 runtime 自行执行 `verify_runtime_integrity.py`，再次重算 100
   个 ELF，然后运行 CPython、Pillow、MarkupSafe 和 PyYAML smoke。

动态 Python ELF 的 `PT_INTERP` 固定到稳定 P4 `current` loader。默认 wrapper 又显式执行
该 loader，因此 `sys.executable` 自执行、`subprocess`、pip 和 console entry 都不会因
绝对 `/lib/ld-musl-...` 路径绕出 P4。

## 6. QEMU/主机门禁

在上板前完成：

| 门禁 | 结果 | 时间 |
|---|---|---:|
| Pillow 初版 runtime build | PASS | 251.601 s |
| 初版 artifact 独立验证 | PASS | 41.285 s |
| 初版 QEMU-user Pillow smoke | PASS | 3.455 s |
| Pillow + MarkupSafe native closure build | PASS | 191.688 s |
| PyYAML 第 1 次构建 | 按预期拒绝宿主 tag/native attempt | 4.763 s |
| PyYAML 第 2 次构建和完整重打包 | PASS | 168.581 s |

最终构建的 QEMU smoke 同时覆盖 `strict-runtime-smoke-ok`、Pillow PNG/JPEG、MarkupSafe
speedups 和 PyYAML pure safe-load。QEMU 结果只说明交叉构建产物可加载，不替代实板结论。

## 7. P4 ext4 发布过程与异常

### 7.1 原子发布原则

部署器不会覆盖现有 `current`。archive 先传入临时文件，SHA 通过后解压到隐藏 staging；
完整性和 smoke 全通过才发布 `releases/<12-hex>` 并原子替换 `current`。失败 trap 会清理
staging 和传输临时文件，保留旧 canonical release。

P3 `/tools` 既不接收文件，也不提供 Python bootstrap。部署使用 initramfs BusyBox 的
HTTP、SHA、tar 和 xz，smoke 只执行刚解出的 P4 runtime。

### 7.2 ENOSPC 与恢复

Pillow + MarkupSafe 的 `e14f2fd9cc6d` 首两次发布分别在解包阶段报告：

```text
[balloc] No free blocks available in all block groups
```

两次均未切换 `current`。第二次已验证新增失败清理路径输出
`stage=cleanup-failed-release`。随后发现三个旧 release 和残留的 archive `.part` 占用 P4；
先记录旧 manifest hash，再只删除非 current 的历史版本。没有删除当时 canonical release，
也没有触碰 P3。

### 7.3 传输路径

最初把 78.5 MiB archive 直接写入 P4 会同时占用 archive、staging 和旧 release 的空间。
改用 P2 `/scratch` 后，传输本身约 6 分钟，明显拖慢发布。最终仅把压缩传输对象放到
`/tmp` tmpfs，约 13 秒完成；解压后的 immutable release、用户态、pyc、临时图像和测试
结果仍全部在 P4 ext4。

最终 `43d7bb2ecf21` 发布总 wall 为 715.134 s，其中主要时间来自 P4 的大量小文件解压、
逐 ELF hash 和 Python import/smoke。该数字是部署耗时，不是 Python workload 性能结果。

### 7.4 `statvfs` 不能作为当前 P4 空间真值

实板在创建和删除数个约 131 MiB 解压 release 前后，`statvfs` 都返回完全相同的：

```text
f_blocks=1048576 f_bfree=264285 f_bavail=264285 f_frsize=4096
```

这与已经出现 ENOSPC、清理后又能成功发布的事实矛盾。源码检查显示当前
`Ext4FileSystem::super_block()` 使用挂载时的 `self.superblock`，而分配路径已经通过
`current_superblock()` 更新当前缓存。因此现有 `statfs/statvfs/df` 的 free block 是陈旧
快照，不能用于部署容量门禁或性能报告。本轮只记录该诊断，没有修改 ext4；它应随队友
新 ext4 实现单独修复和复测。

## 8. 2K1000LA 最终实板结果

所有正式功能结论都来自 P4 ext4 上的 release `43d7bb2ecf21`：

| 测试 | 结果 | wall | 关键证据 |
|---|---|---:|---|
| 默认 Python + SmolAgent 门禁 | PASS | 137.599 s | interpreter、normal site、self-exec、pip、Pillow、SmolAgent import/command 全通过 |
| Pillow P4 ext4 编解码 | PASS | 6.230 s | PNG/JPEG 写入、`fsync`、重开、像素校验 |
| SmolAgent `AgentImage` | PASS | 27.202 s | `AgentImage(...).to_raw()` 返回 `Image (3, 2)` |
| CPython L3-L9 | PASS | 49.046 s | judge `72/72`；L7 工作区明确位于 `/persist/pyperf/...` |
| 最终 rv64 production build | PASS | 83.361 s | 项目 Docker，串行构建 |
| 最终 la64 production build | PASS | 91.543 s | 项目 Docker，串行构建 |

Pillow smoke 的确定性输出：

| 格式 | 字节数 | SHA-256 | P4 文件 |
|---|---:|---|---|
| PNG | 82 | `bc89fe6bb077cd71fb888b588f11bfb4b34463b19b3e8fb9250db461c6c25285` | `/persist/python/tmp/aligned-pillow-smoke-43d7bb2ecf21/aligned-pillow-smoke.png` |
| JPEG | 638 | `6b837514e74822979cb03afcd1f73341d681502a9629a25f2fff4ede69d356f2` | `/persist/python/tmp/aligned-pillow-smoke-43d7bb2ecf21/aligned-pillow-smoke.jpg` |

模块路径也全部落在 canonical release：

```text
PIL/__init__.py:
  /persist/python-runtime/releases/43d7bb2ecf21/usr/lib/python3.14/site-packages/PIL/__init__.py
PIL/_imaging.cpython-314.so:
  /persist/python-runtime/releases/43d7bb2ecf21/usr/lib/python3.14/site-packages/PIL/_imaging.cpython-314.so
MarkupSafe _speedups:
  /persist/python-runtime/releases/43d7bb2ecf21/usr/lib/python3.14/site-packages/markupsafe/_speedups.cpython-314.so
PyYAML:
  /persist/python-runtime/releases/43d7bb2ecf21/usr/lib/python3.14/site-packages/yaml/__init__.py
```

L3-L9 覆盖 Python 入口和 prefix、stdlib、signal、sqlite、P4 ext4 文件/目录/rename/
symlink/fsync、线程、futex、subprocess、pipe/wait、socketpair、DNS、HTTP 和 HTTPS。
`72/72` 说明这些项目门禁通过，不等价于所有 CPython 上游测试通过。

## 9. 失败样本如何解释

本 run 有 48 条结构化 record、51 份 raw log 和 11 份派生报告。`failures.csv` 保留 8 条
控制面或闭包发现失败：

- 一条空的 `pyctl_get` 控制面超时；
- 初版默认门禁因缺 MarkupSafe 失败；
- 两次 P4 ENOSPC 部署失败；
- 一次空间审计命令超时；
- MarkupSafe 版门禁因缺 PyYAML 失败；
- PyYAML 首次构建被纯包门禁拒绝；
- rv64 首次容器显式非 root 导致 `rustup` 权限失败，改回项目 Docker 默认用户后通过。

这些失败都发生在最终门禁之前，并且没有被删除或改写。最终 PASS 只由带完整 begin/end/rc
marker 的后续样本给出；失败时间不混入 Pillow 或 L3-L9 的正式 wall。

## 10. 性能含义与尚未完成的内容

此次结果证明的是“默认 Python/SmolAgent 的当前原生闭包已全部进入 strict-aligned
供应链并能在实板 P4 ext4 正常运行”。它没有新增 workload body 的非对齐 trap 采样，
因此不能把 18 项 benchmark 的 trap=0 自动外推到所有 Pillow 操作；如要做性能优化验收，
应在 Pillow resize/convert/JPEG decode 等实际 workload body 周围重新开短计数窗口。

目前仍存在的性能/范围边界：

- 默认门禁 137.599 s、AgentImage 27.202 s 仍很慢，里面混有 P4 site/import/pyc 和大量
  小文件读取；它们是后续分析入口，不是本轮优化对象；
- PyYAML 使用纯 Python，无 libyaml accelerator；
- Pillow 只启用 JPEG 和 zlib；字体、WebP、TIFF 等尚未构建；
- 没有重跑真实 LLM API，避免把公网和服务端排队误归因内核；
- 没有运行 30 分钟混合稳定性；
- P4 release 在当前权限模型下物理可写，部署时有全 ELF hash 门禁，但每次 Python 启动
  只检查 activation/manifest 身份，避免每进程支付 100 ELF 重哈希成本；
- `/tools` 是只读备份而非内核级 `noexec`。默认 Python、console、chroot、部署和测试链不
  使用它，但不把这一结论夸大为“内核禁止用户显式执行任意 `/tools/...` 文件”。

## 11. 复现与审计入口

构建和主机验证：

```bash
make cpython-la64-runtime-build
make cpython-la64-runtime-verify
```

部署到实板 P4：

```bash
make 2k1000-python-runtime-deploy \
  PERF_RUN_DIR=target/perf-runs/20260717T-aligned-pillow \
  BOARD_SERIAL=/dev/cu.wchusbserial120
```

实板最终门禁：

```sh
/rescue/verify-persist-python --require-smolagents
python3 /persist/python-runtime/current/pillow_strict_smoke.py \
  --output-dir /persist/python/tmp/aligned-pillow-smoke-43d7bb2ecf21
```

宿主重分析和 judge：

```bash
python3 scripts/kernel_perf.py analyze \
  --run-dir target/perf-runs/20260717T-aligned-pillow

python3 judge/judge_cpython-isolated.py \
  < target/perf-runs/20260717T-aligned-pillow/raw/cpython_l3_l9_final-1-f58ddacf.log
```

原始数据已逐字节复制到
[`raw-data/20260717T-aligned-pillow/`](raw-data/20260717T-aligned-pillow/)。archive 本体
未复制进文档目录，完整大小和 SHA-256 已写入 `ARTIFACTS.sha256`。

## 12. OpenAI 可选后端依赖闭包补充

### 12.1 用户报错与真实根因

用户执行真实 SmolAgent OpenAI 路径时，traceback 先成功进入：

```text
/persist/python/user/lib/python3.14/site-packages/openai/__init__.py
/persist/python/user/lib/python3.14/site-packages/openai/_models.py
```

随后在 `_models.py` 的 `import pydantic` 失败。`smolagents.models.OpenAIModel` 捕获该导入
异常后，统一重抛“请安装 `smolagents[openai]`”，所以最外层错误文字不能证明 `openai`
本身未安装。实板 metadata 审计确认精确闭包为：

| 包 | 版本 | 位置/作用 |
|---|---:|---|
| `smolagents` | 1.26.0 | P4 user-site，创建 `OpenAIModel` |
| `openai` | 1.35.15 | P4 user-site，已经存在 |
| `httpx` | 0.27.2 | P4 user-site；与 OpenAI 1.35.15 兼容 |
| `typing_extensions` | 4.16.0 | P4 user-site，满足 Pydantic/OpenAI 约束 |
| `pydantic` | 缺失 → 1.10.26 | 唯一缺失的声明依赖 |

`openai 1.35.15` 的现场 `Requires-Dist` 为 `pydantic>=1.9,<3`，且该旧版本不依赖
`jiter`。因此不能把当前问题按新版 OpenAI 的依赖图直接扩张为
`pydantic-core + jiter` 两套 Rust 原生扩展。版本依据来自
[OpenAI 1.35.15 PyPI metadata](https://pypi.org/pypi/openai/1.35.15/json)。

### 12.2 为什么 Pydantic 不需要 aligned 重编

选定 `pydantic-1.10.26-py3-none-any.whl`，而不是任意旧的 Pydantic v1 或最新 v2：

- 1.10.26 的上游 metadata 明确列出 Python 3.14，并包含 3.14 minimal-support 修正；
- wheel tag 是 `py3-none-any`、`Root-Is-Purelib: true`，包内只有 Python/metadata 文件；
- wheel SHA-256 为
  `c43ad70dc3ce7787543d563792426a16fd7895e14be4b194b5665e36459dd917`；
- 实板导入后 `pydantic.compiled == False`，包目录递归 `.so` 数为 0；
- Pydantic v2 会强制依赖 Rust `pydantic-core`。当前 OpenAI 并不要求 v2，直接引入它会
  无必要地扩大 strict-aligned 原生闭包和实板验证面。

Python 3.14 支持边界参考
[Pydantic 1.10.26 PyPI metadata](https://pypi.org/pypi/pydantic/1.10.26/json) 和
[Pydantic PR #12636](https://github.com/pydantic/pydantic/pull/12636)。上游用词是 minimal
support，所以本报告只声明已覆盖的 OpenAI/模型校验路径，不外推为 Pydantic 全测试通过。

同时固定保留 `httpx 0.27.2`。OpenAI 1.35.15 创建客户端仍会传入后来被 HTTPX 0.28
移除的 `proxies` 参数；若解析器顺手升级到 0.28+，Pydantic 修好后还会在客户端构造处
得到新的 `TypeError`。该移除项见
[HTTPX 0.28.0 release notes](https://github.com/encode/httpx/releases/tag/0.28.0)。本次使用
`--no-deps`/固定 wheel，没有升级任何既有包。

### 12.3 主机下载、实板隔离验证与 P4 安装

主机从 PyPI JSON 选择精确 universal wheel，下载后先核对上游 SHA，再通过已经验证的
本地 GMAC/TFTP 链路传到 P2 `/scratch`。板端再次计算相同 SHA，避免把传输损坏当成
Python 导入问题。

最初尝试 `pip --target /scratch/pydantic-smoke` 暴露了另一个 ext4/VFS 兼容问题：pip
先在 `/tmp` 准备目标树，跨设备 `rename` 返回 `EXDEV` 后转入 `shutil.copytree`，复制文件
元数据时收到 `ENOSYS`。pip 最终输出 `Successfully installed` 后仍抛 `shutil.Error`，目标
只留下部分文件；该样本不得作为安装成功。

隔离验证改为把已校验 wheel 直接解到 ext4 新目录
`/scratch/pydantic-smoke2`。共 59 个文件、0 个 `.so`，随后用 strict runtime 的真实
loader 和显式测试 `PYTHONPATH` 验证：

```text
PYDANTIC 1.10.26 False /scratch/pydantic-smoke2/pydantic/__init__.py
OPENAI 1.35.15 OpenAI
NATIVE_SO 0
SMOLAGENT_OPENAI_MODEL OpenAI
```

正式安装没有复用半成品目录。wheel 重新解入同一 P4 ext4 上的独立 staging：

```text
/persist/python/staging/pydantic-1.10.26-c43ad70d
FILES=59 SO=0
```

确认 user-site 中不存在旧 `pydantic`/dist-info 后，分别以同盘 rename 发布到：

```text
/persist/python/user/lib/python3.14/site-packages/pydantic
/persist/python/user/lib/python3.14/site-packages/pydantic-1.10.26.dist-info
```

发布后执行 `sync`。P3 `/tools` 没有参与下载、bootstrap、导入或回退；100 ELF immutable
runtime 仍是 `43d7bb2ecf21`，本次只补 P4 application user-site 的纯 Python 依赖。

### 12.4 最终实板验证与性能含义

默认入口不设置测试 `PYTHONPATH` 时得到：

```text
DEFAULT_PYDANTIC 1.10.26 False /persist/python/user/lib/python3.14/site-packages/pydantic/__init__.py
DEFAULT_OPENAI 1.35.15 OpenAI
NATIVE_SO 0
DEFAULT_SMOLAGENT_OPENAI_MODEL OpenAI
```

结构化正式样本 `default_openai_smolagent_smoke` 使用 dummy key 和不可达本地 base URL，
只创建客户端、不发送请求，退出码 0，warm wall 为 37.074 s。该时间说明 P4 上
SmolAgent/OpenAI/Pydantic import 与客户端构造仍然昂贵，是后续 ext4 小文件/import
专项的有效入口；它不包含 DNS、TLS、公网或服务端排队，不能与真实 LLM 首 token 时间
混写。

安装后的 `pip check` 不再报告 `openai requires pydantic`。它仍以退出码 1 报告：

```text
MarkupSafe 3.0.3 is not supported on this platform
pillow 12.3.0 is not supported on this platform
```

两者的原生模块已通过实板 import/功能/ELF manifest 验证；这里是 pip 对自建
LoongArch wheel tag/安装 metadata 的兼容判断，不能解释成模块不能运行，也不能把
`pip check` 整体记为 PASS。后续应单独修正 wheel tag/metadata 门禁，使依赖检查能回到
退出码 0。

真实 API 没有重跑：旧串口命令曾把 API key 写入临时主机日志，本次已对日志脱敏，并
要求轮换该 key。没有使用、保存或复述该凭据。最终真实 API 验收必须使用轮换后的密钥，
并与本地固定响应端点分开计时。

本次补充原始证据位于
[`raw-data/20260718T-openai-dependency-audit/`](raw-data/20260718T-openai-dependency-audit/)，
包含 4 条 record、6 份 raw、manifest 和 records 索引。启动前 `openai_metadata` 超时、
两次串口 512-byte 长命令主机门禁失败均原样保留；只有带完整 marker 且 `rc=0` 的版本
审计和默认客户端构造用于成功结论。
