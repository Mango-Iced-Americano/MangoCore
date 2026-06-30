---
title: "代码注释规范"
category: overview
status: draft
author: MangoCore Team
last_update: 2026-06-30
tags: [docs, comment-style, code-standards, review]
---
# 代码注释规范

## 概述

本文档定义 MangoCore 内核代码的注释规范。目标是让代码注释**可信、必要、可维护**，与 MangoCore 已有的外部文档体系保持一致。

### 基本原则

注释不是为了复述代码，而是为了解释代码本身看不出来的信息。

Rust 的类型系统和所有权模型已经表达了许多约束。注释应当覆盖类型系统无法表达的部分：设计意图、同步语义、安全契约、兼容性差异、已知限制。

### 参考来源

- Linux kernel `Documentation/process/coding-style.rst`、`Documentation/doc-guide/kernel-doc.rst`
- Rust API Guidelines（docs、errors、panics、safety 要求）
- DragonOS 各模块的注释风格
- OSComp 历年优秀内核作品的文档组织方式

---

## 注释分类

MangoCore 中的注释分为五类：

| 类别       | 标记                                     | 使用场景                                   |
| ---------- | ---------------------------------------- | ------------------------------------------ |
| 模块级文档 | `//!`                                  | 模块职责、架构、入口点                     |
| 公开项文档 | `///`                                  | `pub` 项：trait、struct、function、field |
| 行内说明   | `//`                                   | 设计决策、约束、代码块边界                 |
| 临时标记   | `// TODO` / `// FIXME` / `// HACK` | 已知限制、待办项、绕过                     |
| 废弃声明   | `#[deprecated]` + `/// # Deprecated` | 已废弃的 API 说明                          |

---

## 使用规则

### `//!` — 模块级文档

用于 `mod.rs` 或模块入口文件开头，说明模块的整体职责。

应包含：

- 模块的整体职责，一句话概括
- 与 Linux/DragonOS 对应模块的关系（如有）
- 模块的已知限制或未覆盖功能（如有）

```rust
//! 物理页帧分配器。
//!
//! 提供 4KB 页帧的分配/释放。支持栈式分配器（默认）和紧急预留帧。
//! 分配器单例由 `FRAME_ALLOCATOR` 管理，初始化在 `mm::init()` 中完成。
//!
//! # Locking
//!
//! `FRAME_ALLOCATOR` 使用内部 `Mutex` 同步，调用者无需额外持锁。
//! 分配路径不能持有任何其他锁，以避免锁顺序反转。
```

### `///` — 公开项文档

用于所有 `pub` 和 `pub(crate)` 项。建议包含以下标准小节（按需选择）。

**必选规则：**

- `pub fn`：至少一句话说明函数语义
- `pub unsafe fn`：必须包含 `# Safety` 小节
- 返回 `Result` 或通过 isize 编码错误：建议包含 `# Errors`
- syscall 实现或 Linux 兼容接口：建议包含 `# Linux Compatibility`
- 函数内部获取锁、要求调用者已持锁、可能阻塞：建议包含 `# Locking`

**标准小节（按推荐顺序）：**

| 小节                      | 内容                              |
| ------------------------- | --------------------------------- |
| `# Semantics`           | 整体行为、参数含义、返回值语义    |
| `# Errors`              | 错误变体及其触发条件              |
| `# Safety`              | 调用方必须保证的安全条件          |
| `# Locking`             | 锁顺序、阻塞路径、调用者持锁要求  |
| `# Linux Compatibility` | flag 支持范围、行为差异、简化实现 |
| `# Limitations`         | 已知限制、功能子集                |
| `# Deprecated`          | 废弃原因、替代方案、删除条件      |

