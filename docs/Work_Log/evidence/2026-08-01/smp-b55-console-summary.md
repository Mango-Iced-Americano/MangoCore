# B55 SMP console 与 panic raw 输出证据

## 结论

B55 将正常 console 输出收敛为跨 CPU 串行的 irq-save 叶子临界区，并为 panic 建立不等待
普通 console/UART 锁的单向 raw 路径。LA64 同时恢复 UART THR-ready 等待，避免 Mutex
重构后忽略 `WouldBlock` 而丢字符。双架构 8 核 focused 与初赛语义门禁均无新增回归。

## 锁与 panic 协议

正常路径：

```text
local_irq_save -> OUTPUT_LOCK -> HAL writer -> [LA64 UART Mutex]
                                      <-              <-
local_irq_restore <- OUTPUT_LOCK drop
```

panic handler 先关闭本地中断，再以 Release 设置 `PANICKING`。后续 `print()` 直接走 raw；
已经等待 `OUTPUT_LOCK` 的 CPU 用 Acquire 观察状态并放弃等待。RV64 raw 后端使用不持 Rust
锁的 MMIO/SBI，LA64 使用只含 MMIO base 的局部 `Ns16550a`，不取得静态 UART Mutex。

该协议只承诺“不等待内核锁”。SBI legacy putchar 和 UART THR-ready 都可能等待真实发送设备，
所以文档没有把 raw path 描述为硬件非阻塞。

## 冻结源码与协作

- 基线 HEAD：`c0cd610a2545ff91da061b7e98ee8ef7a71687c4`
- tracked diff SHA-256：
  `84a9f21f8a6e784696c3d0e2dc52d9efdc4d4b5ac47033d218c0616c450c2ee9`
- DeepSeek 任务：`smp-b55-console-gate`
- 四个 child 的 before/after 指纹一致，均 `mutation_detected=false`。

## 最小 Docker 门禁

| Child | 配置 | 结果 |
|---|---|---|
| `agent-8ddf29263782-r01-rv64-ktest` | RV64，8 核，`KTEST=smp` | 34/34，exit 0，136.5s |
| `agent-8ddf29263782-r02-la64-ktest` | LA64，8 核，`KTEST=smp` | 34/34，exit 0，137.3s |
| `agent-8ddf29263782-r03-rv64-preliminary` | RV64，8 核，`mask=0x003` | raw 309 / semantic 312，369.3s |
| `agent-8ddf29263782-r04-la64-preliminary` | LA64，8 核，`mask=0x003` | raw/semantic 308，379.2s |

没有另跑 build-only：focused 已构建双架构 ktest profile，初赛已构建双架构 normal profile，
重复纯编译不会增加本批证据。四项均无 forbidden marker、panic、fatal 或 timeout。

## RV64 raw/semantic 裁决

RV64 raw judge 的新增差额是 `basic-glibc/test_pipe=1/4`。原始块为：

```text
cpid: 112cpid: 0
  Write to pipe successfully.
```

块内存在恰好两个 cpid 整数，分别为正数和 0，并有 write-success、START/END、无错误。
这符合项目自 B18 起固定的 §8.2 归一化条件：测试程序用多个 write syscall 拼一个 printf，
两个进程可在 syscall 安全点合法交错，官方 judge 按物理行匹配时产生 3 分假阴性。因此保留
raw 309 作为原始事实，同时按同一既有规则记 semantic 312。两个 BusyBox `kill 10` 仍是
既有失败；没有为恢复分数重跑，也没有跨 syscall 锁 TTY 或缓存到换行。

LA64 仍只有两套 `test_brk 1/3` 与两套 `busybox kill 10`，失败集合未变化。

## 证据边界

正常双架构构建、8 CPU 并发启动输出和用户回归已经动态运行。panic raw 分支由无锁调用链
和类型/锁序审查证明；为避免把测试 hook 留在生产 console 中，本批没有注入“持锁后 panic”
的专用动态用例，该项不能写成已动态运行。
