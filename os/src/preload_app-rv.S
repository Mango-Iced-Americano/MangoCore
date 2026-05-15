    .section .data
    .global sinitproc
    .global einitproc
    .align 12
sinitproc:
    .incbin "../user/target/riscv64gc-unknown-none-elf/release/initproc"
einitproc:
    .align 12

    .section .data
    .global sbash
    .global ebash
    .align 12
sbash:
    .incbin "./bash-rv"
ebash:
    .align 12

    .section .data
    .global sbusybox
    .global ebusybox
    .align 12
sbusybox:
    .incbin "./busybox-rv"
ebusybox:
    .align 12

    .section .data
    .global sosconfig
    .global eosconfig
    .align 12
sosconfig:
    .incbin "../os_test.conf"
eosconfig:
    .align 12

    .section .data
    .global sfstest
    .global efstest
    .align 12
sfstest:
    .incbin "../user/target/riscv64gc-unknown-none-elf/release/fs_test"
efstest:
    .align 12
