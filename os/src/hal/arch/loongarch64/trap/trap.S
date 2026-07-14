FP_START = 32
LSX_START = 70*8
.altmacro
.macro SAVE_GP n
    st.d $r\n, $sp, \n*8
.endm
.macro LOAD_GP n
    ld.d $r\n, $sp, \n*8
.endm
.macro SAVE_FP n, m
    fst.d $f\n, $sp, \m*8
.endm
.macro LOAD_FP n, m
    fld.d $f\n, $sp, \m*8
.endm
.macro SAVE_LSX n
    vst $vr\n, $sp, LSX_START+(\n*16)
.endm
.macro LOAD_LSX n
    vld $vr\n, $sp, LSX_START+(\n*16)
.endm
    .section .text.trampoline
    .globl __alltraps
    .globl __restore
    .align 2
.equ CSR_SAVE, 0x30    
.equ CSR_EUEN, 0x2
.equ EUEN_LSX, 0x2
.equ CSR_ERA, 0x6
.equ CSR_PRMD, 0x1
.equ CSR_ASID, 0x18
.equ CSR_PGDL, 0x19
.equ CSR_PGD, 0x1b
__alltraps:
    #==================REMEMBER TO TURN OFF THE INTERRUPT !=======================
    csrwr $sp, CSR_SAVE
    # now sp->*TrapContext in user space, CSR_SAVE->user stack
    SAVE_GP 1
    SAVE_GP 2

    # skip r3(sp)
    .set n, 4
    .rept 28
        SAVE_GP %n
        .set n, n+1
    .endr
    .set n, 0

    .set m, FP_START
    .rept 32
        SAVE_FP %n, %m
        .set n, n+1
        .set m, m+1
    .endr

    # Save complete LSX vectors only when bootstrap enabled EUEN.SXE. Their
    # low halves alias the scalar FPU registers saved above.
    csrrd $t0, CSR_EUEN
    andi $t0, $t0, EUEN_LSX
    beqz $t0, .Lalltraps_lsx_saved
    .set n, 0
    .rept 32
        SAVE_LSX %n
        .set n, n+1
    .endr
.Lalltraps_lsx_saved:

    # we can use t0/t1/t2 freely, because they have been saved in TrapContext
    # save FCSR
    movfcsr2gr $t0, $fcsr0
    st.w $t0, $sp, 64*8

    # save FCC
    movcf2gr $t0, $fcc7
    slli.w $t0, $t0, 1
    movcf2gr $t0, $fcc6
    slli.w $t0, $t0, 1
    movcf2gr $t0, $fcc5
    slli.w $t0, $t0, 1
    movcf2gr $t0, $fcc4
    slli.w $t0, $t0, 1
    movcf2gr $t0, $fcc3
    slli.w $t0, $t0, 1
    movcf2gr $t0, $fcc2
    slli.w $t0, $t0, 1
    movcf2gr $t0, $fcc1
    slli.w $t0, $t0, 1
    movcf2gr $t0, $fcc0
    st.b $t0, $sp, 64*8+4

    # save other general purpose registers
    st.d $a0, $sp, 65*8
    csrrd $t0, CSR_PRMD
    csrrd $t1, CSR_ERA
    st.d $t0, $sp, 66*8
    st.d $t1, $sp, 0
    # read user stack from SAVE and save it in TrapContext
    csrrd $t2, CSR_SAVE
    st.d $t2, $sp, 3*8
    # load kernel_satp into t0
    #ld.d $t0, $sp, 67*8
    # load trap_handler into t1
    ld.d $t1, $sp, 68*8
    # move to kernel_sp
    ld.d $sp, $sp, 69*8
    # Kernel runs through direct-map windows, so entering a trap does not switch
    # PGDL.  Keep user TLB entries here; PTE changes and address-space switches
    # still flush before returning to user mode.
    # jump to trap_handler
    jr $t1

__restore:
    # a0: *TrapContext in user space(Constant),
    # a1: user space token
    # a2: ASID (0 if not allocated)

    slli.d $a1, $a1, 12
    csrrd  $t0, CSR_PGDL
    csrrd  $t1, CSR_ASID
    # CSR_ASID[9:0] 是当前 ASID；第 [23:16] 位描述硬件实现的 ASID 位宽，绝不能
    # 将其按位或到进程 ASID 值中。
    bstrpick.d $t1, $t1, 9, 0
    # PGDL 相同不代表地址空间上下文相同：两个任务可能共享页表根但使用不同 ASID。
    # 任一字段变化时都必须重写这一对寄存器。
    bne    $t0, $a1, 2f
    beq    $t1, $a2, 1f
