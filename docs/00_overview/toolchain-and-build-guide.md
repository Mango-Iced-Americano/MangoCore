---
title: "MangoCore 工具链、Docker 与构建测试使用手册"
category: overview
status: current
author: MangoCore Team
last_updated: 2026-08-19
tags: [toolchain, docker, make, rust, qemu, testing, workflow]
related_docs:
  - "../../README.md"
  - "../../AGENTS.md"
  - "../08_testing/README.md"
  - "../08_testing/l5-integration.md"
---

# MangoCore 工具链、Docker 与构建测试使用手册

本文只讲内核源码之外的开发工具链和工程入口：Docker、Rustup、Cargo、顶层 Make
facade、os/ 构建层、用户态/镜像打包、QEMU、测试配置、日志和常见故障处理。
它不替代各子系统设计文档，也不讲 Rust 内核实现细节。

本文按当前仓库的 Makefile、os/Makefile、docker-compose.yml、rust-toolchain.toml
和 scripts/ 入口整理。除非特别注明，命令都应在项目根目录执行；所有编译、运行和
测试命令都应在 Docker 开发容器内执行。

> 本手册中的命令是操作示例。阅读本文不会自动执行任何命令。

## 0. 先记住的规则

### 0.1 Docker 优先

宿主机主要负责 Git、Docker、编辑器和文件查看。Rust 编译、Cargo、QEMU、镜像处理和
测试脚本优先放在 os-dev 容器内运行：

~~~bash
make docker
~~~

容器把当前仓库挂载到 /app，所以宿主机编辑的文件会立即出现在容器内。

### 0.2 双架构必须串行

RV64 和 LA64 共用根目录的 pinned nightly、Cargo/rustup 缓存以及部分架构生成状态。
不要在两个终端同时执行 RV64 和 LA64 构建。正确顺序是：

~~~text
RV64 → 完成并确认结果 → LA64
~~~

~~~bash
make kernel ARCH=rv64 PROFILE=normal
make kernel ARCH=la64 PROFILE=normal
~~~

### 0.3 正式 facade 命令显式写 ARCH 和 PROFILE

虽然 Makefile 中有默认值，正式目标仍会检查 ARCH 和 PROFILE 是否由命令行或环境显式
提供。推荐总是写完整：

~~~bash
make check ARCH=rv64 PROFILE=normal
~~~

| 参数 | 有效值 | 含义 |
|---|---|---|
| ARCH | rv64、la64 | 选择 RISC-V 64 或 LoongArch 64 |
| PROFILE | normal、regression | 普通启动镜像或零盘回归配置 |
| MODE | release、debug | Cargo/产物构建模式；默认 release |

### 0.4 分清 MODE 和 PROFILE

~~~text
MODE=release/debug          编译优化模式
PROFILE=normal/regression   启动与测试镜像配置
~~~

~~~bash
make build ARCH=rv64 PROFILE=normal MODE=release
make check ARCH=rv64 PROFILE=regression MODE=debug
~~~

run 只接受 PROFILE=normal；test 只接受 PROFILE=regression；user 和 image 也只接受
PROFILE=normal。

### 0.5 提交前不要自行 commit

本项目要求修改、验证和汇报完成后保留未暂存工作树。只有用户明确批准当前批次提交时，
才执行 git add、git commit、push 或创建 PR。

## 1. 开发环境的两层结构

~~~text
项目根目录 Makefile
    ↓ 传递 ARCH / PROFILE / MODE / BUILD_ROOT 等参数
os/Makefile
    ↓ 选择 make/rv64.mk 或 make/la64.mk
架构构建脚本、Cargo、用户态和 QEMU
~~~

### 1.1 根目录 Makefile：日常首选入口

~~~bash
make kernel ARCH=rv64 PROFILE=normal
make build ARCH=rv64 PROFILE=normal
make run ARCH=rv64 PROFILE=normal
~~~

根目录 facade 负责检查 ARCH/PROFILE、准备工具链、传递 BUILD_ROOT/MODE/PROFILE，并
选择 os/ 的架构 Makefile。

### 1.2 os/Makefile：需要精细控制时使用

~~~bash
make -C os ktest-run ARCH=rv64 PROFILE=normal
make -C os conf-inject CONF_ARCH=rv64 CONF_BLK_MODE=virt CONF_FILE=../os_test.conf
~~~

-C os 表示先切换到 os/ 再执行 Make。它适合 ktest 细分入口、测试配置注入、架构特有
运行目标和调试底层 Make graph。

初学时不要绕过根目录 facade 直接调用 make -f os/make/rv64.mk，除非已经明确该底层
目标需要哪些变量。

## 2. 第一次使用：从宿主机到可运行构建

### 2.1 宿主机前置条件

宿主机至少需要 Git、Docker、Docker Compose v2（docker compose），以及足够的磁盘空间。
源码、Rust 工具链、Cargo 缓存、QEMU 和测试镜像会持续增长。网络受限时还要准备
Rust/GitHub 下载代理。

宿主机不要求直接安装项目 pinned nightly，也不建议在宿主机直接执行裸机目标编译。

