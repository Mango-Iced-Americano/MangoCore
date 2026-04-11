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

