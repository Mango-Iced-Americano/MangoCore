# VF2 Live U-Boot FDT MMC Probe

## Scope and Safety

- Host workspace: `/root/projects/MangoCore`.
- Board transport: one exclusive pyserial session on `/dev/ttyUSB0` at 115200 baud.
- No explicit DTR/RTS assignment, reset, power cycle, `saveenv`, storage write, TFTP, or network command was issued.
- Every transmitted U-Boot command below only reads the current control/working FDT or prints command help/version information.

## Commands and Observed Output

```text
StarFive # fdt addr -c
Control fdt: f76df9b0

StarFive # fdt addr
Working fdt: f76df9b0

StarFive # fdt print /aliases
aliases {
    spi0 = "/soc/spi@13010000";
    gpio0 = "/soc/gpio@13040000";
    ethernet0 = "/soc/ethernet@16030000";
    ethernet1 = "/soc/ethernet@16040000";
    mmc0 = "/soc/sdio0@16010000";
    mmc1 = "/soc/sdio1@16020000";
    i2c0 = "/soc/i2c5@12050000";
};

StarFive # fdt print /soc/sdio0@16010000
sdio0@16010000 {
    compatible = "snps,dw-mshc";
    reg = <0x00000000 0x16010000 0x00000000 0x00010000>;
    clocks = <0x0000000f 0x0000005b 0x0000000f 0x0000005d>;
    clock-names = "biu", "ciu";
    resets = <0x00000010 0x00000040>;
    reset-names = "reset";
    assigned-clocks = <0x0000000f 0x0000005d>;
    assigned-clock-rates = <0x02faf080>;
    fifo-depth = <0x00000020>;
    bus-width = <0x00000008>;
    pinctrl-names = "default";
    pinctrl-0 = <0x00000016>;
    status = "okay";
    u-boot,dm-spl;
    phandle = <0x0000005f>;
};

StarFive # fdt print /soc/sdio1@16020000
sdio1@16020000 {
    compatible = "snps,dw-mshc";
    reg = <0x00000000 0x16020000 0x00000000 0x00010000>;
    clocks = <0x0000000f 0x0000005c 0x0000000f 0x0000005e>;
    clock-names = "biu", "ciu";
    resets = <0x00000010 0x00000041>;
    reset-names = "reset";
    assigned-clocks = <0x0000000f 0x0000005e>;
    assigned-clock-rates = <0x02faf080>;
    fifo-depth = <0x00000020>;
    bus-width = <0x00000004>;
    pinctrl-names = "default";
    pinctrl-0 = <0x00000017>;
    status = "okay";
    u-boot,dm-spl;
    phandle = <0x00000060>;
};

StarFive # fdt print /chosen
chosen {
    stdout-path = "/soc/serial@10000000:115200";
    starfive,boot-hart-id = <0x00000001>;
    u-boot,dm-spl;
};

StarFive # help booti
booti - boot Linux kernel 'Image' format from memory
Usage:
booti [addr [initrd[:size]] [fdt]]
    - boot Linux flat or compressed 'Image' stored at 'addr'
    The argument 'initrd' is optional and specifies the address
    of an initrd image. The optional parameter ':size' allows
    specifying the size of a RAW initrd.
    Currently only booting from gz, bz2, lzma and lz4 compression
    types are supported. In order to boot from any of these compressed
    images, user have to set kernel_comp_addr_r and kernel_comp_size environment
    variables beforehand.
    Since booting a Linux kernel requires a flat device-tree, a
    third argument providing the address of the device-tree blob
    is required. To boot a kernel with a device-tree blob but
    without an initrd image, use a '-' for the initrd argument.

StarFive # version
U-Boot 2021.10 (Oct 10 2025 - 11:25:33 +0800), Build: jenkins-github_visionfive2_6.12-16

riscv64-buildroot-linux-gnu-gcc.br_real (Buildroot JH7110_VF2_6.12_v6.0.0) 12.2.0
GNU ld (GNU Binutils) 2.39
```

## Findings

- The live working FDT is present and aliases `mmc0` to the 8-bit `sdio0` controller and `mmc1` to the 4-bit `sdio1` controller.
- Both controllers are enabled and identify as generic DesignWare MMC (`snps,dw-mshc`), not `starfive,jh7110-mmc`.
- The live U-Boot FDT contains no `interrupts`, `starfive,sysreg`, or `data-addr` property in either printed node. PIO-first probing must not infer those fields from this DTB.
- The installed U-Boot supports `booti` and explicitly accepts a DTB argument, so MangoCore can move from `go` to a standard RISC-V Image plus DTB handoff after its image-format work is implemented.

## Live Read-Only Recheck

The target connection was explicitly confirmed as `/dev/ttyUSB0`. The console
was configured for 115200 baud and queried after user authorization. The
session did not use reset, boot, TFTP, network, storage-write, environment
mutation, or DTR/RTS modem-control ioctls.

Commands sent:

```text
<CR>
version
fdt addr
fdt print /aliases
fdt print /soc/sdio0@16010000
fdt print /soc/sdio1@16020000
```

Observed transcript:

```text
U-Boot 2021.10 (Oct 10 2025 - 11:25:33 +0800), Build: jenkins-github_visionfive2_6.12-16
StarFive #

Working fdt: f76df9b0

aliases {
    ethernet0 = "/soc/ethernet@16030000";
    ethernet1 = "/soc/ethernet@16040000";
    mmc0 = "/soc/sdio0@16010000";
    mmc1 = "/soc/sdio1@16020000";
};

sdio0@16010000 {
    compatible = "snps,dw-mshc";
    reg = <0x00000000 0x16010000 0x00000000 0x00010000>;
    clocks = <0x0000000f 0x0000005b 0x0000000f 0x0000005d>;
    resets = <0x00000010 0x00000040>;
    assigned-clocks = <0x0000000f 0x0000005d>;
    assigned-clock-rates = <0x02faf080>;
    fifo-depth = <0x00000020>;
    bus-width = <0x00000008>;
    status = "okay";
};

sdio1@16020000 {
    compatible = "snps,dw-mshc";
    reg = <0x00000000 0x16020000 0x00000000 0x00010000>;
    clocks = <0x0000000f 0x0000005c 0x0000000f 0x0000005e>;
    resets = <0x00000010 0x00000041>;
    assigned-clocks = <0x0000000f 0x0000005e>;
    assigned-clock-rates = <0x02faf080>;
    fifo-depth = <0x00000020>;
    bus-width = <0x00000004>;
    status = "okay";
};
StarFive #
```

This recheck confirms the U-Boot working FDT currently describes both DesignWare
MMC controllers exactly as the archived probe did. It does not boot MangoCore,
and therefore does not validate kernel-side FDT discovery: the current
`boot_rv_uboot_go` contract intentionally treats `a1` as non-DTB data.
