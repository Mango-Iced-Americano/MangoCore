# FS/Net SMP 适配设计（Oracle 产出，2026-08-04）

> 依据：Linux 6.6 directory-locking / inet_hashtables、DragonOS VFS/net 结构、smoltcp 0.10 API、MangoCore-smp 本地源码（file:line 已核验）。
> 流程：WP1 ktest RED 基线 → WP2..WP6 逐个 L3/T3 停审 → WP7 Phase-5 门禁。
> 本文档为决策输入与实施蓝图，工作树保持未暂存未提交。

## Bottom line

采用“**先补 RED 测试与所有权缺口，再按 inode/DeviceStack 拆锁，最后引入 task-context poll worker**”的单一路径。PageCache 和网络分别以 per-page 数据锁、per-device smoltcp 锁为并行边界；`MOUNT_LIST` 保持现状，同一 `SocketSet` 不承诺 per-socket 并行。

本设计基于本地源码只读核验；外部资料仅用于语义和锁模型对照。未执行构建或 QEMU，所有运行结论均列为后续门禁。

---

# 1. 已确认的当前问题

## 1.1 PageCache 用户写绕过 `io_gate`

`PageCache::write()` 在 `os/src/fs/page_cache.rs:1204-1217` 获取 `io_gate`，但 `write_user()` 在 `1468-1563` 直接取得 `PageEntry` 后，通过 `as_slice_mut(&self)` 修改页数据。

这造成两个问题：

1. `write_user()` 可与 `truncate_with_backend()`（`2056-2114`）并发，truncate 可能先摘除 entry，随后 writer 向已脱离缓存的 entry 写入并返回成功。
2. `PageEntry::as_slice_mut(&self)`（`381-383`）从共享引用产生可变 slice；多个 `Arc<PageEntry>` 并发访问时缺少 Rust 可证明的独占边界。

ext4 正在直接调用该路径，并在数据复制前发布请求长度对应的新 EOF：`os/src/fs/ext4/ext4fs.rs:1139-1207`。

## 1.2 `try_poll_irq()` 不保证非阻塞

`try_poll_irq()` 在 `os/src/net/config.rs:764-795`：

1. `try_lock()` 成功；
2. 立即释放 guard；
3. 调用 `poll_once(false)`；
4. `poll_once()` 经 `inner_handler()` 再执行普通阻塞式 `.lock()`。

另一个 CPU 可在步骤 2、3 之间取得锁，导致所谓 IRQ-safe 路径在步骤 4 自旋等待。该路径目前由 CPU0 deferred timer 调用：`os/src/task/manager.rs:2301-2305`。

---

# 2. 总体所有权与锁序

不建立虚假的全局总序。调度、FS、Net 分属三个锁域，跨域操作必须“快照—解锁—进入下一域”。

| 层级 | 锁/状态 | 规则 |
|---|---|---|
| Scheduler-S0 | `task.inner`、`TASK_MANAGER`、单个 runqueue | 不得与 FS/Net 锁嵌套；任何 wake 都在业务锁释放后执行 |
| FS-F0 | per-FS `rename_gate` | 仅跨目录 rename 获取 |
| FS-F1 | parent directory gate | ancestor-first；无祖先关系时按 `inode_id` |
| FS-F2 | child/victim inode metadata | 目录优先于非目录，同类按 `inode_id` |
| FS-F3 | children/negative-dentry/lookup cache | 只能在对应 parent gate 后获取 |
| IO-I0 | per-inode `io_txn` | ext4 extent、EOF、truncate 的事务锁 |
| IO-I1 | PageCache `op_gate` | read/write/writeback 共享；truncate/invalidate/evict 独占 |
| IO-I2 | `entries` → `inner` | 保留当前顺序；二者释放后才获取 page data lock |
| IO-I3 | `PageEntry.data` | 只保护该页 frame bytes 和页内有效范围 |
| Net-N0 | `PortRegistry` 或 route directory | 两者都只做短状态提交；不得与 `DeviceStack` 嵌套 |
| Net-N1 | OS socket lifecycle lock，如 `TcpSocket.inner` | 状态转换可随后访问一个 DeviceStack |
| Net-N2 | 单个 `DeviceStack` | 同一时刻最多一把；内部独占 `Interface + SocketSet + device` |
| Net-N3 | event queue、EventPoll、WaitQueue | 必须在 socket/DeviceStack 释放后通知 |
| Leaf | `OUTPUT_LOCK` | 所有格式参数先在锁外快照；FS/Net/task 锁释放后才能打印 |

禁止关系：

```text
FS/Net lock -> task.inner / runqueue / context switch / IPI wait    禁止
DeviceStack -> TcpSocket.inner / EventPoll / WaitQueue              禁止
PortRegistry -> socket.bind() / DeviceStack                          禁止
directory gate -> faultable uaccess                                  禁止
PageEntry.data -> entries / inner / user copy                        禁止
business lock -> OUTPUT_LOCK                                         禁止
```

---

# 3. PageCache 与文件 I/O 设计

## 3.1 数据结构

```rust
struct PageEntry {
    page: Arc<FrameTracker>,

    // 保护 frame bytes。任何 &[u8]/&mut [u8] 都不能逃出 guard closure。
    data: RwLock<()>,

    state: AtomicU8,
    valid_mask: AtomicU8,
    flags: AtomicU8,
}

struct PageCache {
    // 普通读写和 writeback 取 read；
    // truncate/invalidate/evict/publish-after-I/O 取 write。
    op_gate: RwLock<()>,

    entries: Mutex<Vec<Option<Arc<PageEntry>>>>,
    inner: Mutex<InnerPageCache>,

    backend: Mutex<Option<Arc<dyn PageCacheBackend>>>,
    inode: Mutex<Option<Weak<dyn IndexNode>>>,
    unevictable: AtomicBool,
    clock_hand: AtomicUsize,
}

impl PageEntry {
    fn with_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        let _guard = self.data.read();
        let bytes = self.page.ppn.get_bytes_array();
        f(bytes)
    }

    fn with_bytes_mut<R>(&self, f: impl FnOnce(&mut [u8]) -> R) -> R {
        let _guard = self.data.write();
        let bytes = self.page.ppn.get_bytes_array();
        f(bytes)
    }
}
```

