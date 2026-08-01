---
date: 2026-08-01
timezone: Asia/Shanghai
phase: smp-b59
status: pass
---

# B59 VA-backed UserBuffer 证据摘要

## 目标与结论

B59 删除 uaccess 最后一个生产用锁外物理页 slice 模型。`UserBuffer` 与 `UserIoVec` 现在
只描述 token、访问方向和用户虚拟地址区间；每次实际传输都逐页取得当前 AddressSpace
锁、解析当前 PTE、检查权限并在锁内完成 copy。

双架构最终源码的 `CORE_NUM=8 mask=0x003` preliminary 均完成，失败集合与 B58 基线精确
一致：RV64 312/314、LA64 308/314，无新增失败、panic、fatal、timeout 或源码 mutation。

## 设计裁决

### 1. 预 fault 不是 pin

构造 `UserBufferReader/Writer` 时仍提前 fault-in，用于保持既有 syscall 的副作用排序；对象
内部不保存 PA、frame、direct-map pointer 或 Rust slice。另一 CPU 可以在构造后改变映射，
所以实际 `read_into/write_from/fill_at` 必须重新取得 VM 锁并校验。

### 2. partial 与 exact 分层

流式传输遵循：首字节失败返回 errno，已有进度时返回完成前缀。文件 offset、PageCache
valid range 和 syscall 返回值只按实际 copy 字节数更新。固定格式结构使用
`UserBufferReader::read_exact()` 或 `UserBufferWriter::write_all()`，短 copy 转为 `EFAULT`。

该形状与 Linux 的 iterator/full helper 分层一致：

- [Linux `iov_iter` 实现](https://github.com/torvalds/linux/blob/master/lib/iov_iter.c)
- [Linux `uio` 接口](https://github.com/torvalds/linux/blob/master/include/linux/uio.h)

### 3. pipe nofault 例外

pipe ring 由 `spin::Mutex` 保护，不能在锁内进入可能分配、CoW 或等待文件缺页的 fault
handler。调用方先在锁外预 fault；锁内只使用 crate-private nofault helper 解析仍有效的 PTE。
若并发 remap 使映射或权限变化，则立即 `EFAULT` 或返回完成前缀。

Linux pipe 使用可睡眠 mutex 并在其临界区调用 copy helper；MangoCore 当前锁原语不同，不能
机械照搬其等待能力，但可借鉴“ring 状态与 copy 必须保持一致”的约束：

- [Linux pipe 实现](https://github.com/torvalds/linux/blob/master/fs/pipe.c)

### 4. resolve-first 避免无效 shootdown

uaccess 是软件主动复制，不是每次都发生了硬件 page fault。`fault_in_user_va()` 先解析已有
PTE；权限已经满足时直接返回 PA，只在缺页或写权限尚未建立时进入 handler。否则构造期预
fault 与实际 copy 的二次校验会反复误走 Cow/SharedWrite，产生无意义的 PTE 修改与 TLB
shootdown。

## 源码范围

- `os/src/mm/{address_space,uaccess,mod}.rs`：resolve-first、VA-backed range、逐页
  partial copy、exact wrapper、nofault 受限入口和旧导出删除。
- `os/src/fs/{page_cache,tmpfs/mod,vfs/file,dev/pipe,dev/zero,ext4_lwext4/layout}.rs`：仅为
  新 buffer contract 必须修改的调用点；按实际 copy 计数更新状态，tmpfs 使用锁外 bounce，
  pipe 在 ring 锁内只走 nofault。
- `os/src/net/socket/`、`os/src/net/syscall/`：仅迁移必要的连续/scatter buffer 调用点；
  TCP recv 在 socket 锁内写内核 buffer，解锁后 copy-to-user。
- `os/src/syscall/fs/`、`os/src/syscall/process/{bpf,keyring}.rs`：fixed ABI 使用 exact helper，
  readv/writev/preadv/pwritev/vmsplice 保留流式部分完成语义。
- 当前批次未修改 Driver，也未开展 FS/Net/Driver 的全面 SMP 并发审计。

## AI 协作与人工裁决

- DeepSeek `smp-b59-userbuffer-audit` 只读整理调用矩阵；其中部分读写方向和外层 trait 事实由
  GPT/Codex 直接复核源码后纠正。
- 第一项冻结构建任务在实现仍变化时失去源码指纹，只作为编译反馈，不记为 PASS 证据。
- `smp-b59-compile-r2` 与 `smp-b59-dual-build` 用于中间反馈；最终门禁重新编译最终源码。
- rustfmt 对历史大文件产生整文件格式噪声。处理方式是格式化 clean HEAD、反向应用纯格式
  patch，再把结果与完整格式化快照比较；零差异后才保留精简语义 diff。
- DeepSeek 最终报告对既有 `busybox kill 10` 与 LA64 `test_brk` 的具体根因没有日志证明；
  证据只记录它们与 B58 的精确失败集合一致，不采纳原因猜测。

所有 `cc-codex/` 任务、manifest 和原始日志只保留在本地忽略目录，不上传 GitHub。

## 最终验证

受测源码 tracked diff SHA-256：

```text
cd4e4520895a7292b715689e6585f2d968b456bac99390bbef5037a4b565f1b3
```

| 架构 | 配置 | 结果 | 用时 | 基线对比 |
|------|------|------|------|----------|
| RV64 | `CORE_NUM=8 mask=0x003` | 312/314，exit 0 | 约 350.1s | 与 B58 一致 |
| LA64 | `CORE_NUM=8 mask=0x003` | 308/314，exit 0 | 约 372.9s | 与 B58 一致 |

两个 recipe 都包含最终源码的 Docker 内核编译；均为 `mutation_detected=false`，无 forbidden
marker、panic、fatal 或 timeout。最终源码静态搜索确认生产树不再含：

```text
translated_byte_buffer
translate_user_buffer_checked
translate_single_page_user_bytes
UserBufferSegments
Vec<&'static mut [u8]>
```

## 未运行与边界

- 并发 fork/CoW/`mprotect`/`munmap` 与活跃 UserBuffer copy 的定向动态竞态：NOT RUN；
- `make lint`：NOT RUN；
- 未额外运行 focused ktest：preliminary 已覆盖 read/write/readv/writev、pipe、PageCache、
  xattr、sendmsg/recvmsg 等主要改动路径，按变更风险不重复追加同类门禁；
- B59 证明 uaccess helper 不再泄漏锁外物理页视图，不证明所有调用方锁序均正确；SysV IPC
  等 task/process registry 路径仍需后续审计；
- FS/Net/Driver 全面 SMP 审计由对应负责人完成，当前主线回到 MM、Task、HAL。
