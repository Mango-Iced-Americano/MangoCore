# B90 时间源全局可变状态收口证据

## 冻结对象

- 基线 HEAD：`de128346ebe0828668de781d5bb3a7e748002246`
- tracked diff SHA-256：
  `fa8c3620cb52808126b2a3b86f2d8fd14a89af49d6536a30ddb9afd3e4b3d3ca`
- 执行环境：项目 Docker；双架构 normal build，严格串行
- DeepSeek 任务与完整日志仅保存在本地忽略的
  `cc-codex/runtime/jobs/smp-b90-time-source-validation/`，不上传 GitHub。

## 源码事实

删除前，`TIME_SOURCE: Option<&'static dyn TimeSource>` 只有 `init_time_source()` 一个写入入口，
却没有任何读者；全仓也没有人调用该 init。唯一 `TimeSource` 实现 `MTime`
直接读取 RISC-V virt 的硬编码物理地址，但同样不可达。这套旧抽象即使未实际
引发竞态，也是无同步 `static mut` 和绕过双架构 HAL 的潜在错误入口。

生产数据流在删除前后均为：

```text
raw_ticks()/clock_freq()
  -> hal::get_time()/hal::get_clock_freq()
  -> RV64 time CSR + FDT timebase / LA64 rdtime + CPUCFG CLOCK_FREQ

current_timespec()/current_timeval()
  -> monotonic HAL time + AtomicU64 BOOT_TIME_OFFSET_NS
```

因此本批没有把 trait object 换成另一把全局锁：无读者状态应直接删除，而不是为了
表面 SMP-safe 保留死抽象。

## DeepSeek 冻结门禁

| 子任务 | 配方 | 结果 | 证据 |
|---|---|---|---|
| `agent-439b174cd255-r01-rv64-kernel-build` | RV64 normal build | PASS | exit 0，133.798 s |
| `agent-439b174cd255-r02-la64-kernel-build` | LA64 normal build | PASS | exit 0，137.631 s |

两项 `source_before/source_after` 一致，`mutation_detected=false`，无禁止标记。DeepSeek
只读检查同时确认删除符号在生产源码零调用。

## 验收边界

B90 证明删除不可达注册表不破坏双架构编译，并明确时间数据流的唯一
HAL owner。它不是 timer 算法、IRQ 或调度语义变更，因此本批 QEMU 为 NOT RUN。