### 2.2 克隆、检查、进入容器

~~~bash
git clone <repo-url> MangoCore
cd MangoCore
git status --short
make docker
~~~

进入容器后确认：

~~~bash
pwd
ls
~~~

工作目录通常是 /app。不要因为工作树有未跟踪日志或产物就执行清理命令，先确认它们
是否属于当前工作。

### 2.3 工具链检查和安装

~~~bash
make toolchain-preflight
~~~

这是只读检查，不会安装缺失工具链。若缺失：

~~~bash
make toolchain-setup
make toolchain-preflight
~~~

setup 会读取 rust-toolchain.toml，按 channel、targets 和 components 安装 pinned
toolchain，可能访问网络。

### 2.4 第一次构建和启动

~~~bash
make build ARCH=rv64 PROFILE=normal
make build ARCH=la64 PROFILE=normal
make run ARCH=rv64 PROFILE=normal
~~~

两个 build 必须串行。若目标是评测式双架构完整产物：

~~~bash
make all
~~~

make all 会自动 setup 工具链，在 os/ 内依次构建 RV64、LA64，最后发布兼容产物。它
适合全新环境和正式完整构建，不适合每次只改一行代码时使用。

## 3. Docker 开发容器

### 3.1 当前容器配置

| 配置 | 当前行为 | 影响 |
|---|---|---|
| 工作目录 | /app | 对应宿主机项目根目录 |
| 项目挂载 | .:/app | 源码和产物与宿主机共享 |
| Rustup volume | os_dev_rustup:/root/.rustup | 容器重建后复用工具链 |
| Cargo volume | os_dev_cargo:/root/.cargo | 复用 registry/git 缓存 |
| 网络 | host | QEMU、下载和网络诊断使用宿主网络 |
| 权限 | privileged + SYS_ADMIN | 镜像挂载、loop device 等操作需要 |
| 默认模式 | MODE=release | 容器环境默认 release |

### 3.2 状态、进入和退出

~~~bash
docker compose ps
docker compose logs os-dev
docker compose exec os-dev bash
docker compose stop
docker compose start
docker compose down
~~~

make docker 会在服务未运行时启动它并进入 bash。docker compose down 默认删除容器但
保留 named volumes；带 --volumes 的清理会删除 Rustup/Cargo 缓存，除非明确需要重新
下载工具链，否则不要使用。

### 3.3 更换镜像

~~~bash
DOCKER_IMAGE=<your-image> make docker
~~~

也可以只对当前 Compose 命令设置：

~~~bash
DOCKER_IMAGE=<your-image> docker compose up -d
~~~

更换镜像后必须重新检查：

~~~bash
make toolchain-preflight
~~~

### 3.4 Docker 代理

Compose 支持 MANGO_DOCKER_HTTP_PROXY、MANGO_DOCKER_HTTPS_PROXY、
MANGO_DOCKER_ALL_PROXY 和 MANGO_DOCKER_NO_PROXY：

~~~bash
MANGO_DOCKER_HTTPS_PROXY=http://proxy.example:7890 MANGO_DOCKER_HTTP_PROXY=http://proxy.example:7890 make docker
~~~

Rustup 镜像可以覆盖：

~~~bash
RUSTUP_DIST_SERVER=https://rsproxy.cn RUSTUP_UPDATE_ROOT=https://rsproxy.cn/rustup make toolchain-setup
~~~

项目 setup 默认已经使用 rsproxy.cn。

### 3.5 GitHub submodule 和 Cargo git 依赖代理

~~~bash
export GIT_SUBMODULE_PROXY=https://ghproxy.net/https://github.com/
make prepare-cargo-config
~~~

也可以：

~~~bash
GIT_SUBMODULE_PROXY=https://ghproxy.net/https://github.com/ make toolchain-setup
~~~

setup 会为 Cargo 的 git 依赖写入容器内 Git 配置的 url.insteadOf 规则。不要直接修改
仓库里的依赖 URL 来临时绕过网络问题。

## 4. Rust 工具链与 Cargo

### 4.1 固定版本

rust-toolchain.toml 当前约定：

~~~toml
[toolchain]
channel = "nightly-2026-05-10"
components = ["rust-src", "llvm-tools-preview", "clippy"]
targets = ["riscv64gc-unknown-none-elf", "loongarch64-unknown-linux-gnu"]
~~~

不要用其他 nightly 代替项目版本，也不要手工修改 manifest 来适配局部环境。

### 4.2 RUSTUP_HOME 和 CARGO_HOME

根 Makefile 默认使用：

~~~text
RUSTUP_HOME=$HOME/.rustup
CARGO_HOME=$HOME/.cargo
~~~

Docker 容器中通常对应 /root/.rustup 和 /root/.cargo，并挂载到持久 volume。

~~~bash
echo "$RUSTUP_HOME"
echo "$CARGO_HOME"
rustup toolchain list
rustup target list --installed
rustup component list --installed
rustc --version
cargo --version
~~~

受限环境可以显式指定非默认目录：

