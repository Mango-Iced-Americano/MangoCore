---
title: "MangoCore Python 与 pip 使用指南"
category: testing
status: draft
author: MangoCore Team
last_update: 2026-07-14
tags: [python, pip, cpython, loongarch64, 2k1000, persist, apk]
code_paths:
  - "user/tools/cpython/python3-wrapper.sh"
  - "user/src/bin/initproc.rs"
  - "os/initramfs/apk/usr/bin/persist-shell"
  - "os/initramfs/apk/usr/libexec/mango/persist-profile"
entry_points:
  - "python3"
  - "persist-shell"
  - "la64-2k1000-apk-persist-shell"
arch:
  rv64: supported
  la64: supported
related_docs:
  - "docs/08_testing/cpython-isolated.md"
  - "docs/08_testing/apk-isolated.md"
  - "docs/03_fs/2k1000-full-test-disk.md"
---

# MangoCore Python 与 pip 使用指南

## 1. 适用范围

本文说明如何在 MangoCore 的 2K1000LA 持久环境中使用特制 CPython 和 pip。这里没有
通过 APK 再安装一套系统 Python，而是组合使用：

- SSD P3 中经过测试的只读 CPython 3.14 运行时；
- initramfs 中的 MangoCore Python 启动包装器；
- SSD P4 中可写、跨重启保留的 pyc、pip 和用户包；
- `persist-shell` 提供的 Alpine 应用根和网络环境。

本文命令以 2K1000LA 实板为主。rv64/la64 QEMU 使用相同 Python 包装器，但是否存在
P4 持久目录取决于所用测试镜像。

## 2. 存储布局

| 内容 | 宿主视图 | `persist-shell` 视图 | 属性 |
|---|---|---|---|
| CPython、musl loader、标准库 | `/tools/tests/cpython` | `/tools/tests/cpython` | P3 ext4，只读 |
| Python 字节码缓存 | `/persist/python/pycache` | `/var/cache/mango-python/pycache` | P4 ext4，持久可写 |
| pip 用户安装树 | `/persist/python/user` | `/var/cache/mango-python/user` | P4 ext4，持久可写 |
| Python 临时目录 | `/persist/python/tmp` | `/var/cache/mango-python/tmp` | P4 ext4，可清理 |
| APK 应用根 | `/persist/apk-root` | `/` | P4 ext4，持久可写 |
| 下载缓存 | `/scratch/apk-cache` | `/scratch/apk-cache` | P2 FAT32，可重建 |

包装器在 P4 可用时优先使用 P4；没有 P4 时才回退到 `/scratch/python` 或 tmpfs。
P2 FAT32 不适合作为 pip 工作根，因为它不能完整表达 POSIX 元数据，`ensurepip` 的
`copy2()` 曾因此在更新时间戳时返回 `ENOSYS`。

## 3. 启动正确镜像

在 macOS 项目根目录执行：

```bash
make 2k1000-boot IMAGE=kernel-2k1000-persist-shell.ui
```

2026-07-14 验证镜像信息：

```text
文件：kernel-2k1000-persist-shell.ui
大小：16,745,096 bytes
SHA-256：1dc478dc4774ec260b373da2afa5b31341140f9e36ac70d4740c9bca1113830c
```

启动日志应包含：

```text
[apk-persist-shell] bind /tools -> /persist/apk-root/tools
[apk-persist-shell] bind /persist/python -> /persist/apk-root/var/cache/mango-python
[apk-persist-shell] RESULT=PASS
```

如果仍报告缺少 `/tools/tests/cpython/lib` 下的 musl loader，通常表示开发板仍在运行旧
uImage，或者 P3 CPython 分区未正确挂载。

## 4. 进入 Python 持久环境

内核进入宿主 shell 后执行：

```sh
persist-shell
```

提示符应变为：

```text
MangoPersist:/#
```

检查 Python：

```sh
python3 --version
python3 -c 'import sys; print(sys.executable); print(sys.version)'
```

当前验证版本为 CPython 3.14.5。`python` 和 `python3` 均进入 MangoCore 包装器；包装器
负责选择 LoongArch/RISC-V 私有 musl loader、设置 `PYTHONHOME`、CA 证书、P4 临时目录
和用户安装目录。

## 5. 首次引导 pip

只需要执行一次。直接使用 P3 CPython 自带且已校验的 pip wheel：

```sh
wheel=$(echo /tools/tests/cpython/usr/lib/python3.14/ensurepip/_bundled/pip-*.whl)
PYTHONPATH="$wheel" python3 -m pip install \
    --no-index --no-cache-dir --user "$wheel"
```

检查结果：

```sh
python3 -m pip --version
```

预期路径位于 P4：

```text
pip 26.1.1 from /var/cache/mango-python/user/lib/python3.14/site-packages/pip
```

不要使用以下命令引导：

```sh
python3 -m ensurepip --user
```

P3 来自 Alpine，保留 PEP 668 `EXTERNALLY-MANAGED` 标记；CPython `ensurepip` 又会主动
清除 `PIP_*` 环境变量，因此该入口不能应用 MangoCore 对隔离 P4 用户树的 override。

## 6. 日常安装和管理

安装纯 Python 包：

```sh
python3 -m pip install --user requests
```

检查和导入：

```sh
python3 -m pip show requests
python3 -c 'import requests; print(requests.__version__)'
```

常用命令：

```sh
python3 -m pip list
python3 -m pip install --user --upgrade requests
python3 -m pip uninstall -y requests
python3 -m pip cache info
python3 -m pip check
```

