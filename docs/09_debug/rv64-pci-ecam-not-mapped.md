# rv64 PCI ECAM 未映射导致启动崩溃

## 触发条件

将 rv64 `BLK_MODE` 从 `virt` 切换到 `virt_pci`，QEMU 设备从 `virtio-blk-device` 切换为 `virtio-blk-pci`，内核在 PCI 枚举阶段崩溃并重启：

```
[PCI] ECAM base: 0x30000000
[kernel] logger inited, level= Off    ← 二次启动（崩溃后重启）
[kernel] Console initialized.
...
[kernel] block device 0: official fs (MMIO 0x10001000)  ← 回退到 MMIO
```

## 根因

`kernel_space.rs` 在初始化内核页表时遍历 `crate::hal::MMIO` 常量来建立恒等映射，而该常量定义在 `os/src/hal/platform/riscv/qemu.rs`，**只包含 UART、virtio-mmio、PLIC，缺少 PCI ECAM 和 PCI BAR 窗口**：

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

`MmioCam::new(PCI_ECAM_BASE)` 使用 `0x3000_0000` 作为虚拟地址指针，但由于该地址未映射 → 第一次 PCI config read 触发 page fault → 内核崩溃。

**为什么 `.toml` 配置里有但实际没映射？** `riscv64-qemu-virt.toml` 中的 `mmio-regions` 字段定义了 QEMU 设备树的布局，但内核实际使用的 `MMIO` 常量是独立硬编码在平台文件中的，并非从 toml 动态加载。

**为什么 la64 没问题？** La64 使用 DMW（Direct Mapping Window）直接映射了全部物理地址空间，不依赖 `MMIO` 常量列表。RV64 使用 SV39 页表，需要显式映射每个区域。

## 影响

PCI 改动仅为基础设施（已提交到 `perf/fs` 分支），`rv64.mk` 始终保持 `BLK_MODE := virt`（MMIO）。**所有基准测试均在 MMIO 模式下运行，无正确性退化。**

## 修复

在 `os/src/hal/platform/riscv/qemu.rs` 的 `MMIO` 数组中添加：

```rust
(0x3000_0000, 0x1000_0000), // PCIe ECAM
(0x4000_0000, 0x4000_0000), // PCIe 32-bit MMIO BAR window
```

## 验证

- 添加映射后 rv64 QEMU 使用 `virtio-blk-pci` 应能正常启动
- 需同时添加 `-no-reboot` 到 QEMU 参数以捕获首次 panic 信息

## 相关文件

- `os/src/hal/platform/riscv/qemu.rs` — MMIO 常量定义（需修改）
- `os/src/mm/kernel_space.rs:219-226` — MMIO 恒等映射逻辑
- `os/src/drivers/block/virtio_blk_pci.rs:27-30` — PCI_ECAM_BASE cfg 选择
- `os/src/hal/configs/riscv64-qemu-virt.toml:56-62` — .toml 中的 PCI MMIO 声明（仅供参考，非实际映射来源）

## 归类

- **根因**: PMM / 页表映射缺失
- **严重度**: 功能不可用（实验性特性，未在生产启用）
- **修复难度**: Low（加 2 行常量）
