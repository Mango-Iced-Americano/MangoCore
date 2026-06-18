# ext4 目录项 / initproc / 符号链接 修复计划

## TL;DR

> **核心目标**：修复 `/bin/bash` exec 失败 + 符号链接创建触发的磁盘写放大
>
> **交付物**：
> - 目录块 dump 调试工具（定位 `/bin/bash` 根因）
> - initproc `run_group_once()` 添加 `/bash` 回退逻辑
> - 消除 `try_insert_to_existing_block()` 双写
> - fast symlink 写入 + 读取支持（≤60 字节目标存 `i_block`）
>
> **预估工作量**：Medium
> **并行执行**：YES — 2 波（Wave 1: 4 任务并行，Wave 2: 2 任务并行）
> **关键路径**：Task 1 → Task 5/6 → Task 7（集成测试）

---

## Context

### 原始问题

1. **`/bin/bash` exec 失败**：`execve("/bin/bash")` 时 `dir_find_entry` 在 inode=3217（`/bin` 目录）中找不到 `bash`，但 debugfs 确认 `bash` 已落盘。`dir_find_in_block()` 遇到 `entry_len < 8` 会 `break`，目录项链一旦损坏，后面的条目全部不可见。
2. **磁盘写放大**：`symlinkat("/busybox", "/bin/mkswap")` 创建 8 字节软链接时，触发 `size=0 → 4096 → 8` 的 inode 反复写回（`sync_inode_to_disk`）。目录块存在双写：`try_insert_to_existing_block()` 和 `dir_add_entry()` 各调一次 `sync_blk_to_disk()`。

### 代码库当前状态（4 个 explore 代理确认）

| 关注点 | 文件:行号 | 现状 |
|--------|-----------|------|
| `dir_find_in_block()` | `direntry.rs:368-397` | `entry_len < 8` 时直接 break，无 dump |
| `try_insert_to_existing_block()` | `direntry.rs:603-676` | **内部有** `sync_blk_to_disk()` — 双写确认 |
| `dir_add_entry()` | `direntry.rs:511-592` | 成功后也调 `sync_blk_to_disk()` |
| `symlink()` | `ext4fs.rs:573-593` | 始终走 `write_at()`，不利用 fast symlink |
| `create_inode()` | `file.rs:291-330` | **强制设** `EXT4_INODE_FLAG_EXTENTS` — 阻止 fast symlink |
| `get_pblock_idx()` | `ext4_inode.rs:679-725` | 有 fast symlink 检测（`!extents && is_link && size≤60`）但仅返回 ENOENT |
| `run_group_once()` | `initproc.rs:~589` | **无** `/bash` 回退，仅 `exec("/bin/bash", ...)` |
| `run_bash_cmd()` | `initproc.rs:85-120` | **有** `/bin/bash → /bash` 回退 |

### 关键发现

- **Fast symlink 读取有 BUG**：`read_at()`（file.rs:385）在 `get_pblock_idx()` 返回 `Err(ENOENT)` 时直接填零，不会从 `i_block` 中提取快速符号链接目标。磁盘镜像中由 Linux 工具创建的 fast symlink 会读出空字符串。
- **双写确认**：`try_insert_to_existing_block()` 在修改目录项后立即 `sync_blk_to_disk()`，`dir_add_entry()` 随后算完 checksum 再写一次——同一个块短期内被写两次，第二次才带正确校验和。

---

## Work Objectives

### Core Objective

定位并修复 `/bin/bash` 查找失败的根因（目录项链损坏 vs 条目未创建），消除目录块双写，实现 fast symlink 支持以减少 I/O。

### Concrete Deliverables

- `os/src/fs/ext4/direntry.rs` — 添加 `debug_dump_dir_block()` + 在 `dir_find_entry` 失败路径调用
- `user/src/bin/initproc.rs` — `run_group_once()` 添加 `/bash` 回退 + 自检
- `os/src/fs/ext4/direntry.rs` — 删除 `try_insert_to_existing_block()` 内部 `sync_blk_to_disk()`
- `os/src/fs/ext4/ext4fs.rs` — fast symlink 写入（target ≤ 60 字节存 `i_block`）
- `os/src/fs/ext4/file.rs` — fast symlink 读取（从 `i_block` 提取目标）
- `os/src/fs/ext4/ext4_inode.rs` — `Ext4Inode` 添加 `block_as_bytes()` / `block_mut_as_bytes()` helper

### Definition of Done

- [ ] `make rv64-kernel-build-only` ✅
- [ ] `make la64-kernel-build-only` ✅
- [ ] QEMU 启动不 panic
- [ ] `/bin/bash` exec 后 `dir_find_entry` 失败日志包含目录块 dump（可定位根因）
- [ ] `run_group_once()` 在 `/bin/bash` 不可用时自动回退 `/bash`
- [ ] 创建短符号链接（≤60 字节）不再分配数据块（size 直接 = target.len()）
- [ ] 从磁盘镜像读取 fast symlink 返回正确目标字符串

