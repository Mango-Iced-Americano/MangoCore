# VF2 JH7110 FDT + DesignWare MMC 实现路线图

> **状态:** 已达成架构决策的记录，非实施追踪。
> **日期:** 2026-07-30
> **分支:** feat/vf2-board-support
> **读者:** 后续接手 VF2 MMC 或 FDT 驱动开发的 AI agent / 开发者

---

## 1. 背景与目标

本路线图记录在 `feat/vf2-board-support` 分支上的架构讨论结果。当前已实现的通用基础设施与待完成的 VF2 特定硬件驱动之间有一条清晰的边界。本文旨在防止上下文丢失，让任何后续 agent 在阅读本文后可安全恢复工作。

**已实施（通用基础设施）:**

| 组件 | 文件 | 功能 |
|------|------|------|
| Boot protocol 检测 | `os/src/hal/boot/mod.rs` | 检测 `RiscvFdt`、`UbootGo`、`LoongArchLegacy`、`Test` 协议 |
| FDT pre-heap 快照 + 内存解析 | `os/src/hal/firmware/{mod,fdt}.rs` | `RiscvFdt` 协议下：`capture_fdt_snapshot()` 验证 DTB magic 并将其完整复制到 `#[link_section = ".data.boot"]` 固定容量字节快照，`parse_memory_regions()` 解析 `/memory/reg` 填充 `MEMORY_BUF`；均在 `mem_clear()` 之前完成，零分配 |
| FDT post-heap 设备枚举 | `os/src/hal/firmware/fdt.rs` | `build_platform_info()` 从 `.data.boot` 快照（`fdt_snapshot()`）构造 `fdt::Fdt`，遍历节点收集 node_path、compatible、status、MMIO ranges、raw properties；**不再从原始 firmware DTB 物理地址读取** |
| DeviceInfo 数据模型 | `os/src/hal/platform/info.rs` | `DeviceInfo`、`DeviceKind`、`MmioRange`、`DeviceStatus`、`ResourceValidity` |
| DeviceManager 查询层 | `os/src/hal/device/manager.rs` | 按 compatible、kind、MMIO 索引查询的只读视图 |
| PlatformPolicy trait | `os/src/hal/platform/mod.rs` | 板级默认 root 设备、init 路径、平台名称 |
| BlockDeviceDescriptor | `os/src/drivers/block/descriptor.rs` | 命名的动态描述符，含 major/minor/role/device |
| Boot block registry | `os/src/fs/boot_block.rs` | `BTreeMap<String, BlockDevice>` 启动注册表 + MBR 分区发布 |
| devfs 批量发布 | `os/src/fs/dev/block.rs` | 原子 devfs 设备节点注册 |
| VF2 PlatformPolicy | `os/src/hal/platform/vf2.rs` | `default_root_device = "/dev/mmcblk0"` |

**待实施（VF2 硬件相关）:**

| 组件 | 状态 | 说明 |
|------|------|------|
| FDT Phase 1 原始资源快照 | **已有** | 所有节点保留稳定 path/parent、原始属性名和字节；HAL 不解释 binding-specific cells |
| DeviceKind::Mmc / DeviceKind::DwMmc | **缺失** | 需要配套 classify_device 分支 |
| JH7110 DesignWare dw_mmc 控制器驱动 | **缺失** | PIO first → IRQ → DMA |
| mmcblk0 发布 | **缺失** | 需要真实卡探测 + LBA0 读取 |
| booti Image 路径 (a0/a1 DTB) | **缺失** | 当前仅 `go` 路径 |
| 实板 DTB 证据采集 | **MMC 节点已采集** | 工作 DTB `f76df9b0` 的 `sdio0`/`sdio1` 已归档；完整 DTB blob 和启动 handoff 仍待实现 |
| QEMU vda/vdb 保留 | **已有** | 确保 QEMU 回归不受 VF2 改动影响 |

---

## 2. 架构决策记录

### 2.1 FDT 模型 v1 边界

**已实现的能力:**

- 深度优先遍历所有 FDT 节点，包括无 `compatible` 的父节点和 provider 节点
- 捕获: 完整节点路径（含 unit-address）、parent path、原始 enabled status、全部 `reg` MMIO range 列表、原始属性名和精确字节
- 不假定 `reg` 地址单元或大小单元的长度；使用 fork 的 `fdt` crate 自动适配
- 按 MMIO 基址升序排序设备，确保 QEMU 探测顺序与 slot 编号一致
- 对每个设备记录 `ResourceValidity`（reg 段能否完整解析）
- `DeviceManager::find_enabled_by_exact_compatible()` 仅做精确 compatible 和 enabled 过滤；驱动通过 `raw_property_exact()` 自行声明所需字节形状，缺失与错误长度分别 fail-closed

