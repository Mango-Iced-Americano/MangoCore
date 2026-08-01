    .section .image_header, "ax"
    .balign 4
    .globl riscv_image_header
riscv_image_header:
    j _start
    .word 0
    .dword 0x00200000
    .dword ekernel - skernel
    .dword 0
    .word 0x00000002
    .word 0
    .dword 0
    /* Linux v2 Image header magic fields:
     *   offset 0x30: u64 magic  = "RISCV\0\0\0" (RISCV_IMAGE_MAGIC)
     *   offset 0x38: u32 magic2 = "RSC\x05"     (RISCV_IMAGE_MAGIC2)
     * U-Boot booti validates the 0x38 magic2 word == 0x05435352.
     */
    .ascii "RISCV\0\0\0"
    .balign 4
    .ascii "RSC\x05"
    .word 0
