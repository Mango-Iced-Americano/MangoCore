# 1 下载测例并解压
运行
```
make  testsuits-download
```
解压测例到根目录
```
xz -dkc fs-img-dir/sdcard-la.img.xz > sdcard-la.img
xz -dkc fs-img-dir/sdcard-rv.img.xz > sdcard-rv.img

```

# 2 进入docker环境
运行
```
make docker
```
如果是第一次运行，会拉取镜像，请耐心等待

# 3 编译内核
```
make env
make all
```
若编译成功，根目录应当出现kernel-rv和kernel-la两个内核
# 3 运行测例
```
cd os && make rv64-run 
```
```
cd os && make la64-run
```
分别运行rv和la的测例

# 4 快速更新 os_test.conf（免重新做整套流程）
当你已经编译完，临时想修改测试配置时：

1) 先编辑仓库根目录下的 os_test.conf

2) 注入到目标镜像

la64 + PCI 模式（写入 sdcard 镜像，默认推荐）:
```
make -C os conf-inject CONF_ARCH=la64 CONF_BLK_MODE=virt_pci CONF_FILE=../os_test.conf
```
rv64 + virt 模式（写入 sdcard 镜像）:
```
make -C os conf-inject CONF_ARCH=rv64 CONF_BLK_MODE=virt CONF_FILE=../os_test.conf
```
la64 + virt 模式（写入 sdcard 镜像）:
```
make -C os conf-inject CONF_ARCH=la64 CONF_BLK_MODE=virt CONF_FILE=../os_test.conf
```
说明:
- mem 模式下 rootfs 会被内嵌进内核，注入配置后会自动触发一次内核重编。
- 如果镜像文件权限不足，可在容器内执行，或先调整镜像文件权限。

# 5 分组批量测试脚本（run_test.sh）
仓库根目录提供 `run_test.sh`，可按 12 个分组（basic/busybox/lua/.../ltp）自动循环测试。

常用参数（通过环境变量传入）:
- `TEST_ARCH`: `rv64` / `la64` / `both`（`both` 会按 `la64 -> rv64` 顺序连续跑）
- `TEST_BLK_MODE`: 全局块设备模式（可选）
- `TEST_BLK_MODE_LA`: 仅 la64 的块设备模式（未设置时默认 `virt_pci`）
- `TEST_BLK_MODE_RV`: 仅 rv64 的块设备模式（未设置时默认 `virt`）
- `GROUP_TIMEOUT_SEC`: 每个分组的超时时间（秒）

示例:

仅跑 rv64（默认 virt）:
```
TEST_ARCH=rv64 GROUP_TIMEOUT_SEC=300 bash run_test.sh
```

仅跑 la64（默认 mem，且注入后自动重编）:
```
TEST_ARCH=la64 GROUP_TIMEOUT_SEC=300 bash run_test.sh
```

一次性先跑 la64 再跑 rv64:
```
TEST_ARCH=both GROUP_TIMEOUT_SEC=300 bash run_test.sh
```

说明:
- 结果日志目录: `testresult/la`（la64）和 `testresult/rv`（rv64）。
- 脚本会在超时后强制结束当前组并继续下一组。
- PASS 判定不仅看命令返回码，还会校验 initproc 日志中 musl+glibc 两侧对应组都 `exit_code=0`。