`as_slice()`/`as_slice_mut()` 删除或收窄为只能在上述 closure 内调用的私有 unsafe helper。不得让 slice 被保存到 `Vec<&[u8]>` 后脱离 guard；批量 writeback 应逐页持 data-read guard，或构造持 guard 的局部 `WritebackPage<'a>`。

## 3.2 普通读写

```rust
fn read_kernel(
    &self,
    offset: usize,
    dst: &mut [u8],
) -> Result<usize, SyscallErr> {
    let _op = self.op_gate.read();

    // entries 锁内只定位并 clone Arc。
    let plan: Vec<ReadCopy> = self.build_read_plan(offset, dst.len())?;

    for item in plan {
        item.entry.with_bytes(|src| {
            dst[item.dst_range]
                .copy_from_slice(&src[item.page_range]);
        });
    }
    Ok(dst.len())
}

fn write_kernel(
    &self,
    offset: usize,
    src: &[u8],
    old_size: usize,
) -> Result<usize, SyscallErr> {
    let _op = self.op_gate.read();
    let plan = self.get_write_entries(offset, src.len(), old_size)?;

    // 多页固定按 page_index 升序，但一次仅持一把 page data lock。
    for item in plan {
        item.entry.with_bytes_mut(|dst| {
            dst[item.page_range]
                .copy_from_slice(&src[item.src_range]);
        });
        item.entry.mark_valid(item.page_offset, item.len);
        self.mark_dirty_after_copy(item.page_index, &item.entry);
    }

    Ok(src.len())
}
```

`entries`/`inner` 不能跨实际字节复制。这样不同页可并行，同页读写由 `PageEntry.data` 串行。

## 3.3 用户态读写

用户 copy 不能位于 PageCache、inode、socket 或 file-private 锁内。所有 PageCache-backed direct-user 路径改成 bounded bounce：

```rust
fn read_at_user(
    &self,
    offset: usize,
    len: usize,
    user: &mut UserBuffer,
) -> Result<usize, SyscallErr> {
    let count = len.min(IO_CHUNK_SIZE);
    let mut bounce = try_zeroed_vec(count)?;

    // FS/page locks只覆盖 kernel buffer。
    let read = self.page_cache.read_kernel(offset, &mut bounce)?;

    // 所有FS锁已释放。
    user.write_from_at(0, &bounce[..read])
        .map_err(|_| SyscallErr::EFAULT)
}

fn write_at_user(
    &self,
    offset: usize,
    len: usize,
    user: &UserBuffer,
) -> Result<usize, SyscallErr> {
    let count = len.min(IO_CHUNK_SIZE);
    let mut bounce = try_zeroed_vec(count)?;

    // 先 fault/copy；此处不能持 inode/PageCache 锁。
    let copied = user.read_into_at(0, &mut bounce)
        .map_err(|_| SyscallErr::EFAULT)?;

    self.write_at(offset, copied, &bounce[..copied], ...)
}
```

这与 tmpfs 当前 bounce 模式 `os/src/fs/tmpfs/mod.rs:461-519` 一致，并关闭 ext4 `write_user()` 的直写缺口。

## 3.4 ext4 EOF/extent 事务

```rust
struct Ext4OSInode {
    inode: Arc<Mutex<Ext4InodeRef>>,

    // 普通文件 extent、EOF、truncate 的唯一事务边界。
    io_txn: Mutex<()>,

    dir_gate: Arc<RwLock<()>>,
    page_cache: Mutex<Option<Arc<PageCache>>>,
    // ...
}

fn write_at_bounced(
    &self,
    offset: usize,
    bytes: &[u8],
) -> Result<usize, SyscallErr> {
    let _txn = self.io_txn.lock();

    let before = self.snapshot_inode();
    let old_size = before.size();
    let end = offset.checked_add(bytes.len())
        .ok_or(SyscallErr::EINVAL)?;

    // io_txn -> PageCache op_gate，truncate 必须使用相同方向。
    self.ensure_blocks_allocated(offset, bytes.len())?;

    let page_cache = self.get_new_page_cache()
        .ok_or(SyscallErr::EIO)?;

    let written = match page_cache.write_kernel(offset, bytes, old_size) {
        Ok(n) => n,
        Err(error) => {
            self.restore_inode_snapshot(before);
            page_cache.rollback_failed_extension(old_size);
            return Err(error);
        }
    };

    // 只发布实际成功前缀，而不是请求 len。
    self.commit_size_and_times(max(old_size, offset + written))?;
    Ok(written)
}

fn truncate(&self, new_size: usize) -> Result<(), SyscallErr> {
    let _txn = self.io_txn.lock();
    let page_cache = self.get_new_page_cache()
        .ok_or(SyscallErr::EIO)?;

    page_cache.truncate_with_backend(new_size, || {
        self.truncate_inode_persistent(new_size)
    })?;

    self.commit_size(new_size)
}
```

第一阶段允许同一 ext4 inode 的数据写串行，PageCache 本身仍支持不同页并行。只有 extent allocator 和 EOF 被独立证明可并行后，才考虑缩窄 `io_txn`；本设计不预先增加该复杂度。

## 3.5 writeback 与 truncate

```rust
fn writeback_page(&self, index: usize) -> Result<(), SyscallErr> {
    let _op = self.op_gate.read();
    let entry = self.clone_entry(index)?;

    if !entry.cas_dirty_to_writeback() {
        return Ok(());
    }

    let result = entry.with_bytes(|bytes| {
        self.backend_clone()?.write_page(index, bytes)
    });

    self.finish_writeback(index, entry, result)
}

fn truncate_with_backend(
    &self,
    new_size: usize,
    persistent: impl FnOnce() -> Result<(), SyscallErr>,
) -> Result<(), SyscallErr> {
    let _op = self.op_gate.write();

    persistent()?;

    let tail = {
        let mut entries = self.entries.lock();
        let mut inner = self.inner.lock();
        let tail = detach_pages_after(&mut entries, &mut inner, new_size);
        tail
    };

    if let Some((entry, offset)) = tail {
        entry.with_bytes_mut(|bytes| bytes[offset..].fill(0));
    }
    Ok(())
}
```

