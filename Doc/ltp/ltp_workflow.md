# LTP 适配通用工作流

> 最后更新: 2026-05-22
> 适用范围: FS / NET / 任意内核模块的 LTP 分诊与适配

---

## 一、三阶段流程

```
Phase 0: 体系建设 → Phase 1: 发现扫 + 修 → Phase 2: 稳定 + 回归
```

### Phase 0 — 体系建设

**目标**: 建立分诊框架，摸清可用资源。

| 步骤 | 操作 | 产出物 |
|------|------|--------|
| 0.1 | 本地摸底：`debugfs ls /musl/ltp/testcases/bin/` 列出所有可用 LTP 二进制 | 二进制清单 |
| 0.2 | 上游调研：launch 3-5 个 `librarian` agent 扫 `linux-test-project/ltp` `testcases/kernel/syscalls/` | 上游测例清单 + 分类 |
| 0.3 | 写 `Doc/ltp_<module>_plan.md`：Round 设计、排除规则、晋级条件 | 计划文档 |
| 0.4 | 写 `Doc/ltp_<module>_status.md`：每测例状态表（Round/结果/分类/备注） | 状态文档 |
| 0.5 | 预处理：ramfs 配额、LTP_IPC_PATH、/etc/passwd 等环境问题 | 环境就绪 |

**关键约束**:
- 测例分类: `PASS / FIXABLE_NOW / FIXABLE_LATER / UNSUPPORTED / ENV_FAIL / DANGEROUS_STRESS`
- 排除不支持的 Linux 特性（xattr/ACL/namespace/SCTP/AF_PACKET...）
- `ltp_runner=inline` + `ltp_libc=musl` 做内联发现扫

---

### Phase 1 — 发现扫 + 修复

**目标**: 跑所有可用测例，收集失败分布，按优先级修复。

#### 1.1 分批发现扫

```bash
# 每次 20-50 个测例，人可消化
kernel-dev_kernel_test_config arch=rv64 ltp_include="test01,test02,..."
docker exec ... make rv64-run > /tmp/qemu_batch1.log
```

**规则**:
- 每批 0 panic 是前提
- TPASS / TFAIL / TBROK 分别统计
- TFAIL 按 family 归类

#### 1.2 分类决策

对每个 TFAIL 回答 4 个问题：
1. 这个 testcase 在验证什么 Linux 语义？
2. 对当前比赛目标是否必要？
3. 失败在哪一层（A: syscall errno / B: fd 生命周期 / C: 协议栈状态机 / ...）？
4. 只有 `FIXABLE_NOW` 才允许进入修复流程。

**分类决策树**:
```
TFAIL → 是否涉及不支持特性？→ YES → UNSUPPORTED
       → 是否环境问题？→ YES → ENV_FAIL
       → 是否压力/破坏性？→ YES → DANGEROUS_STRESS
       → 是否依赖未实现前置能力？→ YES → FIXABLE_LATER
       → 否则 → FIXABLE_NOW
```

#### 1.3 集中修复

**原则**:
- **一次推一个 family**，不跨 family 并行修
- **最小修改原则**：只修根因，不重构
- **不允许**为单个 testcase 写硬编码 hack
- **不允许**绕过 VFS/smoltcp/正常路径
- **不允许**看到失败就直接改内核
- **不允许**修一个 testcase 导致已有 PASS testcase 回退
- **FS 模块只动 FS 代码**，不跨模块（如 process/task）
- 参考 DragonOS 的实现模式

**修复后验证**:
- `kernel-dev_kernel_build rv64` + `kernel-dev_kernel_build la64`（双架构编译）
- 跑修复的 testcase + 已有回归集
- QEMU 启动不 panic

#### 1.4 周期性 Oracle 审查

**触发时机**:
- 完成一批修复后（3-5 个修复）
- 遇到无法确定的分类决策
- 发现深层 bug 需要架构分析
- 合并新分支后重新评估策略

**调用方式**:
```
task(subagent_type="oracle", run_in_background=true, load_skills=[],
  prompt="[CONTEXT]... [FINDINGS]... [WHAT I NEED]...")
```

**审查要点**:
- 修复方案是否正确、最小
- 是否引入回归风险
- 优先级的合理性
- 新发现的测例分类建议

---

### Phase 2 — 稳定 + 回归

#### 2.1 全量回归

**时机**: 改动积累到一定程度（5+ commits），且自检无新 panic 后。

**测试配置**:
```
mask=0xFFF
ltp_runner=script    # 切回脚本模式（接近评测环境）
ltp_libc=both        # musl + glibc 都跑
```

**通过标准**:
- 0 kernel panic
- 已 PASS 测例无回退
- LTP musl + glibc 均跑完

**命令**:
```bash
# 注入 config + 跑全量（后台 40 分钟）
make -C os conf-inject ... && nohup docker exec ... make rv64-run > /tmp/full_test.log &
```

#### 2.2 文档更新

每轮完成后必须更新：
- `Doc/ltp_<module>_status.md`：更新测例结果、行动分类、回归集
- `Doc/ltp_<module>_plan.md`：更新阶段状态
- `os_test.conf`：include 列表同步
- git commit 记录修复细节

---

## 二、日常操作模板

### 发现扫

```bash
# 1. 配置
kernel-dev_kernel_test_config arch=rv64 ltp_include="test01,test02,..." ltp_libc=musl ltp_runner=inline mask=0x800

# 2. 编译（如需）
kernel-dev_kernel_build arch=rv64

# 3. 跑
docker exec -w /app/os oskernel2026-mango-os-dev-1 bash -c "LOG=off timeout 180 make rv64-run" > /tmp/qemu.log

# 4. 分析
grep -c panicked /tmp/qemu.log
grep "TPASS\|TFAIL\|TBROK" /tmp/qemu.log | sort | uniq -c
```