### Must Have

- 目录块 dump 仅在 `dir_find_entry` 失败时触发，不对正常路径引入开销
- `try_insert_to_existing_block()` 只改内存，落盘统一由 `dir_add_entry()` 负责
- Fast symlink 写入不改动 extent tree 路径（`create_inode` 保持原样），仅针对 symlink 类型特殊处理
- initproc 改动不影响已有 `run_bash_cmd()` 和 `enter_shell()` 的回退逻辑

### Must NOT Have (Guardrails)

- 不要重构目录块迭代逻辑（只加 dump，不改 `entry_len < 8` break 语义）
- 不要改动 `create_inode()` 的通用 `EXT4_INODE_FLAG_EXTENTS` 逻辑
- 不要加 inode table block 缓存（留到后续优化）
- 不要改动 FAT32 或 VFS 通用层
- 不要添加 `cargo test` / `cargo clippy`（裸机内核不支持）

---

## Verification Strategy

### Test Decision

- **测试基础设施**：无 `cargo test`/`cargo clippy`，唯一的验证 = 编译 + QEMU 集成测试
- **自动化测试**：None（裸机内核）
- **Agent-Executed QA**：每个任务通过编译 + QEMU 日志验证

### QA Policy

每个任务包含 Agent-Executed QA Scenarios。证据保存到 `.sisyphus/evidence/task-{N}-{scenario-slug}.txt`。

- **编译**：`kernel-dev_kernel_build` 工具验证双架构编译
- **QEMU 日志**：`kernel-dev_kernel_run` 工具启动 QEMU 并捕获日志
- **手动检查**：分析日志中的关键标记行

---

## Execution Strategy

### Parallel Execution Waves

```
Wave 1 (Start Immediately — 4 个独立任务，零依赖):
├── Task 1: dir_find_entry 失败时 dump /bin 目录块 [quick]
├── Task 2: initproc run_group_once() 添加 /bash 回退 [quick]
├── Task 3: prepare_symlink() 后添加 /bin/bash 自检 [quick]
└── Task 4: 删除 try_insert_to_existing_block() 内部 sync_blk_to_disk() [quick]

Wave 2 (After Wave 1 — fast symlink 写 + 读):
├── Task 5: Fast symlink 写入 [quick] (depends: 无，但建议等 Wave 1 完成)
└── Task 6: Fast symlink 读取 [quick] (depends: 5 — 需写入端先确认接口)

Wave FINAL (After ALL tasks):
├── Task 7: QEMU 集成验证 — 编译 + 启动 + 日志确认 [quick]
└── F1-F4: 最终审查（如需要）
```

### Dependency Matrix

- **Task 1-4**: 无依赖，可并行
- **Task 5**: 无强依赖，建议 Wave 1 完成后开始
- **Task 6**: Task 5（依赖写入端提供的 `block_mut_as_bytes()` helper 或接口模式）
- **Task 7**: Task 1-6 全部完成

---

## TODOs

### Wave 1 — 调试 + 快速修复（4 任务并行）

