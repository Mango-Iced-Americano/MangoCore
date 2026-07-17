---
title: "随机数安全闭环：2K1000LA 可信熵、ChaCha20 与 fail closed"
category: debug
status: resolved-with-known-limits
author: MangoCore Team
date: 2026-07-15
last_update: 2026-07-15
tags: [loongarch64, 2k1000la, rng, entropy, chacha20, getrandom, urandom, security]
code_paths:
  - "os/src/drivers/rng/mod.rs"
  - "os/src/random.rs"
  - "os/src/hal/platform/loongarch64/2k1000.rs"
  - "os/src/hal/platform/riscv/qemu.rs"
  - "os/src/main.rs"
  - "os/src/syscall/mod.rs"
  - "os/src/fs/dev/urandom.rs"
  - "user/src/bin/rng_test.rs"
related_docs:
  - "docs/07_driver/random.md"
  - "docs/03_fs/devfs.md"
  - "docs/02_syscall/syscall-layer.md"
entry_points:
  - "random::init"
  - "drivers::rng::fill_entropy"
  - "random::fill_bytes"
  - "sys_getrandom"
  - "Urandom::read_at"
---

# 随机数安全闭环：2K1000LA 可信熵、ChaCha20 与 fail closed

## 1. 一句话结论

旧实现中 `/dev/urandom` 固定返回全零，`getrandom(2)` 则用时间、用户 buffer 地址和
长度驱动 xorshift；二者“能返回字节”却都不能提供安全随机性。修复不是换一个更复杂
的伪随机公式，而是建立完整信任链：2K1000LA APB RNG / QEMU VirtIO RNG 提供启动
熵，64 字节样本先做故障健康检查和调理，再播种统一 ChaCha20 CSPRNG；可信播种失败
时安全 API 返回 `EAGAIN`，绝不退回全零、时间戳或地址种子。

## 2. 问题卡

| 项目 | 结论 |
|------|------|
| 旧 `/dev/urandom` | `buf.fill(0)` 后返回成功 |
| 旧 `getrandom` | `time ^ (buf << 17) ^ len` 播种 xorshift，每次请求重新构造 |
| 安全后果 | key/nonce/session token 可预测或重复；“返回长度正确”造成假安全 |
| 2K1000LA 熵源 | APB Device 2，物理 `0x1fe2b000`，DMW2 `0x800000001fe2b000` |
| QEMU 熵源 | LA PCI VirtIO RNG；RV MMIO bus.2 `0x10003000` |
| 用户随机流 | 一个全局 ChaCha20 CSPRNG，`getrandom` 与两个随机设备共享 |
| 启动样本 | 64 字节，两半不得相同，16 个 u32 至少 8 个唯一值 |
| readiness | 只有可信平台样本通过检查后才置 `ready=true` |
| 失败语义 | secure `getrandom`、`/dev/random`、`/dev/urandom` 返回 `EAGAIN` |
| 显式例外 | `GRND_INSECURE` 可用未认证 bootstrap state，但永不提升 ready |
| 根因修复提交 | `5d2f16ef4de6acff2eca759d984bf47dc0875fde` |
| 实板验收 | `2k1000-rng` 初始化；`rng_test` 连续 5 次 PASS；SATA/GMAC/DHCP 正常 |

## 3. 旧实现为什么不是“质量低一点”，而是安全接口失真

### 3.1 `/dev/urandom` 全零仍返回成功

修复前源码明确执行：

```rust
buf.fill(0);
Ok(buf.len())
```

调用者没有错误码可见，会把全零当成有效随机字节。典型后果包括：

- 不同启动生成相同私钥、salt、nonce；
- TLS/HTTPS 库继续运行，但密钥材料可预测；
- 测试只检查 `read() == len` 时错误地报告 PASS。

fail closed 的关键正是不能用“格式正确的弱输出”伪装成功。

### 3.2 xorshift 的 seed 对用户和攻击者可见

旧 `getrandom` 使用：

```text
seed = get_time() ^ (user_buf_address << 17) ^ buflen
repeat per byte:
    seed ^= seed << 13
    seed ^= seed >> 7
    seed ^= seed << 17
```

buffer 地址和长度由调用者掌握，时间只有有限不确定性；xorshift 又是线性、可逆的非
CSPRNG。一旦观测足够输出，可以恢复/预测状态。更严重的是每个 syscall 都从这些公开
量重新播种，缺少系统级私有状态，不能形成跨请求的安全序列。

