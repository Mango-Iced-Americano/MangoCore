# Bug: FS Test #35 hole read data mismatch (50/51)

**状态**: 未修复（根因未定位）  
**架构**: loongarch64  
**日期**: 2026-05-19

## 症状

FS test suite 51 个测试中第 35 个失败：

```
[35/51] lseek beyond EOF + hole read
  FAIL: hole: data at 50 mismatch
```

测试代码（`user/src/bin/fs_test.rs:1001-1022`）：
```rust
let fd = sys_open("/tmp26/hole\0", O_CREAT | O_RDWR);
sys_write(fd as usize, b"0123456789");    // 10 bytes at offset 0
sys_lseek(fd as usize, 50, SEEK_SET);
sys_write(fd as usize, b"DATA_AT_50");    // 10 bytes at offset 50
sys_lseek(fd as usize, 0, SEEK_SET);
let mut buf = [0u8; 70];
let n = sys_read(fd as usize, &mut buf);
// 期望: buf[0..10]="0123456789", buf[10..50]=zeros, buf[50..60]="DATA_AT_50"
// 实际: buf[50..60] != "DATA_AT_50"
```

## 诊断过程

### 第一轮：排除 ext4 / PageCache 路径

通过 `LOG=info` 逐层加诊断日志，确认了整个内核数据链路：

| 检查项 | 结果 |
|--------|------|
| 两次 write 和一次 read 是否用同一个 PageCache 实例 | ✅ `pc=0x805a1410` 相同 |
| PageCache 写入后数据是否正确 | ✅ `snap[50..60]` = "DATA_AT_50" |
| metadata flush 后数据是否正确 | ✅ `snap2[50..60]` 同上 |
| read_at 是否用同一个 page、cached | ✅ `page=0 cached=true` |
| kernel_buf 在 copy-out 前是否正确 | ✅ `kbuf[50..60]` = "DATA_AT_50" |
| UserBufferWriter buf_total_len | ✅ `src_len=60 buf_total_len=60 n_bufs=1` |
| copy-out 后内核读回是否正确 | ✅ `READBACK[50..60]` = "DATA_AT_50", `match=true` |

### 已排除

- ❌ ext4 write_at / read_at 逻辑错误
- ❌ PageCache write / read 逻辑错误
- ❌ PageCache 实例不共享
- ❌ PageCache 页面被 DMA 覆写
- ❌ metadata flush 导致数据丢失
- ❌ 文件 size 计算错误
- ❌ UserBufferWriter 翻译长度不足
- ❌ TLB / D-cache 不一致（内核读回正确）
- ❌ 第二次 sys_write 失败（已加返回值检查，返回 10）

### 剩余假设

**整个内核数据链路确认完全正确，但用户态 Rust 代码看到的 `buf[50..60]` 就是不对。**

可能原因（均未验证）：
1. Rust 编译器优化——`buf` 被放到寄存器或实际地址与传给 `sys_read` 的不同
2. `#[no_std]` 环境下的栈布局问题
3. 用户态 `sys_read` wrapper（`user/src/syscall.rs`）返回值解析问题
4. 前一次测试残留的 `/tmp26/hole` 文件导致旧数据混淆

## 临时诊断日志（需在解决后删除）

| 文件 | 大约行 | 标签 |
|------|--------|------|
| `os/src/fs/ext4/ext4fs.rs` | write_at 内 | `[write_at]` ino/offset/len/old_size/new_size/page/cached/pc |
| `os/src/fs/ext4/ext4fs.rs` | write_at pc.write 后 | `[write_at] after_pc_write` snap[50..60] |
| `os/src/fs/ext4/ext4fs.rs` | write_at flush 后 | `[write_at] after_flush` snap2[50..60] |
| `os/src/fs/ext4/ext4fs.rs` | read_at 内 | `[read_at]` ino/offset/len/file_size/page/cached/pc |
| `os/src/syscall/fs.rs` | read_into_user copy-out 前 | `[read_into_user]` kbuf[50..60] |
| `os/src/syscall/fs.rs` | read_into_user copy-out 后 | `[read_into_user] READBACK[50..60]` |
| `os/src/mm/uaccess.rs` | UserBufferWriter::write_from | `[UserBufferWriter]` src_len/buf_total_len/n_bufs |

## 建议修复方向

1. **最优先**：在测试中打印 `buf[50..60]` 实际字节
   ```rust
   if &buf[50..60] != b"DATA_AT_50" {
       println!("  FAIL: hole: data at 50 mismatch, got {:02x?}", &buf[50..60]);
   }
   ```
2. 用干净镜像重跑，排除旧文件污染
3. 在用户态 `sys_read` 返回后立即用 `sys_write(1, &buf[50..60])` 打印
4. 检查 `user/src/syscall.S` 的 `__syscall` 和 `sys_read` wrapper 返回值传递