- [x] 1. **dir_find_entry 失败时 dump 目录块**

  **What to do**:
  1. 在 `os/src/fs/ext4/direntry.rs` 中新增 `debug_dump_dir_block()` 辅助函数，遍历目录块中所有条目：
     - 循环 `offset < block_size - sizeof(Ext4DirEntryTail)`
     - 从 `block.data[offset..]` 构造 `Ext4DirEntry::try_from()`
     - 打印 `offset, inode, rec_len, name_len, file_type, name`
     - 遇到 `entry_len < 8` 打印 `BAD ENTRY` 并 break
     - 遇 `entry_len > block_size - offset` 打印越界并 break
  2. 在 `dir_find_entry()` 的失败路径（`total_blocks` 循环结束后 `return Err(ENOENT)` 之前）调用 dump：
     - 对每个逻辑块调用 `get_pblock_idx()` → `Block::load_offset()` → `debug_dump_dir_block()`
     - 仅在 `parent_inode` 对应的目录是 `/bin`（inode 3217）或 `name == "bash"` 时触发，避免日志洪水
  3. 保持 `dir_find_in_block()` 和 `dir_find_entry()` 现有逻辑不变

  **Must NOT do**:
  - 不要修改 `entry_len < 8` break 语义
  - 不要对所有目录触发 dump（仅在 `/bin` / target=bash 场景）
  - 不要在 dump 中持有额外的锁

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: `[]`
  - **Reason**: 单文件、纯日志、增量添加，不涉及跨模块逻辑变更

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 2, 3, 4)
  - **Blocks**: None
  - **Blocked By**: None

  **References**:
  - `os/src/fs/ext4/direntry.rs:303-358` — `dir_find_entry()` 完整实现，需在此失败路径加 dump 调用
  - `os/src/fs/ext4/direntry.rs:368-397` — `dir_find_in_block()` 完整实现，dump 函数需复刻其迭代模式（`offset += entry_len`，`entry_len < 8` break，`offset < block_size - sizeof(Ext4DirEntryTail)`）
  - `os/src/fs/ext4/direntry.rs:57-65` — `Ext4DirEntryTail` 结构体，`sizeof(Ext4DirEntryTail) = 12`
  - `os/src/fs/ext4/direntry.rs:29-38` — `Ext4DirEntry` 结构体，`try_from()` 方法，`entry_len()`, `name_len()`, `inode()`, `file_type()` 方法
  - `os/src/fs/ext4/direntry.rs:491-499` — `dir_set_csum()` 中日志风格参考 `log::warn!()`

  **Acceptance Criteria**:
  - [ ] `make rv64-kernel-build-only` 编译通过
  - [ ] `make la64-kernel-build-only` 编译通过

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: dir_find_entry fails for /bin/bash → dump triggered
    Tool: kernel-dev_kernel_run (arch=rv64, log=info, timeout=120)
    Preconditions: QEMU 镜像中 /bin 目录可能损坏（bash 新创建时尤其）
    Steps:
      1. 启动 QEMU 并等待 initproc 执行 prepare_symlink() + run_group_in_dir()
      2. 观察 QEMU 日志，查找关键字 `[dir_dump]`
      3. 如果 /bin/bash exec 失败，确认日志中出现 `[dir_dump] parent=3217` 行
      4. 确认 dump 输出包含每个目录项的 offset/ino/rec_len/name_len/name
      5. 确认遇到损坏条目时有 `[dir_dump] BAD ENTRY` 行
    Expected Result: 当 dir_find_entry 对 inode=3217 失败时，日志包含完整目录块内容
    Failure Indicators: /bin/bash exec 失败但无 [dir_dump] 日志；dump 日志缺失
    Evidence: .sisyphus/evidence/task-1-dir-dump.log

  Scenario: Normal directory lookup (no dump triggered)
    Tool: kernel-dev_kernel_run (arch=rv64, log=info, timeout=120)
    Preconditions: 其他目录查找成功（如 / 目录、/musl 等）
    Steps:
      1. 启动 QEMU 并捕获完整日志
      2. 确认非 /bin 目录查找失败时没有 [dir_dump] 日志
      3. 确认正常成功的目录查找没有引入额外日志开销
    Expected Result: 仅 /bin 或 target=bash 场景触发 dump
    Evidence: .sisyphus/evidence/task-1-no-flood.log
  ```

  **Commit**: YES
  - Message: `fix(ext4): add directory block dump on dir_find_entry failure for /bin`
  - Files: `os/src/fs/ext4/direntry.rs`

- [x] 2. **initproc: run_group_once() 添加 /bash 回退**

  **What to do**:
  1. 在 `user/src/bin/initproc.rs` 的 `run_group_once()` 函数子进程部分，将现在的：
     ```rust
     exec("/bin/bash\0", &["/bin/bash\0", dash_c.as_ptr(), cmd.as_ptr(), null], environ);
     println!("...");
     exit(127);
     ```
     改为两层 try：
     ```rust
     exec("/bin/bash\0", &argv, environ);
     println!("[initproc] /bin/bash failed for {} in {}, fallback /bash", script, log_dir);
     exec("/bash\0", &argv_fallback, environ);
     println!("[initproc] exec failed for {} in {} via both /bin/bash and /bash", script, log_dir);
     exit(127);
     ```
  2. `argv` 和 `argv_fallback` 分别用 `"/bin/bash\0"` 和 `"/bash\0"` 作为 `argv[0]`
  3. 参考 `run_bash_cmd()` 的闭包模式动态构造 argv，保持参数一致（`-c` + cmd）

  **Must NOT do**:
  - 不要改动 `run_bash_cmd()` 的现有回退逻辑
  - 不要改动 `enter_shell()` 的回退逻辑
  - 不要改动 `run_group_once()` 中父进程的超时控制/重试逻辑

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: `[]`
  - **Reason**: 单文件、用户态代码、纯字符串操作，无内核逻辑变更

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 1, 3, 4)
  - **Blocks**: None
  - **Blocked By**: None

  **References**:
  - `user/src/bin/initproc.rs:85-120` — `run_bash_cmd()` 完整实现（闭包 `|shell| -> [*const u8; 4]` 动态构造 argv 的模式，第 96-103 行）
  - `user/src/bin/initproc.rs:~589` — `run_group_once()` 子进程中当前的单次 exec 调用（需定位确切行号）
  - `user/src/bin/initproc.rs:462-477` — `enter_shell()` 中的回退模式参考
  - `user/src/usr_call.rs:40-42` — `exec()` 包装函数签名：`fn exec(path: &str, args: &[*const u8], envp: &[*const u8]) -> isize`

  **Acceptance Criteria**:
  - [ ] `make rv64-kernel-build-only` 编译通过（initproc 作为用户程序编译）
  - [ ] `make la64-kernel-build-only` 编译通过

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: /bin/bash exists → runs normally
    Tool: kernel-dev_kernel_run (arch=rv64, log=info, timeout=120)
    Preconditions: busybox --install -s /bin 成功，/bin/bash 为有效可执行文件
    Steps:
      1. 启动 QEMU
      2. 观察 QEMU 日志，确认 run_group_in_dir 成功执行测试脚本
      3. 确认日志中没有 "fallback /bash" 字样（说明 /bin/bash 正常）
    Expected Result: 测试组以 /bin/bash 执行，无回退
    Evidence: .sisyphus/evidence/task-2-normal.log

  Scenario: /bin/bash missing → falls back to /bash
    Tool: kernel-dev_kernel_run (arch=rv64, log=info, timeout=120)
    Preconditions: /bin/bash 缺失或不可执行（可由 Task 1 修复前自然触发）
    Steps:
      1. 启动 QEMU
      2. 观察 QEMU 日志，确认出现 "[initproc] /bin/bash failed for ... fallback /bash"
      3. 确认 /bash 回退执行成功，没有 "exec failed ... via both" 消息
    Expected Result: /bin/bash 缺失时自动回退 /bash，测试继续
    Evidence: .sisyphus/evidence/task-2-fallback.log
  ```

  **Commit**: YES
  - Message: `fix(initproc): add /bash fallback in run_group_once`
  - Files: `user/src/bin/initproc.rs`

