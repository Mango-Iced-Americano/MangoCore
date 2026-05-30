# MangoCore Mount 子系统 — LTP 测例状态

> 创建: 2026-05-25
> 分支: `fs`
> 对应计划: `Doc/ltp_mount_plan.md`

## 1. 当前基线

### 内核实现状态

| 特性 | 状态 | 文件 |
|------|------|------|
| MountFS / MountFSInode | ✅ 成熟 | `os/src/fs/vfs/mount.rs` (722行) |
| `sys_mount(40)` 正常挂载 | ✅ | `os/src/syscall/fs.rs:2237` |
| `sys_umount2(39)` | ✅ | `os/src/syscall/fs.rs:2030` |
| `sys_mount` flag 路由 (propagation/bind/move/remount) | ✅ | `os/src/syscall/fs.rs:2291-2318` |
| `do_bind_mount` (MS_BIND) | ⚠️ 初版完成 | `os/src/syscall/fs.rs:2108` |
| `do_recursive_bind` (MS_REC) | ⚠️ 初版，backref 指向错误 | `os/src/syscall/fs.rs:2175` |
| MountList 全局注册 | ❌ 定义但未全局使用 | `os/src/fs/vfs/mount.rs:654` |
| `/proc/mounts` | ⚠️ 只列根直接子挂载，硬编码路径名 | `os/src/fs/procfs/files/mounts.rs` |
| self-bind (`mount --bind dir dir`) | ❌ 未支持 | — |
| MS_MOVE | ❌ EINVAL | `os/src/syscall/fs.rs:2311` |
| mount propagation | ⚠️ no-op (返回 SUCCESS) | `os/src/syscall/fs.rs:2293` |
| mount namespace (CLONE_NEWNS) | ❌ 无 | — |

### 已知已修复问题

| 问题 | 状态 |
|------|------|
| `overlaid_inode()` 每次 lookup 覆盖 `self_mountpoint` | ✅ 已修复 (mount.rs:171) |
| ext4 `children` 强引用 `Arc` 导致 heap 泄漏 | ✅ 已修复 (layout.rs:95 已是 Weak) |
| 内存泄漏 (mount-bind-leak-analysis.md 中描述) | ✅ 用户确认已修 |

### 当前内存基准（待记录）

```
MOUNTFS_ALIVE:     ?
MOUNTFSINODE_ALIVE: ?
heap_free:          ?
```

## 2. 测例分类标准

| 分类 | 含义 | 条件 |
|------|------|------|
| **PASS** | 通过 | QEMU 输出 TPASS |
| **FIXABLE_NOW** | 当前阶段可修复 | 依赖 Phase 1/2/3 范围内特性 |
| **FIXABLE_LATER** | 后续阶段修复 | 依赖 MS_MOVE / 传播 / namespace 等 |
| **UNSUPPORTED** | 不支持 | 依赖 cloneNS / 新 mount API / 非目标特性 |
| **ENV_FAIL** | 环境问题 | 缺二进制、缺 /etc/passwd 等 |
| **DANGEROUS_STRESS** | 破坏性/压力 | 可能 OOM/panic |
| **?** | 待分类 | 未跑过，需发现扫确认 |

## 3. 测例状态表

### 3.1 bind/ — MS_BIND 基础 (25个)

| 测例 | 状态 | Round | 结果 | 备注 |
|------|------|-------|------|------|
| fs_bind01.sh | ? | — | — | Phase 2 目标 |
| fs_bind02.sh | ? | — | — | Phase 2 目标 |
| fs_bind03.sh | ? | — | — | Phase 2 目标 |
| fs_bind04.sh | ? | — | — | Phase 2 目标 |
| fs_bind05.sh | ? | — | — | Phase 2 目标 |
| fs_bind06.sh | ? | — | — | Phase 2 目标 |
| fs_bind07.sh | ? | — | — | Phase 2 目标 |
| fs_bind07-2.sh | ? | — | — | Phase 2 目标 |
| fs_bind08.sh | ? | — | — | Phase 2 目标 |
| fs_bind09.sh | ? | — | — | Phase 2 目标 |
| fs_bind10.sh | ? | — | — | Phase 2 目标 |
| fs_bind11.sh | ? | — | — | Phase 2 目标 |
| fs_bind12.sh | ? | — | — | Phase 2 目标 |
| fs_bind13.sh | ? | — | — | Phase 2 目标 |
| fs_bind14.sh | ? | — | — | Phase 2 目标 |
| fs_bind15.sh | ? | — | — | Phase 2 目标 |
| fs_bind16.sh | ? | — | — | Phase 2 目标 |
| fs_bind17.sh | ? | — | — | Phase 2 目标 |
| fs_bind18.sh | ? | — | — | Phase 2 目标 |
| fs_bind19.sh | ? | — | — | Phase 2 目标 |
| fs_bind20.sh | ? | — | — | Phase 2 目标 |
| fs_bind21.sh | ? | — | — | Phase 2 目标 |
| fs_bind22.sh | ? | — | — | Phase 2 目标 |
| fs_bind23.sh | ? | — | — | Phase 2 目标 |
| fs_bind24.sh | ? | — | — | Phase 2 目标 |

### 3.2 rbind/ — MS_REC 递归绑定 (39个)

