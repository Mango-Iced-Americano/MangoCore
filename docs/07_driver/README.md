---
title: "驱动与 DMA 路径"
category: driver
status: stable
author: MangoCore Team
last_update: 2026-08-11
---

# 驱动与 DMA 路径

## VirtIO block DMA bridge

RV64 MMIO 和 LA64 PCI 块驱动共用 `virtio_dma_pool` 的连续页缓冲池。
`virtio-drivers::Hal::share()` 不传递请求上下文，因此上层在提交同步请求前先为
当前逻辑 CPU 发布 `(slot, generation)` reservation，data-buffer `share()` 再在同一
CPU 上一次性消费。

每个 logical CPU 有独立 bridge context；guard 期间关闭本地 IRQ，保证同步 HAL
callback 不会被同 CPU 的 VirtIO 路径嵌套。不同 CPU、不同 VirtIO 设备之间不再
持有覆盖整个请求时长的全局 bridge lock；设备队列自身的互斥仍保留。guard
退出会取消未被 HAL 消费的 reservation，slot generation 防止迟到取消释放已复用
缓冲区。

`/sys/kernel/stats/virtio` 的 DMA bridge schema v2 显式标记 per-hart 模式。
`bridge_wait_ticks` 在该模式下应为 0，`bridge_hold_ticks` 仍表示本地 IRQ-off 的
同步请求窗口，不应解读为全局锁 hold。

相关实现：`os/src/drivers/block/virtio_dma_pool.rs`、`virtio_blk.rs`、
`virtio_blk_pci.rs`。