### 3.3 两个 API 各自“造随机数”导致信任边界不存在

`/dev/urandom` 和 `getrandom` 分别实现，意味着：

- 修复一个接口不自动修复另一个；
- readiness/熵来源没有统一定义；
- OpenSSL、libc 和应用选择不同 API 时得到不同安全级别；
- 无法写出“平台熵源失败后所有 secure consumer 都失败”的负向门禁。

最终架构让设备/syscall 都调用同一个 `random` 子系统，用户入口不再自行生成随机数。

## 4. 设计原理：熵输入和随机输出必须分层

### 4.1 数据路径

```text
2K1000LA APB RNG ----------------------------+
LA QEMU VirtIO RNG (PCI) --------------------+--> 64-byte boot sample
RV QEMU VirtIO RNG (MMIO bus.2) -------------+          |
                                                         v
                                                  health check
                                                         |
                                                         v
                                                  conditioning
                                                         |
                                                         v
                                             global ChaCha20 CSPRNG
                                              /        |         \
                                             v         v          v
                                       getrandom  /dev/random  /dev/urandom
```

硬件输出不直接交给用户。原因不是断言硬件 RNG 一定有偏差，而是把设备短读、重复、
瞬时故障和用户并发从 ABI 层隔离，并由 CSPRNG 提供任意长度输出、锁串行和重键。

### 4.2 三种信任状态不能混淆

| 状态 | 来源 | 是否可供 secure API | 是否能置 ready |
|------|------|---------------------|----------------|
| trusted sample | VirtIO RNG / 2K1000 APB RNG，且健康检查通过 | 是 | 是 |
| bootstrap state | 时间、栈地址、heap size，经 SplitMix64 展开 | 仅 `GRND_INSECURE` | 否 |
| caller input | 写 `/dev/random` 或 `/dev/urandom` 的字节 | 只混入内部状态 | 否 |

“混入了一些未知字节”和“内核可以证明已获得可信熵”是两个不同判断。调用者控制的写入
永远不能把 `ready=false` 改成 true，否则任意进程写几个固定字节就能认证随机池。

## 5. 2K1000LA 熵源接入

### 5.1 地址推导与访问属性

板级 APB 基址和随机寄存器偏移得到：

```text
physical RNG register = 0x1fe2b000
DMW2 alias             = 0x8000000000000000 + 0x1fe2b000
                       = 0x800000001fe2b000
```

`RNG_BASE` 必须保留 `+ HIGH_BASE_EIGHT`。CPU 通过内核已有 DMW2/SUC 别名读取 MMIO；
这里不是设备向内存写入的 DMA，所以不能把 raw PA 与 DMA buffer 规则混用。

### 5.2 每次读取获取一个 32-bit 结果

驱动按 4 字节 chunk 对寄存器做 `read_volatile`，转成 little-endian bytes，读取之间放
compiler fence。64 字节启动样本因此需要 16 次设备读取：

```rust
for chunk in dst.chunks_mut(4) {
    let word = read_volatile(RNG_BASE as *const u32).to_le_bytes();
    chunk.copy_from_slice(&word[..chunk.len()]);
}
```

`volatile` 防止编译器把多次 MMIO 读取合并成一次；compiler fence 约束编译器重排。
它们不证明随机数的熵率，只保证按设备寄存器语义发起读取。

### 5.3 为什么不把寄存器值直接返回给用户

直接透传会让每个调用者承担设备健康、并发和短读语义，也无法在平台 RNG 暂时卡死时
保护已经建立的随机流。APB RNG 只负责播种，ChaCha20 负责用户输出，职责分离后才有
一致 readiness 与失败策略。

## 6. QEMU 熵源与第一次集成故障

### 6.1 LA64：PCI 枚举 VirtIO entropy device

LA QEMU 复用 VirtIO PCI 枚举器，按 `DeviceType::EntropySource` 找设备，循环
`request_entropy()` 直到填满 64 字节。设备不存在、初始化失败、读取失败和短读分别
映射成明确 `EntropyError`。

### 6.2 RV64：设备参数存在，页表映射却缺失

RV QEMU 把 entropy device 放在 virtio-mmio bus.2，即 `0x10003000`。第一次运行在
该地址触发 `LoadPageFault`。这不是 RNG 算法错误，而是：

```text
QEMU command line creates device
  != kernel direct/page-table mapping includes device MMIO page
```