writer 与 writeback 的合法交错：

- writeback 先取得 data-read：writer 设置 `PG_REDIRTIED` 后等待；旧内容写回，writer 随后修改，页面保留 Dirty。
- writer 先取得 data-write：writeback 随后读取新内容；即使观察到 `PG_REDIRTIED`，最多产生额外一次 writeback，不会丢数据。
- truncate 取得 `op_gate.write()` 后，普通 read/write/writeback 均不能跨越。

---

# 4. 目录锁协议

## 4.1 ext4 gate 必须按真实 inode 共享

当前 `Ext4OSInode` 可能存在同 inode 的多个 wrapper；仅在 wrapper 构造时 `Arc::new(RwLock)` 不能证明互斥。gate 由 `Ext4FileSystem` 按 inode number canonicalize：

```rust
struct Ext4FileSystem {
    rename_gate: Mutex<()>,

    inode_gates: Mutex<BTreeMap<u32, Weak<RwLock<()>>>>,
    // existing caches...
}

impl Ext4FileSystem {
    fn inode_gate(&self, ino: u32) -> Arc<RwLock<()>> {
        let mut table = self.inode_gates.lock();

        if let Some(gate) = table.get(&ino).and_then(Weak::upgrade) {
            return gate;
        }

        let gate = Arc::new(RwLock::new(()));
        table.insert(ino, Arc::downgrade(&gate));
        gate
    }
}
```

`Ext4OSInode::new_vfs()` 从 registry 取得 gate。registry 锁只用于 gate 生命周期，不与 gate 本身嵌套。

## 4.2 单目录操作

```rust
fn find(&self, name: &str) -> Result<Arc<dyn IndexNode>, SyscallErr> {
    let _parent = self.dir_gate.read();

    if let Some(hit) = self.children_lookup(name) {
        return Ok(hit);
    }

    let child_ino = self.disk_lookup(name)?;
    let child = self.ext4fs.canonical_inode_object(child_ino);

    // parent read gate仍有效，目录版本不会被 writer 改变。
    self.children_insert(name, &child);
    Ok(child)
}

fn create(&self, name: &str, ty: FileType, mode: InodeMode)
    -> Result<Arc<dyn IndexNode>, SyscallErr>
{
    let _parent = self.dir_gate.write();

    self.ensure_absent_locked(name)?;
    let child = self.disk_create_locked(name, ty, mode)?;

    self.bump_dir_version();
    self.clear_negative_dentry(name);
    self.children_insert(name, &child);
    Ok(child)
}

fn unlink(&self, name: &str) -> Result<(), SyscallErr> {
    let _parent = self.dir_gate.write();

    let victim = self.lookup_locked(name)?;
    let _victim = victim.dir_or_inode_gate().write();

    self.validate_unlink_type(&victim)?;
    self.disk_unlink_locked(name, &victim)?;

    self.bump_dir_version();
    self.children_remove(name);
    self.insert_negative_dentry(name);
    Ok(())
}
```

`children`、negative dentry 和底层 directory lookup cache 均排在 parent gate 后。reclaim 只能 `try_lock` 或先从 registry clone Weak，不能反向取得 parent gate。

## 4.3 跨目录 rename

```rust
fn rename(
    old_parent: &Ext4OSInode,
    old_name: &str,
    new_parent: &Ext4OSInode,
    new_name: &str,
    flags: u32,
) -> Result<(), SyscallErr> {
    let fs = &old_parent.ext4fs;
    ensure_same_fs(fs, &new_parent.ext4fs)?;

    // 冻结跨目录祖先关系。
    let _rename = fs.rename_gate.lock();

    let order = fs.order_parents_ancestor_first_else_ino(
        old_parent,
        new_parent,
    )?;

    let (_first, _second) = lock_two_dir_write(order);

    // 取得锁后必须重新查找，锁前快照不作为提交依据。
    let source = old_parent.lookup_locked(old_name)?;
    let target = new_parent.lookup_optional_locked(new_name)?;

    validate_rename_flags_and_types(flags, &source, target.as_ref())?;
    reject_descendant_cycle_locked(&source, new_parent)?;

    // 目录 victim 优先；同类按 inode_id。
    let _victims = lock_victims_stable(&source, target.as_ref());

    let rollback = RenameRollback::snapshot(
        old_parent,
        new_parent,
        old_name,
        new_name,
        &source,
        target.as_ref(),
    );

    if let Err(error) = fs.apply_disk_rename_locked(...) {
        rollback.restore_locked();
        return Err(error);
    }

    old_parent.publish_rename_source_removed(old_name);
    new_parent.publish_rename_target(new_name, &source);
    update_dotdot_and_nlinks_if_directory(...);
    finalize_overwritten_target(target)?;

    Ok(())
}
```

固定顺序：

```text
cross-FS check
  -> per-FS rename_gate
  -> parent directories: ancestor-first, otherwise lower inode_id first
  -> directory victims
  -> non-directory victims by inode_id
  -> children / negative dentry / lookup caches
  -> unlock all
  -> deferred drop, wake, log
```

同目录 rename 不取 `rename_gate`，只取一次 parent write gate。

## 4.4 tmpfs

`LockedTmpFSInode(pub Mutex<TmpFSInode>)` 改为 `RwLock<TmpFSInode>`，`TmpFS` 增加 `rename_gate`。tmpfs children 中始终保存同一个 `Arc<LockedTmpFSInode>`，因此不需要 ext4 式 gate registry。

```rust
pub struct LockedTmpFSInode(pub RwLock<TmpFSInode>);

pub struct TmpFS {
    root_inode: Arc<LockedTmpFSInode>,
    rename_gate: Mutex<()>,
    // quota...
}
```

`find/list/metadata` 用 read；create/unlink/rmdir 用 parent write；跨目录 rename 使用与 ext4 相同的 `rename_gate → ordered parents → ordered victims`。

---

# 5. Per-device 网络栈设计

