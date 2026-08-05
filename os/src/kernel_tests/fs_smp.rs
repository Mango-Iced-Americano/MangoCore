//! 零盘 FS SMP ktest。
//!
//! `spawn_ktest_task_on()` 只验证调度闭环，绝不承载 FS、设备或用户 MM 工作。
//! 跨 CPU 的 FS 操作一律由 `kernel_tests::probe` 创建的完整用户 TCB 经 syscall 触发。

use alloc::{sync::Arc, vec, vec::Vec};

use crate::{
    config::PAGE_SIZE,
    fs::{
        tmpfs::TmpFS,
        vfs::{File, FileFlags, FileType, IndexNode, InodeMode},
        PageState,
    },
    kernel_tests::{
        fs_smp_fixture::{
            new_cache, read_inode, run_dual_user_writes, FsSmpCacheInode, MountedTmpfsFixture,
        },
        probe::{
            attach_probe_to_runner, build_path_probe, build_user_probe, deadline_after,
            probe_quiesced, reap_probe, stop_probe, ProbeResult,
        },
        runner::KernelTest,
    },
};

const TEST_TIMEOUT_MS: usize = 5_000;
const SYSCALL_FTRUNCATE: usize = 46;

/// 返回本组七项 FS SMP 测试。
pub fn tests() -> Vec<KernelTest> {
    vec![
        KernelTest::with_timeout(
            "fs_smp::pagecache_user_write_vs_truncate",
            pagecache_user_write_vs_truncate,
            TEST_TIMEOUT_MS,
        ),
        KernelTest::with_timeout(
            "fs_smp::pagecache_same_page_no_torn_copy",
            pagecache_same_page_no_torn_copy,
            TEST_TIMEOUT_MS,
        ),
        KernelTest::with_timeout(
            "fs_smp::pagecache_writeback_redirty",
            pagecache_writeback_redirty,
            TEST_TIMEOUT_MS,
        ),
        KernelTest::with_timeout(
            "fs_smp::tmpfs_create_same_name_exactly_once",
            tmpfs_create_same_name_exactly_once,
            TEST_TIMEOUT_MS,
        ),
        KernelTest::with_timeout(
            "fs_smp::tmpfs_cross_rename_opposite_order",
            tmpfs_cross_rename_opposite_order,
            TEST_TIMEOUT_MS,
        ),
        KernelTest::with_timeout(
            "fs_smp::tmpfs_lookup_unlink_generation",
            tmpfs_lookup_unlink_generation,
            TEST_TIMEOUT_MS,
        ),
        KernelTest::with_timeout(
            "fs_smp::different_page_parallel_progress",
            different_page_parallel_progress,
            TEST_TIMEOUT_MS,
        ),
    ]
}

fn wait_status_exit_code(status: u32) -> isize {
    (status >> 8) as isize
}

fn tmpfs_create_same_name_exactly_once() -> Result<(), &'static str> {
    if crate::smp::configured_cpu_count() < 3 {
        return Err("SKIP: requires CPU1 and CPU2 user probes");
    }
    let _fixture = MountedTmpfsFixture::mount()?;
    let path = b"/dev/shm/ktest_same\0";
    let first = build_path_probe(ProbeResult::TmpfsCreate, path)?;
    let second = build_path_probe(ProbeResult::TmpfsCreate, path)?;
    first.set_initial_cpus_allowed(1 << 1);
    second.set_initial_cpus_allowed(1 << 2);
    let first_parent = attach_probe_to_runner(&first)?;
    let second_parent = attach_probe_to_runner(&second)?;
    crate::task::publish_task_on(first.clone(), 1);
    crate::task::publish_task_on(second.clone(), 2);
    let first_clean = probe_quiesced(&first, &first.process, 1, deadline_after(3))
        || stop_probe(&first, &first.process, 1);
    let second_clean = probe_quiesced(&second, &second.process, 2, deadline_after(3))
        || stop_probe(&second, &second.process, 2);
    let first_reaped = reap_probe(&first_parent, &first);
    let second_reaped = reap_probe(&second_parent, &second);
    if !first_clean || !second_clean || !first_reaped || !second_reaped {
        return Err("tmpfs create probes did not quiesce and reap");
    }
    let first_exit = wait_status_exit_code(first.process.exit_code());
    let second_exit = wait_status_exit_code(second.process.exit_code());
    if first_exit + second_exit != 1 {
        return Err("tmpfs create did not produce one O_EXCL success and one EEXIST");
    }
    Ok(())
}