~~~bash
RUSTUP_HOME=/path/to/rustup CARGO_HOME=/path/to/cargo make toolchain-preflight
~~~

目录必须存在或允许创建，且不能传空值。

### 4.3 Cargo 日常检查

优先使用：

~~~bash
make check-fast
make unittest
~~~

需要定位 Cargo 层问题时，可以在容器内执行：

~~~bash
cargo check -p mango-kernel-core
cargo fmt --check -p mango-kernel-core
cargo clippy -p mango-kernel-core
cargo test -p mango-kernel-core
~~~

直接 Cargo 命令前仍应先 make toolchain-preflight。裸机 target、linker、initramfs 和
QEMU 产物不由单独 Cargo 命令完整覆盖，仍要通过 make kernel、make build 或 make check。

### 4.4 格式化、clippy 和 lint 的边界

~~~bash
make -C os lint-format ARCH=rv64 PROFILE=normal
make check-fast
make lint
~~~

lint 不指定 ARCH 时检查 RV64/LA64 × debug/release 四格。cargo fmt 通过不等于双架构
构建通过，clippy 通过也不等于 QEMU 启动通过。

## 5. Make 参数和变量

### 5.1 常用公共变量

| 变量 | 默认/有效值 | 使用场景 |
|---|---|---|
| ARCH | rv64 / la64 | 形式化构建、运行和测试 |
| PROFILE | normal / regression | 镜像和测试配置 |
| MODE | release / debug | 构建优化模式 |
| BUILD_ROOT | ./build | 统一构建输出根目录 |
| COMPAT_OUTPUT_DIR | 项目根目录 | 兼容产物发布位置 |
| LOG | 架构层决定 | QEMU/内核日志级别 |
| CORE_NUM | 架构目标默认值 | QEMU CPU 数量 |
| QEMU_MEMORY | 通常 1G；BuildStorm 默认 8G | QEMU 内存大小 |
| KTEST | all | 选择 ktest 名称 |
| KTEST_FIXTURE | 空 | ktest fixture 选择 |
| FS_MODE | 根层 fat32，架构层常用 ext4 | 文件系统镜像模式 |
| BLK_MODE | 根层 virt | 块设备/QEMU 后端模式 |

### 5.2 构建开关

os/Makefile 还提供：

| 变量 | 默认值 | 含义 |
|---|---:|---|
| BUILD_TOOLS_DISK | 1 | make all 是否制作工具盘 |
| BUILD_ROOTFS | 0 | 是否额外制作 rootfs 盘 |
| BUILD_ROOT | build/ | 所有架构产物的根目录 |

~~~bash
make all BUILD_TOOLS_DISK=0
make all BUILD_ROOTFS=1
~~~

这些变量会改变产物范围或耗时，正式评测前应确认使用项目要求的默认值。

### 5.3 传参原则

推荐统一写在目标后：

~~~bash
make build ARCH=rv64 PROFILE=normal MODE=debug
make build ARCH=rv64 PROFILE=normal BUILD_ROOT=/tmp/mango-build
~~~

不要同时在 shell 环境、命令行和多个 Makefile 中设置相互冲突的同名变量。排查时可以：

~~~bash
env -u ARCH -u PROFILE make check ARCH=rv64 PROFILE=normal
~~~

## 6. 顶层 Make 目标总览

### 6.1 工具链和准备

~~~bash
make docker
make toolchain-preflight
make toolchain-setup
make env
make prepare-cargo-config
~~~

| 目标 | 作用 | 是否可能联网 |
|---|---|---|
| docker | 启动并进入开发容器 | 可能拉取 Docker 镜像 |
| toolchain-preflight | 只读检查 pinned toolchain | 不主动 provisioning |
| toolchain-setup | 按 manifest 安装工具链 | 缺失时需要 |
| env | preflight 的别名 | 同 preflight |
| prepare-cargo-config | 初始化/更新 Git submodule | 可能联网 |

### 6.2 构建和镜像

~~~bash
make kernel ARCH=rv64 PROFILE=normal
make build ARCH=rv64 PROFILE=normal
make user ARCH=rv64 PROFILE=normal
make image ARCH=rv64 PROFILE=normal
make all
~~~

| 目标 | 作用 | PROFILE 限制 |
|---|---|---|
| kernel | 构建对应架构内核/启动所需产物 | normal 或 regression |
| build | 构建对应架构完整产物 | normal 或 regression |
| user | 构建用户态程序 | 仅 normal |
| image | 构建用户态和文件系统镜像 | 仅 normal |
| all | 串行构建双架构并发布兼容产物 | 不需要 ARCH/PROFILE |

### 6.3 检查

~~~bash
make check ARCH=rv64 PROFILE=normal
make check-fast
make lint
make unittest
~~~

| 目标 | 重点 |
|---|---|
| check | 指定架构和 profile 的构建检查 |
| check-fast | Cargo check、fmt、clippy 或对应快速检查 |
| lint | 默认四格：RV64/LA64 × debug/release |
| unittest | mango-kernel-core 单元测试 |

### 6.4 运行、回归和测试

