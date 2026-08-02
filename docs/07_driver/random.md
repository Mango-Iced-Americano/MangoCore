---
title: "随机数与平台熵源"
module: "drivers/rng"
category: driver
status: experimental
last_updated: 2026-08-02
code_paths:
  - "os/src/random.rs"
  - "os/src/drivers/rng/mod.rs"
  - "os/src/drivers/rng/jh7110_trng.rs"
  - "os/src/fs/dev/urandom.rs"
  - "os/src/syscall/mod.rs"
  - "os/src/hal/platform/riscv/qemu.rs"
  - "os/src/hal/platform/loongarch64/2k1000.rs"
entry_points:
  - "random::init()"
  - "random::fill_bytes()"
  - "drivers::rng::fill_entropy()"
arch:
  rv64: supported
  la64: supported
related_docs:
  - "docs/02_syscall/syscall-layer.md"
  - "docs/03_fs/devfs.md"
---

# 随机数与平台熵源

## 数据路径

MangoCore 将“平台熵输入”和“用户可见随机流”分开：

```text
rvqemu:  virtio-rng-mmio bus.2 (0x10003000) --+
laqemu:  virtio-rng-pci ----------------------+-> 64-byte boot sample
2K1000:  APB RNG (DMW2 0x800000001fe2b000) --+       |
VF2:     JH7110 TRNG (0x1600c000) ------------+       |
                                                       v
                                              boot health check
                                                       |
                                                       v
                                                ChaCha20 CSPRNG
                                                       |
                                      +----------------+----------------+
                                      v                                 v
                                getrandom(2)                /dev/random, /dev/urandom
```

硬件输出不会直接复制给用户。`random::init()` 在启动进入用户态前采集 64 字节，
通过基本健康检查和 ChaCha20 调理后建立全局私有状态。每次用户请求结束后再消耗
32 字节隐藏输出重键，因此之后的状态泄露不能直接恢复先前已经返回的字节。
启动样本、调理 seed、重键 seed 和 syscall 临时块均使用 volatile wipe，避免优化器
删除敏感栈数据的清理写入。

## 平台来源

| 平台 | 熵设备 | 接入方式 |
|---|---|---|
| RISC-V QEMU virt | VirtIO entropy device，device id 4 | MMIO bus.2，`0x10003000` |
| VisionFive 2 | JH7110 security TRNG | FDT `starfive,trng` node；轮询 reseed / RAND0..7 |
| LoongArch QEMU virt | VirtIO entropy device，device id 4 | PCI 枚举 `DeviceType::EntropySource` |
| 2K1000LA 星云板 | APB Device 2 RNG | DMW2 非缓存别名 `0x800000001fe2b000` |

2K1000LA 用户手册 23.4 节规定 APB Device 2 BAR 加 `0xb000` 为随机数寄存器，
每次读取返回一个 32 位随机数。板级 APB 基址为 `0x1fe20000`，因此物理地址为
`0x1fe2b000`；CPU 访问使用内核已建立的 DMW2 高地址别名，DMA 地址不参与该路径。

RISC-V 的 RNG 设备页必须同时出现在平台 `MMIO` 表中，否则启用分页后的首次
寄存器访问会触发 `LoadPageFault`。QEMU 启动参数固定占用 virtio-mmio bus.2，
避免与 bus.0/bus.1 块设备和 bus.7 网卡冲突。

VisionFive 2 vendor DTB 将存在的 `starfive,trng` 节点标为 `status = "disabled"`。驱动
因此忽略该节点的 status，但仍在首次 MMIO 前严格校验兼容字符串（同时兼容上游
`starfive,jh7110-trng`）、基址 `0x1600c000`、至少 `0x4000` 的大小、clocks `205/206`
和 reset `131`；QEMU 没有匹配节点，故仍不触碰板级地址并回退到 virtio-rng。clock IDs
`205/206` 属于从 ID `190` 开始的 STG register range，驱动在 STG CRG 的
`0x1023003c/40` 写 bit31 门控两路时钟；同一 STG CRG 的 `0x10230074` bit3 释放
reset，并轮询 `0x10230078`。随后以 256-bit 模式执行 reseed，轮询 ISTAT 的
`SEED_DONE` / `RAND_RDY`，从 RAND0..RAND7 读取每批 32 字节。LFSR lockup 会触发
重播种，超时或失配永不提升 secure-ready。