- [x] 3. **initproc: prepare_symlink() 后添加 /bin/bash 自检**

  **What to do**:
  1. 在 `user/src/bin/initproc.rs` 的 `prepare_symlink()` 函数末尾（或 `main()` 中 `prepare_symlink()` 调用后）添加自检：
     ```rust
     let r = run_bash_cmd("test -x /bin/bash && echo BIN_BASH_OK || echo BIN_BASH_BAD\0", environ);
     println!("[initproc] post-prepare /bin/bash check exit={}", r);
     ```
  2. 使用已存在的 `run_bash_cmd()` 函数（它自带 `/bash` 回退，所以即使 `/bin/bash` 不可用也能执行 `test` 命令）

  **Must NOT do**:
  - 不要在自检失败时中断启动流程（仅日志记录）
  - 不要改动 `prepare_symlink()` 的现有 symlink 创建逻辑

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: `[]`
  - **Reason**: 单行调用，无新逻辑

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 1, 2, 4)
  - **Blocks**: None
  - **Blocked By**: None

  **References**:
  - `user/src/bin/initproc.rs:1342-1374` — `prepare_symlink()` 完整实现
  - `user/src/bin/initproc.rs:1397` — `main()` 中 `prepare_symlink()` 调用位置
  - `user/src/bin/initproc.rs:85-120` — `run_bash_cmd()` 函数签名和用法

  **Acceptance Criteria**:
  - [ ] `make rv64-kernel-build-only` 编译通过
  - [ ] `make la64-kernel-build-only` 编译通过

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: /bin/bash exists after prepare_symlink
    Tool: kernel-dev_kernel_run (arch=rv64, log=info, timeout=120)
    Preconditions: busybox --install -s /bin 成功
    Steps:
      1. 启动 QEMU
      2. 等待 prepare_symlink 完成
      3. 在 QEMU 日志中查找 "[initproc] post-prepare /bin/bash check exit=0"
      4. 确认日志包含 "BIN_BASH_OK"
    Expected Result: exit=0 且输出 BIN_BASH_OK
    Evidence: .sisyphus/evidence/task-3-ok.log

  Scenario: /bin/bash still missing (should not happen after fix, but self-check reveals it)
    Tool: kernel-dev_kernel_run (arch=rv64, log=info, timeout=120)
    Preconditions: /bin/bash 缺失
    Steps:
      1. 启动 QEMU
      2. 在 QEMU 日志中查找 "[initproc] post-prepare /bin/bash check exit=1"
      3. 确认日志包含 "BIN_BASH_BAD"
    Expected Result: exit=1 且输出 BIN_BASH_BAD（帮助快速定位问题）
    Evidence: .sisyphus/evidence/task-3-bad.log
  ```

  **Commit**: YES
  - Message: `debug(initproc): add /bin/bash self-check after prepare_symlink`
  - Files: `user/src/bin/initproc.rs`

- [x] 4. **删除 try_insert_to_existing_block() 内部 sync_blk_to_disk()**

  **What to do**:
  1. 在 `os/src/fs/ext4/direntry.rs` 的 `try_insert_to_existing_block()` 函数中：
     - 找到所有 `block.sync_blk_to_disk(self.block_device.clone())` 调用
     - 删除它们（仅保留内存操作 `copy_to_slice`）
  2. 确认调用方 `dir_add_entry()` 已经负责 `dir_set_csum()` + `ext4block.sync_blk_to_disk()`：
     - 现有代码：`try_insert_to_existing_block()` 返回 Ok → `dir_set_csum()` → `sync_blk_to_disk()`
     - 改动后：`try_insert_to_existing_block()` 只改内存，checksum 和落盘完全由 `dir_add_entry()` 负责
  3. 同时检查 `try_insert_to_existing_block()` 中是否还有其他直接 sync 调用（如情况 A 的空闲项插入路径）

  **Must NOT do**:
  - 不要改动 `dir_set_csum()` 的调用位置
  - 不要改动 `insert_to_new_block()` 的逻辑
  - 不要改动 `dir_remove_entry()` 的 sync 调用

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: `[]`
  - **Reason**: 单文件、删调用行，风险集中在确保调用方有正确的 sync 调用

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 1, 2, 3)
  - **Blocks**: None
  - **Blocked By**: None

  **References**:
  - `os/src/fs/ext4/direntry.rs:603-676` — `try_insert_to_existing_block()` 完整实现，定位 sync 调用（情况 A 空闲项、情况 B 已有项后插入）
  - `os/src/fs/ext4/direntry.rs:511-592` — `dir_add_entry()` 完整实现，确认 `try_insert_to_existing_block()` 成功后的 sync 路径：`dir_set_csum()` → `ext4block.sync_blk_to_disk()`（第 527-531 行附近）
  - `os/src/fs/ext4/direntry.rs:491-499` — `dir_set_csum()` 实现
  - `os/src/fs/ext4/block_group.rs:502-522` — `Block::sync_blk_to_disk()` 确认其语义（将整个 Block.data 写到 disk_offset 对应的磁盘块）

  **Acceptance Criteria**:
  - [ ] `make rv64-kernel-build-only` 编译通过
  - [ ] `make la64-kernel-build-only` 编译通过
  - [ ] QEMU 启动后 `busybox --install -s /bin` 成功执行（目录项正确写入，checksum 正确）

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: busybox --install creates symlinks without double write
    Tool: kernel-dev_kernel_run (arch=rv64, log=info, timeout=120)
    Preconditions: 全新 QEMU 镜像
    Steps:
      1. 启动 QEMU，等待 prepare_symlink() 执行
      2. 在 QEMU 日志中查找 busybox --install 日志
      3. 确认 busybox --install 执行成功（exit=0）
      4. 观察 sync_inode_to_disk / sync_blk_to_disk 日志频率：
         同一 block_id 不应在连续两行中反复出现（除非独立的不同操作）
      5. 确认任务 3 的自检显示 BIN_BASH_OK（说明目录项正确写入）
    Expected Result: busybox --install 成功，/bin 目录项完整，目录块写回次数显著减少
    Failure Indicators: 目录块 checksum 错误导致后续读取失败；busybox --install 失败
    Evidence: .sisyphus/evidence/task-4-no-double-write.log

  Scenario: directory entry removal still works correctly
    Tool: kernel-dev_kernel_run (arch=rv64, log=info, timeout=120)
    Preconditions: 目录中有可删除的条目
    Steps:
      1. 启动 QEMU 并进入 shell（如果配置）
      2. 执行 rm 命令删除 /bin 下某个条目
      3. 确认删除成功且目录块未损坏（ls 仍能列出其余条目）
    Expected Result: 删除操作正常，剩余条目可读
    Evidence: .sisyphus/evidence/task-4-remove-ok.log
  ```

  **Commit**: YES
  - Message: `fix(ext4): remove double sync_blk_to_disk in try_insert_to_existing_block`
  - Files: `os/src/fs/ext4/direntry.rs`