~~~bash
make run ARCH=rv64 PROFILE=normal
make test ARCH=rv64 PROFILE=regression
make ktest ARCH=rv64 PROFILE=normal
make ktest-build-only ARCH=rv64 PROFILE=normal
make regression
~~~

- run 需要 PROFILE=normal；
- test 需要 PROFILE=regression；
- ktest 和 ktest-build-only 需要显式 ARCH、PROFILE；
- regression 是聚合快捷入口；要获得清晰的架构级结论，推荐显式使用 make test。

### 6.5 特殊目标

~~~bash
make runsimple
make change-kernel-only
make buildstorm ARCH=rv64
make qemu-download
~~~

| 目标 | 说明 |
|---|---|
| runsimple | 根目录兼容 wrapper；当前递归调用的 os/ 通用目标未定义，优先用架构专用入口 |
| change-kernel-only | 根目录兼容 wrapper；当前同样依赖 os/ 通用 runsimple，优先用架构专用入口 |
| buildstorm | 官方 x0 userspace chroot 的 BuildStorm 配置 |
| qemu-download | 下载并解包 2K1000LA 专用 QEMU，可能需要 sudo |

BuildStorm 示例：

~~~bash
make buildstorm ARCH=rv64
make buildstorm ARCH=la64 CORE_NUM=8 QEMU_MEMORY=4G
~~~

BuildStorm 内部固定使用 PROFILE=normal。

### 6.6 清理

~~~bash
make clean
~~~

会清理 build/ 以及根目录兼容产物，如 Image、kernel-la、disk.img 和 disk-la.img。
执行前先检查：

~~~bash
git status --short
du -sh build 2>/dev/null
~~~

如果只是怀疑增量产物过时，优先使用明确的架构目标重建；不要习惯性 make clean。

## 7. Shell、QEMU run 和镜像角色切换

### 7.1 三种 shell

| shell | 进入方式 | 作用 |
|---|---|---|
| 宿主机 shell | 直接打开终端 | Git、Docker、查看文件、启动容器 |
| 开发容器 shell | make docker | Rustup、Cargo、Make、QEMU、镜像处理 |
| MangoCore guest shell | QEMU 启动后进入 | 查看挂载、运行用户态命令、手工复现 |

进入开发容器：

~~~bash
make docker
pwd
ls
~~~

退出容器：

~~~bash
exit
~~~

退出只结束当前交互 shell，不会删除容器或 Rustup/Cargo volume。宿主机不建议直接执行
Cargo、os/ 架构 Make 或全量测试脚本，因为这些命令依赖容器里的交叉工具链、QEMU、
debugfs、mkfs 和权限配置。

### 7.2 QEMU guest shell

~~~bash
make run ARCH=rv64 PROFILE=normal
make run ARCH=la64 PROFILE=normal
~~~

QEMU 使用 nographic，guest 串口输出和交互输入直接占用当前终端。进入 rescue/mainline
shell 后可以输入 ls、mount、cat 等 guest 命令。

退出时注意：

- guest 主动关机：等待 QEMU 返回；
- guest shell 退出：可能回到 PID1 或 rescue 逻辑；
- QEMU 仍运行：通常按 Ctrl-a 后按 x；
- 状态不明时，另开宿主机终端检查进程，不要直接关闭整个 Docker 服务。

### 7.3 run 入口对照

| 入口 | QEMU profile | x0 | 是否构建 | 用途 |
|---|---|---|---|---|
| make run ARCH=rv64 PROFILE=normal | development | 当前可变开发 x0 | 是 | 日常运行 |
| make runsimple | compatibility wrapper | 当前可变开发 x0 | 不稳定 | 优先用 rv64-run-only/la64-run-only |
| make -C os rv64-run | competition | 官方 RV64 x0 | 否 | 评测镜像检查 |
| make -C os la64-run | competition | 官方 LA64 x0 | 否 | 评测镜像检查 |
| make -C os rv64-derived-run | derived-competition | RV64 derived x0 | 否 | 注入配置和隔离实验 |
| make -C os la64-derived-run | derived-competition | LA64 derived x0 | 否 | 注入配置和隔离实验 |
| make test ARCH=... PROFILE=regression | regression | 无外部盘 | 是 | L4 回归 |
| make ktest ARCH=... PROFILE=... | ktest | clean ext4 fixture | 是 | L3 内核测试 |

不要把官方 competition x0 当作普通开发镜像写入。image_roles.py 会拒绝这种角色混用。

### 7.4 2K1000LA 板级 shell/run

~~~bash
make -C os la64-2k1000-run-clean
make -C os la64-2k1000-core-tests
make -C os la64-2k1000-shell
make -C os la64-2k1000-mainline
make -C os la64-2k1000-apk-persist-shell
~~~

| 目标 | 关键配置 | 用途 |
|---|---|---|
| la64-2k1000-run-clean | SATA、mode=run | 干净板级测试启动 |
| la64-2k1000-core-tests | SATA、sata_scratch_rw | 核心/磁盘测试 |
| la64-2k1000-shell | Virt、GMAC、profile=rescue | 网络/救援 shell |
| la64-2k1000-mainline | SATA、GMAC、DHCP、root=/dev/sda3 | P3/P4 主线系统 |
| la64-2k1000-apk-persist-shell | SATA、GMAC、DHCP、持久 APK/Python | P4 persist shell |

