# MangoCore 运行与测评说明

本文档区分两种场景：

- **线上测评/正式提交**：保持提交安全配置，让评测机执行 `make all` 后全量测试。
- **本地分批测试**：临时修改 `os_test.conf` 或使用 `run_test.sh` 只跑指定分组。

所有命令默认在 Docker 容器内执行。

## 1. 准备测例镜像

下载并解压测例：

```bash
make testsuits-download

xz -dkc fs-img-dir/sdcard-la.img.xz > sdcard-la.img
xz -dkc fs-img-dir/sdcard-rv.img.xz > sdcard-rv.img
```

## 2. 进入 Docker 环境

```bash
make docker
```

第一次运行会拉取镜像，请耐心等待。

根目录评测入口 `make all` 会派生 HOME 对应的 `RUSTUP_HOME` 和 `CARGO_HOME`，并在需要时自动执行 setup 和 preflight。全新容器首次运行可能使用网络。直接执行 OS、用户态或架构目标前，先运行只读的 `make toolchain-preflight`，这些入口不会自动 provisioning。手动流程仍可运行 `make toolchain-setup` 准备固定的 `nightly-2026-05-10`。LA64 使用的 target 仍为 `loongarch64-unknown-linux-gnu`。

## 3. 线上测评/正式提交

评测机会在项目根目录执行：

```bash
make all
```

当前根目录 `Makefile` 的 `all` 目标会：

 1. 派生 HOME 对应的 `RUSTUP_HOME` 和 `CARGO_HOME`，并按需执行工具链 setup 和 preflight。
 2. 串行执行 `make -C os rv64_all`，构建 RV64 内核、用户态和镜像。
 3. 串行执行 `make -C os la64_all`，构建 LA64 内核、用户态和镜像。
 4. 在项目根目录生成 ELF 格式的 `kernel-rv` 和 `kernel-la`。

提交前建议在 Docker 中手动验证一次：

```bash
make all
file kernel-rv kernel-la
```

`file` 输出中两者都应为 `ELF 64-bit ... executable`。

正式提交时，仓库根目录的 `os_test.conf` 应保持提交安全默认值：

```conf
mode=run
mask=0xFFF
ltp_runner=script
ltp_libc=both
ltp_exclude=
ltp_exclude_musl=
ltp_exclude_glibc=
ltp_include=
ltp_from=
```

注意：

- `mask=0xFFF` 表示全量开放 12 个测试组，由评测机按自己的磁盘镜像和脚本评分。
- `ltp_runner=script` 会运行测例镜像中的官方 `ltp_testcode.sh`，这是提交安全模式；评测端会解析脚本输出。
- 不要把本地调试用的 `mask=0x001`、`ltp_runner=inline`、`ltp_include=...` 等配置提交上去。
- `os_test.conf` 是简单的 `key=value` 配置，不是 JSON；如果以后写 JSON，键值对之间必须用英文逗号 `,`，不能用分号 `;`。

## 4. 本地分批测试：修改 os_test.conf

`os_test.conf` 的 `mask` 字段用 12-bit 控制测试组：

```conf
bit0=basic
bit1=busybox
bit2=lua
bit3=libctest
bit4=iozone
bit5=unixbench
bit6=iperf
bit7=libcbench
bit8=lmbench
bit9=netperf
bit10=cyclictest
bit11=ltp
```

常用本地配置示例：

```conf
mask=0x001    # 只跑 basic
mask=0x002    # 只跑 busybox
mask=0x004    # 只跑 lua
mask=0x040    # 只跑 iperf
mask=0x200    # 只跑 netperf
mask=0x800    # 只跑 ltp
mask=0xFFF    # 全量
```

LTP 本地聚焦调试可临时使用：

```conf
ltp_runner=inline
ltp_libc=musl
ltp_include=read01,write01
ltp_exclude=
ltp_from=
```

`inline` 模式只用于本地枚举 `/ltp/testcases/bin` 并套用 include/exclude/from 过滤。提交前务必恢复为 `ltp_runner=script`。

## 5. 注入 os_test.conf 到镜像

修改根目录 `os_test.conf` 后，需要注入到对应测例镜像。

rv64 + virt：

```bash
make -C os conf-inject CONF_ARCH=rv64 CONF_BLK_MODE=virt CONF_FILE=../os_test.conf
```

la64 + virt_pci，默认推荐：

```bash
make -C os conf-inject CONF_ARCH=la64 CONF_BLK_MODE=virt_pci CONF_FILE=../os_test.conf
```

一次恢复两个架构为当前根目录配置：

```bash
make -C os conf-inject CONF_ARCH=rv64 CONF_BLK_MODE=virt CONF_FILE=../os_test.conf
make -C os conf-inject CONF_ARCH=la64 CONF_BLK_MODE=virt_pci CONF_FILE=../os_test.conf
```

说明：

- `conf-inject` 写入的是 `sdcard-rv.img` 或 `sdcard-la.img` 根目录下的 `/os_test.conf`。
- 内核启动时如果磁盘里已有 `/os_test.conf`，会优先保留磁盘配置，不再用内核内嵌配置覆盖。
- mem 模式下 rootfs 会被内嵌进内核，注入配置后会自动触发一次内核重编。

## 6. 运行当前镜像配置

rv64：

```bash
cd os && make rv64-run
```

la64：

```bash
cd os && make la64-run
```

## 7. 全量测试脚本（推荐）

推荐使用 `scripts/run_full_test.py --serial` 进行全量测试，它会自动完成：串行编译 → 解压镜像 → RV64 后 LA64 QEMU → 评分 → 存档。

```bash
python3 scripts/run_full_test.py --serial
```

结果存档在 `testresult/archive_{timestamp}/`，包含 QEMU 输出和评分汇总。

---

## 7-bis. 旧版 run_test.sh 与并行 Docker runner（已移除）

`run_test.sh` 已从仓库移除。`scripts/run_test_docker_parallel.sh` 和 `make docker-test-parallel` 保留为 fail-closed 弃用入口：它们只输出诊断并以非零退出，绝不会创建容器、编译或启动 QEMU。

双架构共享构建状态，必须使用 canonical runner 的 `--serial` 模式；不要并行启动架构构建或旧 runner。
