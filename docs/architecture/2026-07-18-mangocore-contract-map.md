# MangoCore Phase-0 Repository Contract Map

**日期：** 2026-07-18
**观察提交：** `883f73c2`
**分支：** `chore/clean-up`
**范围：** 只记录 Phase-0 的基线边界和待证明事项，不声明重基线已经完成。

## 1. 读法和状态

本文只使用以下四种状态。状态描述的是证据等级，不是优先级。

| 状态 | 含义 |
|---|---|
| **observed at 883f73c2** | 从观察提交中的受版本控制文件读取到的行为或消费者。它不是认可的未来设计，也不是运行成功证明。 |
| **uncommitted candidate** | 当前工作树中的未提交或未跟踪重基线改动。它不能改变观察提交的事实，也不能作为已完成工作的证据。 |
| **required future contract** | `docs/plans/repository-rebaseline.md` 要求后续实现和验收的目标。它不是当前行为。 |
| **unverified** | 目前没有本轮允许的运行、构建、启动、CI 或元数据证据。静态引用可以保留，但不能写成通过或完成。 |

本次 Phase-0 文档提交允许且只允许包含以下四个文件：`docs/plans/repository-rebaseline.md`、`docs/architecture/2026-07-18-mangocore-contract-map.md`、`docs/architecture/2026-07-18-mangocore-contract-matrix.yaml`、`docs/architecture/2026-07-18-verify-contract-map.sh`。按计划的 no-evidence-commit policy，`docs/Work_Log/**`、evidence、原始日志、镜像、缓存、boulder 状态和任何产品改动都必须保持不在本提交中。

## 2. 基线边界

### 2.1 已观察事实

以下事实来自 `883f73c2` 及其四个历史清理提交 `0c446a77`、`40d86b2f`、`6c4c69e3`、`883f73c2` 的只读检查：

* 当前观察点是 `883f73c2`，不是 `60800fa2`。观察点之后工作树出现了大量候选改动，不能写成基线内容。
* 根 `Makefile` 的 `all` 依次执行 `$(MAKE) prepare-cargo-config`、`$(MAKE) clean`、`$(MAKE) -C os all`。其中 `clean` 是无条件步骤，所以基线根入口不是增量构建入口。
* 根入口先经过根 `prepare-cargo-config`，然后进入 `os/Makefile`。`os/Makefile all` 再执行自己的 `prepare-cargo-config`，然后按 `rv64_all`、`la64_all` 顺序编译。这个顺序来自 Make 配方，不代表双架构已经通过。
* `os/Makefile` 的 RV64 和 LA64 入口会设置不同的 Rustup override，并复制架构对应的 lang item 文件。架构 Makefile 的 `env` 还会调用 `rustup target add` 和 `rustup component add`。
* 基线没有根 `rust-toolchain.toml`、`scripts/rustup-setup.sh` 或 `scripts/rustup-preflight.sh` 作为观察提交中的统一工具链合同。当前工作树中出现的这些文件属于候选工作。
* 基线的 Cargo 配置位于 `cargo-config/{os,user}/config.toml`，目标配置位于 `os/.cargo/config.toml` 和 `user/.cargo/config.toml`。`prepare-cargo-config` 会在目标文件不存在时复制配置，并调用 `scripts/restore-cargo-vendor-checksums.sh restore`。
* 基线架构配方会把 `lang_items.rs.rv` 或 `lang_items.rs.la` 复制到活动的 `lang_items.rs`，把板级 linker 文件复制到活动的 `linker.ld`，并用 `touch` 强制 initramfs 汇编重新编译。
* 基线 lwext4 配方会把 CMake toolchain 和 `ulibc.c` 复制进 `dependency/lwext4_rust/c/lwext4/`，并用 `sed` 修改其 `src/CMakeLists.txt`。这些路径属于依赖树，不是隔离的构建输出。
* 基线构建输出包括根目录的 `kernel-rv`、`kernel-la`、`disk.img`、`disk-la.img`，`fs-img-dir` 下的 rootfs 和 initramfs，以及 `os/target`、`user/target` 和 lwext4 构建目录。部分日志和临时挂载位于 `/tmp` 或 checkout 中。

### 2.2 当前工作树的候选内容