将 `(0x10003000, 0x1000)` 加入 RV 平台 `MMIO` 表后，分页开启后的寄存器访问才合法。
这个负例也证明测试确实走到了新设备，而不是仍在使用旧时间 seed。

## 7. 启动健康检查能证明什么

### 7.1 当前检查

64 字节样本被视为两个 32 字节半区，并拆成 16 个 little-endian `u32`：

```text
condition A: sample[0..32] != sample[32..64]
condition B: unique(u32[0..16]) >= 8
```

它可以拒绝：

- 恒定寄存器；
- 完整 32 字节周期重复；
- 只有少数固定 word 循环的明显卡死输出。

检查失败会擦除 sample，返回 `HealthCheckFailed`，且保持 `ready=false`。

### 7.2 当前检查不能证明

它不是熵率估计，也不是芯片 RNG 认证，不能证明：

- 16 个看似不同的 word 不可预测；
- 两个半区统计独立；
- 无硬件后门或环境相关偏差；
- 长期运行中设备不会退化；
- 达到任何 FIPS/NIST 认证要求。

因此文档和日志只能称它为“启动故障健康检查”，不能称为熵质量认证。

## 8. 调理、播种与重键

### 8.1 64 字节如何形成 32 字节 seed

实现过程为：

```text
first       = sample[0..32]
conditioned = ChaCha20(seed=first).next_32_bytes()
conditioned ^= sample[32..64]
old_state   = bootstrap_rng.next_32_bytes()
conditioned ^= old_state
global_rng  = ChaCha20(seed=conditioned)
ready       = true
```

在两个半区独立的前提下，XOR 结构避免只依赖其中一半；旧私有状态也被混入，但因其
来源只是时间/地址，**不为它记可信熵**。如果硬件两半相关或可预测，此过程不能凭空
制造熵，所以健康检查和平台 trust assumption 仍是前提。

### 8.2 临时材料主动擦除

`sample`、`first`、`conditioned`、`old_state`、每次重键 seed 和 syscall 临时 chunk
都用 volatile write 清零并放 compiler fence，避免优化器删除清理。它降低栈残留泄露
窗口，但不等价于防止所有物理内存取证或微架构侧信道。

### 8.3 每次输出后重键

每个 `fill` 完成用户输出后，再从未公开的 CSPRNG 流取 32 字节作为下一 seed：

```text
public output -> hidden next_seed -> replace generator -> wipe next_seed
```

这提供回溯方向的保护：以后泄露当前状态时，不应直接恢复先前已返回输出。它不提供
无重播种条件下的 compromise recovery：攻击者若得到当前状态，仍可能预测未来，直到
新的可信熵被混入；当前实现尚无周期重播种。

## 9. readiness 与 fail-closed 语义

### 9.1 启动顺序

`rust_main()` 在创建/运行用户任务前调用 `random::init()`：

```text
machine_init
  -> timer_subsystem_init
  -> random::init
  -> remaining device/fs/task startup
  -> userspace
```

平台正常时，首个用户进程启动前已 ready。初始化失败不会把整个内核 panic 掉，而是
打印“secure source unavailable；secure reads will fail”，让不依赖随机数的诊断仍可
运行，同时所有 secure consumer 明确失败。

### 9.2 `getrandom(2)` flag 语义

| flags | 当前行为 |
|-------|----------|
| `0` | ready 时安全输出；未 ready 返回 `EAGAIN` |
| `GRND_NONBLOCK` | 当前与普通安全读取相同；未 ready 返回 `EAGAIN` |
| `GRND_RANDOM` | 当前仍使用同一安全 CSPRNG，无独立 entropy pool |
| `GRND_INSECURE` | 可使用未认证 bootstrap state |
| 未知 bit | `EINVAL` |
| `GRND_RANDOM | GRND_INSECURE` | `EINVAL` |

实现以 256 字节内核块生成、复制到用户 buffer 并擦除临时块。普通无
`GRND_NONBLOCK` 请求目前也不会等待设备恢复，而是立即 `EAGAIN`；这是已知的 Linux
语义差异，但比退回弱输出安全。

### 9.3 `/dev/random` 与 `/dev/urandom`

两个 devfs 名称当前都实例化同一个 `Urandom` 实现：

- 读取调用 secure `random::fill_bytes`，未 ready 为 `EAGAIN`；
- 写入调用 `mix_untrusted`，返回写入长度；
- 写入不会改变 ready；
- `/dev/random` 没有独立阻塞 entropy-count 模型。

