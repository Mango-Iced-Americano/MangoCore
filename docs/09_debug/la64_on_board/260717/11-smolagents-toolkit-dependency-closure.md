---
title: "SmolAgents 内置工具依赖闭包与 strict-aligned 实板发布"
category: debug
status: current
author: MangoCore Team
last_update: 2026-07-18
tags: [loongarch64, 2k1000la, python, smolagents, ddgs, lxml, primp, strict-align, ext4]
code_paths:
  - "scripts/build_cpython_runtime_la64_strict.sh"
  - "scripts/install_cpython_runtime_la64_strict.py"
  - "scripts/board/verify_persist_python.sh"
  - "user/tools/cpython/smolagents_toolkit_smoke.py"
  - "user/tools/cpython/strict_runtime_smoke.sh"
  - "user/tools/cpython/patches/boringssl-loongarch64-generic.patch"
related_docs:
  - "docs/09_debug/la64_on_board/260717/08-persist-strict-python-default.md"
  - "docs/09_debug/la64_on_board/260717/09-aligned-pillow-and-smolagent-closure.md"
  - "docs/09_debug/la64_on_board/260717/10-tty-smolagent-interactive-fix.md"
  - "docs/09_debug/la64_on_board/260717/06-raw-data-index.md"
---

# SmolAgents 内置工具依赖闭包与 strict-aligned 实板发布

## 1. 问题、范围与最终结论

交互式 `smolagent` 在工具选择阶段能列出 `python_interpreter`、`web_search` 和
`visit_webpage`，但实际选择 `web_search` 后直到构造工具才执行
`from ddgs import DDGS`，于是报 `No module named 'ddgs'`。这与此前 OpenAI 后端的
Pydantic 缺失属于同一类问题：console command、`--help` 和顶层 import 只覆盖 CLI
装载路径，不覆盖延迟导入的可选工具工厂。

本轮按 SmolAgents 1.26.0 当前实板源码和各发行包 metadata 闭合三个内置工具，不升级
SmolAgents、OpenAI 或 Pydantic，也不从 `/tools` 回退。最终结果为：

- `python_interpreter` 不新增第三方原生依赖；
- `web_search` 补齐 `ddgs -> click + primp + lxml`，其中 `primp` 和 `lxml` 必须纳入
  strict-aligned 原生闭包；
- `visit_webpage` 补齐 `markdownify -> beautifulsoup4 + six`，并补齐 BeautifulSoup 的
  默认选择器依赖 `soupsieve`；已有 requests/HTTP 基础栈继续由 P4 user site 提供；
- 最终 runtime manifest schema 4，原生 ELF 从 100 增至 113；不可变 release 中固定
  `lxml 6.1.1`、`primp 0.15.0` 和六个 pure Python 包；
- QEMU-user、安装器、双架构内核编译和 2K1000LA P4 ext4 实板门禁全部通过；
- P4 `current` 已原子切换为 release `28f61fb764f3`，默认 `python3`、`smolagent` 和三项
  工具均走该 aligned runtime，P3 `/tools` 没有参与。

## 2. 源码与 metadata 依赖矩阵

不能把 Rich 菜单中显示的工具名当成依赖已就绪。`TOOL_MAPPING[name]()` 才是首个能覆盖
构造期延迟 import 的入口。本轮同时核对工具源码与固定 wheel 的 `Requires-Dist`：

| CLI 工具 | 构造期路径 | 需要补齐的发行包 | 原生边界 | 本轮固定版本 |
|----------|------------|------------------|----------|--------------|
| `python_interpreter` | `TOOL_MAPPING[... ]()` 创建 Python interpreter tool | 无新增第三方包 | 无 | 沿用 SmolAgents 1.26.0 |
| `web_search` | `WebSearchTool.__init__ -> from ddgs import DDGS` | `ddgs`、`click`、`primp`、`lxml` | `primp` 为 Rust/PyO3；`lxml` 为 C 扩展并动态依赖 libxml2/libxslt/libexslt | 9.0.0 / 8.1.8 / 0.15.0 / 6.1.1 |
| `visit_webpage` | 页面获取后由 `markdownify`/BeautifulSoup 解析转换 | `markdownify`、`beautifulsoup4`、`six`、`soupsieve` | 本组均为 pure Python；BeautifulSoup 可调用已固化的 aligned lxml | 0.14.1 / 4.12.3 / 1.17.0 / 2.6 |