**明确不在 v1 范围内的能力:**

| 资源类型 | 原因 |
|---------|------|
| `interrupts` / `interrupt-parent` | HAL 不定义 typed IRQ 语义；未来 IRQ driver 按 binding 解析已保存的 raw bytes |
| `clocks` / `assigned-clocks` | HAL 不定义 phandle/cell 语义，也不硬编码时钟资源；驱动按 binding 解析 raw bytes |
| `resets` | HAL 不定义 reset cell 语义，也不猜测复位序列；驱动按 binding 解析 raw bytes |
| `pinctrl` | JH7110 MMC pinmux 配置通常固化在 U-Boot 或 SPL 中 |
| `dmas` / `dma-names` | DMA 在 IRQ 阶段后才引入，PIO 阶段不需要 |

**结论:** Phase 1 模型产生 `Vec<DeviceInfo>`，其中包含所有节点的 identity/relationship、raw properties、`compatible`、`mmio_ranges` 和 `status` 信息。`DeviceManager` 只提供精确节点筛选；资源的 binding 解释归驱动所有。该快照仍严格要求 `BootProtocol::RiscvFdt`；VF2 的 `UbootGo` 继续走静态 fallback（见第 3 节）。

### 2.2 DesignWare dw_mmc，不是 SDHCI

**决策原因:**

- JH7110 集成的 MMC 控制器是 Synopsys DesignWare `dw_mmc` 核心，非 SDHCI 标准接口
- 寄存器布局和编程模型与 SDHCI 完全不同（FIFO 数据寄存器、CIU/BIU 时钟、DMA 接口 MMC_DMA 而非 ADMA2/SDMA）
- Linux 驱动位于 `drivers/mmc/host/dw_mmc-starfive.c`（`snps,dw-mshc` 兼容），使用 `dw_mmc` 核心框架
- U-Boot MMC 驱动位于 `drivers/mmc/dwc_mmc.c`（`snps,dw-mmc`），也使用 DesignWare IP

**本地参考:** JH7110 没有 `sdhci` 实例。板上有两个 MMC 控制器（mmc0/sdio、mmc1/sd）和一个 SD 控制器，全部通过 DesignWare IP。

**来自 Linux DTS 的兼容字符串（VF2):**

```
mmc@16010000 { /* 对应 ? — 需实板确认地址 */
    compatible = "starfive,jh7110-mmc", "snps,dw-mshc";
    reg = <0x0 0x16010000 0x0 0x10000>;
    interrupts = <65>;
    clocks = <&syscrg JH7110_MMC0>, <&syscrg JH7110_MMC1>;
    clock-names = "biu", "ciu";
    fifo-depth = <0x80>;
    ...
};
```

**注意:** VF2 DTB 实板提取值未知。以下是 VF2 JH7110 的官方参考：