2:
    # 保持 PGDL 与 ASID 同步，两次写寄存器之间不能插入分支。
    csrwr  $a1, CSR_PGDL
    csrwr  $a2, CSR_ASID
    invtlb 0x3, $zero, $zero
1:
    move $sp, $a0
    csrwr  $a0, CSR_SAVE
    # now sp points to TrapContext in user space, start restoring based on it
    # restore FCSR
    ld.w $t0, $sp, 64*8
    movgr2fcsr $fcsr0, $t0
    # restore FCC
    ld.b $t0, $sp, 64*8+4
    movgr2cf $fcc0, $t0
    srli.w $t0, $t0, 1
    movgr2cf $fcc1, $t0
    srli.w $t0, $t0, 1
    movgr2cf $fcc2, $t0
    srli.w $t0, $t0, 1
    movgr2cf $fcc3, $t0
    srli.w $t0, $t0, 1
    movgr2cf $fcc4, $t0
    srli.w $t0, $t0, 1
    movgr2cf $fcc5, $t0
    srli.w $t0, $t0, 1
    movgr2cf $fcc6, $t0
    srli.w $t0, $t0, 1
    movgr2cf $fcc7, $t0
    # restore CSR_PRMD/CSR_ERA
    ld.d $t0, $sp, 66*8
    ld.d $t1, $sp, 0
    csrwr $t0, CSR_PRMD
    csrwr $t1, CSR_ERA
    # FPRs alias the low 64 bits of LSX vector registers. Restoring scalar FPRs
    # after VLD destroys the vector upper halves on real hardware. When LSX is
    # enabled the complete vector snapshot is authoritative; otherwise restore
    # the scalar-only snapshot.
    csrrd $t0, CSR_EUEN
    andi $t0, $t0, EUEN_LSX
    beqz $t0, .Lrestore_scalar_fp
    .set n, 0
    .rept 32
        LOAD_LSX %n
        .set n, n+1
    .endr
    b .Lrestore_fp_done
.Lrestore_scalar_fp:
    .set n, 0
    .set m, FP_START
    .rept 32
        LOAD_FP %n, %m
        .set n, n+1
        .set m, m+1
    .endr
.Lrestore_fp_done:
    # Restore general purpose registers only after all control-flow decisions;
    # the selection above uses $t0 and must not clobber the user's saved value.
    LOAD_GP 1
    LOAD_GP 2
    .set n, 4
    .rept 28
        LOAD_GP %n
        .set n, n+1
    .endr
    # back to user stack
    LOAD_GP 3
    ertn

    .section .text.signaltrampoline
    .globl __call_sigreturn
    .align 2
__call_sigreturn:
    # ecall sys_sigreturn
    addi.d	$a7, $zero, 139
    syscall 0

# Kernel Trap Section
    .section .text.kern_trap
    .globl __kern_trap
    .align 2
__kern_trap:
    # Keep the original $sp in SAVE
    csrwr $sp, CSR_SAVE    
    csrrd $sp, CSR_SAVE
    # Now move the $sp lower to push the registers
    addi.d $sp, $sp, -256
    # Align the $sp
    srli.d  $sp, $sp, 3
    slli.d  $sp, $sp, 3
    # now sp->*GeneralRegisters in kern space, CSR_SAVE->(the previous $sp)

    SAVE_GP 1 # Save $ra
    SAVE_GP 2 # Save $tp

    # skip r3(sp)
    .set n, 4
    .rept 28
        SAVE_GP %n
        .set n, n+1
    .endr
    .set n, 0
    csrrd $t0, CSR_ERA
    st.d $t0, $sp, 0

    move $a0, $sp
    csrrd $sp, CSR_SAVE
    st.d $sp, $a0, 3*8
    move $sp, $a0

    bl trap_from_kernel

    ld.d  $ra, $sp, 0
    csrwr $ra, CSR_ERA
    LOAD_GP 1
    LOAD_GP 2

    # skip r3(sp)
    .set n, 4
    .rept 28
        LOAD_GP %n
        .set n, n+1
    .endr
    .set n, 0
    
    csrrd $sp, CSR_SAVE
    ertn