**uncommitted candidate** 包括根 `rust-toolchain.toml`、`scripts/rustup-setup.sh`、`scripts/rustup-preflight.sh`、`scripts/loongarch64-clang-lld.sh`，以及当前工作树中对根 Makefile、嵌套 Makefile、Cargo 配置、lang item、linker、lwext4、Docker、CI 和文档的修改。它们是重基线的候选实现或配套改动，不能反写为 `883f73c2` 的观察结果。

候选内容未在本轮构建、启动、CI 或纯净性检查中验收。候选文件是否应保留、拆分或回滚，由后续各阶段的独立提交和验收门决定。

### 2.3 未来必须满足的边界

**required future contract** 来自计划第二节及 Phase-0 到 Phase-6 的出口门：

1. 根 `make all` 保留为双架构正式入口，RV64 后 LA64 串行执行，失败必须传播，且不得无条件 clean。
2. 工具链声明为一个经过环境审计的 dated nightly。正式 build、run、test 不得调用 Rustup mutation、下载、隐藏 fallback 或 `|| true`。setup 必须显式，preflight 必须只读。
3. 输出按架构、模式和 profile 隔离到 `build/` 或其他声明的 out-of-tree 路径。只有两个架构阶段都成功后才能发布根兼容产物。
4. Cargo 配置、linker、lang item、initramfs 和 lwext4 集成必须通过声明的非变异输入提供。构建不得修改 tracked source、vendor、配置、checksum 或生成文件。
5. 官方 x0 输入保持不变。正式接口保留 x0 加 x1 两个角色，测试注入使用命名的派生镜像。
6. normal、competition、regression、ktest、development 和 debug profile 必须有可核对的启动合同，其中 debug 是 development 的变体。normal 和 competition 的 PID 1、runner、mount policy、reap 和 shutdown 责任必须分开。
7. warning、check、lint、QEMU smoke、相关测试和 CI 必须有可执行的非绕过门。任何失败都不能靠屏蔽输出或推断结果升级为通过。

## 3. 入口、消费者和产物

下表保留真实的外部消费者，但状态只表示当前证据等级。

| 入口或对象 | 观察到的消费者 | 当前状态 | 说明 |
|---|---|---|---|
| 根 `make all` | 开发者、`scripts/run_full_test.py` 的主流程、CI main 间接调用 | **observed at 883f73c2** | 执行 `prepare-cargo-config`、无条件 `clean`、`make -C os all`。source purity、增量性和双架构成功均未由本轮证明。 |
| `make -C os rv64_all` | CI develop、开发者、根 `os all` | **observed at 883f73c2** | 设置 RV64 override，复制 RV64 lang items，构建用户态、rootfs、initramfs、lwext4 和 kernel。 |
| `make -C os la64_all` | CI develop、开发者、根 `os all` | **observed at 883f73c2** | 设置 LA64 override，复制 LA64 lang items，构建对应产物。 |
| `rv64-run`、`la64-run`、各 `comp` 配方 | 开发者、CI develop、`run_full_test.py` 同类 QEMU 流程 | **observed at 883f73c2** | 使用架构对应 kernel、x0 evaluator image 和 x1 tools image。实际启动结果本轮没有验证。 |
| `regression-all`、`rv64-regression`、`la64-regression` | 开发者和 CI 文件中的历史或显式引用 | **observed at 883f73c2** | 设计为无磁盘 initramfs profile。是否在当前环境完整工作，属于 **unverified**。 |
| `ktest-all`、`rv64-ktest`、`la64-ktest` | 开发者、`bugscan` | **observed at 883f73c2** | 设计为 `mango.mode=ktest` 的无磁盘路径。结果和 runner 行为 **unverified**。 |
| `check-fast`、`unittest`、`bugscan` | 根 Makefile、`os/Makefile`、开发者 | **observed at 883f73c2** | 存在 host 或 RV64 检查路径差异，且根 `check-fast` 的 clippy 失败被 `|| true` 遮蔽。适用性和结果 **unverified**。 |
| `scripts/run_full_test.py` | CI main、开发者 | **observed at 883f73c2** | 先运行根 `make all`，再解压 evaluator images，启动两架构 QEMU，并调用 `judge/run_parse.py`。本轮未运行。 |
| `judge/run_parse.py`、`judge/run_judge.py`、`judge/judge_*.py` | `run_full_test.py`、CI、手工 judge | **observed at 883f73c2** | 是串口结果解析和分组评分的外部消费者。评分结果、输入格式和所有 judge 分支均 **unverified**。 |
| `testsuits-download` | 开发者、CI develop 和 CI main | **observed at 883f73c2** | 下载 `sdcard-rv.img.xz` 和 `sdcard-la.img.xz`。网络、版本、校验和及内容未在本轮验证。 |
| `docker-compose.yml`、根 `docker` | 开发者 | **observed at 883f73c2** | 开发容器入口，使用 `zhouzhouyi/os-contest:20260104`。容器内工具版本和挂载行为未在本轮运行确认。 |
| `.github/workflows/ci-develop.yml` | push 到 `develop` | **observed at 883f73c2** | 使用 `zhouzhouyi/os-contest:20260104`，分别编译两个架构，并并行启动 QEMU。 |
| `.github/workflows/ci-main.yml` | push 到 `main` | **observed at 883f73c2** | 使用同一 CI image，通过 Docker 调用 `scripts/run_full_test.py`，失败由后续 gate 汇总。 |
| 根 `DOCKER_IMAGE` | 根 Makefile 的 competition 或兼容路径 | **observed at 883f73c2** | 基线默认值是 `docker.educg.net/cg/os-contest:20250614`，与 CI 的 `zhouzhouyi/os-contest:20260104` 不同。等价性没有证明。 |
| 根 `DOCKER_IMAGE` 与开发/CI image 的实际关系 | 根 Makefile、开发容器、CI | **unverified** | compose 与两个 CI workflow 在基线均使用 `zhouzhouyi/os-contest:20260104`；根 Makefile 的 distinct default 是 `docker.educg.net/cg/os-contest:20250614`。该默认值是否实际被使用，以及两者的工具和依赖等价性，owner 是环境维护者，exit condition 是同一矩阵上的并列审计和结果记录。 |