fn tmpfs_cross_rename_opposite_order() -> Result<(), &'static str> {
    if crate::smp::configured_cpu_count() < 3 {
        return Err("SKIP: requires CPU1 and CPU2 user probes");
    }
    let _fixture = MountedTmpfsFixture::mount()?;
    let directory =
        crate::fs::vfs_lookup_absolute("/dev/shm").map_err(|_| "tmpfs mount is not visible")?;
    directory
        .create("ktest_rename_a", FileType::File, InodeMode::S_IRWXUGO)
        .map_err(|_| "failed to create tmpfs rename source A")?;
    directory
        .create("ktest_rename_b", FileType::File, InodeMode::S_IRWXUGO)
        .map_err(|_| "failed to create tmpfs rename source B")?;
    let mut forward = [0u8; 256];
    let mut reverse = [0u8; 256];
    forward[..24].copy_from_slice(b"/dev/shm/ktest_rename_a\0");
    forward[128..152].copy_from_slice(b"/dev/shm/ktest_rename_b\0");
    reverse[..24].copy_from_slice(b"/dev/shm/ktest_rename_b\0");
    reverse[128..152].copy_from_slice(b"/dev/shm/ktest_rename_a\0");
    let first = build_path_probe(ProbeResult::TmpfsRename, &forward)?;
    let second = build_path_probe(ProbeResult::TmpfsRename, &reverse)?;
    first.set_initial_cpus_allowed(1 << 1);
    second.set_initial_cpus_allowed(1 << 2);
    let first_parent = attach_probe_to_runner(&first)?;
    let second_parent = attach_probe_to_runner(&second)?;
    crate::task::publish_task_on(first.clone(), 1);
    crate::task::publish_task_on(second.clone(), 2);
    let first_clean = probe_quiesced(&first, &first.process, 1, deadline_after(3))
        || stop_probe(&first, &first.process, 1);
    let second_clean = probe_quiesced(&second, &second.process, 2, deadline_after(3))
        || stop_probe(&second, &second.process, 2);
    let first_reaped = reap_probe(&first_parent, &first);
    let second_reaped = reap_probe(&second_parent, &second);
    if !first_clean || !second_clean || !first_reaped || !second_reaped {
        return Err("tmpfs rename probes did not quiesce and reap");
    }
    if wait_status_exit_code(first.process.exit_code()) != 0
        || wait_status_exit_code(second.process.exit_code()) != 0
    {
        return Err("tmpfs opposite rename syscall failed");
    }
    let a_exists = directory.find("ktest_rename_a").is_ok();
    let b_exists = directory.find("ktest_rename_b").is_ok();
    if a_exists == b_exists {
        return Err("tmpfs opposite rename left an inconsistent final tree");
    }
    Ok(())
}

