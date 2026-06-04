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
    .incbin "../user/tools/riscv64/bin/bash"
ebash:
    .align 12

    .section .data
    .global sbusybox
    .global ebusybox
    .align 12
sbusybox:
    .incbin "../user/tools/riscv64/bin/busybox"
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

    .section .data
    .global sltpcompat
    .global eltpcompat
    .align 12
sltpcompat:
    .incbin "../user/tools/riscv64/lib/ltp_proto_compat-rv.so"
eltpcompat:
    .align 12

    .section .data
    .global sltprunner
    .global eltprunner
    .align 12
sltprunner:
    .incbin "../user/target/riscv64gc-unknown-none-elf/release/ltprunner"
eltprunner:
    .align 12
