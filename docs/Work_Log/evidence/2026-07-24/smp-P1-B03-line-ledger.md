# SMP-P1-B03 line ledger

本批经用户明确批准不受“关键实现约 50 行”限制；以下仍保留可审计计数，
该例外不自动延续到下一批 SMP 修改。

基线重建方法：

1. 从当前 `HEAD` 导出临时源码树；
2. 依次应用已经归档的 P0-B01、P1-B01、P1-B02 代码补丁；
3. 将该临时树与当前 B03 目标文件逐文件执行 `git diff --no-index`。

这避免把前三批已经人工确认的 CORE_NUM 契约和双架构 boot-stack 改动重复
计入本批。结果保存于 `smp-P1-B03-code-diff.patch` 和
`smp-P1-B03-code-numstat.txt`。

| 文件 | Added | Deleted |
|---|---:|---:|
| `os/make/rv64.mk` | 9 | 5 |
| `os/make/la64.mk` | 7 | 4 |
| `os/src/main.rs` | 22 | 2 |
| `os/src/smp.rs` | 174 | 0 |
| `os/src/hal/mod.rs` | 3 | 1 |
| `os/src/hal/arch/mod.rs` | 4 | 3 |
| `os/src/hal/arch/riscv/mod.rs` | 17 | 1 |
| `os/src/hal/arch/riscv/sbi.rs` | 69 | 0 |
| `os/src/hal/arch/loongarch64/boot.rs` | 2 | 0 |
| `os/src/hal/arch/loongarch64/entry.asm` | 3 | 0 |
| `os/src/hal/arch/loongarch64/mod.rs` | 80 | 26 |
| **Raw total** | **390** | **42** |

按“去掉空行以及以 `//`、`#`、`/*`、`*` 开头的直接注释行”机械分类：

| Class | Added | Deleted | Changed |
|---|---:|---:|---:|
| Implementation-shaped | 264 | 38 | 302 |
| Direct comment or blank | 126 | 4 | 130 |
| **Raw total** | **390** | **42** | **432** |

机械分类会把仅含括号、属性和类型签名的行计为 implementation-shaped，因此
它是偏保守的上界，不是“复杂逻辑行数”。本批行数显著超过常规门禁的原因是
必须同时闭合：

- 通用 BSP/AP 状态机与硬件/逻辑 CPU ID 映射；
- RISC-V SBI v0.2 HSM 调用；
- LoongArch QEMU mailbox + IPI slave-ROM 协议；
- 双架构 HAL 接口、公共入口 ABI 和 QEMU 参数传递。

本批没有新增锁、堆分配或 MM/TLB 修改；并发共享面限定为固定大小原子状态。