### 3.1 产物和外部输入

| 对象 | 生产者或来源 | 外部消费者 | 状态 |
|---|---|---|---|
| `kernel-rv`、`kernel-la` | `os/make/{rv64,la64}.mk` 的 `mv` | QEMU `-kernel`、judge 流程、CI artifact | **observed at 883f73c2**，未证明当前内容可启动 |
| `rootfs-rv.img`、`rootfs-la.img` | `buildfs.sh` | normal profile QEMU x0 | **observed at 883f73c2**，分区、标签和 mount 语义 **unverified** |
| `disk.img`、`disk-la.img` | tools disk 配方 | normal 和 competition QEMU x1 | **observed at 883f73c2**，内容清单和分区元数据 **unverified** |
| `sdcard-rv.img.xz`、`sdcard-la.img.xz` | OSComp release 下载 | competition QEMU x0、judge | **observed at 883f73c2**，外部内容和 checksum **unverified** |
| `initramfs-*.cpio`、`initramfs-regression-*.cpio` | `build_initramfs.sh` | kernel incbin、regression QEMU | **observed at 883f73c2**，PID 1、runner 和 payload 完整性 **unverified** |
| `os/target/*`、`user/target/*` | Cargo | kernel、用户态和 image builder | **observed at 883f73c2**，输出隔离属于 **required future contract** |
| lwext4 `.a` 和 `build_lwext4-*` | `os/make/{rv64,la64}.mk` | kernel link | **observed at 883f73c2**，当前会污染依赖树；移出依赖源是 **required future contract** |

## 4. Rustup、配置和源树变异

### 4.1 基线已观察到的变异

这些不是候选设计，而是 `883f73c2` 配方中可见的行为：