smoltcp 0.10 的 `Interface::poll()` 和 `SocketSet::get_mut()` 都要求 `&mut SocketSet`。因此安全并行边界是整个 `DeviceStack`，不是单 socket。

## 5.1 数据结构

```rust
struct NetInterface {
    directory: Mutex<NetDirectory>,
    next_route_id: AtomicUsize,
    poll: NetPollControl,
}

struct NetDirectory {
    stacks: BTreeMap<u32, Arc<DeviceStackCell>>,
    routes: BTreeMap<RouteSocketHandle, RouteDirectoryEntry>,
}

struct RouteDirectoryEntry {
    stack: Weak<DeviceStackCell>,
    protocol: InetProtocol,
    state: RouteState,
}

enum RouteState {
    Active,
    Migrating,
    Draining,
}

struct DeviceStackCell {
    ifindex: u32,
    state: AtomicU8, // Active / Draining / Dead
    inner: Mutex<DeviceStackInner>,
}

struct DeviceStackInner<'a> {
    nic: Arc<dyn Iface>,
    device: IfaceDevice,
    iface: Interface,
    sockets: SocketSet<'a>,

    // 在与 SocketSet 相同的锁域内重验。
    bindings: BTreeMap<RouteSocketHandle, LocalSocketBinding>,

    dhcp_handle: Option<SocketHandle>,
    pending_dhcp_event: Option<DhcpLeaseEvent>,
}

struct LocalSocketBinding {
    handle: SocketHandle,
    protocol: InetProtocol,
}
```

`RouteSocketHandle` 继续单调递增且不复用。即使 smoltcp `SocketHandle` slot 被复用，route ID 重验也能阻止旧 route 访问新 socket。

## 5.2 路由访问与重验

```rust
fn routed_tcp<R>(
    &self,
    route: RouteSocketHandle,
    op: impl FnOnce(&mut tcp::Socket) -> R,
) -> Option<R> {
    let stack = {
        let dir = self.directory.lock();
        let entry = dir.routes.get(&route)?;
        if entry.state != RouteState::Active
            || entry.protocol != InetProtocol::Tcp
        {
            return None;
        }
        entry.stack.upgrade()?
    }; // directory released

    let mut stack_guard = stack.inner.lock();

    let local = stack_guard.bindings.get(&route)?;
    if local.protocol != InetProtocol::Tcp {
        return None;
    }

    let socket = stack_guard
        .sockets
        .get_mut::<tcp::Socket>(local.handle);

    Some(op(socket))
}
```

删除的线性化点是 route directory 中 `Active → Draining/removed`。已经取得 stack Arc 的调用者：

- 若先取得 stack lock，则操作在线性化于 remove 前；
- 若 remove 先删除 local binding，则调用者重验失败；
- 不会访问复用后的 smoltcp slot。

## 5.3 add/remove/rebind

```rust
fn add_socket<T: AnySocket>(
    &self,
    ifindex: u32,
    protocol: InetProtocol,
    socket: T,
) -> Result<RouteSocketHandle, SyscallErr> {
    let stack = self.stack_arc(ifindex)?;
    let route = RouteSocketHandle(
        self.next_route_id.fetch_add(1, Ordering::Relaxed)
    );

    {
        let mut guard = stack.inner.lock();
        ensure_stack_active(&stack)?;
        let handle = guard.sockets.add(socket);
        guard.bindings.insert(
            route,
            LocalSocketBinding { handle, protocol },
        );
    }

    {
        let mut dir = self.directory.lock();
        // local binding已建立后才对读者发布。
        dir.routes.insert(route, RouteDirectoryEntry {
            stack: Arc::downgrade(&stack),
            protocol,
            state: RouteState::Active,
        });
    }

    Ok(route)
}

fn remove_socket(&self, route: RouteSocketHandle) {
    let entry = {
        let mut dir = self.directory.lock();
        dir.routes.remove(&route)
    };

    let Some(stack) = entry.and_then(|e| e.stack.upgrade()) else {
        return;
    };

    let removed = {
        let mut guard = stack.inner.lock();
        guard.bindings.remove(&route)
            .map(|binding| guard.sockets.remove(binding.handle))
    };

    // Arc/Socket析构在 stack 锁外。
    drop(removed);
}
```

跨 stack rebind：

```text
directory: Active -> Migrating
  -> unlock directory
  -> source stack: remove exact route/socket; unlock
  -> target stack: add replacement + local binding; unlock
  -> directory: Migrating -> Active(target)
```

失败则按相反方向恢复 source；任何时刻不同时持有两把 `DeviceStack` 锁。

---

# 6. 专用 poll worker 与 no-lost-wake 证明

## 6.1 数据结构与入口

不使用容易出现 clear/set ABA 的单一 `bool`，采用单调 generation：

```rust
struct NetPollControl {
    requested: AtomicU64,
    completed: AtomicU64,

    // hard IRQ只置位，由安全点转换成WaitQueue wake。
    deferred_wake: AtomicBool,

    wait_queue: Mutex<WaitQueue>,
}

fn kick_from_task(control: &NetPollControl) {
    control.requested.fetch_add(1, Ordering::Release);

    // 当前是task context，且未持DeviceStack/socket锁。
    control.wait_queue.lock().wake_all();
}

fn kick_from_irq(control: &NetPollControl) {
    control.requested.fetch_add(1, Ordering::Release);
    control.deferred_wake.store(true, Ordering::Release);

    // 禁止poll、WaitQueue、分配、打印。
}

fn run_deferred_net_wake(control: &NetPollControl) {
    // 只从现有task/idle安全点调用，进入时无业务锁。
    if control.deferred_wake.swap(false, Ordering::AcqRel) {
        control.wait_queue.lock().wake_all();
    }
}
```

`try_poll_irq()` 删除或退化为 `kick_from_irq()`，不再触碰 `NET_INTERFACE`/smoltcp 锁。

## 6.2 Worker

