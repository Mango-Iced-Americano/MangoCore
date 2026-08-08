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

normal/development 与 competition 均严格使用 `x0 x1` 两盘顺序；regression 是
零盘 initramfs profile。KTest 使用每次启动前重新格式化的独立 ext4 `x0` fixture，
不挂载 x1，确保文件系统契约测试不会依赖可变 rootfs 镜像。不得增加永久第三盘，
也不得把 normal/competition 的 x0/x1 对调。

## 输入来源与派生镜像

外部评测盘的来源记录为 `oscomp/testsuits-for-oskernel pre-20250615`。下载归档
及解压后的外部输入应由使用方记录 SHA-256；本仓库不在构建阶段下载、格式化或
写入这些输入。`conf-inject` 对 virt/virt_pci 默认先复制为
`build/development/<arch>/sdcard-<arch>-derived.img`，随后只写该派生镜像；
`run_full_test.py` 与 LTP 自动化脚本也只消费该 manifest 导出的派生 x0。

在 `cp`、`e2fsck` 或 `debugfs` 前，注入路径必须先通过角色表验证：输入的
official x0 校验 SHA-256，输出必须为当前架构的命名 derived x0。验证会拒绝
路径中的符号链接、解析后等于 official x0 的路径、以及与任一 official x0
共享 device/inode 的硬链接；RV64 不可把 LA64 的 derived 或 official 名称作为
目标。`make -n` 同样在解析阶段拒绝把 development x0 override 解析为 official
x0，避免 dry-run 掩盖危险配置。

工具盘构建使用每次调用独有的 `mktemp -d` workspace，并在 trap 清理 loop mount；
不会使用共享的 `/tmp/tools-mnt`。workspace 创建失败会立即报错；若 unmount
失败，构建以非零状态退出并保留 workspace 路径以保全诊断。

## 合同验证

在 Docker 开发容器中执行：

```sh
sh scripts/test-image-role-contract.sh
for fixture in \
    swapped-drive third-drive missing-payload mutate-official-x0 \
    remaining-consumer cross-arch-derived symlink-alias hardlink-alias \
    make-override mktemp-failure unmount-failure; do
    sh scripts/test-image-role-contract.sh --fixture "$fixture"
done
```

该检查只读取 Make、脚本和 MBR metadata，不构建外部镜像、不启动 QEMU。它验证
RV64/LA64 development/competition 消费者均引用角色表、x1 的 P1/P2 所有权、regression
零盘和 KTest 独立 ext4 fixture、禁止第三盘，以及外部 x0 的不可变性。对注入 guard 的 fixture 还以
`cp`、`e2fsck`、`debugfs` 哨兵验证拒绝发生在任何可变操作之前。