### Wave 2 — Fast Symlink（写 + 读）

- [x] 5. **实现 fast symlink 创建**

  **What to do**:
  1. 在 `os/src/fs/ext4/ext4_inode.rs` 的 `Ext4Inode` 上添加两个 helper：
     ```rust
     pub fn block_as_bytes(&self) -> &[u8; 60] {
         unsafe { &*(self.block.as_ptr() as *const [u8; 60]) }
     }
     pub fn block_mut_as_bytes(&mut self) -> &mut [u8; 60] {
         unsafe { &mut *(self.block.as_mut_ptr() as *mut [u8; 60]) }
     }
     ```
     （参考 `extent.rs:570-571` 的 transmute 模式）
  2. 修改 `os/src/fs/ext4/ext4fs.rs` 的 `symlink()` 方法（约第 573 行）：
     - 在 `create()` 之后、`write_at()` 之前插入判断
     - 如果 `target.as_bytes().len() <= 60`：
       - 清除 `EXT4_INODE_FLAG_EXTENTS` 标志：`new_ref.inode.flags &= !(EXT4_INODE_FLAG_EXTENTS as u32)`
       - 将 target 字节写入 `new_ref.inode.block_mut_as_bytes()[..target.len()]`
       - 剩余字节填零：`block_mut_as_bytes()[target.len()..60].fill(0)`
       - `new_ref.inode.set_size(target.len() as u64)`
       - `self.ext4fs.write_back_inode(&new_ref)` 写回
       - 返回（不调用 `write_at`）
     - 否则走原有 `write_at` 路径
  3. 在 `os/src/fs/ext4/mod.rs` 中确保 `EXT4_INODE_FLAG_EXTENTS` 在 symlink 代码中可用（确认是 `pub const`）

  **Must NOT do**:
  - 不要改动 `create_inode()` 的通用逻辑（`EXT4_INODE_FLAG_EXTENTS` 仍对普通文件/目录默认设置）
  - 不要在 fast symlink 路径调用 `extent_tree_init()`
  - 不要在没有先判断 `target.len() <= 60` 的情况下修改 extent 标志

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: `[]`
  - **Reason**: 明确的两文件改动，add helper + if-else 分支

  **Parallelization**:
  - **Can Run In Parallel**: YES（与 Task 6 可并行，但建议顺序——写入端先完成，读取端据此测试）
  - **Parallel Group**: Wave 2
  - **Blocks**: Task 6（读取端依赖写入端提供的 `block_as_bytes()` helper）
  - **Blocked By**: 无强依赖（建议 Wave 1 完成后开始）

  **References**:
  - `os/src/fs/ext4/ext4fs.rs:573-593` — `symlink()` 当前实现（create + write_at）
  - `os/src/fs/ext4/ext4_inode.rs:48-78` — `Ext4Inode` 结构体，`block: [u32; 15]` 字段（第 63 行）
  - `os/src/fs/ext4/ext4_inode.rs:116-123` — `set_size()` 方法
  - `os/src/fs/ext4/ext4_inode.rs:332-334` — `is_link()` 方法
  - `os/src/fs/ext4/extent.rs:570-571` — `transmute(&[u32;15] → &[u8;60])` 参考模式
  - `os/src/fs/ext4/ext4_inode.rs:642-657` — `write_back_inode()` 方法调用方式
  - `os/src/fs/ext4/mod.rs:43` — `EXT4_INODE_FLAG_EXTENTS` 常量定义

  **Acceptance Criteria**:
  - [ ] `make rv64-kernel-build-only` 编译通过
  - [ ] `make la64-kernel-build-only` 编译通过
  - [ ] 创建短符号链接（如 `ln -s /busybox /bin/ls`）后：inode size = target 长度（非 4096）
  - [ ] 创建长符号链接（>60 字节）仍走 `write_at()` 路径（正常分配数据块）

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: Short symlink (≤60 bytes) → stored in i_block
    Tool: kernel-dev_kernel_run (arch=rv64, log=info, timeout=120)
    Preconditions: prepare_symlink() 执行 busybox --install -s /bin（创建大量短 symlink）
    Steps:
      1. 启动 QEMU，等待 prepare_symlink() 执行
      2. 在 QEMU 日志中查找 symlink 创建的日志（WRITE_CALLER sync_inode_to_disk 行）
      3. 确认短 symlink（如 /bin/ls → /busybox，目标 < 60 字节）的 size 字段：
         日志中不应出现 size=4096（块分配）的中间状态
      4. 确认日志中 mode=0o120777（S_IFLNK + 0777）
      5. 确认 busybox --install 成功完成
    Expected Result: symlink 创建时 size 直接等于目标长度，无数据块分配步骤
    Failure Indicators: 仍出现 size=4096 → size=8 序列；extent tree 错误导致后续 panic
    Evidence: .sisyphus/evidence/task-5-fast-symlink.log

  Scenario: Long symlink (>60 bytes) → still uses data blocks
    Tool: kernel-dev_kernel_run (arch=rv64, log=info, timeout=120)
    Preconditions: 创建目标长度超过 60 字节的符号链接
    Steps:
      1. 启动 QEMU
      2. 通过 bash 执行：busybox ln -s /very/long/path/that/exceeds/sixty/bytes/target/string /tmp/longlink
      3. 观察日志确认走 write_at 路径（有数据块分配日志）
    Expected Result: 长符号链接正常分配数据块，功能正常
    Evidence: .sisyphus/evidence/task-5-long-symlink.log
  ```

  **Commit**: YES
  - Message: `feat(ext4): fast symlink creation for targets <= 60 bytes`
  - Files: `os/src/fs/ext4/ext4fs.rs`, `os/src/fs/ext4/ext4_inode.rs`

- [x] 6. **实现 fast symlink 读取**

  **What to do**:
  1. 在 `os/src/fs/ext4/file.rs` 的 `read_at()` 方法中添加 fast symlink 处理逻辑：
     - 在读取循环之前（或作为特殊情况），检查 inode 是否为 fast symlink
     - 判断条件：`!uses_extents && is_symlink && inode_size <= 60`
     - 当条件满足时：直接从 `inode_ref.inode.block_as_bytes()[..inode_size]` 拷贝到 `read_buf`
     - 返回实际拷贝的字节数
  2. 利用 Task 5 中新增的 `block_as_bytes()` helper 读取 `i_block` 内容
  3. 注意边界：`inode_size` 为 `u64`，需要 `as usize`；确保 `read_buf.len() >= inode_size`

  **Must NOT do**:
  - 不要改动 `get_pblock_idx()` 中已有的 fast symlink 检测（它返回 `Err(ENOENT)`，调用方 `read_at()` 需在调用前拦截）
  - 不要影响普通文件的 `read_at()` 路径
  - 不要改动 PageCache 路径（fast symlink 读取绕过 PageCache 是合理的，目标 ≤60 字节）

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: `[]`
  - **Reason**: 单函数内添加前置检查，逻辑简单

  **Parallelization**:
  - **Can Run In Parallel**: YES（建议 Task 5 完成后开始）
  - **Parallel Group**: Wave 2
  - **Blocks**: None
  - **Blocked By**: Task 5（依赖 `block_as_bytes()` helper）

  **References**:
  - `os/src/fs/ext4/file.rs:385-473` — `read_at()` 完整实现，需在开头或 `get_pblock_idx()` 调用前加 fast symlink 拦截
  - `os/src/fs/ext4/file.rs:685-708` — 已有的 fast symlink truncation 逻辑（`is_symlink && !uses_extents` 分支）——参考其 inode 访问模式
  - `os/src/fs/ext4/ext4_inode.rs:679-725` — `get_pblock_idx()` 中 fast symlink 检测：`!extents && is_link && size <= 60` → `Err(ENOENT)`
  - `os/src/fs/ext4/ext4_inode.rs:116-123` — `size()` 方法返回 `u64`
  - `os/src/syscall/fs.rs:925-992` — `sys_readlinkat()` 调用 `inode.read_at(0, link_len, ...)` 读取符号链接目标

  **Acceptance Criteria**:
  - [ ] `make rv64-kernel-build-only` 编译通过
  - [ ] `make la64-kernel-build-only` 编译通过
  - [ ] 从磁盘镜像中读取已有 fast symlink（Linux 工具创建）返回正确目标
  - [ ] `busybox readlink /bin/ls` 返回 `/busybox`（非空字符串）

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: Read fast symlink created by this kernel (Task 5)
    Tool: kernel-dev_kernel_run (arch=rv64, log=info, timeout=120)
    Preconditions: prepare_symlink() 已通过 Task 5 创建 fast symlinks
    Steps:
      1. 启动 QEMU
      2. 通过 bash 执行：busybox readlink /bin/ls
      3. 确认输出为 "/busybox"（正确的目标路径）
      4. 执行：busybox readlink /bin/cat
      5. 确认输出为 "/busybox"
    Expected Result: 所有 short symlink 读回正确目标字符串
    Failure Indicators: readlink 返回空字符串或乱码；返回 errno
    Evidence: .sisyphus/evidence/task-6-readlink.log

  Scenario: Read fast symlink from disk image (Linux-created)
    Tool: kernel-dev_kernel_run (arch=rv64, log=info, timeout=120)
    Preconditions: 磁盘镜像中包含 Linux 工具创建的 fast symlink（无 EXTENTS flag）
    Steps:
      1. 在 QEMU 内执行：busybox readlink /lib/ld-musl-riscv64-sf.so.1
      2. 确认返回 "/musl/lib/libc.so"
      3. 确认不报错且不为空
    Expected Result: 磁盘上已有 fast symlink 能被正确读取
    Evidence: .sisyphus/evidence/task-6-existing-symlink.log
  ```

  **Commit**: YES
  - Message: `feat(ext4): fast symlink reading from i_block`
  - Files: `os/src/fs/ext4/file.rs`