```rust
fn net_poll_worker() -> ! {
    pin_current_to_cpu(BOOT_CPU_ID);

    let control = &NET_INTERFACE.poll;
    let mut observed = control.completed.load(Ordering::Acquire);

    loop {
        let wait = WaitQueue::wait_event_interruptible(
            &control.wait_queue,
            || {
                let requested = control.requested.load(Ordering::Acquire);
                (requested != observed).then_some(1)
            },
        );

        if matches!(wait, WaitResult::Interrupted) {
            handle_lifecycle_stop_or_continue();
        }

        loop {
            let target = control.requested.load(Ordering::Acquire);

            poll_each_stack_bounded();

            control.completed.store(target, Ordering::Release);
            observed = target;

            // poll期间产生的新请求不得被旧target覆盖。
            if control.requested.load(Ordering::Acquire) == target {
                break;
            }
        }
    }
}

fn poll_each_stack_bounded() {
    let stacks = NET_INTERFACE.snapshot_stack_arcs();

    for stack in stacks {
        let outcome = {
            let mut guard = match stack.inner.try_lock() {
                Some(guard) => guard,
                None => {
                    record_stack_busy(stack.ifindex);
                    continue;
                }
            };

            poll_stack_with_packet_and_cycle_budget(&mut guard)
        }; // DeviceStack released

        commit_dhcp_events(outcome.dhcp);
        refresh_socket_pollee(outcome.changed_routes);
        wake_socket_and_epoll_waiters(outcome.changed_routes);
    }
}
```

worker 遇到 busy stack 时跳过并重新发布一次 request，不能让单个 stack 饥饿其他 stack。

## 6.3 no-lost-wake

1. **producer 先 set，worker 尚未登记**：worker 的首次条件检查看到 `requested != observed`。
2. **producer 在首次检查后、登记前 set**：`WaitQueue` 在 `prepare_to_wait()` 后持 queue lock 再检查条件，见 `os/src/task/manager.rs:1167-1204`。
3. **task producer 在登记后 set**：producer 先 Release 增加 generation，再取得同一 WaitQueue 并 wake；waiter 已可见。
4. **hard IRQ 在登记窗口 set**：IRQ 只保留 generation 和 `deferred_wake`；task/idle 安全点在 waiter 完成 Blocking/Blocked 发布后执行 wake。
5. **poll 期间 set**：worker 只把 `completed` 写为 poll 开始时快照的 `target`；结束后发现 `requested != target`，不睡眠而继续 poll。
6. **延迟的旧 wake**：最多产生一次空唤醒；generation 相等时 worker重新等待，不会重复消费状态。

10ms fallback timer可以保留为故障兜底，但不得作为正确性证明。

---

# 7. 端口 `reserve → bind → commit/abort`

## 7.1 每 netns registry

```rust
struct NetNamespace {
    id: u64,
    device_list: Mutex<BTreeMap<usize, Arc<dyn Iface>>>,
    router: Mutex<Router>,
    ports: Mutex<PortRegistry>,
}

struct PortRegistry {
    next_ephemeral: u16,
    next_token: u64,

    // protocol在key中，因此TCP/UDP可使用相同数字端口。
    buckets: BTreeMap<PortKey, Vec<PortOwner>>,
}

#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
struct PortKey {
    protocol: TransportProtocol,
    family: AddressFamily,
    address: Option<IpAddress>, // None = wildcard
    port: u16,
    ifindex: Option<u32>,
}

struct PortOwner {
    token: u64,
    socket: Weak<dyn Socket>,
    state: PortOwnerState,
    reuse_addr: bool,
    reuse_port: bool,
    ipv6_v6only: bool,
}

enum PortOwnerState {
    Reserved,
    Bound,
}

struct PortReservation {
    namespace: Weak<NetNamespace>,
    key: PortKey,
    token: u64,
}
```

bind options和 endpoint 先在 socket 锁内快照成 kernel-owned `BindIntent`，释放 socket 锁后才进入 registry。

## 7.2 事务

```rust
fn reserve(
    ns: &Arc<NetNamespace>,
    mut intent: BindIntent,
    socket: &Arc<dyn Socket>,
) -> Result<PortReservation, SyscallErr> {
    let mut registry = ns.ports.lock();
    registry.prune_dead_owners();

    if intent.port == 0 {
        intent.port = registry.select_free_ephemeral(&intent)?;
    }

    registry.check_conflict(&intent)?;

    let token = registry.allocate_nonzero_token()?;
    let key = PortKey::from_intent(&intent);

    registry.buckets.entry(key.clone()).or_default().push(
        PortOwner {
            token,
            socket: Arc::downgrade(socket),
            state: PortOwnerState::Reserved,
            reuse_addr: intent.reuse_addr,
            reuse_port: intent.reuse_port,
            ipv6_v6only: intent.ipv6_v6only,
        },
    );

    Ok(PortReservation {
        namespace: Arc::downgrade(ns),
        key,
        token,
    })
}

fn bind_port(...) -> SyscallRet {
    let intent = socket.snapshot_bind_intent(endpoint)?;
    let reservation = reserve(&ns, intent, socket)?;

    let actual = reservation.key.to_endpoint();
    match socket.bind(&Endpoint::Ip(actual)) {
        Ok(value) => {
            if let Err(error) = ns.ports.lock().commit(&reservation, socket) {
                socket.rollback_bind();
                ns.ports.lock().abort(&reservation);
                return Err(error);
            }
            socket.install_port_reservation(reservation);
            Ok(value)
        }
        Err(error) => {
            ns.ports.lock().abort(&reservation);
            Err(error)
        }
    }
}
```

`commit()`/`abort()`/`release()`都必须匹配 `(PortKey, token, Weak socket identity)`。UDP release 只删除自己的 owner，不能像当前 `unregister_udp_bind(port)` 一样删除整个端口。

冲突规则：

- wildcard 与同 family 的具体地址冲突；
- IPv6 wildcard 且 `!IPV6_V6ONLY` 时与 IPv4 wildcard/具体地址冲突；
- `SO_REUSEADDR/SO_REUSEPORT` 按双方快照判断；
- TCP 与 UDP key 独立；
-不同 `NetNamespace` registry 完全独立；
- `Reserved` 与 `Bound` 同样参与冲突检查，关闭 check-bind-register 窗口。

---

# 8. Before/after 热路径

## 8.1 文件读