| 路径或状态 | 基线行为 | 状态 |
|---|---|---|
| Rustup default/override | 根 `env` 执行 `rustup default $(LA_TOOLCHAIN)`；RV64 和 LA64 入口执行对应 `rustup override set` | **observed at 883f73c2** |
| Rustup target/component | RV64 `env` 添加 target、`rust-src`、`llvm-tools-preview`；LA64 `env` 添加 target、`rust-src` | **observed at 883f73c2** |
| `os/src/lang_items.rs`、`user/src/lang_items.rs` | 架构入口从 `.rv` 或 `.la` 复制覆盖活动文件 | **observed at 883f73c2** |
| `os/.cargo/config.toml`、`user/.cargo/config.toml` | `prepare-cargo-config` 按条件复制，并恢复 vendor checksum | **observed at 883f73c2** |
| `os/src/hal/arch/riscv/linker.ld` | RV64 kernel 配方复制板级 linker | **observed at 883f73c2** |
| `os/src/hal/arch/loongarch64/linker.ld` | LA64 kernel 配方复制板级 linker，并带 fallback shell 逻辑 | **observed at 883f73c2** |
| `os/src/initramfs-rv.S`、`os/src/initramfs-la.S` 及 regression 变体 | initramfs 配方用 `touch` 强制 Cargo 重编译 | **observed at 883f73c2** |
| `dependency/lwext4_rust/c/lwext4/*` | CMake toolchain、`ulibc.c` 和 `src/CMakeLists.txt` 被复制或编辑 | **observed at 883f73c2** |
| vendor checksum 和依赖状态 | restore 脚本可能恢复或改变 vendor checksum；本轮没有执行前后清单 | **unverified**，owner 是构建维护者，exit condition 是基线前后状态清单和失败注入结果 |

### 4.2 未来合同和候选实现的区别

统一的根 `rust-toolchain.toml`、只读 `toolchain-preflight`、显式 `toolchain-setup`、不调用 Rustup mutation 的正式入口，以及 out-of-tree 的 Cargo、linker、lang item、initramfs 和 lwext4 输入，均属于 **required future contract**。当前工作树中对应文件和修改属于 **uncommitted candidate**，不是观察基线，也没有本轮验收证明。

基线的 Rustup mutation、tracked lang-item/linker/Cargo 配置变异、lwext4 源树变异和 source-adjacent 输出，都是 Phase-0 需要保留的 RED 风险事实。不能因为候选脚本声称 preflight 或 setup 就把基线写成已经隔离。

## 5. 镜像、磁盘和 QEMU 边界

### 5.1 已观察的角色

本文统一使用以下 profile 名称：normal、competition、regression、ktest、development、debug；其中 debug 是 development 的变体，不是独立的产品 profile。

基线 QEMU 配方表达了两个正式磁盘角色：`x0` 为 evaluator 或 rootfs 输入，`x1` 为 tools 或 scratch image。RV64 默认使用 virtio-mmio，`BLK_MODE=virt_pci` 时使用 virtio-pci；LA64 使用 virtio-pci。regression 和 ktest 配方设计为零磁盘启动。以上仅是 **observed at 883f73c2** 的命令和路径，不是启动成功证明。

**required future contract** 要求保留 x0 加 x1 的双盘 ABI，禁止正常构建修改 evaluator x0，测试注入必须创建派生镜像，并用标签、UUID、分区元数据或集中 role map 表达角色，不能散落固定 `/dev/vdb2` 之类的假设。

### 5.2 磁盘元数据和启动结果

下列内容是 **unverified**：

* x0 和 x1 的分区表、文件系统、label、UUID、容量、挂载目标和设备枚举顺序。
* normal、competition、regression、ktest、development、debug 的实际 kernel 参数、固件路径、网络、超时、串口 marker 和 shutdown 结果。
* `/sbin/init`、`/initproc`、runner、reap、mount policy、PID 1 failure marker 和缺失 runner 时的非零结果。
* RV64 与 LA64 在当前 Docker 镜像和当前外部 sdcard 内容上的对称性。

owner 是 boot/image maintainer，exit condition 是逐 profile 的命令、镜像 checksum、分区元数据、完整串口日志和退出状态矩阵。没有这些材料，不得声称 QEMU boot、PID 1、runner 或 disk ABI 已完成。

## 6. Docker、CI、warning 和 proof 状态

### Docker 与根 Makefile 的 image 边界

**observed at 883f73c2：** `docker-compose.yml`、CI develop 和 CI main 都使用 `zhouzhouyi/os-contest:20260104`。根 Makefile 的 `DOCKER_IMAGE` 默认值则是 distinct 的 `docker.educg.net/cg/os-contest:20250614`。

根 Makefile 的 `DOCKER_IMAGE` 是否在当前入口中实际使用，以及该 image 与 `zhouzhouyi/os-contest:20260104` 的内容等价性、工具链版本、QEMU、binutils、CMake、debugfs、网络和磁盘空间条件是 **unverified**。owner 是 CI 和环境维护者，exit condition 是记录官方 image digest、组件和工具版本，并在同一 clean environment acceptance matrix 上复现正式命令。

