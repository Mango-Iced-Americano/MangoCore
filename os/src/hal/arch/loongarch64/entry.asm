    .section .text.entry
    .globl _start
_start:
    # Save the firmware handoff before the DMW window switch.
    # LoongArch QEMU direct boot follows the Linux ABI: a0 is the EFI flag,
    # a1 is the command line and a2 is the EFI system table.  The firmware
    # layer resolves the FDT from that table; keeping a1 here loses the table
    # and makes every QEMU boot fail before console initialization.
    la.global   $t0,    RAW_HART_ID
    st.d        $a0,    $t0,    0
    la.global   $t0,    RAW_DTB_PADDR
    st.d        $a2,    $t0,    0
    # Original DMW window switch dance.
# 把默认0x8…和9的窗给关了，全开成0的窗，这样就相当于0的这个部分是地址恒等映射，直接继承原来的代码
    pcaddi      $t0,    0x0
    srli.d      $t0,    $t0,    0x30
    slli.d      $t0,    $t0,    0x30
    addi.d      $t0,    $t0,    0x11
    csrwr       $t0,    0x181   # Make sure the window remains the same after the switch.
    # 前5行是把当前PC所在段给保留下来,存到DMW1
    # 然后改DMW0
    # 使用sub生成0,因为有些版本的虚拟机上面zero会被赋值,避免使用zero
    sub.d       $t0,    $t0,    $t0  # 使$t0为0
    addi.d      $t0,    $t0,    0x11
    csrwr       $t0,    0x180        # 将DMW0设置为0
    pcaddi      $t0,    0x0          # 获取当前PC
    slli.d      $t0,    $t0,    0x10 # 左移16位
    srli.d      $t0,    $t0,    0x10 # 右移16位
    # 上面两条指令的作用为将当前PC的高16位清零
    jirl        $t0,    $t0,    0x10 # 跳0段的下一条指令
    # The barrier
    sub.d       $t0,    $t0,    $t0
    csrwr       $t0,    0x181
    sub.d       $t0,    $t0,    $t0
    la.global $sp, boot_stack_top
    # 2K1000LA remains single-core; normalize the common Rust entry ABI.
    sub.d       $a0,    $a0,    $a0
    # U-Boot 在 a2 中传 EFI system table。DMW 切换只改变地址解释方式，
    # 不会改写寄存器内容，因此此处仍可直接取得原始 a2。
    # 2K1000 的 PALEN=40；左移再逻辑右移 24 位可清除 0x9000... DMW 别名，
    # 同时保留已经是物理地址的低 40 位指针。
    slli.d      $a1,    $a2,    0x18
    srli.d      $a1,    $a1,    0x18
    bl          rust_main

    .section .bss.stack
    .globl boot_stack
boot_stack:
    .space 4096 * 64
    .globl boot_stack_top
boot_stack_top:
