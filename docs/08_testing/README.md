# MangoCore 分层测试体系

> 状态：设计阶段 → 最小可用实现中
> 最后更新：2026-07-10

## 1. 目标

建立一套从底层单元测试到内核自检、再到用户态回归测试和官方测试的多层 bug 扫描工具链。不再单纯依赖集成测试（LTP/lmbench）来调试，而是把 bug 定位逐步下沉到更底层、更快的测试层。

```
cargo test (L1/L2)
    → 判断纯逻辑模块是否正确
ktest / L3
    → 判断真实内核机制是否正确
user regression / L4
    → 判断用户态可见行为是否正确
LTP/lmbench/official / L5
    → 判断系统兼容性、性能和比赛表现
```

## 2. 测试分层

### L0：编译与静态检查

| 命令 | 说明 |
|------|------|
| `cargo check` | 类型检查 |
| `cargo fmt --check` | 格式检查 |
| `cargo clippy` | Lint 检查 |

入口：`make check-fast`

目标：秒级反馈，CI 第一道关卡。

### L1：纯逻辑单元测试

- 使用 `cargo test` 在 host 上运行
- 测试对象：bootargs parser、bitmap、id allocator、buddy 算法、ring buffer、path 解析、pagecache 状态转换、dentry tree 纯逻辑等
- 要求：低耦合、可在 host 上独立运行

#### 运行方式

```bash
# 从仓库根目录运行
cargo test -p mango-kernel-core

# 显示测试输出（打印 pass/fail 细节）
cargo test -p mango-kernel-core -- --nocapture

# 运行特定测试
cargo test -p mango-kernel-core test_cmdline_parse
```

#### 当前覆盖

| 模块 | 文件 | 测试数 | 说明 |
|------|------|--------|------|
| bootargs parser | `libs/mango-kernel-core/src/bootargs.rs` | 15 | BootConfig 解析、Cmdline get/get_list/get_usize/get_bool/has |

#### 如何添加更多 L1 测试

1. 在 `libs/mango-kernel-core/src/` 下创建新模块（如 `bitmap.rs`）
2. 在 `libs/mango-kernel-core/src/lib.rs` 中声明 `pub mod bitmap;`
3. 在模块底部添加 `#[cfg(test)] mod tests { ... }`
4. 运行 `cargo test -p mango-kernel-core` 验证
5. 将从 `os/src/` 移动的纯逻辑模块在 `lib.rs` 中 `pub mod` 导出
6. 在 `os/src/` 中通过 `use mango_kernel_core::xxx;` 或 `pub use` 引用

**规则：** 只有无 `os/` 内部依赖、不 `unsafe`、可在 host 上运行的纯逻辑才能放入 `mango-kernel-core`。

### L2：属性测试 / 模型测试（未来）

- proptest：pagecache、dentry cache 状态机性质测试
- loom：waitqueue、pipe wakeup 并发状态机测试
- 当前阶段仅设计接口，不强制引入依赖

### L3：内核态 self-test（当前优先）

- 内核内部运行，特殊 boot mode（`mango.mode=ktest`）
- 不启动普通用户态 init
- 测试对象：allocator、scheduler、timer、waitqueue、page table、VFS、pagecache 等
- 输出：TAP 格式
- 特性：timeout、repeat、failfast

### L4：用户态 regression test

- 每遇到一个 bug，沉淀最小用户态复现程序
- 目录：`user/src/bin/regression_*.rs`
- 入口：`make regression`

### L5：官方测试

- LTP、lmbench、iperf、libc-test、比赛测例
- 最终验收和性能趋势观察
- 如果 L5 发现 bug，尽量下沉成 L4 → L3 → L1

## 3. L3 内核测试框架

### 3.1 目录结构

```
os/src/
  config.rs              # BootConfig 结构体
  bootargs.rs            # key=value 解析器
  kernel_tests/
    mod.rs               # 注册所有测试，run_from_bootargs 入口
    runner.rs            # TAP 输出、timeout、repeat、failfast
    mm.rs                # alloc_free_one_page, alloc_many_pages
    sched.rs             # spawn_and_yield
    timer.rs             # sleep_returns
    waitqueue.rs         # wake_once, wake_all, wake_before_wait_should_not_sleep
    fs.rs                # tmpfs_create_write_read_unlink
    pagecache.rs         # basic_insert_lookup_evict
    block.rs             # read_first_block
```

### 3.2 测试项结构

```rust
pub struct KernelTest {
    pub name: &'static str,
    pub func: fn() -> Result<(), &'static str>,
    pub timeout_ms: usize,
}
```

### 3.3 Runner 职责

1. 根据 BootConfig.tests 选择测试组
2. 运行测试（支持 repeat）
3. 每个测试分配独立超时
4. 统计 passed/failed/skipped
5. 打印 TAP 格式日志
6. 支持 failfast（第一个失败即停）
7. 测试结束后调用 HAL `shutdown()`

### 3.4 TAP 输出格式

```
TAP version 13
1..5
ok 1 waitqueue::wake_once
ok 2 waitqueue::wake_all
not ok 3 waitqueue::wake_before_wait_should_not_sleep
  ---
  reason: timeout after 5000ms
  ...
ok 4 timer::sleep_returns
ok 5 sched::spawn_and_yield
```

### 3.5 启动流程

```
rust_main()
  → bootstrap_init()
  → mem_clear()
  → console::log_init()
  → trace::init()
  → mm::init()
  → machine_init()
  → timer_subsystem_init()
  → [NEW] BootConfig::load()
  → [NEW] if mode == Ktest: kernel_tests::run_from_bootargs() → shutdown()
  → [continue normal init...]
```

### 3.6 两阶段测试

由于部分测试（fs、pagecache、block）需要文件系统初始化后才能运行，L3 测试分两阶段：

- **Phase 1**（fs init 前）：waitqueue、timer、scheduler、mm 基础分配
- **Phase 2**（fs init 后）：tmpfs/pagecache/block 等需要 FS 的测试

等价的分组方式：
- `mango.test=basic` → Phase 1 测试
- `mango.test=fs` → Phase 2 测试
- `mango.test=all` → 全部

## 4. Bootargs 设计

### 4.1 格式

```
mango.mode=normal|ktest|regression
mango.test=all|waitqueue|sched|timer|mm|fs|pagecache|block|arch|basic
mango.test.repeat=100
mango.test.timeout_ms=5000
mango.test.failfast=1
mango.trace=waitqueue,sched,timer
mango.init=/bin/sh
mango.root=/dev/vda
```

解析规则：
- 空格分隔
- `key=value`
- 逗号列表
- 不要求引号/转义

### 4.2 当前 Workaround

由于内核尚不支持 DTB/EFI 读取 cmdline，使用**编译期常量**：

```rust
// 编译时通过环境变量注入
// MANGO_CMDLINE="mango.mode=ktest mango.test=waitqueue"
pub const CMDLINE: &str = env!("MANGO_CMDLINE");
```

Makefile 构造：
```makefile
MANGO_CMDLINE = "mango.mode=ktest mango.test=$(TEST) mango.test.repeat=$(REPEAT)"
cargo build --release --features "board_rvqemu ..."
```

后续真 bootargs 支持后，优先从 DTB/EFI 读取，编译期常量作为 fallback。

### 4.3 HAL/Arch 分层原则

- HAL/arch 层只负责提供**事实**：如何拿到 cmdline、shutdown、timer、console
- 通用内核层负责**策略**：解析 `mango.mode`、选择测试、控制行为
- 同一串 `mango.mode=ktest mango.test=waitqueue` 在 RV 和 LA 上语义一致

## 5. 用户态 Regression Test 规范

### 5.1 目录

```
user/src/bin/regression_pipe_lost_wakeup.rs
user/src/bin/regression_pipe_close_read_eof.rs
user/src/bin/regression_fork_fd_table.rs
user/src/bin/regression_tmpfs_unlink_open_file.rs
user/src/bin/regression_select_100fds.rs
```

### 5.2 文件格式

每个 regression 文件开头写注释：

```rust
//! Regression: LTP pipe13 hang
//! Bug: reader sleeps forever after writer wake
//! Expected: process exits within 1s
//! Related subsystem: pipe / waitqueue / scheduler
//! Fix commit: <commit hash>
```

### 5.3 入口

```bash
make regression   # 启动 MangoCore 正常模式，运行所有 regression_* 程序
```

## 6. Makefile 入口

```bash
# L0 - 静态检查
make check-fast

# L1/L2 - 单元测试
make unittest        # cargo test 可测试 crate

# L3 - 内核自检
make ktest TEST=waitqueue REPEAT=100 TIMEOUT_MS=5000
make rv64-ktest TEST=waitqueue TRACE=waitqueue,sched REPEAT=1000
make la64-ktest TEST=waitqueue TRACE=waitqueue,sched REPEAT=1000

# L4 - 用户态回归
make regression

# 复合入口
make bugscan         # check-fast + unittest + ktest + regression

# L5 - 官方测试
make official        # LTP + lmbench + iperf
```

## 7. 实现计划

### 第一阶段（最小闭环）

1. [x] 设计方案文档
2. [ ] 创建 `os/src/bootargs.rs` — bootargs 解析器
3. [ ] 创建 `os/src/config.rs` — BootConfig + 编译期常量
4. [ ] 创建 `os/src/kernel_tests/` 模块骨架
5. [ ] 实现 `runner.rs` — TAP 输出 + timeout + repeat
6. [ ] 实现 `waitqueue.rs` 测试 — wake_once, wake_all, wake_before_wait_should_not_sleep
7. [ ] 实现 `timer.rs` 测试 — sleep_returns
8. [ ] 实现 `sched.rs` 测试 — spawn_and_yield
9. [ ] 实现 `mm.rs` 测试 — alloc_free_one_page
10. [ ] 修改 `main.rs` — 插入 ktest 分支
11. [ ] 添加 `make rv64-ktest` Makefile 目标
12. [ ] QEMU 验证

### 第二阶段（完整化）

1. [ ] LA 架构 ktest 支持
2. [ ] `make la64-ktest` 目标
3. [ ] `make bugscan` 复合入口
4. [ ] 更多 L3 测试（fs, pagecache, block）
5. [ ] trace group 接入 bootargs

### 第三阶段（L4 回归）

1. [ ] `user/src/bin/regression_*` 框架
2. [ ] `make regression` 目标
3. [ ] 沉淀 3+ 历史 bug 的 regression

## 8. 参考

- Tock OS: in-kernel test vs cargo test 分层
- Rust-for-Linux: KUnit 集成模式
- Theseus OS: test application crate 组织方式
- phil-opp: no_std 自定义 test runner
- zCore/rCore: 测试命令统一入口 + rootfs 组织
- DragonOS: Rust 内核工程结构参考
