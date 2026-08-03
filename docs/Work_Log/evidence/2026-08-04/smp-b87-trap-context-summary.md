# B87 trap context 直映射借用收口证据

## 冻结对象

- 基线 HEAD：`8381657818f19e441a60988166de3dc1c8d411e3`
- tracked source diff SHA-256：
  `3e30ccb16e587c7d4de2dca14a0ceac1684610936bebc65b23f151522b8420b5`
- 执行环境：项目 Docker；`CORE_NUM=8`，`KTEST=smp`
- DeepSeek 完整 task、stdout/stderr、result 位于本地忽略的
  `cc-codex/runtime/jobs/smp-b87-trap-context-r1/`，不上传 GitHub。

## 所有权审查

`PhysAddr::{get_ref,get_mut,get_bytes_ref,get_bytes_mut}` 没有调用者；
`PhysPageNum::get_mut()` 的唯一调用者是 `TaskControlBlockInner::trap_context_mut()`。
这些安全函数却能返回与 owner 无关的 `'static` 引用，因此调用点注释无法让编译器约束
类型、存活期和独占权。

删除 helper 后，局部 unsafe 由三项事实支撑：trap frame 物理页随 TCB 存活；页首 4 KiB
对齐满足双架构 `repr(C) TrapContext`；`&mut TaskControlBlockInner` 来自 `task.inner` guard，
返回引用生命周期绑定到该独占借用。trap return 释放 guard 后仅把同一 frame 地址交给
不返回的恢复汇编，current 槽继续持有任务 owner，不存在 Rust 可变引用与汇编并存。

## DeepSeek 四项冻结门禁

| 子任务 | 配方 | 结果 | 证据 |
|---|---|---|---|
| `agent-54187a7f8e7c-r01-rv64-kernel-build` | RV64 normal build | PASS | exit 0，139.8 s |
| `agent-54187a7f8e7c-r02-la64-kernel-build` | LA64 normal build | PASS | exit 0，137.2 s |
| `agent-54187a7f8e7c-r03-rv64-ktest` | RV64 8 核 SMP | PASS | 34/34，138.9 s |
| `agent-54187a7f8e7c-r04-la64-ktest` | LA64 8 核 SMP | PASS | 34/34，144.2 s |

四项 source before/after 指纹一致，`mutation_detected=false`；日志没有 panic、timeout、
fatal trap 或缺失的 TAP marker。编译警告属于既有 baseline，与本次修改无关。

## 验收边界

本节点证明 trap context 的 Rust 可变引用不能再从通用安全物理地址接口逃逸，并证明双架构
任务创建、clone/exec、signal 与用户 trap 往返没有退化。它没有处理仍被 MM/PageCache/FS
共同使用的整页 byte view，也没有新增人工测试路径。