因此不能根据设备名推断 Linux 完整 `/dev/random` 语义；当前安全承诺是“同一个已播种
CSPRNG 或明确失败”。

## 10. 调试追溯过程

### 10.1 从“接口存在”回到输出来源审计

最初不能用 syscall 已注册、`read()` 返回长度证明随机数可用。沿三个用户入口逆向审计：

```text
/dev/urandom -> all zeros
getrandom    -> per-call public seed + xorshift
/dev/random  -> same weak device implementation
```

这一步把问题定性为安全根因，而不是补一个随机测试让全零不再出现。

### 10.2 先建立统一内核状态，再接平台设备

修复顺序为：

1. 定义 `ready` 和 secure/insecure 两条读取路径；
2. 让 syscall/devfs 全部汇入统一 CSPRNG；
3. 接 RV/LA VirtIO 与 2K1000 APB source；
4. 在进入用户态前播种；
5. 加正向 output test 和“无设备”负向门禁。

这样设备失败时不会意外落回旧实现，因为旧生成路径已从用户入口删除。

### 10.3 正向测试不足，必须故意移除熵源

只看“输出非零且两次不同”仍可能由旧时间 xorshift 通过。LA QEMU 因而额外移除
VirtIO RNG，期望：

```text
random::init -> DeviceUnavailable
rng_test     -> secure getrandom unavailable, exit 1
dd urandom   -> EAGAIN, 0 bytes
```

负向结果证明 secure API 受 readiness 约束，没有悄悄 fallback。该轮完整无 RNG raw
log 当前未保留，以上属于 `docs/Work_Log.md` 记录。

## 11. 证据链

### 11.1 源码前后证据

| 证据 | 证明 |
|------|------|
| `5d2f16e^:os/src/fs/dev/urandom.rs` | 旧设备确实全零成功 |
| `5d2f16e^:os/src/syscall/mod.rs` | 旧 getrandom seed 只含时间/地址/长度 |
| `os/src/drivers/rng/mod.rs` | 三个平台可信 source 有明确错误返回 |
| `os/src/random.rs` | ready、健康检查、调理、重键、wipe 在同一状态机 |
| `os/src/syscall/mod.rs::sys_getrandom` | secure/insecure 分流及 flag 校验 |
| `os/src/fs/dev/urandom.rs` | 两随机设备读 secure stream，写不记熵 |

### 11.2 可直接读取的当前原始日志

虽然最初专项 `rng_test` 串口未单独保留，后续真实运行日志持续证明平台 source 仍在
使用：

```text
logs/net-perf-board-baseline-run-20260715.log:78
  [kernel] random: initialized from 2k1000-rng

logs/ext4-apk-board-final-20260715.log:78
  [kernel] random: initialized from 2k1000-rng

logs/ext4-fs-test-la64-direct-20260715.log:51
  [kernel] random: initialized from virtio-rng
```

这些行只证明初始化 source 和健康检查通过，不单独证明用户输出统计质量或专项 flag
测试通过。

### 11.3 专项验收记录

提交 `5d2f16e` 对应 Work_Log 记录：

| 平台/场景 | 结果 |
|-----------|------|
| RV QEMU 首轮 | `0x10003000` LoadPageFault，补 MMIO 后 PASS |
| RV QEMU 最终 | `virtio-rng`，`rng_test` 连续 2 次 PASS |
| LA QEMU 最终 | `random: initialized from virtio-rng`，`rng_test` PASS |
| LA QEMU 无 RNG | `DeviceUnavailable`；test exit 1；urandom EAGAIN/0 byte |
| 2K1000LA | `random: initialized from 2k1000-rng`；连续 5 次 PASS |
| 2K1000LA 联合回归 | SATA scratch、GMAC、DHCP 保持正常，无 panic |

仓库未保留这一轮所有专项 raw logs，因此次数和负向结果应标注为 Work_Log 证据；后续
日志中的初始化行提供了独立、较新的持续证据。

## 12. `rng_test` 覆盖和没覆盖的内容

测试执行：

1. 两次 secure `getrandom`，每次 64 字节；
2. 检查各自非全零、至少存在相邻差异、两块不相同；
3. 未知 flag 和 `RANDOM|INSECURE` 必须 `EINVAL`；
4. `/dev/urandom` 读取 64 字节，非全零且不同于第一次输出。