### Warning policy

**observed at 883f73c2：** 根 `check-fast` 的 clippy 命令带有 `2>/dev/null || true`，因此该路径不能作为非绕过 lint gate。基线没有四格 warning inventory，也没有证明 first-party、维护依赖和第三方 vendor warning 的归属。

**required future contract：** 记录 RV64 和 LA64 的 debug/release warning facts，优先处理高风险 first-party warning，提供非绕过的 `make check` 和 `make lint`，禁止用 broad allow、隐藏输出或 `|| true` 掩盖失败。未来 gate 当前为 **unverified**。

### CI 和 QEMU proof

Phase-0 verifier 已成功运行并检查了本地图的静态结构和声明一致性；这不构成 build、QEMU、CI 或 source-purity proof。因此：

* 基线编译是否成功，状态为 **unverified**。
* 双架构串行是否完成，状态为 **unverified**。
* QEMU normal、competition、regression、ktest 是否启动、到达 PID 1、运行 runner 或正常退出，状态为 **unverified**。
* judge 分数、CI gate、warning 数量和工作树清洁度，状态为 **unverified**。
* 任何已有日志、evidence 目录或工作树候选文件都不能替代本轮缺失的当前 proof。

## 7. 未知项登记

| 未知项 | 当前状态 | owner | exit condition |
|---|---|---|---|
| x0/x1 分区、label、UUID 和 mount target | **unverified** | image/boot maintainer | 读取每个 profile 的镜像元数据并记录 role map 和 checksum |
| Docker image 等价性 | **unverified** | CI/environment maintainer | 固定 digest，记录工具链和工具版本，在本地与 CI 矩阵复现 |
| `/sbin/init`、`/initproc` 和 runner 的真实关系 | **unverified** | boot/PID1 maintainer | 从 image manifest、kernel bootstrap、PID 1 和 runner 串口图建立调用图，并完成双架构 smoke |
| judge 输入和所有脚本的实际消费者 | **unverified** | test maintainer | 用静态引用、CI 配置、历史和一次独立 dry review 闭合每个消费者，保留 owner 决策 |
| vendor checksum restore 的实际变异集合 | **unverified** | build maintainer | 在 `883f73c2` 基线捕获命令前后文件清单，确认失败和恢复路径 |
| `judge/cancel_purge`、`scripts/analyze_drift.py`、`auto_*_ltp.py`、`diag_smoke_test.sh` 及若干 legacy 文件 | **unverified** | plan executor | 搜索源码、文档、CI、生成清单和历史，决定保留、接入或删除，并记录替代路径 |
| warning 四格事实和适用的 test/lint 命令 | **unverified** | quality maintainer | RV64/LA64、debug/release 各跑一次适用 gate，分类 warning 并记录非零失败 |

未知项不能写成“未使用”或“已废弃”，除非对应 owner 和 exit condition 产生了新证据。

## 8. Phase-0 出口和禁止声明

**required future contract** 的 Phase-0 出口是：地图列出公开命令及消费者、最终产物、磁盘角色、启动 profile、候选路径和所有带 owner 的未知项；基线的 clean 和 source mutation 以 RED 事实记录；候选工作不被写入观察状态；没有 proof 的项目保持 **unverified**。

本文件不声明以下任何事项已经完成：统一 nightly、Rustup 隔离、增量构建、source purity、out-of-tree 输出、lwext4 隔离、镜像或分区契约、normal boot、PID 1、runner、warning policy、CI gate、QEMU 测试、judge 结果或最终 clean checkout。

本 Phase-0 文档提交也不包含 Work_Log、evidence、raw logs、generated reports、temporary images、cache contents、container state 或 boulder。后续实现必须按四文件 allowlist 拆分为独立、可审查、可回滚的提交，并在缺少 proof 时保持阻塞状态。

## 9. 参考边界

* 观察事实：`883f73c2` 及其四个前置 cleanup commit。
* 执行计划：`docs/plans/repository-rebaseline.md`。
* Phase-0 配套 YAML 和 verifier 属于本次文档提交的四文件 allowlist；verifier 的成功运行只证明静态检查通过，不证明 build、run、Make、Cargo、Docker、QEMU、Rustup、network、CI 或 source purity。