| 测例 | 状态 | Round | 结果 | 备注 |
|------|------|-------|------|------|
| fs_bind_rbind01.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind02.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind03.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind04.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind05.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind06.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind07.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind07-2.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind08.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind09.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind10.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind11.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind12.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind13.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind14.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind15.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind16.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind17.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind18.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind19.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind20.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind21.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind22.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind23.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind24.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind25.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind26.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind27.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind28.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind29.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind30.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind31.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind32.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind33.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind34.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind35.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind36.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind37.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind38.sh | ? | — | — | Phase 3 目标 |
| fs_bind_rbind39.sh | ? | — | — | Phase 3 目标 |

### 3.3 move/ — MS_MOVE 移动挂载 (22个)

| 测例 | 状态 | Round | 结果 | 备注 |
|------|------|-------|------|------|
| fs_bind_move01.sh | FIXABLE_LATER | — | — | 依赖 MS_MOVE (Phase 3 optional) |
| fs_bind_move02.sh | FIXABLE_LATER | — | — | 同上 |
| fs_bind_move03.sh | FIXABLE_LATER | — | — | 同上 |
| fs_bind_move04.sh | FIXABLE_LATER | — | — | 同上 |
| fs_bind_move05.sh | FIXABLE_LATER | — | — | 同上 |
| fs_bind_move06.sh | FIXABLE_LATER | — | — | 同上 |
| fs_bind_move07.sh | FIXABLE_LATER | — | — | 同上 |
| fs_bind_move08.sh | FIXABLE_LATER | — | — | 同上 |
| fs_bind_move09.sh | FIXABLE_LATER | — | — | 同上 |
| fs_bind_move10.sh | FIXABLE_LATER | — | — | 同上 |
| fs_bind_move11.sh | FIXABLE_LATER | — | — | 同上 |
| fs_bind_move12.sh | FIXABLE_LATER | — | — | 同上 |
| fs_bind_move13.sh | FIXABLE_LATER | — | — | 同上 |
| fs_bind_move14.sh | FIXABLE_LATER | — | — | 同上 |
| fs_bind_move15.sh | FIXABLE_LATER | — | — | 同上 |
| fs_bind_move16.sh | FIXABLE_LATER | — | — | 同上 |
| fs_bind_move17.sh | FIXABLE_LATER | — | — | 同上 |
| fs_bind_move18.sh | FIXABLE_LATER | — | — | 同上 |
| fs_bind_move19.sh | FIXABLE_LATER | — | — | 同上 |
| fs_bind_move20.sh | FIXABLE_LATER | — | — | 同上 |
| fs_bind_move21.sh | FIXABLE_LATER | — | — | 同上 |
| fs_bind_move22.sh | FIXABLE_LATER | — | — | 同上 |

### 3.4 cloneNS/ — 命名空间 (7个)

| 测例 | 状态 | Round | 结果 | 备注 |
|------|------|-------|------|------|
| fs_bind_cloneNS01.sh | UNSUPPORTED | — | — | 依赖 CLONE_NEWNS |
| fs_bind_cloneNS02.sh | UNSUPPORTED | — | — | 同上 |
| fs_bind_cloneNS03.sh | UNSUPPORTED | — | — | 同上 |
| fs_bind_cloneNS04.sh | UNSUPPORTED | — | — | 同上 |
| fs_bind_cloneNS05.sh | UNSUPPORTED | — | — | 同上 |
| fs_bind_cloneNS06.sh | UNSUPPORTED | — | — | 同上 |
| fs_bind_cloneNS07.sh | UNSUPPORTED | — | — | 同上 |

### 3.5 其他

| 测例 | 状态 | Round | 结果 | 备注 |
|------|------|-------|------|------|
| fs_bind_regression.sh | ? | — | — | mount bind 回归测试 |
| fs_bind_lib.sh | — | — | — | 库文件，非测例 |

## 4. 统计总览

| 分类 | bind/ | rbind/ | move/ | cloneNS/ | 其他 | 合计 |
|------|:-----:|:------:|:-----:|:--------:|:----:|:----:|
| ? (待分类) | 25 | 39 | 0 | 0 | 1 | **65** |
| FIXABLE_LATER | 0 | 0 | 22 | 0 | 0 | **22** |
| UNSUPPORTED | 0 | 0 | 0 | 7 | 0 | **7** |
| PASS | 0 | 0 | 0 | 0 | 0 | **0** |
| **合计** | **25** | **39** | **22** | **7** | **1** | **94** |

> 注：fs_bind_lib.sh 为库文件，不计入测例。

## 5. 变更记录

| 日期 | 变更 | 影响 |
|------|------|------|
| 2026-05-25 | 创建文档，初始基线 | — |

## 6. os_test.conf 同步状态

| 字段 | 值 |
|------|-----|
| `ltp_include` 中 fs_bind 数量 | ~94 (auto_include 脚本自动加入) |
| `ltp_exclude` 中 fs_bind 数量 | ~93 (全量排除中) |
| 当前实际运行 | **0** 个 (exclude 优先) |

**下一步**: Phase 1 修复完成后，逐步从 ltp_exclude 移除通过测例。
