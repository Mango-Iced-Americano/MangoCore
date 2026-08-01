# B57 fixed-size uaccess 映射同步证据摘要

## 1. 交付范围

- 基线 HEAD：`61aee74bf7fcedf5a414ae1ef05b0bd14b8688c2`
- 冻结源码 diff SHA-256：
  `e2d2c106b4ab176646811116b67b01102c1ba5cde08cf21acbcfef151d6830f4`
- 生产改动：删除 `translated_ref*` 和旧 single-page copy fallback；固定对象/数组 copy
  在逐页 VM 锁内完成 fault、权限后验检查与 raw direct-map copy；迁移 ioctl 剩余调用点。
- 明确不在本节点：`UserBuffer`、`translated_byte_buffer()`、`translated_str()` 重构；
  并发 fork/munmap 与 fixed copy 的专用动态竞态注入。

## 2. 原理依据

1. [Rust Reference：undefined behavior](https://doc.rust-lang.org/stable/reference/behavior-considered-undefined.html)
   规定 Rust 引用必须遵守 alias 约束；`unsafe` 不会自动豁免 `&mut` 的独占性要求。
2. [Rust for Linux：uaccess/UserSlice](https://rust.docs.kernel.org/6.14/src/kernel/uaccess.rs.html)
   保存用户地址与长度，在实际 read/write 时执行复制，并明确提醒用户内存的 TOCTOU 边界。
3. [Linux kernel hacking：uaccess](https://docs.kernel.org/6.11/kernel-hacking/hacking.html)
   要求通过 copy API 访问用户指针，并指出该路径可能 fault/sleep，不应带入 spinlock。
4. [Linux pin_user_pages](https://docs.kernel.org/next/core-api/pin_user_pages.html)
   区分页的普通引用和写入/DMA pin 语义；单纯延长 frame 生命周期不能证明 VA 映射稳定。

MangoCore 的具体裁决是：映射稳定由 `AddressSpace` VM 锁提供，Rust 侧只在锁内创建瞬时 raw
pointer；不引入额外 pin 体系，也不从 helper 返回用户物理页引用。

## 3. 协作审查

- `smp-b57-uaccess-design`：DeepSeek 只读确认旧路径存在
  `translate -> unlock -> use` 竞态，认可 B57 先收口 fixed-size copy、B58 再处理 buffer。
- `smp-b57-uaccess-gate`：父任务运行期间实现发生变化，冻结协议正确标记 FAILED；该父任务
  结论不作为提交证据。
- `smp-b57-uaccess-final`：最终指纹冻结后运行 LA64 focused 与 RV64 preliminary；结合前轮
  同指纹的 RV64 focused 与 LA64 preliminary，形成双架构互补门禁。
- DeepSeek 最终文字错误地声称初赛全部通过；GPT/Codex 直接解析两个原始 judge JSON，纠正
  为 RV64 312/314、LA64 308/314。模型摘要不替代原始证据。

`cc-codex/` 中的 task、manifest、stdout/stderr 和分析文件均为本地忽略工件，不上传 GitHub。

## 4. 采纳的冻结测试

| Job | 架构/场景 | 结果 | 用时 |
|---|---|---:|---:|
| `agent-d56bc85383f8-r03-rv64-ktest` | RV64，8 核，`KTEST=smp` | 34/34 PASS | 136.174s |
| `agent-e13e52e9d905-r01-la64-ktest` | LA64，8 核，`KTEST=smp` | 34/34 PASS | 137.198s |
| `agent-e13e52e9d905-r02-rv64-preliminary` | RV64，8 核，`mask=0x003` | 312/314 | 351.035s |
| `agent-d56bc85383f8-r04-la64-preliminary` | LA64，8 核，`mask=0x003` | 308/314 | 366.844s |

四项均为 process exit 0、`mutation_detected=false`，无 forbidden marker、panic、fatal 或
timeout。初赛精确失败集合：

- RV64：musl/glibc 两项 `busybox kill 10`；
- LA64：musl/glibc `test_brk` 各 1/3，以及两项 `busybox kill 10`。

失败集合与 B57 前基线一致，没有新增回归。

## 5. 未采纳/负面证据

- 第一轮 RV64 compile 因 closure `Ok(())` 无法推断 error type 失败；实现补充显式
  `Result<(), isize>` 后才进入最终冻结门禁。
- 第一轮 LA64 focused 为 33/34，#24 报 `stale-TLB timer isolation evidence was incomplete`；
  相同最终源码指纹复跑 34/34。该失败与 B57 无直接调用链，但根因未定位，所以只记为既有
  TLB 测试敏感点，不写成“已修复”。
- 没有添加生产 test hook，也没有用重复运行刷绿取代上述失败披露。

## 6. 证据边界

本节点证明 fixed-size copy 的源码锁域已闭合，并证明双架构 8 核 focused/初赛没有可见回归；
没有动态制造 sibling fork/CoW/munmap 与 copy 的每一种时序。B58 必须继续消除可变长 buffer
和字符串路径的锁外物理页视图，并审计 SysV IPC 等既有 fixed-copy 调用方跨 registry 锁的
问题；在完成前不得解除普通用户任务的共享子系统门禁。