`persist-profile` 已设置：

```text
PIP_BREAK_SYSTEM_PACKAGES=1
PIP_ROOT_USER_ACTION=ignore
```

该 override 只用于 P4 的独立 `--user` 安装树，不会修改只读 P3 CPython，也不会把 pip
包写入 APK 管理的系统目录。日常安装仍必须保留 `--user`。

## 7. 为什么统一使用 `python3 -m pip`

不要把以下命令作为规范入口：

```sh
pip
pip3
```

pip 生成的 console script shebang 会直接指向 P3 的 Python ELF，可能绕过 MangoCore
包装器，继而找不到私有 `libpython3.14.so.1.0`。`python3 -m pip` 会先经过包装器，再在
正确的 loader、library path、`PYTHONHOME` 和 P4 用户目录下运行 pip。

同理，运行安装包提供的 Python console script 时，如果遇到 loader 错误，优先寻找其
对应模块入口，例如：

```sh
python3 -m pytest
python3 -m http.server 8000
```

并非所有项目都提供 `python3 -m <module>` 入口；这类脚本后续需要单独的 MangoCore
launcher 处理，不能通过全局导出库路径污染整个 shell。

## 8. 网络与证书

pip 在线安装依赖开发板网络、DNS、系统时间和 CA 证书均正常。快速检查：

```sh
date
cat /proc/net/resolv.conf
curl https://pypi.org/
python3 -c 'import ssl; print(ssl.get_default_verify_paths())'
```

包装器默认使用 P3 中的 CA bundle。不要通过 `--trusted-host` 或关闭 TLS 校验掩盖 DNS、
时间或证书问题。macOS Internet Sharing 下若 DHCP DNS 代理与部分客户端不兼容，应先
修正网络/DNS 配置，而不是把不安全参数写入 pip 全局配置。

## 9. 包兼容性

通常可直接使用：

- `six`、`idna`、`requests` 等纯 Python wheel；
- 不依赖本地编译工具链的源码包；
- 标记为 `py3-none-any` 的通用 wheel。

可能失败：

- 只有 x86_64/aarch64 wheel、没有 LoongArch wheel 的包；
- 需要 C/C++/Rust 编译器和开发头文件的扩展；
- 依赖内核尚未实现 syscall 或设备能力的包；
- 依赖 `manylinux` glibc ABI、但没有 musl/源码回退的 wheel。

排查 wheel 兼容性：

```sh
python3 -m pip debug --verbose
python3 -m pip install --user -v PACKAGE_NAME
```

MangoCore 的目标运行时是 LoongArch64 + musl。不能强制安装其他架构 wheel，也不要用
`--ignore-platform` 绕过 ABI 检查。

## 10. 持久化与同步

pip 和用户包写入 P4，因此正常重启后仍然存在。安装或卸载完成后建议执行：

```sh
sync
```

验证持久化：

```sh
python3 -m pip install --user six
python3 -c 'import six; print(six.__version__)'
sync
```

复位并重新进入 `persist-shell` 后：

```sh
python3 -m pip --version
python3 -c 'import six; print("PIP_PERSIST_OK", six.__version__)'
```

QEMU 已在同一非 snapshot P4 上验证 pip 26.1.1、six 1.17.0 和 idna 3.18 的安装、导入
及跨重启保留。

## 11. 常见错误

### 缺少 musl loader

```text
python3: CPython runtime is unavailable: missing musl loader under /tools/tests/cpython/lib
```

检查：

```sh
mount
ls -l /tools/tests/cpython/lib/ld-musl-loongarch64.so.1
```

如果只在 `persist-shell` 内失败，确认启动日志中存在 `/tools` bind。该问题已在最新
`kernel-2k1000-persist-shell.ui` 修复。

### `ensurepip` 报 `Function not implemented`

```text
OSError: [Errno 38] Function not implemented: ...pip-*.whl
```

这是旧包装器把临时目录放到 P2 FAT32 的表现。应启动最新镜像，并确认：

```sh
python3 -c 'import os; print(os.environ["TMPDIR"])'
```

输出应为 `/var/cache/mango-python/tmp`。

### PEP 668 externally managed

不要删除 P3 的 `EXTERNALLY-MANAGED` 标记。重新进入 `persist-shell`，确认：

```sh
echo "$PIP_BREAK_SYSTEM_PACKAGES"
```

应输出 `1`。首次引导必须使用本文第 5 节的 bundled wheel 命令，而不是 `ensurepip`。

### `pip3` 找不到 `libpython`

```text
Error loading shared library libpython3.14.so.1.0
```

改用：

```sh
python3 -m pip --version
```

不要向 `/etc/profile` 全局导出 P3 的 `LD_LIBRARY_PATH`。

### 安装成功但 import 失败

检查安装位置和 Python 用户目录：

```sh
python3 -m pip show PACKAGE_NAME
python3 -m site
python3 -c 'import sys; print("\n".join(sys.path))'
```

正常用户 site 路径应位于
`/var/cache/mango-python/user/lib/python3.14/site-packages`。

## 12. 最小操作清单

```sh
# 1. 进入持久应用根
persist-shell

# 2. 首次启动 pip，仅执行一次
wheel=$(echo /tools/tests/cpython/usr/lib/python3.14/ensurepip/_bundled/pip-*.whl)
PYTHONPATH="$wheel" python3 -m pip install --no-index --no-cache-dir --user "$wheel"

# 3. 日常使用
python3 -m pip install --user requests
python3 -c 'import requests; print(requests.__version__)'

# 4. 刷盘
sync
```