---

### Wave FINAL — 集成验证 + 审查

- [x] 7. **QEMU 集成验证**

  **What to do**:
  1. 编译双架构：`make rv64-kernel-build-only` + `make la64-kernel-build-only`
  2. 配置测试为 basic 组（mask=0x001）：
     ```bash
     make -C os conf-inject CONF_ARCH=rv64 CONF_BLK_MODE=virt CONF_FILE=../os_test.conf
     ```
  3. 启动 QEMU：`make -C os rv64-run LOG=info`
  4. 检查关键日志行：
     - `[initproc] post-prepare /bin/bash check exit=0` → `/bin/bash` 可用
     - `#### OS COMP TEST GROUP END` → 测试组完成
     - 无 panic / 无 crash
  5. 验证 disk I/O：统计 `sync_inode_to_disk` / `sync_blk_to_disk` 行数，确认不再有同一 block_id 连续双写
  6. 验证 symlink：`busybox readlink /bin/sh` 返回 `/busybox`

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: `[]`
  - **Reason**: 编译 + 日志分析，无代码修改

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: 最终集成波
  - **Blocks**: None
  - **Blocked By**: Task 1, 2, 3, 4, 5, 6

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: Full QEMU boot with basic test group
    Tool: kernel-dev_kernel_run (arch=rv64, log=info, timeout=300)
    Preconditions: 所有 Wave 1+2 改动已应用
    Steps:
      1. 执行双架构编译并通过
      2. 注入 basic 测试配置
      3. 启动 QEMU，等待 initproc 初始化 + 执行 basic 测试组
      4. 检查关键日志行：
         a. "BIN_BASH_OK" 或 fallback 日志
         b. "#### OS COMP TEST GROUP END basic-glibc ####"
         c. 无 panic 行
      5. 统计 sync_blk_to_disk 日志：确认没有同一 block_id 连续出现
      6. 验证 busybox readlink /bin/ls 输出
    Expected Result: basic 测试组全部通过，/bin/bash 可用，symlink I/O 减少
    Failure Indicators: panic、测试组超时、/bin/bash 仍不可用（需检查 Task 1 dump）
    Evidence: .sisyphus/evidence/task-7-integration.log
  ```

  **Commit**: YES（如编译需微调则追加，否则合并到上一个 commit）
  - Message: `test: QEMU integration verification for ext4 fixes`
  - Files: 无新增

---

## Final Verification Wave (MANDATORY — after ALL implementation tasks)

> 4 审查代理并行运行。ALL must APPROVE。向用户展示汇总结果并等待明确的 "okay"。
> F1-F4 在获得用户确认前不得标记为已完成。

- [x] F1. **编译 + 基本冒烟** — `quick`
  双架构编译确认零 error/warning：`make rv64-kernel-build-only` + `make la64-kernel-build-only`。QEMU 启动 basic 测试组通过。
  Output: `Build rv64 [PASS/FAIL] | Build la64 [PASS/FAIL] | Boot [PASS/FAIL] | VERDICT`

- [x] F2. **代码变更审计** — `quick`
  检查 `git diff` 中所有变更文件是否符合 scope 边界：
  - 只改动 `os/src/fs/ext4/direntry.rs`, `os/src/fs/ext4/ext4fs.rs`, `os/src/fs/ext4/file.rs`, `os/src/fs/ext4/ext4_inode.rs`, `user/src/bin/initproc.rs`
  - 未触碰 FAT32、VFS 通用层、PageCache、`create_inode()` 通用逻辑
  Output: `Files [N/N in-scope] | Contamination [CLEAN/N issues] | VERDICT`

- [x] F3. **Guardrails 合规** — `quick`
  核对 Must NOT Have 列表：
  - `create_inode()` 未修改（仍对所有非 symlink 设置 EXTENTS flag）
  - 无 `cargo test`/`cargo clippy` 调用
  - 无新引入的 unsafe 块（除 Task 5 中已审核的 transmute）
  - `dir_find_in_block()` 的 `entry_len < 8` break 语义未变
  Output: `Guardrails [N/N passed] | VERDICT`

- [x] F4. **日志证据收集** — `quick`
  汇总 `.sisyphus/evidence/` 下所有 task-N-*.log 文件，确认每个 task 的证据文件存在且内容合理。
  Output: `Evidence [N/N present] | VERDICT`

---

## Commit Strategy

- **Task 1**: `fix(ext4): add directory block dump on dir_find_entry failure` — `os/src/fs/ext4/direntry.rs`
- **Task 2**: `fix(initproc): add /bash fallback in run_group_once` — `user/src/bin/initproc.rs`
- **Task 3**: `debug(initproc): add /bin/bash self-check after prepare_symlink` — `user/src/bin/initproc.rs`
- **Task 4**: `fix(ext4): remove double sync in try_insert_to_existing_block` — `os/src/fs/ext4/direntry.rs`
- **Task 5**: `feat(ext4): fast symlink creation for targets <= 60 bytes` — `os/src/fs/ext4/ext4fs.rs` + `os/src/fs/ext4/ext4_inode.rs`
- **Task 6**: `feat(ext4): fast symlink reading from i_block` — `os/src/fs/ext4/file.rs`

---

## Success Criteria

### 验证命令
```bash
# 双架构编译
make rv64-kernel-build-only
make la64-kernel-build-only

# QEMU 启动（basic 测试组）
make -C os conf-inject CONF_ARCH=rv64 CONF_BLK_MODE=virt CONF_FILE=../os_test.conf
make -C os rv64-run LOG=info
```

### 最终检查清单
- [ ] 双架构编译通过
- [ ] QEMU basic 测试组通过
- [ ] 日志中可见 `[dir_dump]` 行（如果 `/bin/bash` 仍失败）
- [ ] 日志中可见 `fallback /bash` 行（如果 `/bin/bash` 不可用）
- [ ] 日志中可见 `BIN_BASH_OK` 或 `BIN_BASH_BAD`
- [ ] symlink 创建不再出现 `size=4096` 中间态