- [Linux DeviceTree JH7110](https://github.com/torvalds/linux/blob/master/arch/riscv/boot/dts/starfive/jh7110.dtsi)
- [Linux dw_mmc-starfive 驱动](https://github.com/torvalds/linux/blob/master/drivers/mmc/host/dw_mmc-starfive.c)
- [JH7110 MMC YAML 绑定](https://github.com/torvalds/linux/blob/master/Documentation/devicetree/bindings/mmc/starfive,jh7110-mmc.yaml)

### 2.3 PIO first → IRQ → DMA

**分阶段路线:**

```
Phase 1: PIO (轮询)
    └─ 读取卡状态寄存器，发送命令，轮询 FIFO
    └─ 读取 LBA0 作为冒烟验证
    └─ 无需中断处理程序

Phase 2: IRQ interrupt-driven
    └─ 注册 PLIC 中断处理程序
    └─ 等待 CMD/DONE/EBE 中断替代轮询
    └─ 需要 FDT v1 IRQ 解析 或 硬编码 IRQ number

Phase 3: DMA (内部 DMA 接口)
    └─ 配置 MMC_DMA 寄存器
    └─ 基于框架的 DMA 传输
    └─ 需要 DMA API 支持
```

**门禁:** Phase 1 完成后，`mmcblk0` 可发布为 `BlockDevice`，并通过 `BlockDeviceDescriptor` 挂载 ext4 根文件系统。Phase 2 和 3 是性能优化，不阻止功能验收。

### 2.4 mmcblk0 仅在真实 LBA0 读取后发布

**决策:** 在未验证 `read_block(0, buf)` 返回 `BlockDeviceResult::Ok` 且缓冲区内容与预期文件系统签名匹配前，驱动程序不得调用 `publish_block_descriptors` 或注册到 boot block registry。

**MUST NOT:** 在未完成卡初始化序列前发布带有假 `size_bytes()` 值的 `BlockDevice`。若卡初始化失败（无卡、CMD0 无响应、OCR 无效），驱动应返回 `Err(BlockDeviceError::DeviceUnavailable)` 并记录日志。

**理由:** 启动块挂载路径依赖 `resolve_block_device("mmcblk0")` 返回的 `BlockDevice`。若返回假设备，ext4 超级块读取将静默失败，导致 initramfs 回退生效但原因隐晦。

### 2.5 booti Image 路线取代 bootelf/go

**关键事实:** RISC-V 标准启动 ABI 要求:

- `a0` — hart ID
- `a1` — DTB 物理地址
- 内核入口状态: 寄存器最高值（Supervisor Mode）、页表 identity mapping、`satp` = 0

U-Boot 的 `bootelf` 和 `go` **不保证** 满足此 ABI。`go` 跳转 kernel 入口时，`a1` 的值是未定义的——它可能是 DTB 地址、垃圾值或零。

**U-Boot `booti` 命令** (`arch/riscv/lib/bootm.c`) 遵循上述 ABI:

```c
// 来自 https://github.com/u-boot/u-boot/blob/master/arch/riscv/lib/bootm.c
static int do_booti(struct cmd_tbl *cmdtp, int flag,
                    int argc, char *const argv[])
{
    // ... 加载 Image 到 load_addr ...
    // 设置 a0=hartid, a1=dtb_addr
    images.os.arch = IH_ARCH_RISCV;
    images.os.boot = IH_OS_LINUX;  // 需验证是否与 MangoCore 预期兼容
    do_bootm_linux(&images, flag, argc, argv, 0);
}
```

**已验证的 U-Boot 能力:** VF2 实板 U-Boot 2021.10（2025-10-10 构建）已提供 `booti [addr [initrd[:size]] [fdt]]`，并明确要求 flat DTB 第三参数。工作 FDT/control FDT 均为 `0xf76df9b0`。完整只读证据见 `docs/Work_Log/evidence/2026-07-30/vf2-live-fdt-mmc-probe.md`。

**当前约束:** MangoCore 构建系统生成 ELF（非 Image），通过 `go` 跳转。后续需要:

1. 修改构建流程，从最终 ELF 生成 `Image`（与 Linux 兼容的头部格式）或构建系统添加 `make ARCH=rv64 PROFILE=normal BOARD=vf2 image-type=raw` 选项
2. 更新 `entry.asm` 以处理 `booti` 传递的 `a0`/`a1`
3. U-Boot 配置启用 `booti` 命令

**参考:** [RISC-V Boot Protocol 文档](https://docs.kernel.org/arch/riscv/boot.html)

### 2.6 DTB 来源: U-Boot 或 TFTP

**VF2 实板 DTB 可能有以下来源:**

| 来源 | 优先级 | 说明 |
|------|--------|------|
| U-Boot internal FDT | `CONFIG_OF_BOARD` | 由 SPL/SPL DT 编译进 U-Boot，可在 `booti` 调用前通过 `fdt` 命令访问 |
| TFTP DTB | `tftpboot ${fdt_addr} vf2.dtb` | 可在 U-Boot shell 中手动加载并传递给 `booti` |
| Kernel 内嵌 DTB | `CONFIG_BUILTIN_DTB` | 不推荐——与 FDT 解耦目标冲突 |

**实板证据要求:**

已从实板工作 FDT 采集 MMC 节点和 `/chosen`。其中 `mmc0` 与 `mmc1` 的别名分别指向 `/soc/sdio0@16010000` 和 `/soc/sdio1@16020000`，两者均为 enabled `snps,dw-mshc`，MMIO ranges 分别为 `[0x16010000, 0x16020000)` 与 `[0x16020000, 0x16030000)`；实际节点不含 `interrupts`、`starfive,sysreg` 或 `data-addr`。完整 DTB blob 与 `/memory` 仍需在 handoff 实装前保存。后续采集可使用：

```bash
# 进入 U-Boot shell 后:
fdt addr ${fdt_addr}          # 设置工作 FDT（若已加载）
fdt list /soc/mmc@...         # 确认节点存在且 status=okay
fdt get value mmc_reg /soc/mmc@... reg   # 提取 reg 值
fdt list /chosen              # 确认 bootargs
```

**MUST NOT:** 在实板直接确认前，不得编写猜测的寄存器地址或兼容字符串。JH7110 DTS 的 GitHub 版本 (`jh7110.dtsi`) 可作参考，但特定电路板变体的地址可能不同。

### 2.7 QEMU vda/vdb 语义保留

**决策:** VF2 MMC 实现不得破坏或改变 QEMU virt 平台的块设备行为。具体要求:

- QEMU `block_virt` 继续通过 `virtio_blk::probe_from_device_manager` 发现 virtio-mmio 设备
- `describe_block_devices` 继续为 virtio 设备生成 `vd*` 名称
- `PLATFORM_POLICY.default_root_device` 有条件的值：QEMU 板上 `/dev/vda`，VF2 板上 `/dev/mmcblk0`
- QEMU 的 `root=/dev/mmcblk0` 选择**不应**尝试探测 virtio-mmio；启动块设备解析应仅通过兼容字符串匹配已知设备

**实现建议:**

```rust
// 在 build_platform_info() 或 init_platform_info 之后，根据 platform_info 模型决定
// QEMU 模型: "riscv-virtio,qemu" → root device = /dev/vda
// VF2 模型: "starfive,visionfive-v2" → root device = /dev/mmcblk0
```

### 2.8 无假设备 / 无猜测寄存器

此为硬性约束，不得例外。

- **不允许:** 在实板 DTB 验证前硬编码 dw_mmc 基地址 `0x16010000`（此为 JH7110 SoC data sheet 的通用参考值，需板级确认）
- **不允许:** 使用非 DesignWare IP 的 MMC/SD 兼容字符串欺骗驱动识别
- **不允许:** 在卡初始化前提供 `mmcblk0` 设备，即使作为占位符
- **允许:** 使用已验证的 GMAC JH7110 寄存器（`SYS_CRG_BASE`、`AON_CRG_BASE`）——它们已在 2026-07-25 和 2026-07-28 的实板测试中确认
- **允许:** 为 ktest（内核自测）提供假的 `FailingBlockDevice` 仅用于测试基础设施路径，不用于生产启动

---

## 3. 当前关键路由限制

### 3.1 `build_platform_info` 对 VF2 返回 `None`

```rust
// os/src/hal/firmware/fdt.rs:240
let bi = crate::hal::boot::boot_info();
if !matches!(bi.protocol, BootProtocol::RiscvFdt) {
    return None;  // ← VF2 (UbootGo) 走此路径
}
```

**影响:** VF2 无法使用 FDT 设备枚举，因为 `build_platform_info` 在协议检查处返回 `None`。当前启用了 FDT 协议的电路板（QEMU RISC-V）依赖此路径，但 VF2 的 `UbootGo` 协议显式排除了它。

**修复选项:**

1. 让 U-Boot 使用 `booti` 命令（见第 2.5 节），配合 `CONFIG_OF_BOARD` 传递有效 DTB → `RiscvFdt` 协议匹配
2. 放宽 `build_platform_info` 的协议检查，使其对 VF2 也尝试解析 FDT（需要验证 `go` 后的 `a1` 是否真有 DTB）

**非决策选项:** 通过 `elf2ima` 或以手动方式预先处理 DTB 供 VF2 使用。

### 3.2 `classify_device` 无 MMC 类别

```rust
// os/src/hal/firmware/fdt.rs:214
fn classify_device(compatible: &[String]) -> DeviceKind {
    // 当前仅识别: ns16550a, virtio,mmio, riscv,plic0, pci-host-ecam-generic
    // MMC/SD 控制器 (snps,dw-mshc 等) → DeviceKind::Other
}
```

**影响:** 即使 FDT 路径对 VF2 可用，dw_mmc 节点也只能归类为 `Other`，无法被 `device_manager.find_block_devices()` 发现（该函数查 `VirtioBlock`）。

**决策:** 本阶段不增加 `DeviceKind::Mmc` 或 `DeviceKind::DwMmc`，也不在 HAL 中识别 `snps,dw-mshc`。未来驱动以 exact compatible 查询取得节点后，自行解析其 binding-owned raw properties。

### 3.3 `describe_block_devices` 仅生成 `vd*` 名称

```rust
// os/src/drivers/block/mod.rs:76
fn virtio_block_name(index: usize) -> Option<String> {
    // 生成 vda, vdb, vdc, ...
}
```

**影响:** VF2 的 dw_mmc 设备应生成 `mmcblk0`，而非 `vda`。

**决策:** 在 MMC 驱动侧生成 `BlockDeviceDescriptor` 时直接指定名称 `mmcblk0`，major = `179`（Linux MMC block major），并跳过 `describe_block_devices` 名称生成函数。该函数仅用于 QEMU virtio 设备。

---

## 4. 分阶段实施建议

### Phase A: FDT 模型 v2 + DTB 证据采集（无 MMC 驱动）

**目标:** 在 VF2 上搭建 FDT 设备发现路径，验证 DTB 可用性。

| 步骤 | 描述 | 证据要求 |
|------|------|----------|
| A1 | VF2 实板 U-Boot 执行 `fdt list /` 和 `fdt list /soc`，抓取完整 DTB 输出 | 完整串口日志，显示节点、compatible、reg、status |
| A2 | 提取 dw_mmc 节点的 reg、interrupts、clocks 等字段 | 与 JH7110.dtsi 的差异清单 |
| A3 | 使用 `booti` 的标准 RISC-V DTB handoff；不放宽 `RiscvFdt` 协议检查 | VF2 ktest 输出 FDT 节点快照 |
| A4 | 保持 HAL binding-agnostic，驱动侧以 exact compatible 取得 raw snapshot | ktest 验证 raw bytes、缺失与错误长度 fail-closed |

### Phase B: PIO dw_mmc 驱动 + mmcblk0 发布

**目标:** 能在 VF2 实板上读取 SD 卡的 LBA0，挂载 ext4 根文件系统。

| 步骤 | 描述 | 证据要求 |
|------|------|----------|
| B1 | 新建 `os/src/drivers/block/dw_mmc.rs`，实现 `BlockDevice` trait | 编译通过，无警告 |
| B2 | 实现卡初始化序列: CMD0→CMD8→ACMD41→CMD2→CMD3→CMD7 | ktest 输出卡状态寄存器值 |
| B3 | 实现 PIO 读取: 使用 MMC_FIFO 寄存器读取单个块 | `read_block(0, &buf)` 返回 `Ok` |
| B4 | 验证 LBA0 内容: 检查 ext4 超级块签名 `0xef53` | 打印超级块魔法值 |
| B5 | 注册 `mmcblk0` 到 boot block registry，挂载 `/sdcard` | QEMU 正常启动 + VF2 实板 shell 到达 |

### Phase C: IRQ + DMA 优化

**目标:** 提升吞吐量，通过 IRQ 驱动的 I/O 减少 CPU 轮询开销。

（此阶段在本路线图中仅概述，具体设计需在 Phase B 完成后重新评估。）

---

## 5. 参考代码路径

### 本地代码（MangoCore）

| 角色 | 路径 |
|------|------|
| FDT pre-heap 快照 | `os/src/hal/firmware/{mod,fdt.rs}` — `populate_memory_regions()` → `capture_fdt_snapshot()`；验证 DTB magic 并通过 `ptr::copy` 写入 `#[link_section = ".data.boot"]` 缓冲区 |
| FDT 设备枚举 | `os/src/hal/firmware/fdt.rs` — `build_platform_info()`（从 `.data.boot` 快照构造 `fdt::Fdt`）→ `enumerate_devices()` → `walk_nodes()` → `classify_device()` |
| 设备分类 | `os/src/hal/firmware/fdt.rs:214` — `classify_device()` |
| DeviceInfo 快照 | `os/src/hal/platform/info.rs` — 节点关系、`RawProperty` 和严格 raw accessors |
| DeviceManager 查询 | `os/src/hal/device/manager.rs` — `find_by_compatible()`, `find_enabled_by_exact_compatible()`, `unique_enabled_compatible()` |
| Boot protocol | `os/src/hal/boot/mod.rs` — `BootProtocol::UbootGo` 用于 VF2 |
| PlatformPolicy | `os/src/hal/platform/vf2.rs` — `default_root_device = "/dev/mmcblk0"` |
| 块设备描述符 | `os/src/drivers/block/descriptor.rs` — `BlockDeviceDescriptor`、`BlockDeviceNode` |
| 块设备 trait | `os/src/drivers/block/block_dev.rs` — `BlockDevice` |
| 块设备探测 | `os/src/drivers/block/mod.rs` — `probe_block_devices()`, `describe_block_devices()` |
| Boot registry | `os/src/fs/boot_block.rs` — `register_boot_block_devices()`, `mount_boot_block_devices()` |
| devfs 发布 | `os/src/fs/dev/block.rs` — `BlockDevInode::from_descriptor()` |
| JH7110 GMAC 驱动参考 | `os/src/drivers/net/gmac_jh7110.rs` — 时钟/复位地址模式 |
| JH7110 MMIO 常量 | `os/src/drivers/net/gmac_jh7110/mmio.rs` — `SYS_CRG_BASE`, `AON_CRG_BASE`, `read_mmio()` |
| VF2 board 配置 | `os/src/hal/platform/riscv/vf2.rs` — `CLOCK_FREQ`, VF2 identity MMIO 映射 |
| 静态设备目录 | `os/src/hal/platform/fallback.rs` — `vf2_devices()` 最小 UART 目录 |
| 当前已实现 ktest | `os/src/kernel_tests/platform.rs`, `platform_resources.rs`, `platform_fdt_snapshot.rs`, `block_publication.rs` |

### 官方参考

| 资源 | URL |
|------|-----|
| RISC-V Boot Protocol | https://docs.kernel.org/arch/riscv/boot.html |
| U-Boot RISC-V bootm (booti) | https://github.com/u-boot/u-boot/blob/master/arch/riscv/lib/bootm.c |
| Linux JH7110 dw_mmc 驱动 | https://github.com/torvalds/linux/blob/master/drivers/mmc/host/dw_mmc-starfive.c |
| JH7110 MMC Devicetree Binding | https://github.com/torvalds/linux/blob/master/Documentation/devicetree/bindings/mmc/starfive,jh7110-mmc.yaml |
| Linux JH7110 DTS | https://github.com/torvalds/linux/blob/master/arch/riscv/boot/dts/starfive/jh7110.dtsi |
| DesignWare dw_mmc 核心驱动 | https://github.com/torvalds/linux/blob/master/drivers/mmc/host/dw_mmc.c |
| U-Boot dw_mmc 驱动 | https://github.com/u-boot/u-boot/blob/master/drivers/mmc/dwc_mmc.c |

---

## 6. 验收门禁

### 基础设施门禁（当前已通过）

- [x] FDT post-heap 全节点快照捕获 node_path/parent/status/MMIO/compatible/raw properties
- [x] 动态 `BlockDeviceDescriptor` 发布至 devfs + boot block registry
- [x] FDT pre-heap `capture_fdt_snapshot()` 将验证过的 DTB 复制到 `#[link_section = ".data.boot"]` 静态缓冲区（2 MiB）；post-heap `build_platform_info()` 从该快照解构，不再读原始 firmware DTB 物理地址
- [x] MMIO排序的 VirtIO 探测顺序（QEMU x0/x1 语义保留）
- [x] VF2 PlatformPolicy 声明 `/dev/mmcblk0`
- [x] `build_platform_info` 的 `RiscvFdt` 限定路由

### Phase A 门禁

- [ ] VF2 U-Boot 输出完整 DTB 节点列表（`fdt list /`）
- [x] dw_mmc 节点 reg/clocks/resets/clock rate/FIFO/bus-width 与明确缺失资源已记录为 raw snapshot ktest fixture
- [ ] `build_platform_info` 可在 VF2 上从 DTB 枚举设备
- [x] HAL 保持 binding-agnostic；不新增 MMC DeviceKind 或分类分支

### Phase B 门禁

- [ ] dw_mmc PIO 卡初始化成功（CMD8 + ACMD41 响应）
- [ ] `read_block(0)` 返回 `Ok` 并打印 ext4 超级块签名
- [ ] `mmcblk0` 发布至 `/dev/mmcblk0`，major=179
- [ ] QEMU ktest `block_publication` 仍保持 5/5 PASS
- [ ] VF2 实板 kernel shell 访问 `/dev/mmcblk0` 和根文件系统

---

## 7. 关键技术参考（JH7110 MMC 寄存器速查）

以下寄存器地址基于 Linux DTS 和 DesignWare 手册，**在 Phase A 实板确认前不得视为已验证**:

| 寄存器 | 偏移 | 功能 |
|--------|------|------|
| CMD | `0x00` | 命令与参数 |
| CMDARG | `0x04` | 命令参数 |
| RESP[0..3] | `0x10-0x1C` | 响应寄存器 |
| RINTSTS | `0x20` | 原始中断状态 |
| INTMASK | `0x24` | 中断掩码 |
| CTRL | `0x28` | 控制寄存器 |
| STATUS | `0x2C` | 状态寄存器 |
| FIFO | `0x80` | FIFO 数据寄存器（PIO 数据通路） |
| FIFOTH | `0x90` | FIFO 阈值 |
| CLKDIV | `0x94` | 时钟分频 |
| CLKSRC | `0x98` | 时钟源 |
| CLKENA | `0x9C` | 时钟使能 |
| TMOUT | `0xA0` | 超时寄存器 |
| CTYPE | `0xA4` | 卡类型（1/4/8-bit） |
| BLKSIZ | `0xA8` | 块大小 |
| BYTCNT | `0xAC` | 字节计数 |
| INTMSK | `0xB0` | 中断掩码（别名） |
| BMCNT | `0xE0` | DMA 突发计数（Phase 3） |
| DMAC | `0xE4` | DMA 控制（Phase 3） |
| BMOD | `0xE8` | 总线模式（Phase 3） |
| DBADDR | `0xEC` | 描述符列表基址（Phase 3） |
| IDR | `0xF0` | DMA 空闲寄存器（Phase 3） |

**注意:** 实际 FIFO 偏移可能因 DesignWare IP 配置而异。Phase B 期间，所有寄存器值必须通过实板读取验证。

---

## 8. 已知约束与风险

| 风险 | 影响 | 缓解 |
|------|------|------|
| VF2 U-Boot 版本可能缺少 `fdt` 命令 | 无法直接提取 DTB | 通过 TFTP 加载 FDT blob，或升级 U-Boot |
| `booti` 可能需要 U-Boot 重新编译 | 非技术障碍，但需用户操作 | 文档化 U-Boot 配置选项 |
| dw_mmc FIFO 深度因节点而异 | PIO 轮询策略需按 binding 解析 | 仅保留 DTB raw `fifo-depth`；已观测 MMC 节点为 32，不在 HAL 硬编码 |
| 卡初始化超时（无卡插入） | mmcblk0 不存在，启动回退 | 在 `probe_block_devices` 中处理 `DeviceUnavailable` |
| MMC 驱动与现有 ext4 测试不兼容 | QEMU 回归 | `block_publication` ktest 确保 QEMU 路径不受影响 |
| 实板板卡不稳定（如 UART 丢字符） | 调试困难 | 使用 paced PIO 操作；所有 DTB 证据采集应有重试机制 |

---

## 9. 本地代码引用路径速查

```bash
# FDT pre-heap 快照捕获入口
os/src/hal/firmware/mod.rs:100  populate_memory_regions() → capture_fdt_snapshot()
# FDT 后堆枚举入口（从 .data.boot 快照构造 fdt::Fdt）
os/src/hal/firmware/fdt.rs:328  build_platform_info()

# 当前 classify_device 分类器
os/src/hal/firmware/fdt.rs:214  classify_device()

# DeviceKind 枚举（需要增加 DwMmc）
os/src/hal/platform/info.rs:103 enum DeviceKind

# 块设备 trait
os/src/drivers/block/block_dev.rs:24  trait BlockDevice

# 块设备探测（需要增加 dw_mmc 分支）
os/src/drivers/block/mod.rs:52  probe_block_devices()

# VF2 平台策略
os/src/hal/platform/vf2.rs

# VF2 板级配置（identity MMIO）
os/src/hal/platform/riscv/vf2.rs

# Boot protocol 检测
os/src/hal/boot/mod.rs:13  enum BootProtocol

# Boot block registry（mmcblk0 注册入口）
os/src/fs/boot_block.rs:122  publish_block_descriptors()

# 参考: GMAC JH7110 驱动结构
os/src/drivers/net/gmac_jh7110.rs

# QEMU 块探测（确保不受影响）
os/src/drivers/block/virtio_blk.rs:364
```
