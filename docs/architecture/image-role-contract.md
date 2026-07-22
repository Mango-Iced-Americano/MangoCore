# T4 镜像角色合同

`os/make/image-roles.mk` 是开发启动镜像的唯一机器可读角色表。该表只定义
镜像输入与 QEMU drive ID 的边界；它不改变内核挂载策略、PID 1 或 T5 QEMU
launcher 的设计。

## 开发与评测角色

| 角色 | RV64 | LA64 | 所有者与限制 |
| --- | --- | --- | --- |
| bootstrap root | `initramfs-rv.cpio` | `initramfs-la.cpio` | 内嵌内核的初始根；normal 构建产物 |
| development x0 | `rootfs-rv.img` | `rootfs-la.img` | 项目构建的开发 rootfs |
| competition x0 | `sdcard-rv.img` | `sdcard-la.img` | OSComp 外部输入；文件名和内容均不得由项目写入 |
| x1 P1 | `disk.img` | `disk-la.img` | 项目 tools ext4 payload |
| x1 P2 | 同一 x1 的 FAT32 分区 | 同一 x1 的 FAT32 分区 | scratch，LTP 设备合同为 `/dev/vdb2` |

normal/development 与 competition 均严格使用 `x0 x1` 两盘顺序；regression 和
ktest 是零盘 initramfs profile。不得增加永久第三盘，也不得把 x0/x1 对调。

## 输入来源与派生镜像

外部评测盘的来源记录为 `oscomp/testsuits-for-oskernel pre-20250615`。下载归档
及解压后的外部输入应由使用方记录 SHA-256；本仓库不在构建阶段下载、格式化或
写入这些输入。`conf-inject` 对 virt/virt_pci 默认先复制为
`build/<arch>/<mode>/development/image/sdcard-<arch>-derived.img`，随后只写该
派生镜像；直接指定 `sdcard-rv.img` 或 `sdcard-la.img` 会被拒绝。

工具盘构建使用每次调用独有的 `mktemp -d` workspace，并在 trap 清理 loop mount；
不会使用共享的 `/tmp/tools-mnt`。

## 合同验证

在 Docker 开发容器中执行：

```sh
sh scripts/test-image-role-contract.sh
for fixture in swapped-drive third-drive missing-payload mutate-official-x0; do
    sh scripts/test-image-role-contract.sh --fixture "$fixture"
done
```

该检查只读取 Make、脚本和 MBR metadata，不构建外部镜像、不启动 QEMU。它验证
RV64/LA64 development/competition 消费者均引用角色表、x1 的 P1/P2 所有权、零盘
profile、禁止第三盘，以及外部 x0 的不可变性。