固定 metadata 给出的直接关系为：

```text
ddgs 9.0.0
  -> click >= 8.1.8
  -> primp >= 0.15.0
  -> lxml >= 5.3.0

markdownify 0.14.1
  -> beautifulsoup4 >= 4.9, < 5
  -> six >= 1.15, < 2

beautifulsoup4 4.12.3
  -> soupsieve > 1.2
```

`beautifulsoup4` metadata 中的 lxml、html5lib、charset-normalizer 等属于显式 extra，
不是默认安装闭包；本轮没有把未被当前三项工具选择的开发依赖和 parser extra 无界加入
runtime。另一方面，lxml 已被 ddgs 的必选依赖纳入，因此 VisitWebpage 路径也可使用
aligned lxml，而不会从 user site 引入未审计 native wheel。

## 3. 版本策略与为何没有升级 OpenAI/Pydantic

当前板上保留 `smolagents 1.26.0`、`openai 1.35.15` 和 pure Python
`pydantic 1.10.26`。OpenAI 1.35.15 的约束允许 Pydantic v1，且不需要 `jiter`；为了补齐
SmolAgents 内置工具而升级到 Pydantic v2，会额外引入 `pydantic-core`/Rust 原生边界，
扩大本轮目标并让已通过的 OpenAIModel 路径失去可比性。因此本轮只加入当前工具源码
真正需要的闭包。

pure Python 依赖也不直接依赖板上联网 pip 的最新解析结果，而是固定版本、下载 URL、
SHA-256 和 wheel tag。这样相同构建脚本会产生相同依赖选择，未知平台 wheel 或 native
fallback 会 fail closed。

## 4. strict-aligned 原生构建链

### 4.1 lxml、libxml2 与 libxslt

`lxml 6.1.1` 从 sdist 构建精确
`lxml-6.1.1-cp314-cp314-linux_loongarch64.whl`，不接受宿主 wheel，也不把 Cython 作为
目标端依赖。底层使用 `libxml2 2.14.6`、`libxslt 1.1.43` 和 libexslt：

- GCC/C/C++ 统一使用
  `-march=loongarch64 -mabi=lp64d -mstrict-align`；
- libxml2 构建日志审计 121 个编译单元，libxslt 审计 88 个，lxml 审计 7 个；任一编译
  命令缺少 strict flag 或目标编译单元数量异常都会中止；
- wheel 必须是 cp314/LoongArch tag，安装后要求 lxml 原生扩展存在；
- manifest 记录 `libxml2.so.16`、`libxslt.so.1`、`libexslt.so.0` 的动态闭包，ELF hash 和
  `DT_NEEDED` 继续由原有完整性检查逐项验证。

### 4.2 primp、Rust 与 BoringSSL

`primp 0.15.0` 是 `cp38-abi3` Rust/PyO3 扩展，不能用 CFLAGS 证明 Rust 代码已严格对齐。
构建固定 Rust nightly `2025-01-18`、maturin 1.8.3 和 sdist 的 `Cargo.lock`：

- Rust 目标为 `loongarch64-unknown-linux-musl`，使用
  `RUSTFLAGS='-C target-feature=-ual'` 禁止 Rust 生成非对齐访问；构建日志必须出现该参数；
- C/C++ 子依赖继续使用 `-mstrict-align`；
- `boring-sys2 4.15.11` 携带的 BoringSSL 不认识 LoongArch。项目补丁只增加 64 位 generic
  LoongArch CPU 分类，定义 `OPENSSL_NO_ASM`，禁用架构汇编，并生成 compile database；
- BoringSSL 278 个编译单元逐项要求同时含 `-mstrict-align` 和
  `-DOPENSSL_NO_ASM`；不是只检查最终 `.so` 文件名；