```text
Before
sys_read
  -> clone File Arc, release fd table
  -> File::read_user
  -> inode.read_at_user
  -> PageCache entry Arc
  -> raw page &[u8]
  -> faultable copy_to_user（页数据无同步）

After
sys_read
  -> clone File Arc, release fd table
  -> allocate bounded kernel bounce
  -> inode snapshot size, release inode lock
  -> PageCache op_gate.read
  -> entry.data.read -> copy page to bounce
  -> release page/op locks
  -> copy bounce to user
  -> advance offset by actual copied prefix
```

## 8.2 文件写

```text
Before
sys_write
  -> UserBuffer
  -> File::write_user
  -> ext4 write_at_user
  -> publish requested EOF
  -> PageCache::write_user（无 io_gate）
  -> raw &mut page bytes

After
sys_write
  -> copy user prefix to bounded kernel bounce
  -> inode io_txn
  -> allocate extents
  -> PageCache op_gate.read
  -> entry.data.write -> copy bounce
  -> publish Dirty
  -> release PageCache
  -> commit actual EOF/timestamps
  -> release io_txn
```

## 8.3 网络发送

```text
Before
sys_sendto
  -> NET_INTERFACE.try_poll/full global lock
  -> TcpSocket.inner 或 fast_state
  -> global NetInterface lock
  -> bindings lookup
  -> SocketSet::get_mut
  -> send_slice
  -> 再次全局 poll

After
sys_sendto
  -> user copy到kernel buffer，释放VM锁
  -> TcpSocket.inner 快照 lifecycle/route，释放或保持固定 N1->N2 顺序
  -> route directory clone target，释放 directory
  -> target DeviceStack lock
  -> route ID/protocol/handle 重验
  -> send_slice
  -> 刷新该 socket ready bits
  -> release DeviceStack/socket locks
  -> kick poll worker
  -> 必要时锁外通知EventQueue/epoll
```

## 8.4 网络接收

```text
Before
sys_recvfrom
  -> global try_poll
  -> socket/global NetInterface locks
  -> recv_slice
  -> kernel buffer
  -> copy_to_user

After
sys_recvfrom
  -> target route/DeviceStack重验
  -> recv_slice到kernel buffer
  -> 更新真实 post-recv readiness
  -> release DeviceStack/socket
  -> copy kernel buffer到用户页
  -> 若EAGAIN：kick worker后进入纯条件WaitQueue
```

`EventPoll::scan()` 当前在 `os/src/fs/eventpoll.rs:149-173` 直接调用 `NET_INTERFACE.poll()`；改为 `kick_from_task()` + 基于现有 pollee/event queue 的状态扫描，WaitQueue 条件闭包不得 poll。

---

# 9. ktest 设计

ktest 为 zero-drive：RV64/LA64 profile 都不带 `NET_DEV`，见 `os/make/{rv64,la64}.mk:18-24`。但 ktest 启动仍执行 `drivers::init_net_device(); net::config::init();`，因此有 loopback：`os/src/main.rs:141-177`。

FS 使用 `TmpFS::new()`/`RamFS::new()` 自建对象；ext4 使用现有内存块设备 fixture。所有 helper 由 `spawn_ktest_task_on()` 固定 CPU，runner 保持 CPU0。

## 9.1 `fs_smp`

| 测试名 | setup/交错 | FAIL-before | PASS-after |
|---|---|---|---|
| `fs_smp::pagecache_user_write_vs_truncate` | CPU1 在 `write_user` 取得 entry 后暂停；CPU0 truncate；再放行 writer | writer 成功但 entry 已被摘除、数据/EOF丢失 | truncate 与 writer 由 `op_gate` 排序，结果等价于完整先后顺序 |
| `fs_smp::pagecache_same_page_no_torn_copy` | 两 CPU 对同页写完整 A/B pattern，读者持续校验 | 拆锁错误时出现混合 pattern | 每次快照只能为完整 A 或 B |
| `fs_smp::pagecache_writeback_redirty` | CPU0 writeback，CPU1 在 backend hook 期间重写 | 丢 Dirty、永久 Writeback 或写入丢失 | `PG_REDIRTIED` 与计数闭环 |
| `fs_smp::ext4_create_same_name_exactly_once` | 2/8 CPU 同时 create 同名 | 多个调用成功、目录缓存/磁盘不一致 | 一次成功，其余 `EEXIST` |
| `fs_smp::ext4_cross_rename_opposite_order` | 两 CPU 执行 A/x→B/x 与 B/y→A/y，原子 barrier 同时进入 | 死锁、双丢失、双发布 | 有界完成，目录树满足一种串行结果 |
| `fs_smp::tmpfs_lookup_unlink_generation` | reader 循环 lookup/list，writer unlink/recreate 同名不同 inode | stale Arc/negative dentry 命中新对象 | 每代 inode identity 自洽 |
| `fs_smp::truncate_tail_zero_after_extend` | truncate 到半页后并发 extend/read | 旧尾部数据重新可见 | 新扩展区全零 |
| `fs_smp::different_page_parallel_progress` | CPU1 锁住 page0 hook，CPU2 写 page1 | 全 cache gate 使 page1 无进展 | page1 在 page0 data lock 持有时完成 |

第一个 race 需要窄范围 ktest hook：

```rust
trait PageCacheTestHook {
    fn after_write_entry_acquired(&self, page: usize);
}
```

仅在 ktest 构造的 PageCache 注入，生产实例为 `None`。不得用 sleep 扩大概率窗口。

## 9.2 `net_smp`