## 启动与健康检查

启动样本必须满足以下最低条件：

1. 前后两个 32 字节半区不能完全相同。
2. 16 个 32 位字中至少有 8 个不同值，用于拒绝明显卡死或常量设备。
3. 只有检查通过后才设置 secure-ready；失败会清零临时样本并保持未就绪。

这是一项启动连续性检查，不是对硬件 RNG 的统计认证。它用于阻止明显失效的熵源
被静默当成安全来源，不能代替芯片级随机源评估。

## 用户 ABI

### getrandom(2)

| flag | 当前语义 |
|---|---|
| `0` | 从已播种的 ChaCha20 流读取；未就绪返回 `EAGAIN` |
| `GRND_NONBLOCK` | 就绪后与普通读取相同；未就绪返回 `EAGAIN` |
| `GRND_RANDOM` | 当前与同一个安全 CSPRNG 流一致 |
| `GRND_INSECURE` | 允许使用未认证启动状态，但绝不把该输出计为可信熵 |

未知 flag 返回 `EINVAL`；`GRND_RANDOM | GRND_INSECURE` 返回 `EINVAL`；零长度请求
返回 0。实现按 256 字节内核临时块生成并写入用户 buffer，返回前清零临时块。

正常平台会在创建首个用户任务前完成播种，因此常规请求不会看到未就绪状态。
当前未实现 Linux 的“无 `GRND_NONBLOCK` 时等待熵源”路径；平台熵源初始化失败时
统一 fail closed 为 `EAGAIN`，避免启动永久阻塞或退回弱随机数。

### 随机设备

`/dev/random` 与安全 `getrandom` 一致：仅从已播种的 CSPRNG 读取，未就绪时返回
`EAGAIN`。`/dev/urandom` 优先读取该安全流；未就绪时改用同一 ChaCha20 的显式
`GRND_INSECURE` 启动状态，因而始终填满请求 buffer 且不返回 `EAGAIN`。仅当该内部
回退流也意外失败时，才使用原子计数器驱动的本地 PRNG 保证非阻塞 ABI；这些回退输出
绝不提高 secure-ready，也不应被当作可信熵。写入会混入私有状态，但调用方可控数据
不提高 secure-ready，也不被计为熵。

## 构建与验证

QEMU Make 目标会自动添加 VirtIO RNG。可选的自包含回归程序通过
`RNG_TEST_RUNTIME=1` 注入为 `/bin/rng_test`，验证：

- 连续 `getrandom` 输出不是全零且彼此不同；
- 非法和互斥 flag 返回 `EINVAL`；
- 连续 `/dev/urandom` 输出不是全零且彼此不同。

2K1000LA 实板分支可用以下 feature 组合构建 shell 镜像：

```bash
make -f make/la64.mk uimage BOARD=2k1000 BLK_MODE=sata \
  RNG_TEST_RUNTIME=1 EXTRA_FEATURES="sata_scratch_rw gmac_dhcp board_shell"
```

启动日志应出现 `random: initialized from 2k1000-rng`，随后执行：

```bash
/bin/rng_test
```

VisionFive 2 的下一次启动应打印 `random: initialized from jh7110-trng`，不再出现
`PRNG init warning: Entropy(DeviceUnavailable)`；QEMU 则仍打印 `virtio-rng`。

## 已知边界

- 当前只在启动时采集可信硬件熵，尚未实现按时间或输出量周期性重播种。
- 健康检查只拒绝明显常量/重复输出，不对熵率作定量估计。
- `/dev/random` 尚未实现独立的阻塞熵计数模型；它与 `/dev/urandom` 共用 CSPRNG。
- 实现未持久化 VirtIO RNG 队列或 2K1000 RNG 设备状态；运行期重播种需要先补齐
  生命周期、锁顺序和错误恢复设计。