它能发现全零、卡死、明显重复、接口没接统一池和 flag 错误。它不能证明：

- 输出通过统计随机性套件；
- 达到某个熵 bit 数；
- ChaCha 实现没有密码学缺陷；
- 硬件 RNG 不可预测；
- 周期重播种/compromise recovery 已实现。

“live and distinct” 是活性回归，不是熵认证，测试名称和结论不能越界。

## 13. 排除的错误修法

### 13.1 用时间戳给更复杂 PRNG 播种

算法再强也无法从公开/低熵 seed 创造秘密。CSPRNG 必须由可信平台输入建立初始状态。

### 13.2 熵源失败时退回旧 xorshift

这会让 API 在最需要暴露故障时返回“看起来随机”的成功输出。正确结果是 EAGAIN。

### 13.3 直接把 APB RNG 字节交给用户

会失去统一 readiness、设备故障隔离、任意长度流和重键保护，也让每次用户读取都依赖
MMIO 可用性。

### 13.4 用户写入随机设备后置 ready

调用者控制的输入不是可信熵；允许它认证随机池会使权限边界失效。

### 13.5 只做正向“非零”测试

旧 xorshift 也能输出非零。必须移除设备验证 secure API 明确失败，才能证明无 fallback。

## 14. 修复后的不变量

1. 所有 secure 用户入口共享同一个 ChaCha20 状态和同一个 ready bit。
2. ready 只能由可信平台 sample 通过健康检查后设置。
3. bootstrap seed 永远只服务 `GRND_INSECURE`，不能被宣传为安全。
4. 用户写入只混入状态，不提升 entropy credit/readiness。
5. 每次输出后重键，临时 seed 和 syscall chunk 主动擦除。
6. 熵源初始化失败时继续启动诊断环境，但 secure 读取统一 `EAGAIN`。
7. APB/VirtIO 原始输出只用于播种，不直接作为用户随机流。
8. QEMU 设备参数与平台 MMIO/PCI 可达性必须同时成立。

## 15. 已知边界和后续工作

- 只在启动时采集一次可信熵，没有按时间、输出量或设备事件周期重播种。
- 没有 compromise recovery；状态泄露后的未来输出要等新可信熵才能恢复安全。
- 健康检查只能拒绝明显卡死，不做在线连续检测或熵率估计。
- `/dev/random` 没有独立 entropy counter/阻塞模型，与 `/dev/urandom` 相同。
- 普通 blocking `getrandom` 在未 ready 时立即 `EAGAIN`，尚未实现等待队列语义。
- 全局 `spin::Mutex` 串行所有随机请求；当前单核足够，未来高并发/SMP 需评估锁时长。
- 2K1000 APB RNG 的 trust 来自板级手册和实测，不代表经过第三方密码学认证。
- 尚未建立故障后的运行期重新初始化、设备热失效或周期 health test 状态机。

## 16. 证据索引

| 类型 | 位置 |
|------|------|
| 根因修复提交 | `5d2f16ef4de6acff2eca759d984bf47dc0875fde` |
| 旧全零设备 | `5d2f16e^:os/src/fs/dev/urandom.rs` |
| 旧时间/xorshift | `5d2f16e^:os/src/syscall/mod.rs::sys_getrandom` |
| 平台熵驱动 | `os/src/drivers/rng/mod.rs` |
| 2K1000 地址 | `os/src/hal/platform/loongarch64/2k1000.rs::RNG_BASE` |
| 随机状态机 | `os/src/random.rs` |
| syscall ABI | `os/src/syscall/mod.rs::sys_getrandom` |
| devfs ABI | `os/src/fs/dev/urandom.rs`、`os/src/fs/mod.rs` |
| 用户专项测试 | `user/src/bin/rng_test.rs` |
| 专项验收 | `docs/Work_Log.md` 2026-07-13 `random` 条目 |
| 后续实板 raw evidence | `logs/net-perf-board-baseline-run-20260715.log:78` |

## 17. 可复用调试模式

随机接口的验收必须同时回答四个问题：

```text
bytes returned?
  -> source trusted?
  -> source failure detected?
  -> all user APIs share readiness?
  -> negative test proves no weak fallback?
```

“非零、两次不同”只能回答第一层。真正的安全闭环是：可追溯的熵源、有限但明确的健康
检查、集中式 CSPRNG、严格 readiness，以及无熵时宁可报错也不伪造成功。