典型输出：

~~~text
build/la64/release/normal/board/2k1000/
~~~

这些目标会传入 BOARD、BLK_MODE、EXTRA_FEATURES 和 KERNEL_CMDLINE，不是普通 QEMU run
的别名；实际启动还需要串口、板级镜像和 boot_2k1000_tftp.py。

### 7.5 QEMU 命令预览

~~~bash
make -C os -f make/rv64.mk qemu-profile-dry-run QEMU_PROFILE=development
make -C os -f make/rv64.mk qemu-profile-dry-run QEMU_PROFILE=competition
make -C os -f make/rv64.mk qemu-profile-dry-run QEMU_PROFILE=ktest
make -C os -f make/la64.mk qemu-profile-dry-run QEMU_PROFILE=regression
python3 scripts/run_full_test.py --dry-run
~~~

前四条用于单架构 QEMU profile，最后一条用于全量测试矩阵。

## 8. 测例选择与测试配置

### 8.1 mask 测试组

| bit | mask | 测试组 |
|---:|---:|---|
| 0 | 0x001 | basic |
| 1 | 0x002 | busybox |
| 2 | 0x004 | lua |
| 3 | 0x008 | libctest |
| 4 | 0x010 | iozone |
| 5 | 0x020 | unixbench |
| 6 | 0x040 | iperf |
| 7 | 0x080 | libcbench |
| 8 | 0x100 | lmbench |
| 9 | 0x200 | netperf |
| 10 | 0x400 | cyclictest |
| 11 | 0x800 | LTP |

常用 mask：

~~~text
0x001  basic
0x003  basic + busybox
0x010  iozone
0x040  iperf
0x100  lmbench
0x200  netperf
0x800  LTP
0xFFF  全量
~~~

mask 只是选择组；ltp_exclude、架构差异和当前 runner 仍会影响实际用例集合。

### 8.2 runner mode

| mode | 行为 |
|---|---|
| run | 按 mask 运行测试并收尾 |
| shell | 进入 guest shell |
| run_then_shell | 测试完成后进入 shell |
| drift_window | 重复性能测量窗口 |
| regression | 使用零盘回归入口 |

示例：

~~~text
mode=shell
mask=0x001
~~~

~~~text
mode=run_then_shell
mask=0x003
~~~

注入后启动：

~~~bash
make -C os conf-inject CONF_ARCH=rv64 CONF_BLK_MODE=virt CONF_FILE=../os_test.conf
make -C os rv64-derived-run
~~~

### 8.3 LTP runner、libc 和过滤

~~~text
ltp_runner=inline
ltp_runner=script
ltp_runner=suite
ltp_libc=musl
ltp_libc=glibc
ltp_libc=both
ltp_suites=syscalls,fs_bind
ltp_include=read01,write01
ltp_exclude=execve05,mmap16
ltp_from=read01
~~~

inline 适合最小调试，script 适合脚本流程，suite 通过 ltprunner/runtest 执行，适合
正式套件。include 只纳入指定用例，exclude 排除阻塞或暂不关注用例，from 用于长 suite
分段调试。

### 8.4 drift 窗口

~~~text
mode=drift_window
drift_windows=4
drift_libc=musl
drift_pre_mask=0x003
drift_measure=full
~~~

drift_measure=null 只测最小 null syscall，drift_measure=full 才执行完整 lmbench。
性能 A/B 前必须记录窗口数、libc、前置 mask、配置 checksum 和镜像来源。

### 8.5 临时配置

~~~bash
cp os_test.conf /tmp/os_test-focus.conf
make -C os conf-inject CONF_ARCH=rv64 CONF_BLK_MODE=virt CONF_FILE=/tmp/os_test-focus.conf
~~~

不要把临时 include/exclude 配置注入 official x0；复杂实验从干净 derived 镜像开始。

## 9. 文件系统、initramfs 和工具盘制作

### 9.1 产物类别

| 类型 | 入口 | 用途 |
|---|---|---|
| initramfs CPIO | make kernel 的依赖、initramfs-rv/la | 内核嵌入启动根 |
| x0 rootfs | make image、架构 fs-img | development QEMU 根盘 |
| x1 tools disk | tools-disk-rv/la、make all | 测试程序和工具 |
| ktest ext4 fixture | ktest-clean-ext4 | L3 文件系统测试盘 |

### 9.2 用户态、initramfs、rootfs

~~~bash
make user ARCH=rv64 PROFILE=normal
make user ARCH=la64 PROFILE=normal
make build ARCH=rv64 PROFILE=normal
make -C os initramfs-rv
make -C os initramfs-la
make -C os initramfs-all
make image ARCH=rv64 PROFILE=normal FS_MODE=ext4
make image ARCH=la64 PROFILE=normal FS_MODE=ext4
make -C os fs-img ARCH=rv64 FS_MODE=ext4
make -C os fs-img ARCH=la64 FS_MODE=ext4
~~~

