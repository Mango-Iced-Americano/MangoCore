    .section .data
    .global sinitramfs
    .global einitramfs
    .align 12
sinitramfs:
    .incbin "../fs-img-dir/initramfs-regression-rv.cpio"
einitramfs:
    .align 12
