# B84 真实远端 mprotect 与 LA64 写保护证据

- 状态：`pass`
- 基线 HEAD：`5bc481fd0f0b08c5acfed67c55a3ca943ec48dff`
- 最终生产/测试 diff SHA-256：`a047abea976c910945bf3f6fce3e6757559f64fb7f9bc307f790b9b98dc41e3b`
- Docker container：`mangocore-smp-integration-20260725-os-dev-1`
- QEMU：RV64/LA64 均为 `10.0.2`

## 合同与根因

[LoongArch 官方卷一](https://loongson.github.io/LoongArch-Documentation/LoongArch-Vol1-EN.html)
规定：普通页表的 W 位用于 page walk，填入 TLB 的 EntryLo 不含 W；硬件以 D 位决定 store
是否触发 PageModifyFault。`INVTLB 0x5` 会删除 non-global、ASID 和 VA 都匹配的完整 TLB 项，
并不存在“只刷新 PPN、不刷新权限”的语义。

[Linux v6.6 LoongArch `pgtable.h`](https://github.com/torvalds/linux/blob/v6.6/arch/loongarch/include/asm/pgtable.h)
的 `pte_wrprotect()` 同时清 `_PAGE_WRITE | _PAGE_DIRTY`。MangoCore 旧
`LAFlexPageTableEntry::revoke_write()` 只清 W；即使远端精准失效和 ack 完全正确，重新
page walk 仍把 D=1 填入 TLB，用户 store 因而继续成功。修复把 W/D 清除收敛到该共同底层，
并删除 `block_and_ret_mut*()` 的重复 `clear_dirty()`。

## 真实用户证据

CPU1 在 timer 静默窗口中不经 syscall/yield，依次执行：

1. 读取旧页，CPU0 完成真实私有 CoW 后读取新 PPN canary；
2. CPU0 正式 `munmap + MAP_FIXED_NOREPLACE` 后读取第三个 frame canary；
3. 在旧 RW 权限下成功 store，证明测试页并非原本只读；
4. CPU0 的 `mprotect(RW -> R)` 返回且远端 ack 完成后再次 store。

第 4 步必须在写入前触发 SIGSEGV。用例同时检查子进程 wait status、只读 frame canary、
handler observed generation 和 full-user request 不增长，避免用其它 trap 的全刷掩盖缺陷。

## DeepSeek 冻结验证与人工裁决

1. RV64 `CORE_NUM=8 KTEST=smp KREPEAT=1`
   - job：`smp-b84-mprotect-rv-red-r1`
   - child：`agent-b6e4232306e6-r01-rv64-ktest`
   - 34/34 PASS，远端 StorePageFault 最终为 SIGSEGV。
2. LA64 修复前 RED
   - job：`smp-b84-final-gates-r1`
   - build：RV64/LA64 均 PASS。
   - LA64 8 核：33/34；唯一失败
     `CPU1 store bypassed the mprotect downgrade`。
   - DeepSeek 初步归因为 INVTLB 权限失效；GPT/Codex 依据官方完整项失效语义和 PTE 位布局，
     将根因纠正为新 page walk 仍读取 D=1。
3. LA64 W/D 修复后 GREEN
   - job：`smp-b84-la64-wrprotect-fix-r1`
   - child：`agent-2abcd3fca315-r01-rv64-kernel-build`、
     `agent-2abcd3fca315-r02-la64-kernel-build`、
     `agent-2abcd3fca315-r03-la64-ktest`。
   - 双架构 build 退出码 0；LA64 8 核 34/34，关键路径出现预期 PageModifyFault，
     SIGSEGV status/canary/精准刷新检查全部通过。
4. LA64 初赛非回归
   - job：`smp-b84-la64-preliminary-r1`
   - child：`agent-7ad09e9922c3-r01-la64-preliminary`
   - 308/314；仅既有双 libc `test_brk` 和 `busybox kill 10` 失败，集合未扩大。

所有最终 child 均 `mutation_detected=false`，无 panic、timeout、fatal trap 或全用户 TLB
fallback。DeepSeek prompt、manifest 与完整 stdout/stderr 只保存在本地忽略的
`cc-codex/`，不纳入 Git 或上传。