脚本参数顺序：

~~~text
scripts/build_initramfs.sh <arch> <mode> <output_path> [profile]
~~~

build_rootfs.sh 会挂载镜像、复制用户态程序和工具、卸载镜像，需要容器的
privileged/CAP_SYS_ADMIN 能力。手工调用时要保证 USER_OUTPUT_ROOT 指向本轮产物。

### 9.3 x1 工具盘

~~~bash
make -C os tools-user-rv
make -C os tools-disk-rv
make -C os tools-user-la
make -C os tools-disk-la
make -C os tools-disk
make all BUILD_TOOLS_DISK=0
~~~

x1 通常包含 busybox/bash、公共 /etc、basic/fs/inet/unix 测试程序、Alpine 工具和库、
可选 CPython、apk 静态工具、keys、APKINDEX、本地包和 FAT32 scratch 分区。payload 默认
约 2048MB，典型输出是 build/rv64/release/normal/tools/disk.img 和
build/la64/release/normal/tools/disk-la.img。

### 9.4 Alpine、apk、CPython

~~~bash
make -C os tools-alpine-rv
make -C os tools-alpine-la
make -C os tools-alpine
make -C os tools-apk-rv
make -C os tools-apk-la
make -C os tools-apk
make -C os tools-cpython-rv
make -C os tools-cpython-la
make -C os tools-cpython
make -C os tools-cpython-clean
~~~

这些目标可能访问 ALPINE_MIRROR。CPYTHON_AUTO=1 时 maybe-tools-cpython 路径会尝试
下载而不中断整个工具盘；需要明确失败时直接调用 tools-cpython-rv 或 tools-cpython-la。

## 10. 根目录 Makefile 全目标

### 10.1 环境和准备

| 目标 | 说明 |
|---|---|
| docker | 启动并进入 os-dev |
| toolchain-preflight | 只读检查 pinned toolchain |
| toolchain-setup | 安装 manifest 工具链 |
| env | preflight 别名 |
| prepare-cargo-config | 初始化 submodule、应用 proxy |

### 10.2 构建和测试

| 目标 | 说明 |
|---|---|
| all | setup、串行双架构完整构建、发布兼容产物 |
| build | 单架构完整产物 |
| kernel | 单架构内核和启动产物 |
| user | 单架构用户态，仅 normal |
| image | 用户态和 rootfs，仅 normal |
| check | 指定架构/profile 构建检查 |
| check-fast | Cargo check、fmt、clippy |
| lint | RV64/LA64 × debug/release warning 门禁 |
| unittest | mango-kernel-core 单元测试 |
| run | development QEMU，必须 normal |
| runsimple | 根 wrapper；当前 os/ 没有通用同名目标，优先架构专用入口 |
| change-kernel-only | 根 wrapper；当前 os/ 没有通用同名目标，优先架构专用入口 |
| test | regression QEMU，必须 regression |
| regression | os 层 regression 聚合 |
| ktest | 构建并运行 L3 kernel test |
| ktest-build-only | 只构建 ktest |
| full-test | 全量测试便利入口 |
| bugscan | unittest 后运行 RV64 ktest |

### 10.3 资源和兼容目标

| 目标 | 说明 |
|---|---|
| testsuits-download | 下载 sdcard-rv/la 压缩测试镜像 |
| qemu-download | 下载并解包 2K1000 QEMU |
| clean | 清理 build 和根兼容产物 |
| rv64-only | os 层 RV64 all 兼容入口 |
| buildstorm | BuildStorm |
| validate-run | run 参数校验 |
| print-logo | 打印 logo |

docker-test-parallel 和 test-docker-parallel 已废弃并 fail-closed。全量验收使用：

~~~bash
python3 scripts/run_full_test.py --serial
~~~

## 11. os/Makefile 全目标

### 11.1 聚合和公共目标

| 目标 | 说明 |
|---|---|
| all | rv64_all、la64_all、publish-compatibility |
| arch-build | 单架构参数化完整构建 |
| kernel | 单架构内核 |
| user | 用户态 |
| image | rootfs |
| run | 参数校验后进入架构 run |
| test | 参数校验后进入 regression-run |
| check | 架构 check |
| lint | warning gate |
| lint-format | cargo fmt |
| buildstorm | BuildStorm |
| clean | 清理全部架构产品 |
| laclean | 只清 LA64 |
| regression-all | RV64 后 LA64 regression |
| ktest-all | RV64 后 LA64 全部 ktest |
| publish-compatibility | 发布 kernel、Image 和 disk 兼容副本 |

### 11.2 架构快捷目标

| 目标 | 说明 |
|---|---|
| rv64_all / la64_all | 对应架构完整 normal 构建 |
| rv64-only / la64-only | 进入对应架构 all |
| rv64-kernel-build-only / la64-kernel-build-only | 只构建对应架构 kernel |
| rv64-debug / la64-debug | debug 入口 |
| rv64-run-only / la64-run-only | development，不完整重建 |
| rv64-run / la64-run | competition |
| rv64-derived-run / la64-derived-run | derived-competition |
| rv64-regression / la64-regression | zero-disk regression |
| rv64-gdb / la64-gdb | 架构 GDB |
| gdb | 按当前架构进入 GDB |
| comp | 当前架构 competition |
| comp-gdb | competition GDB |
| derived-comp | derived competition |