```rust
/// 等待指定条件满足，期间允许被可处理信号中断。
///
/// # Semantics
///
/// 当 `condition` 返回 `Some(value)` 时立即返回该值；当条件暂不满足时，
/// 当前任务加入等待队列并让出 CPU。如果 `deadline` 到达时条件仍未满足，
/// 返回 `WaitResult::TimedOut`。
///
/// # Locking
///
/// 调用者不得在持有 inode 内部锁、PageCache entries 锁或 scheduler 全局锁时
/// 调用该函数。条件闭包内部不得再次获取同一个 `WaitQueue` 的锁。
///
/// # Errors
///
/// - `-ERESTART`：等待期间被可处理信号中断。
/// - `-EAGAIN`：非阻塞模式下数据暂不可用。
```

### `//` — 行内注释

用于以下场景：

- **设计理由**：解释"为什么这样做"而非"做了什么"
- **代码块边界**：如 `// ── Page fault handler ──`、`// ── WaitQueue ──`
- **复杂操作**：位运算、协议字段偏移、内存布局假设
- **bug 修复标注**：在修复点留下根因说明

行内注释不应用于：

- 复述显而易见的代码逻辑
- 标准命名方法的冗余说明（如 `// 构造函数` 对 `new()`）

---

## Safety 注释

`unsafe` 代码必须在注释中说明为什么在此处是安全的。

### `unsafe fn`

每个 `unsafe fn` 的 rustdoc 必须包含 `# Safety` 小节，列举调用方必须保证的安全条件：

```rust
/// 从给定物理地址读取 `T` 类型的值。
///
/// # Safety
///
/// - `pa` 必须指向一个合法的、已映射的物理地址。
/// - `pa` 对应的物理页必须在 `size_of::<T>()` 字节范围内可读。
/// - 调用方必须保证该物理地址在当前上下文中具有定义良好的行为
///   （非 MMIO 保留区域、非已被回收的帧）。
pub unsafe fn read_from_phys<T: Copy>(pa: PhysAddr) -> T { ... }
```

### `unsafe` 块

每个 `unsafe` 块必须伴随行内注释，说明在此处安全的具体理由：

```rust
// Safety: 调用方在 `veth_pair_delete` 入口处保证 `iface` 的实际类型是
// `VethInterface`（`iface.kind() == DeviceKind::Veth`）。
// 此转换仅在当前函数内使用，不会泄露到外部。
let veth_iface: &VethInterface =
    unsafe { &*(Arc::as_ptr(&iface) as *const VethInterface) };
```

### 用户指针访问

使用 `translated_ref`、`copy_from_user`、`translated_byte_buffer` 等函数时，如果操作涉及跨页访问或不安全假设，应注释说明边界检查的方式。这些函数本身已包含安全检查（`check_user_range`、缺页处理），调用方仅在打破常规使用模式时需要注释。

### 内联汇编

每个 `asm!` 块应注释说明操作数约束、内存副作用、架构特定行为（如 TLB 刷新）。

---

## Locking 注释

### WaitQueue

使用 `WaitQueue` 的地方应说明：

- 谁唤醒、谁等待
- 条件闭包的锁持有范围
- 兜底定时器是否启用及其作用

```rust
/// `WAIT_IO_FALLBACK_MS` 防止因丢失唤醒导致的永久阻塞。
/// 此定时器是防卫性措施，不应依赖它作为正常唤醒机制。
const WAIT_IO_FALLBACK_MS: usize = 10;
```

### 锁顺序

当函数需要获取多把锁时，应注释说明获取顺序。违反锁顺序可能导致死锁。

```rust
// 锁顺序：entries.lock → inner.lock
// 逆向获取会导致死锁（issue #104）。
let mut entries = self.entries.lock();
// ...
let mut inner = self.inner.lock();
```

### 信号检查与持锁

信号检查必须在释放 `task.inner` 锁后调用，应在注释中标明：

```rust
// 必须 drop(task_inner) 后再检查信号。持有 task 锁时调用
// has_actionable_signal 可能通过 signal handler 路径导致锁顺序反转。
drop(task_inner);
if has_actionable_signal(task) {
    return -(SyscallErr::ERESTART as isize);
}
```

