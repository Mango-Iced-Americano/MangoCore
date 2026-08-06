# develop Batch 6 read/pread 可写前缀证据

## 变更目标

develop commit `8cdcbbfa` 希望避免 read/pread 在真正取得数据前对完整用户输出区间制造假 CoW
和 TLB flush。集成分支不能直接 cherry-pick：旧实现会保存物理页 slice，与 B57—B59 已建立的
“uaccess 对象只保存 VA、实际 copy 在 VM 锁内重验 PTE”约束冲突。

本批保留性能意图，重新实现为：

1. `new_writable_prefix()` 在一次 VM 临界区内扫描当前已有可写 PTE；
2. 若首页不可写且前缀为空，只 fault-in 首页；
3. 后续不可写页立即结束扫描，不提前触发 lazy allocation、CoW 或 shootdown；
4. 返回 `(VA-backed writer, accessible_len)`，文件对象最多只消费该长度；
5. 真正用户 copy 逐页重新取得 VM 锁并验证当前映射。

## 静态审查

DeepSeek 只读任务 `develop-batch6-uaccess-review-r1-20260803` 状态 `SUCCEEDED/REVIEWED`，未发现
阻塞项。其确认 VM 临界区只带出前缀长度，文件 I/O 不持 VM 锁，实际 copy 可应对并发
`munmap/mprotect/CoW`。GPT/Codex 采纳了 `next <= current` 防御检查；对报告中把单字段
newtype 称为 ZST 的表述不采纳，该类型只是零额外抽象，并非零大小。

## 失败与修正

首轮任务 `develop-batch6-uaccess-validation-r1-20260803` 的 RV64 child
`agent-f57c4a9fc173-r01-rv64-regression-8core` 在 1.627s 后 exit 2。原因是新回归从仅在 LA64
导出的 `user_lib::layout` 导入 `PAGE_SIZE`，RV64 用户程序编译失败，未进入 QEMU；before/after
diff 指纹一致。LA64 已被预留但在确认测试源码问题后中止，不能记为通过。

修正为回归程序内的双架构 4 KiB 基础页常量后，以新任务重跑，未覆盖旧失败记录。

## 冻结验证

父任务：`develop-batch6-uaccess-validation-r2-20260803`

| 架构 | child job | 耗时 | 结果 | online | 回归 |
|------|-----------|------|------|--------|------|
| RV64 | `agent-3694999a27f2-r01-rv64-regression-8core` | 142.744s | PASS / exit 0 | `0xff` | 7/7 |
| LA64 | `agent-3694999a27f2-r02-la64-regression-8core` | 138.651s | PASS / exit 0 | `0xff` | 7/7 |

两架构新增场景均输出：

```text
cross-page detail: protected=true first=8 prefix_ok=true second=8 tail_ok=true
```

这证明第二页只读时，第一次 16-byte pipe read 仅消费第一页末尾 8 字节，第二次 read 仍取得
余下 8 字节。原 NULL buffer `EFAULT` 后数据保留场景也继续通过。两次测试均无 forbidden
marker、panic、fatal trap、timeout 或源码变异。

## 源码指纹

- HEAD：`24aeb6da8a59cbb399648c15b83e43ce3ff68951`
- status SHA-256：`b3a5035f099a0442bbedfebe7d703ac0e3cdfc8bf02b96c756d7ad2374cc4b0c`
- tracked diff SHA-256：`1d84128dfafd47509e7cd816cf52bf9c4f8a030dc8dafdea0a595f7fca69904d`
- untracked content SHA-256：`e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`

DeepSeek 完整 prompt、分析与原始日志只保存在本地忽略的 `cc-codex/`，不上传 GitHub。