### 11.3 测试、镜像和依赖库

| 目标 | 说明 |
|---|---|
| ktest-build-only | 参数化 ktest 构建 |
| ktest-run | 参数化 ktest 运行 |
| rv64-ktest / la64-ktest | 固定架构 ktest |
| rv64-ktest-build-only / la64-ktest-build-only | 固定架构只构建 |
| conf-inject | 注入 os_test.conf |
| lwext4-rv64 / lwext4-la64 | 构建对应 lwext4 C 库 |
| bugscan | 快速检查加 RV64 ktest |
| docker | os 层进入容器 |
| initramfs-rv / initramfs-la / initramfs-all | 生成 CPIO |
| tools-user-rv / tools-user-la | 构建工具盘用户态 |
| tools-disk-rv / tools-disk-la / tools-disk | 制作 x1 |
| tools-alpine-rv / tools-alpine-la / tools-alpine | 下载 Alpine |
| tools-apk-rv / tools-apk-la / tools-apk | 准备 apk |
| tools-cpython-rv / tools-cpython-la / tools-cpython | 下载 CPython |
| tools-cpython-clean | 清理 CPython 缓存 |

la64-inject-runtime 和 inject-test 当前拒绝直接修改官方 x0，应使用 derived image。

## 12. 架构 Makefile 全目标

### 12.1 RV64

~~~bash
make -C os -f make/rv64.mk <target>
~~~

目标：

~~~text
all debug stage-kernel mv mv-debug build
toolchain-preflight env user fs-img kernel clean check
run runsimple monitor gdb comp derived-comp comp-gdb
buildstorm buildstorm-input ktest-build-only ktest-run
regression-run net-irq-run net-irq-qemu
lwext4-rv64 clean-lwext4-rv qemu-profile-dry-run
~~~

### 12.2 LA64

~~~bash
make -C os -f make/la64.mk <target>
~~~

目标：

~~~text
all debug stage-kernel mv mv-debug build
toolchain-preflight env user fs-img kernel uimage clean check
run runsimple comp derived-comp comp-gdb buildstorm buildstorm-input
ktest-build-only ktest-run regression-run qemu-profile-dry-run
lwext4-la64
la64-2k1000-run-clean la64-2k1000-core-tests
la64-2k1000-shell la64-2k1000-mainline
la64-2k1000-apk-persist-shell
~~~

## 13. 全部常用 Make 变量

### 13.1 基础构建和工具链

| 变量 | 作用 |
|---|---|
| ARCH | rv64 或 la64 |
| PROFILE | normal 或 regression |
| MODE | release 或 debug |
| BUILD_ROOT | 构建输出根 |
| COMPAT_OUTPUT_DIR | 根兼容产物目录 |
| FS_MODE | rootfs 文件系统 |
| BLK_MODE | 块设备模式 |
| LA64_BLK_MODE | LA64 块模式覆盖 |
| DOCKER_IMAGE | Docker 镜像 |
| RUSTUP_HOME / CARGO_HOME | 工具链和 Cargo 数据目录 |
| RUSTUP_DIST_SERVER / RUSTUP_UPDATE_ROOT | Rustup 镜像 |
| GIT_SUBMODULE_PROXY | GitHub URL 替换 |

### 13.2 产品、内核和用户态

| 变量 | 作用 |
|---|---|
| PRODUCT_ROOT | 当前产品根 |
| KERNEL_OUTPUT_ROOT | kernel Cargo 输出 |
| USER_OUTPUT_ROOT | 用户态输出 |
| KERNEL_CMDLINE | 启动命令行 |
| EXTRA_FEATURES | 额外 Cargo feature |
| BOARD | laqemu 或 2k1000 |
| LOG | 日志 feature/等级 |
| DNS_SERVER | initramfs DNS |
| MANGO_NO_TEST_DISKS | 跳过 initramfs loop 测试盘 |
| BUILD_TOOLS_DISK | make all 是否制作 x1 |
| BUILD_ROOTFS | make all 是否制作额外 rootfs |

### 13.3 QEMU 和测试

| 变量 | 作用 |
|---|---|
| CORE_NUM | CPU 数；RV64 1/2/4/8，LA64 1/2/4/8/12 |
| QEMU_MEMORY | QEMU 内存 |
| QEMU_TIMEOUT | full-test 总体 timeout |
| NET_DEV | 覆盖网络设备参数 |
| SDCARD_RV / SDCARD_LA | x0 竞赛盘覆盖 |
| DISK_LA | LA64 x1 覆盖 |
| NET_IRQ_HOST_PORT | net_irq 宿主 UDP 端口 |
| BUILDSTORM_ARCHIVE | public image gzip |
| BUILDSTORM_PRODUCT_ROOT | BuildStorm 产品根 |
| BUILDSTORM_KERNEL_RV / BUILDSTORM_KERNEL_LA | BuildStorm kernel |
| KTEST | ktest 名称 |
| KREPEAT | ktest 重复次数 |
| KTIMEOUT_MS | 单测试超时 |
| KTRACE | ktest trace |
| KTEST_QEMU_TIMEOUT | ktest 外层 timeout |
| KTEST_EXT4_IMAGE | clean ext4 fixture |
| KTEST_FIXTURE | 特殊 fixture |