### 非重入锁

使用 `spin::Mutex`（非重入）时，应注释哪些函数不能在同一路径中嵌套调用。

```rust
// `TicketMutex` 不可重入。PageCache invalidate 时不能持有 inode 锁，
// 因为 invalidate 路径可能通过回调间接获取同一把 inode 锁。
drop(inode_lock);
page_cache.invalidate(...);
```

---

## Linux Compatibility 注释

每个 syscall 处理函数或 Linux 兼容接口的实现中，需标注与 Linux 的行为差异。

```rust
// Linux 6.6 在 `O_DIRECTORY` 未设置时返回 `EISDIR`，我们同样处理。
if flags & !O_DIRECTORY == 0 && inode_is_dir {
    return Err(SyscallErr::EISDIR);
}
```

在 rustdoc 中应覆盖以下内容：

| 内容        | 格式示例                                                   |
| ----------- | ---------------------------------------------------------- |
| 支持的 flag | `仅支持 `MAP_SHARED`、`MAP_PRIVATE`、`MAP_ANONYMOUS` |
| errno 语义  | `fd 不可读时返回 `-EBADF``                               |
| 行为差异    | `与 Linux 不同：当 `addr`为`NULL` 时自动选择地址`    |
| 简化实现    | `当前简化：不支持 `MREMAP_DONTUNMAP``                    |

---

## TODO / FIXME / HACK 注释

### TODO

```rust
// TODO(<scope>): <具体行动>. Exit condition: <可验证条件>
```

- `scope`：归属主题，如 `waitqueue-cleanup`、`linux-compat`、`pagecache-reclaim`
- `action`：具体要做的事
- `Exit condition`：满足此条件后可以删除该 TODO

### FIXME

`FIXME` 仅用于已知的 correctness bug。

```rust
// FIXME(<scope>): <bug 描述、根因、触发条件>.
// Risk: <不修复的影响范围>.
```

### HACK / workaround

```rust
// HACK(<scope>): <为什么需要绕过>.
// Reference: <关联的 Linux/测试行为>.
// Remove when: <可以移除的条件>.
```

HACK 必须说明参考的外部行为（Linux 版本、测试用例名称、上游 commit）和移除条件。

### 反例

```rust
// TODO: fix later          // 无 scope、无退出条件
// FIXME: buggy             // 无风险描述
// TODO: 实现真正的随机数生成 // scope 不明确
```

### 正例

```rust
// TODO(waitqueue-cleanup): 将 `wait_io_core` 的 yield 轮询替换为 `WaitQueue` 注册。
// Exit condition: 所有遗留调用方已迁移到 `WaitQueue` API。