- bindgen 显式使用目标 sysroot，最终 runtime 同时打包工具链精确
  `libgcc_s.so.1` 和 `libstdc++.so.6.0.34`，闭合 primp/BoringSSL 的运行时动态依赖；
- 最终 wheel 必须为
  `primp-0.15.0-cp38-abi3-linux_loongarch64.whl`，其他 tag 一律拒绝。

调通中依次暴露过 BoringSSL `Unknown target CPU`/`BN_ULONG`、上游 pq patch 的前置关系、
bindgen 目标头文件、BoringSSL archive 输出路径和运行时 `libgcc_s` 缺失。最终补丁围绕
“LoongArch generic C + no asm”建立可审计边界，没有开启未经 strict 审计的汇编快路径。

### 4.3 pure Python 包

`ddgs`、`markdownify`、`beautifulsoup4`、`soupsieve`、`six` 和 `click` 只接受固定 universal
wheel。安装器校验 wheel tag、RECORD 路径和版本；manifest 逐包记录 wheel SHA。此处的
“pure”只说明这些发行包没有新增 ELF，并不取消其功能门禁。

## 5. 两层 smoke：不可变 release 与默认有效环境

P4 默认 Python 会加载 `/persist/python/user`。该目录允许 pure Python 应用包存在，因此
例如用户已安装的 `click 8.4.2` 可以遮蔽 release 内的 `click 8.1.8`。若只在正常 site
模式要求所有版本精确等于构建锁定值，会把兼容的 pure Python 遮蔽误报为 aligned native
闭包失败；若完全接受遮蔽，又无法证明新 release 本身完整。

最终门禁分成两层：

1. **exact release smoke**：`python -S` 手动加入当前 release 的 site-packages，所有八个
   工具包版本必须精确匹配，lxml 解析、HTML/Markdown 转换、soupsieve/six/click 行为以及
   `primp.Client`、`DDGS` 离线构造均通过；
2. **effective default smoke**：使用默认 wrapper 和 normal site；lxml 6.1.1、primp 0.15.0
   两个 native 包仍必须精确来自 manifest release，pure Python 包按已审计兼容 major
   验收，并继续禁止 `/persist/python/user` 出现任何 `.so`/`.so.*`。

`verify_persist_python.sh --require-smolagents` 又增加第三层应用门禁：从 SmolAgents 的真实
`TOOL_MAPPING` 构造 `python_interpreter`、`web_search`、`visit_webpage`，而不是只 import
底层发行包。

所有网络客户端使用构造期离线测试，没有发真实搜索或 LLM 请求。这样能确定依赖和 ABI
闭包，但不能把网络服务可用性、公网延迟或搜索结果语义算作本轮已验证项。

## 6. 制品与主机/QEMU 验证

最终 canonical artifact：

| 字段 | 值 |
|------|----|
| 文件 | `cpython-la64-strict-3.14.5-28f61fb764f3.tar.xz` |
| 大小 | 87,057,368 B |
| SHA-256 | `28f61fb764f3c25ba2f5b032259b47a491334f382ed243475cfcbaaad1d1e75e` |
| manifest SHA-256 | `79b62ebc16e710347dba720969036dda8ebaf73c453c1ce84febbe263ddec70c` |
| manifest schema / ELF | 4 / 113 |
| Python | 3.14.5，PGO + LTO，P4 PT_INTERP |
| 内核非对齐模拟器 | 未修改 |

QEMU-user 的 `python -S` exact smoke 输出全部锁定版本，并完成 lxml、primp/DDGS constructor
路径。主机安装器对 archive member、安全链接、artifact/manifest/113 ELF hash、PT_INTERP
和 11 个 Python 包版本验证通过；原子解压到独立目录后再次通过相同 QEMU smoke。

项目 Docker 内严格串行执行：

```text
make rv64-kernel-build-only  -> exit 0
make la64-kernel-build-only  -> exit 0
```

RV64 首次非 root 容器尝试因 rustup 目录权限被拒绝，随后按项目容器权限模型重跑成功；
这不是内核编译错误。双架构没有并行切换 nightly，也没有手工编辑生成的
`lang_items.rs`。