### 提交

```bash
git add <changed_files>
git commit -m "<module>: <what fixed> → TPASS improvement

<root cause analysis>
<affected testcases and before/after counts>"
```

### Oracle 审查

```bash
task(subagent_type="oracle", run_in_background=true, load_skills=[],
  prompt="[CONTEXT]: MangoCore kernel, <module> LTP adaptation.
[FINDINGS]: <N TPASS, M TFAIL, K TBROK>. <List key failures>.
[WHAT I NEED FROM YOU]: <specific questions>")
```

---

## 三、常见问题速查

| 问题 | 排查 | 修复 |
|------|------|------|
| ENOSYS (38) 全线返回 | syscall 号未注册（`syscall_id.rs`）或缺实现 | 注册 + 实现 stub |
| ENOSPC (28) 长任务频繁出现 | ramfs `/tmp` 配额太小 | `RamFS::new_with_quota(4096)` → `quota(0)` 不限 |
| LTP_IPC_PATH is not defined | inline runner 没设环境变量 | 加 `export LTP_IPC_PATH=/tmp` |
| 大量 `FAIL LTP CASE : 0` 但无测试输出 | 镜像里没这个二进制 | 排除该测例（NO_BIN） |
| getpwnam ENOENT | `/etc/passwd` 缺条目或不可读 | initproc 里补 `/etc/passwd` 写入 |
| EACCES 全线失败 | 进程始终是 root（缺 seteuid） | 需实现 `sys_seteuid`（process 模块） |
| EFAULT 被 ENOTCONN 覆盖 | 参数校验顺序：连接状态先于指针 | 指针探针写入移到连接检查前 |
| EINVAL 被 ENOPROTOOPT 覆盖 | optlen 校验在 optname 匹配之后 | optlen 校验移到 match 之前 |

---

## 四、模块独立性原则

每个模块的 LTP 适配**独立在对应分支**进行：

| 模块 | 分支 | 代码范围 |
|------|------|----------|
| FS | `fs` | `os/src/fs/` + `os/src/syscall/fs.rs` |
| NET | `net` | `os/src/net/` + `os/src/net/syscall/` |
| PROCESS | `process` | `os/src/syscall/process/` |

**跨模块合并**: 在基础模块稳定后，`git merge <source>` 到目标分支。

---

## 五、新模块快速上手

```bash
# 1. 切模块分支
git checkout <module>

# 2. Merge 已有稳定模块（如 FS 的基础修复）
git merge fs

# 3. 写计划 + 状态文档
# 参考: Doc/ltp_fs_plan.md, Doc/ltp_fs_status.md

# 4. 本地摸排
docker exec ... debugfs -R "ls -l /musl/ltp/testcases/bin/" sdcard-rv.img | grep -oE "<pattern>"

# 5. Launch librarian 扫上游
task(subagent_type="librarian", run_in_background=true, load_skills=[], prompt="...")

# 6. 开始 Phase 1 发现扫
```

---

## 六、DragonOS 参考模式

DragonOS（[DragonOS-Community/DragonOS](https://github.com/DragonOS-Community/DragonOS)）是本项目的设计标杆，尤其在 VFS 架构、Socket trait、权限模型方面。修复前应优先查阅 DragonOS 的对应实现。

### 查阅方式

```bash
# librarian agent 搜索 DragonOS 仓库
task(subagent_type="librarian", run_in_background=true, load_skills=[],
  prompt="Search DragonOS-Community/DragonOS on GitHub.
[CONTEXT]: MangoCore, implementing <feature>.
[GOAL]: Find how DragonOS handles <specific pattern>.
[REQUEST]: Show code patterns, file paths, trait definitions.")
```

### 已验证的 DragonOS 模式

| 场景 | DragonOS 做法 | MangoCore 应用 |
|------|-------------|---------------|
| **O_NOFOLLOW** | `openat2` 中双层检查：`lookup_follow_symlink2(follow=false)` + 后检查 `file_type==SymLink→ELOOP` | `open_file_at` 同模式 (`fs.rs:180`) |
| **VFS 分层** | `File trait → IndexNode trait → FileSystem trait` | 沿袭同一分层设计 |
| **Socket trait** | `Socket` trait + `PSOCK` enum + `impl_file_for_socket!` 宏 | 复用同一架构 |
| **procfs fd 目录** | `ProcFS` 动态 inode + `find_hook/list_hook` | 使用 `LockedProcInode` + `set_hooks` |
| **EPTAL/errno 顺序** | 参数校验 → 连接状态检查（EFAULT 先于 ENOTCONN） | `Socket::peer_addr` 中 `probe_user_write` 先于 `remote_endpoint` |

### 关键文件映射

| MangoCore | DragonOS 对应 |
|-----------|-------------|
| `os/src/fs/vfs/file.rs` | `kernel/src/filesystem/vfs/file.rs` |
| `os/src/fs/vfs/index_node.rs` | `kernel/src/filesystem/vfs/mod.rs` (IndexNode trait) |
| `os/src/net/socket/mod.rs` | `kernel/src/net/socket/mod.rs` |
| `os/src/net/socket/inet/` | `kernel/src/net/socket/inet/` |
| `os/src/syscall/fs.rs` | `kernel/src/filesystem/vfs/syscall/` |
| `os/src/fs/procfs/` | `kernel/src/filesystem/procfs/` |