| 测试名 | setup/交错 | FAIL-before | PASS-after |
|---|---|---|---|
| `net_smp::irq_poll_is_publish_only` | hook 固定旧 `try_lock→drop→poll_once` 窗口，CPU1 抢全局锁 | IRQ模拟调用阻塞超过 deadline | 调用只递增 generation，固定上界返回 |
| `net_smp::port_reserve_exactly_once` | 2/8 CPU 同时 bind 同 endpoint | check/register 分离导致多次成功 | 仅一个成功，其余 `EADDRINUSE` |
| `net_smp::udp_reuse_release_exact_owner` | 两个 reuse owner，关闭其中一个 | 整个 UDP port bucket 被删除 | 仅对应 token 消失 |
| `net_smp::tcp_udp_same_numeric_port` | TCP/UDP 同 port | 当前 ephemeral/global 检查错误排斥 | 两协议均成功 |
| `net_smp::namespace_port_isolation` | 两个 netns 同 endpoint | 全局表产生假冲突 | 两边均成功 |
| `net_smp::route_handle_reuse_rejected` | 旧 route lookup 与 remove/new socket slot reuse 精确交错 | 旧 route 修改新 socket | stack 内 route ID 重验失败 |
| `net_smp::per_stack_poll_progress` | loopback + ktest veth；stack A hook 暂停 | 全局锁阻止 B 进展 | B 的 progress counter 增长 |
| `net_smp::poll_worker_no_lost_wake` | 分别在首次检查、登记、Blocking、poll中发 kick | worker 睡死或需偶然 timer 才恢复 | generation 最终 `completed == requested` |
| `net_smp::tcp_dual_sender_exact_bytes` | loopback server/client；CPU1/2 同一 socket 发送带序号 frame | 数据缺失、重复、交叉破坏 | 每 frame 完整且各出现一次 |
| `net_smp::epollet_concurrent_edge` | receiver drain 到 EAGAIN；另一 CPU 发送并触发 edge | ready edge 丢失或永久 runnable | 每次 empty→ready 恰能重新唤醒 |

需要 syscall/epoll ABI 的用例复用 `smp.rs` 的 dual-arch raw user probe 模式：

1. 构造 ktest PCB 和 fd table；
2. 插入 tmpfs file/socket/epoll fd；
3. 映射双架构小型用户 probe 与共享 checkpoint 页；
4. 设置 `cpus_allowed`；
5. CPU0/CPU1 通过 checkpoint 精确同步 raw syscall；
6. parent 等待 PCB 最终状态，而非只观察 TCB Zombie。

所有测试必须检查：

- helper exactly-once；
- task 最终 `Zombie`；
- `current` 与 runqueue 清空；
- timeout 是 FAIL，不接受“任一结果”；
- repeat 前清理 registry、route、port、PageCache 和 hook。

---

# 10. Regression / L5

## 10.1 mask

| bit | mask | 用途 |
|---|---:|---|
| bit4 | `0x010` | iozone：PageCache、truncate、writeback |
| bit5 | `0x020` | unixbench：文件、pipe、调度固定开销 |
| bit6 | `0x040` | iperf：TCP throughput/poll fairness |
| bit9 | `0x200` | netperf：RR/CRR、小包 syscall 延迟 |
| bit11 | `0x800` | LTP：syscalls、fs、fs_perms_simple、fs_readonly、fcntl-locktests |
| 合并 | `0xA70` | Phase-5 FS/Net broad gate，不替代 `0x003` |

日常先分 mask 定位，阶段出口再运行 `0xA70`。不得日常运行 `0xFFF`。

## 10.2 CORE_NUM=8 串行方法

对每个 mask 生成未跟踪临时配置，记录其哈希：

```text
mode=run
mask=<0x010|0x020|0x040|0x200|0x800|0xA70>
ltp_runner=suite
ltp_libc=both
ltp_suites=syscalls,fs,fs_perms_simple,fs_readonly,fcntl-locktests
```

执行顺序：

```bash
make -C os conf-inject CONF_ARCH=rv64 CONF_BLK_MODE=virt \
  CONF_FILE=<frozen-config>
CORE_NUM=8 make run ARCH=rv64 PROFILE=normal

# RV64完全结束并归档后
make -C os conf-inject CONF_ARCH=la64 CONF_BLK_MODE=virt_pci \
  CONF_FILE=<frozen-config>
CORE_NUM=8 make run ARCH=la64 PROFILE=normal
```

全部构建/QEMU 在 Docker 内；运行前记录容器 ID、mount、QEMU 版本和源码/runner/config 指纹。

## 10.3 场景与判定

### 同文件并发读写

- 精确竞态由 `fs_smp` 使用 `pread/pwrite`，避免把 open-file offset 语义与 PageCache 混为一谈。
- L5 运行 iozone + focused LTP truncate/fsync/rename/fcntl。
- 判定：无 torn pattern、旧 tail 泄漏、Dirty/Writeback 残留、文件尺寸回退、panic 或 timeout。

### 同 socket 双 sender

- 精确 framing 由 `net_smp::tcp_dual_sender_exact_bytes`。
- L5 使用 iperf parallel TCP。
- 判定：每个 sequence frame exactly-once；无永久 EAGAIN、lost wake 或 reset；iperf 3 次同环境中位数不低于冻结基线 95%，任何单次低于 90% 均需调查。

### epoll 并发

- raw syscall probe：drain→EAGAIN 后由另一 CPU 发送。
- focused LTP epoll/eventfd/socket。
- 判定：empty→ready 不丢 edge，不持续虚假 ready，不在 WaitQueue condition 内 poll。

### 端口抢占

- 精确 reserve race 由 `net_smp`。
- focused LTP bind/listen/reuse tests。
- 判定：非 reuse 恰一成功；UDP exact-owner release；netns 与 TCP/UDP 隔离。

### 性能信号

- iperf 与 netperf 必须同时报告，不能互相替代。
- unixbench/iperf/netperf 使用同容器、同 QEMU、同配置的 3 次中位数。
- 默认门槛：中位数 ≥ 基线 95%；低于门槛先关闭诊断探针重跑，不能直接接受。
- 计数器默认关闭；只在 profile window 开启，防止探针污染。

### §8.2

每个改变普通用户路径的 L3 包及最终 Phase 出口都运行：

- `CORE_NUM=8`
- normal profile
- `mask=0x003`
- RV64 后 LA64
- `configured=8`、`online_mask=0xff`
- 四组 START/END 与 exit 0
- judge 识别 314 点
- RV64 semantic ≥ 312/314
- LA64 semantic ≥ 308/314
- failure identity multiset 不扩大

---

# 11. Action plan：7 个工作包

## 1. Harness 与 RED 基线 — L2/T2