/// CPU1 循环 lookup、CPU2 并发 unlink 同一目录项：unlink 恰好一次成功，
/// 删除后 stale lookup 必须立即 ENOENT（目录 generation），最终树中该名字消失。
fn tmpfs_lookup_unlink_generation() -> Result<(), &'static str> {
    if crate::smp::configured_cpu_count() < 3 {
        return Err("SKIP: requires CPU1 and CPU2 user probes");
    }
    let _fixture = MountedTmpfsFixture::mount()?;
    let directory =
        crate::fs::vfs_lookup_absolute("/dev/shm").map_err(|_| "tmpfs mount is not visible")?;
    directory
        .create("ktest_gen", FileType::File, InodeMode::S_IRWXUGO)
        .map_err(|_| "failed to create tmpfs generation target")?;
    let path = b"/dev/shm/ktest_gen\0";
    // lookup 探针循环 openat 直到 ENOENT；unlink 探针执行一次 unlinkat。
    let looker = build_path_probe(ProbeResult::TmpfsLookup, path)?;
    let unlinker = build_path_probe(ProbeResult::TmpfsUnlink, path)?;
    looker.set_initial_cpus_allowed(1 << 1);
    unlinker.set_initial_cpus_allowed(1 << 2);
    let looker_parent = attach_probe_to_runner(&looker)?;
    let unlinker_parent = attach_probe_to_runner(&unlinker)?;
    // 两个 TCB 均在 runner 观察到任一完成前发布；不以 sleep 伪造窗口。
    crate::task::publish_task_on(looker.clone(), 1);
    crate::task::publish_task_on(unlinker.clone(), 2);
    let looker_done = probe_quiesced(&looker, &looker.process, 1, deadline_after(3));
    let unlinker_done = probe_quiesced(&unlinker, &unlinker.process, 2, deadline_after(3));
    let looker_clean = looker_done || stop_probe(&looker, &looker.process, 1);
    let unlinker_clean = unlinker_done || stop_probe(&unlinker, &unlinker.process, 2);
    let looker_reaped = reap_probe(&looker_parent, &looker);
    let unlinker_reaped = reap_probe(&unlinker_parent, &unlinker);
    if !looker_clean || !unlinker_clean || !looker_reaped || !unlinker_reaped {
        return Err("lookup/unlink probes did not quiesce and reap");
    }
    // unlink 必须恰好成功一次（exit 0）；lookup 必须在 unlink 生效后 ENOENT（exit 1）。
    if wait_status_exit_code(unlinker.process.exit_code()) != 0 {
        return Err("concurrent tmpfs unlink did not succeed exactly once");
    }
    if wait_status_exit_code(looker.process.exit_code()) != 1 {
        return Err("stale tmpfs lookup did not observe ENOENT after unlink");
    }
    // 最终树中该名字必须消失：generation 使任何残留目录项失效。
    if directory.find("ktest_gen").is_ok() {
        return Err("tmpfs unlink left a stale generation entry");
    }
    Ok(())
}

/// CPU1 写整页、CPU2 同时 truncate(0)。最终状态只能完整落在两种合法顺序之一：
/// truncate 最后提交时文件为空；write 最后提交时文件为一整页且内容完整。
fn pagecache_user_write_vs_truncate() -> Result<(), &'static str> {
    if crate::smp::configured_cpu_count() < 3 {
        return Err("SKIP: requires CPU1 and CPU2 user probes");
    }
    let cache = new_cache();
    let inode: Arc<dyn IndexNode> = Arc::new(FsSmpCacheInode::new(cache.clone(), TmpFS::new()));
    let writer_file =
        File::new(inode.clone(), FileFlags::O_WRONLY).map_err(|_| "failed to create writer fd")?;
    let truncater_file = File::new(inode.clone(), FileFlags::O_WRONLY)
        .map_err(|_| "failed to create truncater fd")?;
    let writer = build_user_probe(
        ProbeResult::WritePage,
        writer_file,
        64,
        Some(&[0xa5; PAGE_SIZE]),
    )?;
    let truncater = build_user_probe(ProbeResult::Zero, truncater_file, SYSCALL_FTRUNCATE, None)?;
    writer.set_initial_cpus_allowed(1usize << 1);
    truncater.set_initial_cpus_allowed(1usize << 2);
    let writer_parent = attach_probe_to_runner(&writer)?;
    let truncater_parent = attach_probe_to_runner(&truncater)?;
    // 两个用户 TCB 连续发布到不同 CPU，不在生产 PageCache 中安装测试 hook。
    crate::task::publish_task_on(writer.clone(), 1);
    crate::task::publish_task_on(truncater.clone(), 2);
    let writer_done = probe_quiesced(&writer, &writer.process, 1, deadline_after(3));
    let truncater_done = probe_quiesced(&truncater, &truncater.process, 2, deadline_after(3));
    let writer_clean = writer_done || stop_probe(&writer, &writer.process, 1);
    let truncater_clean = truncater_done || stop_probe(&truncater, &truncater.process, 2);
    let writer_reaped = reap_probe(&writer_parent, &writer);
    let truncater_reaped = reap_probe(&truncater_parent, &truncater);
    if !writer_clean || !truncater_clean || !writer_reaped || !truncater_reaped {
        return Err("pagecache user probes did not quiesce and reap");
    }
    if writer.process.exit_code() != 0 || truncater.process.exit_code() != 0 {
        return Err("pagecache user probe syscall failed");
    }

    let size = inode
        .metadata()
        .map_err(|_| "failed to read final inode metadata")?
        .size;
    if size == 0 {
        if cache.contains_page(0) {
            return Err("truncate won but retained the detached page");
        }
    } else if size == PAGE_SIZE as i64 {
        let mut snapshot = [0u8; PAGE_SIZE];
        let read = read_inode(&inode, 0, &mut snapshot)
            .map_err(|_| "failed to read write-after-truncate result")?;
        if read != PAGE_SIZE || !snapshot.iter().all(|byte| *byte == 0xa5) {
            return Err("write won but final page was partial or torn");
        }
    } else {
        return Err("write/truncate published an impossible file size");
    }
    Ok(())
}

