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

la64 + mem 模式（写入 rootfs 镜像）:
```
make -C os conf-inject CONF_ARCH=la64 CONF_BLK_MODE=mem CONF_FILE=../os_test.conf
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
- mem 模式下 rootfs 会被内嵌进内核，默认会自动触发一次内核重编（可通过 AUTO_REBUILD_MEM=0 关闭）。
- 如果镜像文件权限不足，可在容器内执行，或先调整镜像文件权限。