// HACK(virtio-blk): QEMU virtio-blk 后端要求 512 字节对齐。
// Reference: QEMU virtio-blk 处理非对齐访问时静默返回错误。
// Remove when: QEMU 修复或切换到 virtio-blk-v2。
```

---

## Deprecated 注释

### 格式

```rust
/// # Deprecated
///
/// <废弃原因，非技术债务的具体说明>
///
/// **替代方案：** <新代码应使用的 API 路径>
///
/// **保留原因：** <当前为什么还保留>
///
/// **删除条件：** <什么时候可以删除>
#[allow(deprecated)]
pub fn old_function(...) -> ... { ... }
```

### 示例

```rust
/// # Deprecated
///
/// 使用无条件 yield 轮询等待 I/O 就绪，不注册到具体的 `WaitQueue`。
/// 这可能隐藏丢失唤醒的 bug，并增加不必要的调度开销。
///
/// **替代方案：** `WaitQueue::wait_until_interruptible`
///
/// **保留原因：** pipe、tty、socket 的部分调用者尚未迁移。
///
/// **删除条件：** 所有对 `wait_io` 的引用已移除。
```

---

## 语言规范

### 规则

| 内容                                    | 语言                         | 格式                                |
| --------------------------------------- | ---------------------------- | ----------------------------------- |
| 叙述性描述、设计解释                    | 中文                         | 普通中文段落                        |
| 代码标识符（函数、变量、trait、struct） | 英文                         | `` `反引号` ``                      |
| Linux flag / errno / syscall            | 英文                         | `` `-EAGAIN` ``、`` `MAP_SHARED` `` |
| 文件路径                                | 英文                         | `` `os/src/mm/uaccess.rs` ``        |
| 底层 unsafe、ABI、memory ordering 注释  | 中英皆可，同一文件内保持一致 |                                     |

### 示例

推荐：

```rust
/// 调用 `sys_read` 时，如果底层文件返回 `-EAGAIN` 且 fd 处于阻塞模式，
/// 当前任务必须进入对应对象的 `WaitQueue`，不能使用无条件 yield 轮询。
```

不推荐：

```rust
/// 调用 sys_read 来读取数据，如果 EAGAIN 就等一下
```

### 禁止使用的表达

| 表达式                     | 问题                 |
| -------------------------- | -------------------- |
| "暂时"、"先这样"、"应急用" | 无删除条件，无法审计 |
| "有点问题"、"有一定问题"   | 未说明风险和场景     |
| "之后再修"、"回头再改"     | 无时间点无责任人     |
| "就是"、"说白了"           | 口语化，不够严谨     |

---

## 应避免的注释模式

### 低信息注释

以下注释应删除或省略，因为它们没有提供代码本身看不出的信息：

```rust
/// 构造函数
pub fn new() -> Self { ... }

/// 判断等待队列是否为空
pub fn is_empty(&self) -> bool { ... }
```

**例外：** 如果方法名称不是 Rust 标准命名惯例，仍需要注释。如 `compact_stale`、`block_and_ret_mut` 等非标准方法名需要解释。

### 复述代码

```rust
// 将 a 加 1
a += 1;
```

### 口语化注释

```rust
// 注意这个函数本质是轮询，有一定问题，等其他地方都修好应该弃用
```

应改写成标准 TODO 或 Deprecated 格式。

### 与实现不一致的注释

注释内容必须与当前代码一致。修改实现后应同步更新相关注释。

---

## 审查清单

PR 审查时，注释相关的检查项如下：

### 完整性与必要性

- [ ] 新增公开 trait / struct / function 是否包含必要的 rustdoc
- [ ] unsafe / raw pointer / uaccess 是否包含 Safety 说明
- [ ] 可能阻塞或唤醒的路径是否说明 WaitQueue / signal 语义
- [ ] 持锁调用是否说明 Locking 约束（锁顺序、非重入限制）
- [ ] Linux 兼容 syscall 是否说明 flag 支持范围和 errno 语义
- [ ] 模块入口是否包含 `//!` 模块文档

### 格式与质量

- [ ] TODO / FIXME / HACK 包含 scope、原因和退出条件
- [ ] 没有口语化、临时性的注释（"暂时""应急""有点问题"等）
- [ ] 没有单纯复述代码的低信息注释
- [ ] 没有与当前实现矛盾的历史注释
- [ ] Deprecated API 说明替代方案和删除条件
- [ ] 注释中引用的文件路径、函数名、errno 真实存在

---

## 附录：现有注释的清理建议

本附录简要说明如何处理仓库中已有的不合规注释，供阶段性清理参考。

### 处理方式

1. **可删除的**：已过期的 TODO、低信息 getter/setter 注释、复述代码的注释 → 直接删除
2. **需改写的**：口语化注释、无退出条件的 TODO → 按本标准格式改写
3. **需补充的**：缺少 Safety/Locking/Linux Compatibility 说明的关键函数 → 按本标准补充
4. **需跟踪的**：短期内无法清理的 TODO → 在 `docs/Work_Log.md` 或 issue 中登记跟踪


