# virtio_blk: 非 BLOCK_SZ 对齐 write 触发 assert panic

## 触发条件

LTP `mmap16` 用例用 `mkfs.ext4 -b 1024` 格式化为 1KB 块大小的 ext4，mount 时内核写 ext4 元数据（1KB 块），触发 panic：

```
[fs] found ext4 filesystem
[kernel] panicked at 'assertion failed: buf.len() % BLOCK_SZ == 0',
    src/drivers/block/virtio_blk.rs:33:9
```

## 根因

`virtio_blk.rs` 的 `read_block`/`write_block` 硬编码断言 `buf.len() % BLOCK_SZ == 0`（`BLOCK_SZ = 4096`）：

```rust
fn write_block(&self, block_id: usize, buf: &[u8]) {
    assert!(buf.len() % BLOCK_SZ == 0);  // ← 此行 panic
    let mut dev = self.0.lock();
    for (i, chunk) in buf.chunks(VIRT_IO_BLOCK_SZ).enumerate() {
        dev.write_blocks(block_id * BLOCK_RATIO + i, chunk)
            .expect("Error when writing VirtIOBlk");
    }
}
```

当 ext4 块大小非 4096 时（如 `-b 1024`），上层传来的 `buf` 可能是 1KB、2KB、3KB（都不是 4096 的倍数），断言失败。

`BlockDevInode`（`fs/dev/block.rs`）已经实现了字节级 RMW（bounce buffer + read-modify-write），但 `virtio_blk` 的 `BlockDevice` trait 实现仍是硬断言。

## 影响范围

任何使用非 4096 块大小的文件系统（ext2/ext3/ext4 `-b 1024` 或 `-b 2048`）在 mount 时会 panic。

## 修复方向

在 `virtio_blk::read_block`/`write_block` 中实现类似 `BlockDevInode` 的字节级 RMW：
- 对齐 chunk → 直通 virtio
- 不对齐头尾 → bounce buffer + read-modify-write

## 发现日期

2026-06-09

## 触发用例

LTP `mmap16`（`ltp_runner=suite`, `ltp_suites=syscalls`）
