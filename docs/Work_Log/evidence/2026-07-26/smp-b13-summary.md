# SMP-P2-B13 证据摘要

- 基线 commit：`0ea66050bfb544f260c4a48548966346fa9f90a0`
- 最终验证时 tracked source diff SHA-256：
  `5b6fa8b38d3f082c11f7adda9125c4a76d133055492c48feb015524710dda9c6`
- 配置：QEMU `CORE_NUM=4`，focused `KTEST=smp KREPEAT=2`

## 实现边界

- CPU0 向 online AP 发布 STOP，并有界等待独立 stopped ack。
- AP hard IRQ 只发布请求；真正关闭本地 interrupt source、发布 ack 和
  永久 idle 均发生在 AP 独立 idle stack。
- 正常 CPU0 shutdown 复用相同幂等协议。
- ktest 的 terminal 用例在全部普通 repeat 后只运行一次。

## RED

初轮 RV64 build 与 15/15 ktest 已通过；LA64 build 因 LLVM 汇编器拒绝
`iocsrwr.w zero, ...` 失败，LA64 ktest 因编译失败未运行。官方 QEMU
9.2.1 源码确认 IOCSR `CORE_EN` 位于 `0x1004` 且使用直接赋值语义；
真正的编译根因是 LoongArch inline asm 要求零寄存器写作 `$zero`。

## GREEN

| 命令 | 结果 | 用时 |
|---|---:|---:|
| `make kernel ARCH=rv64 PROFILE=normal CORE_NUM=4` | PASS | 124.312s |
| `make kernel ARCH=la64 PROFILE=normal CORE_NUM=4` | PASS | 128.363s |
| `make ktest ARCH=rv64 PROFILE=normal CORE_NUM=4 KTEST=smp KREPEAT=2` | PASS（15/15） | 初轮证据 |
| `make ktest ARCH=la64 PROFILE=normal CORE_NUM=4 KTEST=smp KREPEAT=2` | PASS（15/15） | 131.017s |

两个 QEMU 用例均观察到 `online_mask=0xf`。TAP 计划为 `1..15`：
7 个普通测试各运行两次，`smp::secondary_cpus_stop_and_ack` 只作为第 15 项
运行一次；结果为 15 passed、0 failed，`[KTEST RESULT: PASS]` 后正常退出。

最终修正只改动 LA64 条件编译汇编操作数，故最终轮重跑双架构 build 与
LA64 QEMU，并沿用修正前已通过且不受该 cfg 改动影响的 RV64 QEMU 证据。
所有记录的 runner 都报告源码前后指纹一致、未检测到 mutation。