- 新增 `fs_smp`/`net_smp`、barrier、hook、loopback/tmpfs/memblk fixture。
- 必须实际看到 `pagecache_user_write_vs_truncate` 与 `irq_poll_is_publish_only` RED；其余保护性测试可先 GREEN。
- 验证：双架构 build，CORE_NUM=1/2 focused；不触发 §8.2。
- 证据：`manifest.md`、source/runner/config hash、双架构 build、RED serial log、case inventory。

## 2. PageCache + ext4/tmpfs uaccess — L3/T3，完成后停审

- 引入 `op_gate`/`PageEntry.data`/bounded bounce；ext4 `io_txn` 关闭 EOF/extent 回滚窗口。
- ktest：PageCache 四项、tail-zero、different-page progress。
- regression：focused ext4 ktest、LTP truncate/fsync、iozone `0x010`。
- §8.2：必须。
- 证据前缀：`fs-net-smp-b2-pagecache-*`，含完整 QEMU、计数器前后值和 RED→GREEN 对照。

## 3. ext4/tmpfs 目录锁 — L3/T3，完成后停审

- ext4 inode gate registry、per-FS rename gate、tmpfs RwLock、固定 rename 顺序。
- ktest：same-name create、lookup/unlink generation、opposite rename、rollback。
- regression：LTP fs/fs_perms_simple/fs_readonly、rename/link/unlink/rmdir。
- §8.2：必须。
- 证据前缀：`fs-net-smp-b3-directory-*`，附锁序图和超时/回滚 fixture。

## 4. per-netns PortRegistry — L3/T3，完成后停审

- 实现 reservation token、协议/netns 隔离、exact-owner release；删除 check-bind-register 窗口。
- ktest：port race、reuse release、TCP/UDP same port、netns isolation。
- regression：focused bind/listen/reuse LTP。
- §8.2：必须。
- 证据前缀：`fs-net-smp-b4-port-*`，保存 token/owner 终态和失败注入。

## 5. per-device DeviceStack — L3/T3，完成后停审

- route directory、stack-local bindings、route revalidation、remove/rebind 两阶段协议。
- ktest：stale route、slot reuse、cross-stack progress。
- regression：loopback socket、netperf `0x200` 无诊断 baseline。
- §8.2：必须。
- 证据前缀：`fs-net-smp-b5-stack-*`，记录热结构尺寸和锁 busy/progress。

## 6. Poll worker + socket/epoll 接入 — L3/T3，完成后停审

- 删除 hard-IRQ poll；实现 generation worker；TCP/UDP/RAW/EventPoll 改为 target-stack + kick。
- ktest：lost-wake 全窗口、dual sender、EPOLLET edge、poll starvation。
- regression：focused socket/epoll LTP、iperf `0x040`、netperf `0x200`。
- §8.2：必须。
- 证据前缀：`fs-net-smp-b6-poll-*`，含 requested/completed/wake 序列与无探针性能基线。

## 7. Phase-5 broad gate 与文档 — T3 阶段门禁，完成后停审

- 双架构 `CORE_NUM=8` 依次运行 `0x010/0x020/0x040/0x200/0x800`，最后 `0xA70` 和 §8.2 `0x003`。
- 更新 `lock-order.md`、FS/Net 文档、AGENTS 能力边界、Work Log；不声明 per-socket 并行、lock-free mount 或默认全核用户调度。
- 证据前缀：`fs-net-smp-phase5-*`，含 raw judge、semantic judge、完整 failure multiset 和性能三次中位数。

目录格式统一：

```text
docs/Work_Log/evidence/YYYY-MM-DD/
  fs-net-smp-<batch>-manifest.md
  fs-net-smp-<batch>-source-fingerprint.txt
  fs-net-smp-<batch>-environment.txt
  fs-net-smp-<batch>-commands.log
  fs-net-smp-<batch>-rv64-build.log
  fs-net-smp-<batch>-la64-build.log
  fs-net-smp-<batch>-rv64-ktest.log
  fs-net-smp-<batch>-la64-ktest.log
  fs-net-smp-<batch>-rv64-qemu.log
  fs-net-smp-<batch>-la64-qemu.log
  fs-net-smp-<batch>-judge-raw.json
  fs-net-smp-<batch>-judge-semantic.json
  fs-net-smp-<batch>-verdict.md
```

---

# 12. 外部对照边界

本地已核验的是上述 MangoCore 文件和行号。外部来源只采纳职责边界，不声称已逐行移植：

- Linux 6.6 directory locking：  
  https://docs.kernel.org/6.6/filesystems/directory-locking.html
- Linux 6.6 bind hash参考：  
  https://github.com/torvalds/linux/blob/v6.6/net/ipv4/inet_hashtables.c
- smoltcp 0.10 `Interface::poll`：  
  https://github.com/smoltcp-rs/smoltcp/blob/v0.10.0/src/iface/interface/mod.rs
- smoltcp 0.10 `SocketSet`：  
  https://github.com/smoltcp-rs/smoltcp/blob/v0.10.0/src/iface/socket_set.rs
- DragonOS MountFS结构参考：  
  https://github.com/DragonOS-Community/DragonOS/blob/master/kernel/src/filesystem/vfs/mount/mod.rs

未采纳：

- Linux RCU dcache/mount namespace复杂度；
- Linux 完整 inet hash/table 分层；
- DragonOS 与 MangoCore 不一致的 VFS 生命周期；
- smoltcp 外部补丁或内部 unsafe 并行拆分。

---

# 13. 依赖图与 Effort

```text
WP1 Harness/RED
 ├─> WP2 PageCache/uaccess ──> WP3 directory locks ──┐
 └─> WP4 PortRegistry ───────> WP5 DeviceStack ──> WP6 poll/socket
                                                     │
WP2 + WP3 + WP4 + WP5 + WP6 ───────────────────────> WP7 Phase-5 gate
```

**Effort：Large（预计 8–12 个工程日）**。WP2、WP3、WP4、WP5、WP6 和 WP7 都是 L3/T3 人工停审点；工作树默认保持未暂存、未提交。

`mango-workflow: loaded, references: harness-patterns.md, debugging-patterns.md`
