# RV64 PCI ECAM/BAR 未映射导致启动崩溃（已修复）

## 触发条件（历史）

将 rv64 `BLK_MODE` 从 `virt` 切换到 `virt_pci`，QEMU 设备从 `virtio-blk-device` 切换为 `virtio-blk-pci`，内核在 PCI 枚举阶段崩溃并重启：

```
[PCI] ECAM base: 0x30000000
[kernel] logger inited, level= Off    ← 二次启动（崩溃后重启）
[kernel] Console initialized.
...
[kernel] block device 0: official fs (MMIO 0x10001000)  ← 回退到 MMIO
```

## 根因

旧实现的预堆 FDT 解析只收集节点 `reg`，而 PCI host 的 BAR MMIO window 存在于 `ranges`。因此即使 host ECAM `reg` 可见，驱动为 VirtIO PCI BAR 分配并访问的 memory range 仍可能没有恒等映射；SV39 首次访问会触发 page fault。

同时，RV64 驱动将 ECAM 写死为 `0x3000_0000`，这与 firmware 描述平台资源的设计不一致。以下旧版常量列表说明了当时的症状，但不再是修复方式：

```rust
// os/src/hal/platform/riscv/qemu.rs
pub const MMIO: &[(usize, usize)] = &[
    (0x1000_1000, 0x1000), // virtio-mmio bus.0
    (0x1000_2000, 0x1000), // virtio-mmio bus.1
    (0x1000_8000, 0x1000), // virtio-mmio bus.7 (net)
    // 缺少：
    // (0x3000_0000, 0x1000_0000), // PCIe ECAM — 未映射！
    // (0x4000_0000, 0x4000_0000), // PCIe BAR 窗口 — 未映射！
];
```

`MmioCam::new()` 过去使用硬编码 `PCI_ECAM_BASE` 作为虚拟地址指针；没有对应映射时，第一次 PCI config read 会触发 page fault 并重启。

**为什么 `.toml` 配置里有但实际没映射？** `riscv64-qemu-virt.toml` 中的 `mmio-regions` 字段定义了 QEMU 设备树的布局，但内核实际使用的 `MMIO` 常量是独立硬编码在平台文件中的，并非从 toml 动态加载。

**为什么 la64 没问题？** La64 使用 DMW（Direct Mapping Window）直接映射了全部物理地址空间，不依赖 `MMIO` 常量列表。RV64 使用 SV39 页表，需要显式映射每个区域。

## 修复

1. `parse_pci_mmio_ranges()` 在预堆阶段严格解析 PCI `ranges` 的 28-byte QEMU entry，仅接纳 memory space、非零且不溢出的范围，并写入 early MMIO buffer。
2. 后堆 `resolve_pci_host()` 从 enabled/valid `pci-host-ecam-generic` 节点解析 ECAM `reg` 与 memory `ranges`，构造 `PciHost`。
3. RV64 `pci_ecam_base()` 优先读取 `PlatformInfo::pci_host()`；仅 FDT 未提供可用 host 时 warning 并回退到 `0x3000_0000`。

这项修复没有修改 `MMAP_BASE` 或其他地址布局常量，也没有改变 LA64 的静态 PCI 路径。

## 验证

- `make kernel ARCH=rv64 PROFILE=normal` ✅
- `make kernel ARCH=la64 PROFILE=normal` ✅
- `make test ARCH=rv64 PROFILE=regression` ✅（`=== REGRESSION PASS ===`）
- `timeout --foreground 90 make run ARCH=rv64 PROFILE=normal BLK_MODE=virt_pci` ✅（发现 VirtIO PCI block device）

## 相关文件

- `os/src/hal/firmware/fdt.rs` — PCI `ranges` early mapping 和 post-heap host 解析
- `os/src/hal/platform/info.rs` — `PciHost` 与 `PlatformInfo::pci_host()`
- `os/src/drivers/block/virtio_blk_pci.rs` — FDT ECAM 选择与 warning-only fallback
- `os/src/kernel_tests/platform_fdt_snapshot.rs` — QEMU PCI host snapshot test

## 归类

- **根因**: PCI `ranges` 未纳入预堆 MMIO 映射，且 RV64 ECAM 基址硬编码
- **严重度**: PCI VirtIO 路径不可用
- **修复状态**: 已修复；验证证据见 `docs/Work_Log/evidence/2026-07-31/`
