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

## 3. 线上测评/正式提交

评测机会在项目根目录执行：

```bash
make all
```

当前根目录 `Makefile` 的 `all` 目标会：

1. 恢复被评测 clone 过滤掉的隐藏 Cargo 配置和 vendor checksum。
2. 清理旧构建产物。
3. 编译 rv64 和 la64。
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

## 7. 分组批量测试脚本 run_test.sh

仓库根目录提供 `run_test.sh`，会按分组生成临时配置、注入镜像、运行 QEMU，并记录日志。

常用参数通过环境变量传入：

- `TEST_ARCH`: `rv64` / `la64` / `both`
- `TEST_GROUPS`: 指定分组，例如 `basic`、`basic,ltp`；不设置时按脚本默认顺序跑
- `TEST_BLK_MODE`: 全局块设备模式
- `TEST_BLK_MODE_LA`: 仅 la64 的块设备模式，默认 `virt_pci`
- `TEST_BLK_MODE_RV`: 仅 rv64 的块设备模式，默认 `virt`
- `GROUP_TIMEOUT_SEC`: 每个分组的超时时间，单位秒

示例：

```bash
TEST_ARCH=rv64 TEST_GROUPS=basic GROUP_TIMEOUT_SEC=180 bash run_test.sh
TEST_ARCH=la64 TEST_GROUPS=basic GROUP_TIMEOUT_SEC=180 bash run_test.sh
TEST_ARCH=both TEST_GROUPS=basic,busybox GROUP_TIMEOUT_SEC=300 bash run_test.sh
```

说明：

- 结果日志目录：`testresult/rv` 和 `testresult/la`。
- 脚本超时后会强制结束当前组并继续下一组。
- PASS 判定不仅看 QEMU 返回码，还会校验 initproc 日志中 musl 和 glibc 对应组都 `exit_code=0`。
- `run_test.sh` 会临时改镜像里的 `/os_test.conf`。跑完本地分组后，如果要准备提交，请按第 5 节重新注入根目录的提交默认配置。