## 7. 2K1000LA P4 ext4 发布与实板证据

部署器先确认 `/persist` 为 rw ext4，再把 archive 传入临时位置、校验 SHA、解包到 P4
同文件系统 staging，执行 exact runtime smoke 和 113 ELF 完整性检查，最后才用 rename
发布 release 并原子更新 `current`。P3 `/tools` 没有被读取、写入或执行。

第一次候选制品在 exact release smoke 已通过，但默认环境中 user-site 的 click 8.4.2
遮蔽 locked click 8.1.8，旧的单层 smoke 因精确版本断言拒绝发布；部署器保持旧 current
并清理 staging。引入上节的 exact/effective 双层门禁后，重新打包得到最终 artifact，发布
wall 为 440.199 s，成功切换：

```text
/persist/python-runtime/current
  -> /persist/python-runtime/releases/28f61fb764f3
```

最终实板结果：

| 门禁 | 结果 | wall |
|------|------|-----:|
| default effective toolkit smoke | lxml/primp 精确；六个 pure 包兼容；离线构造通过 | 37.502 s |
| 默认路径检查 | `CPYTHON_ROOT`、`sys.executable`、lxml native 模块均指向 28f release | 20.386 s |
| `smolagent --help` | 退出 0 | 60.258 s |
| 三项真实 `TOOL_MAPPING` 构造 | `python_interpreter,web_search,visit_webpage` 全部通过 | 62.117 s |
| user-site native 审计 | `/persist/python/user` 无 `.so`；primp 从 28f release 加载 | 28.390 s |

运行板卡仍是本轮修改前的 persist-shell 启动镜像，尚无新打包的
`/rescue/verify-persist-python`，因此调用该路径返回 127；相同的关键子门禁已经用 release
脚本和真实默认命令逐项执行。新构建的 initramfs 已包含更新后的 verifier，但本轮按用户
要求没有为了这个脚本重启板卡。

一个 `python -c` 形式的工具构造探针在 `python-dotenv.find_dotenv()` 中把 `<string>` 当作
真实脚本路径并失败；将完全相同代码写入实板 `/tmp/t` 后，三项工具构造通过。真实
`smolagent.real` 是磁盘脚本，不走 `<string>`，所以该条保留为诊断命令边界，不作为
SmolAgent 工具失败。

## 8. 数据质量、剩余边界与复现

原始 run `20260718T050500Z-smolagents-toolkit-closure` 有 15 条 record，其中 11 条退出 0；
四条非零分别是：首个候选部署被版本遮蔽门禁安全回滚、旧镜像缺 verifier、上述
`python -c`/dotenv 探针，以及检查不存在的 stale prompt 路径 `/persist/apk-root`。另外
两条超过 512 字节的宿主命令在串口发送前就被 harness 拒绝，没有形成 record。失败日志
全部保留，最终结论只引用后续相同目的的成功门禁。

已确认：当前 release 自身依赖闭合、默认有效环境可用、所有 native 扩展均来自 aligned
manifest、三个内置工具能构造。尚未确认：真实 DDGS 搜索、公网页面下载、真实 LLM 请求、
服务端排队和 30 分钟交互稳定性。未来执行这些网络测试时，应继续把本地构造/固定响应端点
与公网 wall time 分开，不把服务端波动归因内核。

主机复现入口：

```text
make cpython-la64-runtime-build
make cpython-la64-runtime-verify

python3 scripts/install_cpython_runtime_la64_strict.py \
  --artifact target/cpython-strict/artifacts/cpython-la64-strict-3.14.5-28f61fb764f3.tar.xz \
  --destination target/cpython-strict/install-smoke

python3 scripts/kernel_perf.py analyze \
  --run-dir target/perf-runs/20260718T050500Z-smolagents-toolkit-closure
```

详细串口、records、派生报告和构建日志见
[raw-data/20260718T-smolagents-toolkit-closure/](raw-data/20260718T-smolagents-toolkit-closure/)。