### 13.4 镜像和工具盘

| 变量 | 作用 |
|---|---|
| CONF_ARCH | conf-inject 架构 |
| CONF_BLK_MODE | conf-inject 块模式 |
| CONF_FILE | 注入文件 |
| CONF_IMAGE | 显式目标镜像 |
| IMAGE_PATH | 注入脚本目标 |
| DERIVED_IMAGE_PATH | derived 输出 |
| AUTO_REBUILD_MEM | mem 注入后是否重建 |
| TOOLS_IMG_RV / TOOLS_IMG_LA | x1 输出路径 |
| TOOLS_SIZE_RV / TOOLS_SIZE_LA | x1 payload 大小 |
| CPYTHON_AUTO | maybe CPython 是否尝试 |
| ALPINE_MIRROR | Alpine 下载源 |
| BOARD_2K1000_ARTIFACT_ROOT | 板级 uImage 目录 |
| BOARD_2K1000_TEST_CONFIG | 板级配置 |
| KERNEL_UIMG | uImage 路径 |
| LA_LOAD_ADDR / LA_ENTRY_POINT | LA64 uImage 地址 |

## 14. 镜像角色、脚本和目标发现

角色约定：

~~~text
x0 = 根/启动消费者
x1 = 项目拥有的 tools disk
regression = 不挂外部盘
ktest = 干净 ext4 x0 fixture
~~~

查询角色：

~~~bash
python3 scripts/image_roles.py official --repo-root . --arch rv64
python3 scripts/image_roles.py derived --repo-root . --arch rv64
python3 scripts/image_roles.py validate-mutable --repo-root . --arch rv64 --path build/development/rv64/sdcard-rv-derived.img
~~~

周边脚本：

| 脚本 | 用途 |
|---|---|
| rustup-setup.sh / rustup-preflight.sh | 工具链 provisioning/check |
| run_full_test.py | 全量测试、dry-run、serial、fixture |
| inject_os_test_conf.sh | 镜像配置注入 |
| image_roles.py | official/derived/mutable 合同 |
| build_initramfs.sh | newc CPIO |
| build_rootfs.sh | x0 rootfs |
| make_mbr_tools_disk.py | x1 MBR/分区布局 |
| fetch_cpython_runtime.py | CPython runtime |
| make_2k1000_tools_partition.py | 2K1000 工具分区 |
| make_2k1000_full_test_disk.py | 2K1000 测试盘 |
| boot_2k1000_tftp.py | 板级 TFTP/串口启动 |
| configure_2k1000_local_boot.py | 板级本地启动 |
| lint-check.sh | warning regression gate |
| check-la64-regression-log.sh | LA64 regression 状态 |
| run_ext4_backend_ab.sh | ext4 backend A/B |

陌生脚本先查看帮助：

~~~bash
python3 scripts/run_full_test.py --help
python3 scripts/image_roles.py --help
python3 scripts/boot_2k1000_tftp.py --help
scripts/inject_os_test_conf.sh --help
~~~

发现新增目标：

~~~bash
rg -n '^[A-Za-z0-9_.-]+:' Makefile
rg -n '^[A-Za-z0-9_.-]+:' os/Makefile os/make
make -qp
make -C os -qp
make -C os -f make/rv64.mk -qp
~~~

查看命令图而不主动执行：

~~~bash
make -n kernel ARCH=rv64 PROFILE=normal
make -C os -n rv64-ktest KTEST=fs_smp
~~~

## 15. 最终推荐工作流

### 普通共享代码

~~~bash
make docker
make toolchain-preflight
make check-fast
make kernel ARCH=rv64 PROFILE=normal
make kernel ARCH=la64 PROFILE=normal
~~~

### 测例定位

~~~bash
cp os_test.conf /tmp/os_test-focus.conf
make -C os conf-inject CONF_ARCH=rv64 CONF_BLK_MODE=virt CONF_FILE=/tmp/os_test-focus.conf
make -C os rv64-derived-run
~~~

### 文件系统改动

~~~bash
make user ARCH=rv64 PROFILE=normal
make image ARCH=rv64 PROFILE=normal FS_MODE=ext4
make run ARCH=rv64 PROFILE=normal
~~~

### 内核测试

~~~bash
make ktest ARCH=rv64 PROFILE=normal KTEST=fs_smp KREPEAT=1
make ktest ARCH=la64 PROFILE=normal KTEST=fs_smp KREPEAT=1
~~~

### 双架构验收

~~~bash
make lint
make test ARCH=rv64 PROFILE=regression
make test ARCH=la64 PROFILE=regression
python3 scripts/run_full_test.py --serial
~~~
