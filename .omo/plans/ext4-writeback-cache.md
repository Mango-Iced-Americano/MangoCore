# Ext4 写路径接入 PageCache（Write-Back）

## TL;DR

> **Quick Summary**: ext4 读路径已走 PageCache，但写路径是 write-through + invalidate（直写盘然后废缓存）。改为 write-back（写缓存标记脏，Drop 时回写），参考 FAT32 已有模式。
>
> **核心改动**: `Ext4OSInode::write_at` 一行替换 + 文件大小预扩展
>
> **Estimated Effort**: Quick
> **Parallel Execution**: 单波即可

---

## Context

### 当前状态

| 路径 | 行为 | 性能 |
|------|------|------|
| `read_at` | `pc.read()` → PageCache | ✅ 缓存命中 |
| `write_at` | `ext4fs.write_at()` → 直写盘 → `pc.invalidate_range()` 废缓存 | ❌ 每次打盘 |

### 目标状态（对齐 FAT32）

| 路径 | 行为 | 性能 |
|------|------|------|
| `read_at` | `pc.read()` → PageCache | ✅ 不变 |
| `write_at` | `pc.write()` → 写缓存标记脏 | ✅ 批量回写 |

### FAT32 参考代码 (fat_inode.rs:1146-1170)
```rust
fn write_at(&self, offset, len, buf, _data) -> Result<usize, SyscallErr> {
    // 1. 预扩展文件大小
    let old_size = self.get_file_size() as usize;
    let diff = write_len as isize + offset as isize - old_size as isize;
    if diff > 0 { self.modify_size_lock(&inode_lock, diff, false); }
    // 2. 写缓存
    let pc = self.get_new_page_cache();
    pc.write(offset, &buf[..actual_len]).map_err(|_| SyscallErr::EIO)
}
```

---

## Work Objectives

### Core Objective
ext4 普通文件写操作走 PageCache write-back，减少同步块设备 I/O。

### Concrete Deliverables
- `Ext4OSInode::write_at` 改为 `pc.write()` 替代 `ext4fs.write_at()` + invalidate
- 写入前预扩展文件大小（如果超出当前大小）
- 写入后更新 inode 元数据（i_size, mtime, ctime）
- Drop 时 writeback_all（已有，确认生效）

### Must Have
- symlink/目录/设备文件写路径不受影响
- 写缓存与磁盘最终一致（Drop/fsync 时回写）
- rv64 + la64 编译通过

### Must NOT Have
- 不改 BlockDevice / virtio
- 不改 PageCache 语义
- 不引入新的 on-disk 格式

---

## Execution Strategy

单波实施，因为改动集中在两个文件。

```
Wave 1 (单波):
├── Task 1: 修改 Ext4OSInode::write_at 走 write-back [deep]
├── Task 2: rv64 + la64 编译验证 [quick]
└── Task 3: QEMU fs_test 验证正确性 + 性能 [unspecified-high]
```

---

## TODOs

- [ ] 1. 修改 `Ext4OSInode::write_at` — 走 write-back cache

  **What to do**:
  - 在 `os/src/fs/ext4/ext4fs.rs` 的 `write_at` 方法（行 399-422）中：
    1. 保留目录检查：`if inode_lock.inode.is_dir() { return Err(SyscallErr::EISDIR); }`
    2. 预扩展文件大小（如果本次写入超出当前 i_size）：
       ```rust
       let file_size = inode_lock.inode.size() as usize;
       let new_end = offset + write_len;
       if new_end > file_size {
           inode_lock.inode.set_size(new_end as u64);
       }
       ```
    3. **核心改动**：将 `self.ext4fs.write_at(...)` + `pc.invalidate_range(...)` 替换为：
       ```rust
       if let Some(pc) = self.get_new_page_cache() {
           pc.write(offset, &buf[..write_len]).map_err(|_| SyscallErr::EIO)?;
       } else {
           self.ext4fs.write_at(inode_num, offset, &buf[..write_len]).map_err(|_| SyscallErr::EIO)?;
       }
       ```
    4. 更新 inode 时间戳（mtime, ctime）和写回 inode 元数据（调用 `write_back_inode`）
    5. 删除 `pc.invalidate_range` 调用（缓存已是最新数据）
    6. 删除 `get_inode_ref` 重新加载 inode 的逻辑（inode 元数据已更新）

  **Must NOT do**:
  - 不要改动 symlink/目录/设备文件的读路径
  - 不要删除 Drop 中的 `writeback_all()` (layout.rs:84)

  **Reference**: FAT32 的 `write_at` (fat_inode.rs:1146-1170) 作为写缓存参考模式

  **Agent Profile**: `deep` — 需要理解内核文件系统语义

  **QA Scenarios**:
  ```
  Scenario: write-through-cache
    Tool: Bash (QEMU run)
    Steps:
      1. make rv64-run
      2. grep "fs_test.*passed"
    Expected: 50/51 passed
    Evidence: .sisyphus/evidence/task-1-fs-test.txt

  Scenario: write-read-consistency
    Tool: Bash (QEMU run)
    Steps:
      1. 确认 fs_test 中 [2/51] file create+write 和 [3/51] file read 均 PASS
      2. 确认 [28/51] O_APPEND + lseek atomicity PASS
      3. 确认 [43/51] stress: create 50 files + verify PASS
      4. 确认 [47/51] stress: large file 64KB write+read PASS
    Expected: 上述测试全部 PASS
    Evidence: .sisyphus/evidence/task-1-consistency.txt
  ```

- [ ] 2. rv64 + la64 编译验证

  **What to do**: 运行 `make rv64-kernel-build-only` 和 `make la64-kernel-build-only`，确认双架构编译通过。

  **Agent Profile**: `quick`

  **QA Scenarios**:
  ```
  Scenario: dual-arch build
    Tool: Bash (make)
    Steps:
      1. docker exec ... make rv64-kernel-build-only
      2. docker exec ... make la64-kernel-build-only
    Expected: 两者 exit_code=0
    Evidence: .sisyphus/evidence/task-2-build.log
  ```

- [ ] 3. QEMU fs_test 完整验证

  **What to do**: 注入测试配置 (mask=0x001)，运行 QEMU，验证：
  - busybox install 不 panic
  - fs_test 50/51 passed
  - 无新增失败

  **Agent Profile**: `unspecified-high`

  **QA Scenarios**:
  ```
  Scenario: full-fs-test
    Tool: Bash (QEMU run)
    Steps:
      1. kernel_test_config arch=rv64 mask=0x001
      2. 清理残留 QEMU 进程
      3. timeout 300 make rv64-run
      4. grep "=== FS Test:" 确认分数
      5. grep "FAIL" 确认失败项
    Expected: 
      - busybox --install 成功
      - === FS Test: 50/51 passed === (仅 linkat 失败)
      - 无 kernel panic
    Evidence: .sisyphus/evidence/task-3-fs-test.log
  ```

---

## Final Verification Wave

- [ ] F1. `make rv64-kernel-build-only` + `make la64-kernel-build-only` ✅
- [ ] F2. QEMU fs_test 50/51 passed
- [ ] F3. 日志确认 writeback 正常（无 EIO）

---

## Commit Strategy

- **Wave 1**: `perf(ext4): switch write path from write-through to write-back page cache` — ext4fs.rs

---

## Success Criteria

```bash
make rv64-kernel-build-only && make la64-kernel-build-only
cd os && timeout 300 make rv64-run 2>&1 | grep "fs_test.*passed"
# Expected: === FS Test: 50/51 passed ===
```
