# Learnings - Net Subsystem Upgrade

## DeviceStack Refactoring Pattern

- DeviceStack now uses `Arc<dyn Iface>` for metadata (nic_id, name, flags) instead of duplicated fields
- smoltcp Interface and SocketSet remain directly on DeviceStack for poll loop performance (no Mutex overhead)
- `NetDeviceEntry` in net_core serves as registry entry; DeviceStack's `Arc<dyn Iface>` may reference the same object
- When net_core has eth0 registered (NIC present), both DeviceStack and registry share the same `Arc<dyn Iface>`
- When no NIC, DeviceStack creates a local `NetDeviceEntry` just for metadata (not in registry)
- Arc<VethInterface> auto-coerces to Arc<dyn Iface> — works in no_std (CoerceUnsized)

## sys_setns() fd → NetNamespace Resolution Pattern

- `NetNsFile` wraps `Arc<NetNamespace>` as an `IndexNode` (following pidfd.rs pattern)
- `File::new(inode, flags)` creates the fd-able file; use O_RDONLY (open for reading in Linux)
- In sys_setns: resolve fd → downcast `MountFSInode::unwrap_inode()` → `downcast_ref::<NetNsFile>()`
- Switch via `task.process.acquire_inner_lock().net = new_ns` (access ProcessInner directly)
- nstype validation: accept 0 (auto-detect) or CLONE_NEWNET=0x40000000; reject everything else with EINVAL
- Future /proc/[pid]/ns/net should return the same `NetNsFile` inode type for seamless downstream compatibility

## LA64 Cross-Compiler

- LA64 build fails with `linker loongarch64-linux-gnu-gcc not found` in Docker container
- This is a pre-existing environment issue, not caused by code changes
- RV64 build is the primary verification target