fn pagecache_same_page_no_torn_copy() -> Result<(), &'static str> {
    let cache = new_cache();
    let inode: Arc<dyn IndexNode> = Arc::new(FsSmpCacheInode::new(cache.clone(), TmpFS::new()));
    // CPU1/CPU2 在同一发布阶段写同一页；结束后只能看到其中一个完整副本，不能是字节拼接。
    run_dual_user_writes(inode.clone(), 0, &[0x41; PAGE_SIZE], 0, &[0x42; PAGE_SIZE])?;
    let mut snapshot = [0u8; PAGE_SIZE];
    read_inode(&inode, 0, &mut snapshot).map_err(|_| "failed to read same-page snapshot")?;
    if !snapshot.iter().all(|byte| *byte == 0x41) && !snapshot.iter().all(|byte| *byte == 0x42) {
        return Err("same-page copy exposed a torn pattern");
    }
    Ok(())
}

fn pagecache_writeback_redirty() -> Result<(), &'static str> {
    let cache = new_cache();
    let inode: Arc<dyn IndexNode> = Arc::new(FsSmpCacheInode::new(cache.clone(), TmpFS::new()));
    run_dual_user_writes(inode.clone(), 0, &[0x57; PAGE_SIZE], 0, &[0x52; PAGE_SIZE])?;
    cache.writeback_page(0).map_err(|_| "writeback failed")?;
    // writeback 完成后再次由两颗 AP 经 write syscall 标脏，避免 runner 直接改 frame 伪造路径。
    run_dual_user_writes(inode, 0, &[0x52; PAGE_SIZE], 0, &[0x57; PAGE_SIZE])?;
    if cache.state_of(0) != Some(PageState::Dirty) || !cache.is_dirty(0) {
        return Err("writeback/redirty lost Dirty state");
    }
    Ok(())
}

fn different_page_parallel_progress() -> Result<(), &'static str> {
    let cache = new_cache();
    let inode: Arc<dyn IndexNode> = Arc::new(FsSmpCacheInode::new(cache.clone(), TmpFS::new()));
    run_dual_user_writes(inode, 0, &[0x11; PAGE_SIZE], PAGE_SIZE, &[0x31; PAGE_SIZE])?;
    if !cache.contains_page(0) || !cache.contains_page(1) {
        return Err("different-page writes did not retain independent entries");
    }
    Ok(())
}
