## OSKernel2025-NPUcore-BLOSSOM(RISC-V + LoongArch)

#### 团队成员及指导老师

队长：沈天乐

队员：李惟鑫、杨峥巍

指导老师：张羽

#### 一、简介

- 内核概况

NPUcore-BLOSSOM 是来自西北工业大学的三位同学基于 NPUcore-lwext4 框架，参考借鉴往年内核赛道优秀参赛队伍与 Linux 内核的诸多优秀设计，完善其内部功能实现并进行迭代升级而形成的竞赛操作系统。

- 仓库分支介绍

`RvLaMerge` —— 最新代码及文档

`la_ext4_init` —— 支持 ext4 文件系统的 la 分支

`main` —— rv 版本的 baseline

- 初赛完成情况

rv 架构下，basic 与 lua-musl 测例拿到满分，busybox-musl 测例接近满分，支持大部分 libctest、lmbench测例

la 架构下，basic 与 lua 测例拿到满分，busybox 测例接近满分，支持大部分 libctest、lmbench测例

- 运行demo(网盘链接)

[RISC-V架构评测demo](https://pan.baidu.com/s/1e3hPr7XcntKtftyBOO9lDA?pwd=gqps)

[LoongArch架构评测demo &amp; 内核运行示例](https://pan.baidu.com/s/1u-LsPOT_bQ3bQmGJq7aRIQ?pwd=8ibb)

#### 二、特性

- 系统调用支持完善：NPUcore-BLOSSOM 支持 100+ 系统调用，并实现了信号、线程等关键功能。

- 内存管理高效：支持懒分配、写时复制、zRAM 和虚拟内存等机制，提升内存利用率。

- 基于等待队列的阻塞机制：使用等待队列实现如 futex 的阻塞系统调用，避免轮询浪费，提高效率。

- 双缓存加速 I/O：实现 Buffer Cache 和 Page Cache，有效提升文件系统读写性能。

- 缓存一致性与策略优化：通过共享物理页与区分策略减少冗余，提高 Fat 区和数据区访问效率。

- 激进缓存策略：Page Cache 不设上限，结合 LRU 回收机制，读写性能显著提升。

- 虚拟文件系统重构：引入目录树缓存，减少底层访问，加速文件操作。

- Trait 化文件操作：借鉴 Linux VFS 的多态设计，使用 Rust Trait 实现模块化和可扩展性。

#### 三、增量

- 对以往的NPUcore(LoongArch、RISC-V)进行项目整合，实现大赛所要求的HAL

- 新增支持EXT4文件系统，提高文件系统性能

- 更新virtio-drivers库版本，LA版本支持PCI设备块驱动

#### 四、基础环境配置（ bash ）

1. 安装 Rust 版本管理器 rustup 和 Rust 包管理器 cargo，并设置环境变量

```bash
curl https://sh.rustup.rs -sSf | sh
source $HOME/.cargo/env
```

2. 安装 rustc 的 nightly 版本

```bash
rustup install nightly
rustup default nightly
```

3. 切换 rust 版本

```bash
# 不指定 host，让 rustup 在 macOS 开发机上使用 macOS 工具链，
# 在 Docker/Linux 中使用 Linux 工具链。
rustup toolchain install nightly-2025-01-18
rustup override set nightly-2025-01-18
```

4. 安装相关软件包

```bash
cargo install cargo-binutils
rustup component add llvm-tools-preview
rustup component add rust-src
```

5. 安装 qemu-9.2.1

前往[系统能力培养赛操作系统大赛评测镜像](https://gitlab.educg.net/wangmingjian/os-contest-2024-image)下载 qemu-9.2.1.tar.xz

解压后按如下步骤进行，默认安装至 `/usr/local/bin`

```bash
# 安装
cd qemu-9.2.1 
./configure \
	--target-list=loongarch64-softmmu,riscv64-softmmu \
	--enable-gcov \
	--enable-debug \
	--enable-slirp 
make -j$(nproc) 
sudo make install
```

6. 安装 rv64 工具链

```bash
# 添加 rv64 交叉编译支持
rustup target add riscv64gc-unknown-none-elf

# 添加 debugger for rv64
git clone https://github.com/riscv-collab/riscv-gnu-toolchain.git
cd riscv-gnu-toolchain
./configure --prefix=$HOME/OS/rv64tool --target=riscv64-unknown-elf --enable-gdb
make
make install
```

7. 安装 la64 工具链

前往[oscomp-toolchains-for-oskernel](https://gitee.com/link?target=https%3A%2F%2Fgithub.com%2FLoongsonLab%2Foscomp-toolchains-for-oskernel%2Ftree%2Fmain)下载 loongarch64-linux-gnu-gdb.tgz 并解压

前往[系统能力培养赛操作系统大赛评测镜像](https://gitee.com/link?target=https%3A%2F%2Fgitlab.educg.net%2Fwangmingjian%2Fos-contest-2024-image)下载 gcc-13.2.0-loongarch64-linux-gnu-nw.tgz 并解压


#### 五、文档

- **文档仓库地址**

包含开发文档、平台输出留档、分数记录等

[npucore-blossom-docs](https://gitee.com/differential1012/npucore-blossom-docs)

- **模块与特性相关文档**

[信号](https://gitlab.eduxiji.net/T202510699995278/oskernel2025-npucore-blossom/-/blob/RvLaMerge/Doc/%E4%BF%A1%E5%8F%B7.md)

[futex](https://gitlab.eduxiji.net/T202510699995278/oskernel2025-npucore-blossom/-/blob/RvLaMerge/Doc/futex.md)

[nanosleep](https://gitlab.eduxiji.net/T202510699995278/oskernel2025-npucore-blossom/-/blob/RvLaMerge/Doc/Nanosleep.md)

[tgkill](https://gitlab.eduxiji.net/T202510699995278/oskernel2025-npucore-blossom/-/blob/RvLaMerge/Doc/tgkill.md)

- **Debug相关文档**

[Virtio-drivers更新-LA支持PCI设备](https://gitee.com/differential1012/npucore-blossom-docs/blob/master/develop/%E6%9B%B4%E6%96%B0virtio%E8%AE%B0%E5%BD%95.md)

[动态链接重定向](https://gitee.com/differential1012/npucore-blossom-docs/blob/master/develop/%E5%AE%98%E6%96%B9img%E9%87%8C%E6%97%A0%E6%B3%95%E6%89%A7%E8%A1%8C%E5%8F%AF%E6%89%A7%E8%A1%8C%E6%96%87%E4%BB%B6%E8%A7%A3%E5%86%B3%E6%96%B9%E6%B3%95.md)

#### 六、参考资料

- Rust 教程

[1]  [官方文档](https://doc.rust-lang.org/book/index.html)

[2]  [Rust 语言圣经](https://course.rs/about-book.html)

- RISC-V

[3]  [RISC-V Linux syscall table](https://jborza.com/post/2021-05-11-riscv-linux-syscalls/)

- ext4 文件系统

[4]  [一文看懂linux ext4文件系统工作原理](https://www.cnblogs.com/liwen01/p/18237062)

- virtio库

[5]  [virtio-drivers](https://github.com/rcore-os/virtio-drivers)

- baseline

[6]  [proj7-广告位招租](https://gitlab.eduxiji.net/T202410701994223/project2608126-269837)

[7]  [NPUcore-IMPACT](https://github.com/Fediory/NPUcore-IMPACT/tree/ext4)

#### 七、致谢

- 感谢张羽老师的督促和支持

- 感谢西电的LXL同学给我们实现硬件抽象层和EXT4文件系统提供巨大的支持和帮助

- 感谢OSKernel2024-NPUcore-重生之我是菜狗队长郭睆学长的答疑和帮助
