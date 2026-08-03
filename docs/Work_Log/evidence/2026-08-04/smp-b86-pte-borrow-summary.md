# B86 页表可变借用边界收口证据

## 冻结对象

- 基线 HEAD：`54768fd2ccb06f7917c382c745d8b718da5f376c`
- 最终 tracked source diff SHA-256：
  `8fa1b0b0b8f0e1084fe99d5b581b71a4d928d5d1648e7a98caed2090f395fc9e`
- 执行环境：项目 Docker；QEMU 10.0.2；`CORE_NUM=8`，`KTEST=smp`
- DeepSeek 完整 task、stdout/stderr、result 只保存在本地忽略的 `cc-codex/runtime/`。

## 问题与修复

`PhysPageNum::get_pte_array()` 原为安全函数，却能返回与调用方生命周期无关的
`&'static mut [T]`。RV64/LA64 的 `find_pte_refmut(&self)` 因此可从共享 PageTable 借用制造
可变 PTE。虽然主要生产调用点当前受 `AddressSpace` VM 锁保护，这一动态事实没有进入 API
合同，后续调用点很容易绕过独占约束。

修复把 raw PTE view 拆为 crate-private unsafe 只读/可写接口；只读 walker 只生成共享引用，
可写 walker 和 `block_and_ret_mut*()` 必须先取得 `&mut PageTable`。unsafe 只负责物理页的
类型和存活期，页表可变借用与 VM 锁共同负责独占性。PTE 编码、修改顺序和 TLB 协议未变。

## RED 到 GREEN

1. `smp-b86-pte-mut-r1`：子任务
   `agent-dd95a26384fd-r01-rv64-kernel-build` 在当时源码上 PASS；但 GPT 在任务提交后删除一处
   重复注释，wrapper 检出 before/after diff 指纹不同并正确判 FAILED。该轮不计入最终门禁。
2. `smp-b86-pte-mut-r2`：
   `agent-8cfa9e5b14bb-r01-rv64-kernel-build` PASS；
   `agent-8cfa9e5b14bb-r02-la64-kernel-build` RED，包含 Rust 2018 数组 `.into_iter()` 产生
   `&usize` 的 E0277，以及可变 PTE 借用跨越 `self.invalidate_page()` 的 E0502。GPT 修复
   两个根因并以 SIGTERM 停止该轮后续无效测试，parent exit 143。
3. `smp-b86-pte-mut-r3`：冻结最终源码，未在验证期间修改任何源文件。

## 最终门禁

| 子任务 | 配方 | 结果 | 证据 |
|---|---|---|---|
| `agent-a70c13ac3491-r01-la64-kernel-build` | LA64 normal build | PASS | exit 0，139.293 s |
| `agent-a70c13ac3491-r02-rv64-ktest` | RV64 8 核 SMP | PASS | 34/34，136.045 s |
| `agent-a70c13ac3491-r03-la64-ktest` | LA64 8 核 SMP | PASS | 34/34，140.305 s |

三项 `source_before` 与 `source_after` 的 tracked diff SHA-256 均为最终指纹，且
`mutation_detected=false`。日志无 panic、timeout、fatal trap；第 24 项出现的 RV64
`StorePageFault` / LA64 `PageModifyFault` 是 mprotect 降权用例的预期行为，TAP 均判 PASS。

## 验收边界

本节点证明页表写入的 Rust API 必须持有独占借用，并证明既有双架构 PTE/TLB 运行语义没有
退化；它没有新增用户态测试、临时计数器或全量初赛回归。真实远端权限降级和 8 发起者
生产 PTE writer 分别由 B84、B85 的永久 SMP ktest 覆盖。
