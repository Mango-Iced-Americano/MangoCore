# FS/Net SMP 执行计划（Batch 1: WP1 ktest 框架 + RED 基线）

> 设计蓝图：`docs/10_plan/fs-net-smp-adaptation.md`（Oracle 决策级，1272 行）
> 本文件是**执行台账**：记录当前批次做什么、谁做、怎么验证。执行时严格照此推进。
> 时间：2026-08-04

## 总路线（7 个工作包，依赖见设计文档 §13）

```
WP1 测试框架+RED → WP2 PageCache → WP3 目录锁 → WP4 端口 → WP5 DeviceStack → WP6 poll → WP7 Phase-5 门禁
```

## 本批次 = WP1（L2/T2，无需 §8.2 门禁）

### 目标
新增 `fs_smp` / `net_smp` 两个 ktest 组 + 可测性 hook，**必须看到两条 RED**：
1. `fs_smp::pagecache_user_write_vs_truncate` — writer 成功后 entry 已被 truncate 摘除
2. `net_smp::irq_poll_is_publish_only` — IRQ 模拟调用阻塞超过 deadline

其余 ktest 为保护性用例，可先 GREEN。

### 产出文件（agent 分工，无重叠）
| 文件 | 内容 | agent |
|---|---|---|
| `os/src/kernel_tests/fs_smp.rs` | 8 用例 + tmpfs/ramfs/memblk fixture（参考 Linux tmpfs 并发语义） | deep |
| `os/src/kernel_tests/net_smp.rs` | 10 用例 + loopback fixture + poll hook（参考 DragonOS NAPI 线程模型） | deep |
| `os/src/fs/page_cache.rs` | 注入 `PageCacheTestHook`（仅 ktest 构造实例生效，生产 None，零行为变化） | quick |
| `os/src/net/config.rs` | 注入 `NetPollTestHook`（同前，仅 ktest 生效） | quick |
| `os/src/kernel_tests/mod.rs` | 注册 `fs_smp`/`net_smp` 组（唯一共享文件，由一人改） | quick |

### 硬约束（写给所有 agent）
- **禁止运行 make/cargo/qemu**——构建只能由主线程串行执行（双架构共享 nightly + 生成状态）
- 只改自己名下的文件；`mod.rs` 由指定 agent 独占
- ktest 是零盘 MTTCG：fs 测试只用 tmpfs/ramfs/内存块设备，net 测试只用 loopback（ktest 启动已初始化）
- 参考实现：fs 对照 Linux tmpfs/dcache 语义；net 对照 DragonOS `driver/net` 的 per-device 锁与 NAPI 线程
- hook 只在 ktest 构造的实例注入，生产路径必须零行为变化（`Option<fn>` 判断，不引入锁）
- SMP 注释用中文；不得 workaround

### 验证（主线程串行执行）
1. `CORE_NUM=8` 双架构串行 build（rv64 → la64），exit 0
2. `make -C os ktest-run ARCH=rv64 PROFILE=normal CORE_NUM=4 KTEST=fs_smp` — 看到 RED
3. `make -C os ktest-run ARCH=rv64 PROFILE=normal CORE_NUM=4 KTEST=net_smp` — 看到 RED
4. 保护性用例统计：运行数 / 通过数 / 失败数，无 panic、runner 正常收尾
5. evidence 归档 `docs/Work_Log/evidence/2026-08-04/fs-net-smp-wp1-*`

### 状态
- [ ] plan agent WP1 操作分解（wave 顺序）
- [ ] deep: fs_smp.rs
- [ ] deep: net_smp.rs
- [ ] quick: page_cache.rs hook
- [ ] quick: net config.rs hook
- [ ] quick: mod.rs 注册
- [ ] 主线程串行 build + ktest RED 验证
- [ ] evidence + Work Log（mango-workflow A→D）

## 批次 2 = WP2（PageCache 正确性，L3/T3，完成后停审 + §8.2 门禁）

### 固定契约（两个 agent 并行，先定 API）
**A: os/src/fs/page_cache.rs（agent A 独占，含非 ext4 调用方 tmpfs/ramfs 适配）**
- `PageEntry` 增加 `data: RwLock<()>`；`as_slice()/as_slice_mut()` 收窄为仅 `with_bytes/with_bytes_mut` closure 内可用的私有 unsafe helper，slice 不得逃逸
- `PageCache` 增加 `op_gate: RwLock<()>`：read/write/writeback 取 read；truncate/invalidate/evict 取 write
- 新 API：`read_kernel(offset, dst)`、`write_kernel(offset, src, old_size)`（多页固定升序、一次只持一页 data lock）、`read_at_user/write_at_user`（有界 bounce，uaccess 在锁外）
- ktest hook 保留（entry 获取后、拷贝前触发）；`PageCache::new()/new_with_test_hook` 不变
- dirty 标记移出 inner 锁（mark_dirty_after_copy）

**B: os/src/fs/ext4/*（agent B 独占）**
- `Ext4OSInode` 增加 `io_txn: Mutex<()>`；写路径：io_txn → snapshot → ensure_blocks → page_cache.write_kernel → commit 实际 size/times（非请求 len）；失败回滚
- truncate：io_txn → page_cache.truncate_with_backend
- 用户写走 bounce（write_at_user）；锁序固定 io_txn → op_gate（禁止反向）
- ext4 调用方适配 A 的新 API

### 依赖
B 依赖 A 的 API 名（契约已定，可并行）；tmpfs/ramfs 调用方归 A。

### 验证（主线程串行）
1. 双架构 CORE_NUM=8 build
2. fs_smp ktest：**测试 1 转 GREEN**（原 RED），其余 GREEN，无 panic
3. ext4/tmpfs 既有 ktest 组不回归
4. **§8.2 门禁**：CORE_NUM=8 mask=0x003，RV64 后 LA64，基线 RV64 312/314、LA64 308/314
5. evidence 归档 fs-net-smp-wp2-* + Work Log

### 状态
- [ ] agent A: page_cache.rs 重构
- [ ] agent B: ext4 io_txn + API 适配
- [ ] 双架构 build + fs_smp GREEN 验证
- [ ] ext4/tmpfs ktest 回归
- [ ] §8.2 门禁 + evidence + Work Log

## 批次 3 = WP3（目录锁协议，L3/T3，完成后停审 + §8.2 门禁）

### 契约（设计 §4）
**ext4（os/src/fs/ext4/ext4fs.rs + layout.rs + 目录相关，agent A 独占）**
- `Ext4FileSystem` 增加 `rename_gate: Mutex<()>` + `inode_gates: Mutex<BTreeMap<u32, Weak<RwLock<()>>>>`；`inode_gate(ino)` canonicalize 同 inode 多 wrapper
- 单目录操作：find/create/unlink 持 dir_gate read/write；children/negative-dentry/lookup cache 只能在 parent gate 后
- 跨目录 rename：rename_gate → ancestor-first 否则 inode_id 序 parents → dir victims 优先 → 非目录按 inode_id → 磁盘 rename → 发布；同目录 rename 不取 rename_gate
- reclaim 只能 try_lock 或 clone Weak，禁止反向取 parent gate；复用现有 InodeLock

**tmpfs（os/src/fs/tmpfs/mod.rs，agent B 独占）**
- `LockedTmpFSInode(pub Mutex<...>)` → `pub RwLock<...>`；TmpFS 增加 rename_gate
- find/list/metadata read；create/unlink/rmdir parent write；跨目录 rename 同固定顺序

### 验证（主线程串行）
双架构 CORE_NUM=8 build → fs_smp（8/8 GREEN 保持）→ ext4/tmpfs ktest 回归 → **§8.2 门禁**（derived-competition，基线 RV64 312/314、LA64 308/314）→ evidence fs-net-smp-wp3-* + Work Log

### 状态
- [x] agent A: ext4 inode gate registry + rename（实现完成，等待主线程串行构建/ktest/§8.2 门禁）
- [ ] agent B: tmpfs RwLock + rename_gate
- [ ] 双架构 build + ktest 回归
- [ ] §8.2 门禁 + evidence + Work Log

## 已完成批次
- WP1（ktest RED 基线）✅ 2026-08-04
- WP2（PageCache op_gate + PageEntry.data + bounce + ext4 io_txn）✅ §8.2 PASS（RV64 312/314、LA64 308/314）

## 批次 4 = WP4（per-netns PortRegistry，L3/T3，完成后停审 + §8.2 门禁）

### 目标（设计 §7）
- `net_smp::port_reserve_exactly_once` → GREEN（现 RED："concurrent bind reserved one UDP endpoint more than once"）
- `net_smp::udp_reuse_release_exact_owner` → GREEN（现 RED："closing one UDP reuse owner removed the whole port bucket"）
- 其余 net_smp 保持现状（irq/poll RED 属 WP6）

### 契约（设计 §7 伪代码）
- `NetNamespace` 增加 `ports: Mutex<PortRegistry>`；PortRegistry：next_ephemeral、next_token、buckets: BTreeMap<PortKey, Vec<PortOwner>>
- PortKey = protocol + family + address(Option=wildcard) + port + ifindex(Option)；PortOwner = token + Weak<socket> + state(Reserved/Bound) + reuse_addr/reuse_port/ipv6_v6only
- 事务：锁内 reserve（含 ephemeral 选择 + check_conflict + token）→ 锁外 socket.bind → 锁内 commit/abort；commit/abort/release 按 (key, token, Weak identity) 匹配
- 冲突规则：wildcard 与同 family 具体地址冲突；IPv6 wildcard 非 v6only 与 IPv4 冲突；reuse 按双方快照；TCP/UDP key 独立；netns 隔离；Reserved 参与冲突
- UDP release 只删对应 owner（修复整桶误删）；显式与 ephemeral 共用线性化点
- bind options/endpoint 先锁内快照成 BindIntent，锁外进入 registry

### 验证（主线程串行）
双架构 CORE_NUM=8 build → net_smp ktest（2 个 port RED → GREEN，其余不变）→ fs_smp/ext4 回归 → **§8.2 门禁** → evidence fs-net-smp-wp4-* + Work Log

### 状态
- [ ] agent: PortRegistry 事务 + bind 路径接入
- [ ] 双架构 build + net_smp GREEN 验证
- [ ] fs_smp/ext4 回归 + §8.2 门禁 + evidence

## 已完成批次
- WP1 ktest RED 基线 ✅
- WP2 PageCache ✅ §8.2 PASS（312/314、308/314）
- WP3 目录锁 ✅ §8.2 PASS（312/314、308/314）

## 批次 5 = WP5（per-device DeviceStack，L3/T3，完成后停审 + §8.2 门禁）

### 目标（设计 §5）
- net_smp::route_handle_reuse_rejected / per_stack_poll_progress 由保护性转真实（FAIL-before 语义就位）
- 其余 net_smp 保持（irq RED 属 WP6）
- 单一全局 NET_INTERFACE 锁 → route directory（短持有）+ Arc<Mutex<DeviceStackCell>>

### 契约（设计 §5 伪代码）
- `NetInterface { directory: Mutex<NetDirectory>, next_route_id: AtomicUsize, poll: NetPollControl }`（test_poll_hook 保留）
- `DeviceStackCell { ifindex, state: AtomicU8, inner: Mutex<DeviceStackInner> }`；inner 含 Interface+SocketSet+bindings+dhcp
- route 访问：directory 锁内查 entry（state==Active + protocol）→ clone Arc → 解锁 → stack 锁内重验 local binding（route+protocol+handle）
- remove：directory Active→Draining/remove → stack 锁内摘 binding/socket；add：stack 锁内先建 binding 再对读者发布 route；跨 stack rebind 两阶段（Migrating → source remove → target add → Active），不同时持两把 stack 锁
- RouteSocketHandle 单调不复用；socket 操作先取目标 stack 再重验

### 验证（主线程串行）
双架构 build → net_smp（route/per-stack 转真实，irq RED 保持）→ fs_smp/ext4 回归 → §8.2 门禁 → evidence fs-net-smp-wp5-*

### 状态
- [ ] agent: DeviceStack 拆分 + route 重验
- [ ] 双架构 build + net_smp 验证 + 回归
- [ ] §8.2 门禁 + evidence

## 已完成批次
- WP1 ktest RED 基线 ✅
- WP2 PageCache ✅ §8.2 PASS
- WP3 目录锁 ✅ §8.2 PASS
- WP4 PortRegistry ✅ §8.2 PASS

## 批次 6 = WP6（generation poll worker，L3/T3，完成后停审 + §8.2 门禁）

### 目标（设计 §6）
- `net_smp::irq_poll_is_publish_only` → GREEN（现 RED："IRQ poll blocked after releasing try_lock"）
- 删除 hard-IRQ poll；`try_poll_irq` 变 publish-only（IrqAfterTryLockBeforeDrop hook → IrqBeforePublish）

### 契约（设计 §6 伪代码）
- `NetPollControl { requested: AtomicU64, completed: AtomicU64, deferred_wake: AtomicBool, wait_queue: Mutex<WaitQueue> }`
- kick_from_task：requested.fetch_add(Release) + wait_queue.wake_all()（task 上下文）
- kick_from_irq：requested.fetch_add(Release) + deferred_wake=true（IRQ 只置位，禁 poll/WaitQueue/分配/打印）
- run_deferred_net_wake：仅安全点调用，swap deferred_wake 后 wake_all
- net_poll_worker：pin CPU0；WaitQueue wait 条件 requested!=completed；逐 stack 有界 poll（try_lock，busy 跳过）；完成后 completed.store(target)；poll 期间新请求不被旧 target 覆盖
- no-lost-wake 六点证明（设计 §6.3）；10ms fallback timer 仅兜底不作正确性
- socket/epoll 接入：syscall 路径 target-stack + kick；EventPoll::scan 不内联 poll（改 kick + 状态扫描）；Waiter 条件闭包不 poll

### 验证（主线程串行）
双架构 build → net_smp（**10/10 全绿**）→ fs_smp/ext4 回归 → §8.2 门禁 → evidence fs-net-smp-wp6-*

### 状态
- [ ] agent: poll worker + socket/epoll 接入
- [ ] 双架构 build + net_smp 10/10
- [ ] 回归 + §8.2 门禁 + evidence

## 已完成批次
- WP1 ktest RED 基线 ✅
- WP2 PageCache ✅ §8.2 PASS
- WP3 目录锁 ✅ §8.2 PASS
- WP4 PortRegistry ✅ §8.2 PASS
- WP5 DeviceStack ✅ §8.2 PASS

## 批次 7 = WP7（Phase-5 汇总门禁 + 文档收尾，T3 阶段门禁）

### 范围（设计 §11 WP7）
1. 双架构 CORE_NUM=8 依次跑 mask 矩阵：0x010(iozone)/0x020(unixbench)/0x040(iperf)/0x200(netperf)/0x800(LTP) → 合并 0xA70 → §8.2 0x003（最后阶段出口）
2. 更新 lock-order.md、FS/Net 文档、AGENTS 能力边界、Work Log；不声明 per-socket 并行、lock-free mount、默认全核用户调度
3. evidence fs-net-smp-phase5-*（raw judge、semantic judge、failure multiset、性能三次中位数）

### 状态
- [x] mask 矩阵优先集 + `0xA70` + `0x003`（完成：**FAIL**；`0xA70` 双架构 timeout，RV64 `0x003` `292/314`）
- [x] 文档收尾 + evidence + Work Log（完整矩阵和性能三次中位数为 NOT RUN；见 `fs-net-smp-phase5-{manifest,verdict}.md`）

## 已完成批次（全部 §8.2 PASS）
- WP1 ktest RED 基线 ✅
- WP2 PageCache ✅ 312/314、308/314
- WP3 目录锁 ✅ 312/314、308/314
- WP4 PortRegistry ✅ 312/314、308/314
- WP5 DeviceStack ✅ 312/314、308/314
- WP6 poll worker ✅ 312/314、308/314（含 boot 卡死修复）

### WP7 最终结论（2026-08-04 收尾）
- **验收门禁（§8.2 0x003 = basic+busybox）：双架构 PASS** — RV64 312/314、LA64 308/314（tools-disk 工件修复后复验）
- mask 矩阵（0x010/0x020/0x040/0x200/0x800/0xA70）：**按用户决定取消**，非验收标准；0xA70 尝试在 LTP 段 40 分钟超时。覆盖由各 WP §8.2 门禁 + 零盘 ktest（fs_smp 8/8、net_smp 10/10、ext4 7/7 双架构）替代
- 已知限制：矩阵未完整执行；不声称的性能结论（iperf/netperf/iozone 基线）未产出

## 批次 8 = 8 项审计改进（Oracle 审查后，2026-08-04）

### Oracle 裁决
1(REAL潜伏/S2→6) 2(REAL/S1→5) 3(REAL/S1→1) 4(REAL刻意/S0→7) 5(**S0数据丢失**→3,含M1 msync空操作+M2 filemap逃逸) 6(REAL/S1→4) 7(REAL/S1→2 quick) 8(当前NOT-AN-ISSUE；develop分批导入)

### 批次结构（Oracle 顺序）
- **A（本批并行）**: A1 lwext4 全局门（quick，ext4_lwext4/*）; A2 测试真实性（deep，kernel_tests/*）
- **B**: MAP_SHARED 工作包（mm/filemap + page_cache + mprotect + msync，关键路径）
- **C**: poll 契约（net/config + sockets + eventpoll + syscall）
- **D**: auto-bind reservation（port.rs + stream/udp + syscall）
- **E**: native ext4 canonical inode txn（ext4fs/layout）
- **F**: 开放普通用户多核 affinity + 全量门禁
- 每批：双架构 build + 相关 ktest + 视情况 §8.2

### 状态
- [ ] A1 lwext4 全局门
- [ ] A2 测试真实性（SKIP 机制 + AP 违规修复 + 真实交错）
- [ ] B MAP_SHARED
- [ ] C poll 契约
- [ ] D auto-bind
- [ ] E ext4 事务
- [ ] F affinity + 门禁

## 用户决策（2026-08-04）：item 4 降级
- **用户任务跨核（item 4）不做**——非主要工作；验收标准 = basic+busybox（§8.2 mask=0x003）跑通
- 批次 F（affinity 开放）取消，改为"每个生产批次后跑 §8.2 门禁确认 basic+busybox 不回归"
- 剩余批次：C（poll 契约）→ D（auto-bind）→ E（native ext4 事务）→ 最终门禁

## 当前批次：批次 A+B 后门禁（basic+busybox）
- 目的：确认 MAP_SHARED/FaultOutcome/lwext4 门的生产改动未破坏用户路径
- 状态：[ ] 运行中（gate agent）
