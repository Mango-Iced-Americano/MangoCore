//! FAT32 簇表并发 ktest（零盘、内存块设备）。
//!
//! 在 `MemBlockDevice` 中构造一个簇数满足 FAT32 判定的最小卷，经
//! `EasyFileSystem::open()` 挂载后，以两颗 AP 上的完整用户探针经 syscall
//! 并发驱动簇表的 alloc/free：
//!
//! - CPU1（writer）：文件只创建一次，循环 ftruncate(0) → ftruncate(2048B=4 簇)，
//!   最后一轮保留 4 簇供重挂载扫描。
//! - CPU2（freer）：文件只创建一次，循环 ftruncate(2048B) [alloc 4]
//!   → ftruncate(512B) [free 3] → ftruncate(0) [free 1]。
//!
//! 并发结束后丢弃全部 inode/fs Arc，在同一块设备上重新 `EasyFileSystem::open()`
//! remount，并做簇表完整性扫描：簇链无环（步数上限）、无重复归属（全局 owner 表）、
//! 无越界（簇号 < max_cluster_exclusive）、无游离孤儿簇。
//!
//! fixture 在 ktest 内构造，不依赖宿主 `mkfs.vfat`，也不会把测试镜像打进 normal
//! initramfs。

use alloc::{sync::Arc, vec, vec::Vec};

use crate::{
    drivers::block::partition::BlockSizeAdapter,
    drivers::block::BlockDevice,
    fs::{
        fat32::{EasyFileSystem, FatInode},
        vfs::{
            BackendLifecycle, FileSystem as _, FileType, IndexNode, InodeMode, MountFS,
            MountFSInode, MountFlags,
        },
    },
    hal::BLOCK_SZ,
    kernel_tests::{
        mem_block::MemBlockDevice,
        probe::{
            attach_probe_to_runner, build_path_probe, deadline_after, probe_quiesced, reap_probe,
            stop_probe, ProbeResult,
        },
        runner::KernelTest,
    },
};

const TEST_TIMEOUT_MS: usize = 20_000;
/// FAT 表项中 ≥ 该值的都是链结束标记（EOC/坏块/保留）。
const FAT_RESERVED_END: u32 = 0x0FFF_FFF8;
const FAT_BYTES_PER_SECTOR: usize = 512;
const FAT_RESERVED_SECTORS: usize = 32;
const FAT_DATA_CLUSTERS: usize = 67_000;
const FAT_ENTRY_COUNT: usize = FAT_DATA_CLUSTERS + 2;
const FAT_SECTORS: usize = (FAT_ENTRY_COUNT * core::mem::size_of::<u32>() + FAT_BYTES_PER_SECTOR
    - 1)
    / FAT_BYTES_PER_SECTOR;
const FAT_TOTAL_SECTORS: usize = FAT_RESERVED_SECTORS + FAT_SECTORS + FAT_DATA_CLUSTERS;

/// 构造一个最小但规范的 FAT32 卷。
///
/// 只写 BPB、FSInfo 和 FAT 头三个表项，其余区域由 `MemBlockDevice` 保持为零。
/// 67,000 个数据簇高于当前生产挂载路径的 FAT32 判定阈值，因此测试不需要放宽
/// BPB 校验，也不需要在 normal initramfs 中携带外部镜像。
fn new_fat32_device() -> Result<Arc<MemBlockDevice>, &'static str> {
    let device = Arc::new(MemBlockDevice::new(
        FAT_TOTAL_SECTORS * FAT_BYTES_PER_SECTOR,
    ));

    let mut boot = [0u8; FAT_BYTES_PER_SECTOR];
    boot[0..3].copy_from_slice(&[0xeb, 0x58, 0x90]);
    boot[3..11].copy_from_slice(b"MANGOFS ");
    boot[11..13].copy_from_slice(&(FAT_BYTES_PER_SECTOR as u16).to_le_bytes());
    boot[13] = 1; // 每簇一个扇区，便于测试精确控制分配簇数。
    boot[14..16].copy_from_slice(&(FAT_RESERVED_SECTORS as u16).to_le_bytes());
    boot[16] = 1; // 测试卷只保留一份 FAT，避免无关的镜像写放大。
    boot[21] = 0xf8;
    boot[24..26].copy_from_slice(&63u16.to_le_bytes());
    boot[26..28].copy_from_slice(&255u16.to_le_bytes());
    boot[32..36].copy_from_slice(&(FAT_TOTAL_SECTORS as u32).to_le_bytes());
    boot[36..40].copy_from_slice(&(FAT_SECTORS as u32).to_le_bytes());
    boot[44..48].copy_from_slice(&2u32.to_le_bytes()); // 根目录使用簇 2。
    boot[48..50].copy_from_slice(&1u16.to_le_bytes());
    boot[50..52].copy_from_slice(&6u16.to_le_bytes());
    boot[64] = 0x80;
    boot[66] = 0x29;
    boot[67..71].copy_from_slice(&0x4d41_4e47u32.to_le_bytes());
    boot[71..82].copy_from_slice(b"MANGO KTEST");
    boot[82..90].copy_from_slice(b"FAT32   ");
    boot[510..512].copy_from_slice(&[0x55, 0xaa]);
    device.write_bytes(0, &boot)?;
    device.write_bytes(6 * FAT_BYTES_PER_SECTOR, &boot)?;

    let mut fs_info = [0u8; FAT_BYTES_PER_SECTOR];
    fs_info[0..4].copy_from_slice(&0x4161_5252u32.to_le_bytes());
    fs_info[484..488].copy_from_slice(&0x6141_7272u32.to_le_bytes());
    fs_info[488..492].copy_from_slice(&u32::MAX.to_le_bytes());
    fs_info[492..496].copy_from_slice(&3u32.to_le_bytes());
    fs_info[508..512].copy_from_slice(&0xaa55_0000u32.to_le_bytes());
    device.write_bytes(FAT_BYTES_PER_SECTOR, &fs_info)?;

    let mut fat_head = [0u8; 12];
    fat_head[0..4].copy_from_slice(&0x0fff_fff8u32.to_le_bytes());
    fat_head[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    fat_head[8..12].copy_from_slice(&0x0fff_ffffu32.to_le_bytes());
    device.write_bytes(FAT_RESERVED_SECTORS * FAT_BYTES_PER_SECTOR, &fat_head)?;
    Ok(device)
}

/// 返回本组 FAT32 簇表并发测试。
pub fn tests() -> Vec<KernelTest> {
    vec![KernelTest::with_timeout(
        "fs_fat_smp::cluster_alloc_free_consistency",
        cluster_alloc_free_consistency,
        TEST_TIMEOUT_MS,
    )]
}

fn wait_status_exit_code(status: u32) -> isize {
    (status >> 8) as isize
}

/// 从裸块设备 block 0 读 BPB 的 BytsPerSec（FAT 原生扇区大小）。
fn read_bpb_bytes_per_sector(dev: &Arc<MemBlockDevice>) -> usize {
    let mut buf = [0u8; BLOCK_SZ];
    dev.read_block(0, &mut buf);
    u16::from_le_bytes([buf[11], buf[12]]) as usize
}

/// 把 `MemBlockDevice`（平台块大小）适配为 FAT 原生扇区大小后 `open()`。
///
/// FAT32 的 BPB 通常声明 512B 扇区；平台块设备是 `BLOCK_SZ`（RV64/LA64 QEMU 均为
/// 4096）。`BlockSizeAdapter` 把以 BytsPerSec 为单位的逻辑块号映射到物理字节偏移，
/// 与生产 `sys_mount` 挂载路径使用同一适配层。
fn open_fat(raw: Arc<MemBlockDevice>) -> Arc<EasyFileSystem> {
    let bps = read_bpb_bytes_per_sector(&raw);
    let adapted: Arc<dyn BlockDevice> = if bps == BLOCK_SZ {
        raw
    } else {
        Arc::new(BlockSizeAdapter::new(raw, bps))
    };
    EasyFileSystem::open(adapted)
}

/// 已挂载的 FAT32 ktest fixture：持有 mount / fs / 适配器，Drop 时 detach。
struct MountedFatFixture {
    mount: Arc<MountFS>,
    fs: Arc<EasyFileSystem>,
    _adapted: Arc<dyn BlockDevice>,
}

impl MountedFatFixture {
    fn mount(raw: Arc<MemBlockDevice>) -> Result<Self, &'static str> {
        let bps = read_bpb_bytes_per_sector(&raw);
        let adapted: Arc<dyn BlockDevice> = if bps == BLOCK_SZ {
            raw
        } else {
            Arc::new(BlockSizeAdapter::new(raw, bps))
        };
        let fs = EasyFileSystem::open(adapted.clone());

        // 在 /mnt 下创建（或复用）挂载点并挂载 FAT 卷；探针通过绝对路径 syscall 触达。
        let mnt_dir =
            crate::fs::vfs_lookup_absolute("/mnt").map_err(|_| "ktest has no /mnt mountpoint")?;
        let mountpoint = match mnt_dir.find("fat-smp") {
            Ok(inode) => inode,
            Err(_) => mnt_dir
                .create("fat-smp", FileType::Dir, InodeMode::S_IRWXUGO)
                .map_err(|_| "failed to create /mnt/fat-smp")?,
        };
        let target = mountpoint
            .as_any_ref()
            .downcast_ref::<MountFSInode>()
            .ok_or("/mnt/fat-smp is not a MountFSInode")?;
        let mount = target
            .mount_subtree(
                BackendLifecycle::new(fs.clone()),
                fs.root_inode(),
                MountFlags::empty(),
                Some("/mnt/fat-smp".into()),
            )
            .map_err(|_| "failed to mount ktest FAT32")?;
        Ok(Self {
            mount,
            fs,
            _adapted: adapted,
        })
    }

    /// 把 FAT 根目录的目录项页缓存写回块设备；probe 已 quiesce，无并发写者。
    fn flush(&self) -> Result<(), &'static str> {
        let root = self.fs.root_inode();
        root.sync()
            .map_err(|_| "failed to sync FAT root before remount")
    }
}

impl Drop for MountedFatFixture {
    fn drop(&mut self) {
        // probe 已在调用方 quiesce/reap；这里只从可见挂载树 detach，
        // 生命周期由 Arc 回收（根 inode/FS 的 page cache 写回在 drop 内完成）。
        let _ = self.mount.detach_recursive();
    }
}

/// 从 `start` 出发沿 FAT 链前进，把链上每个簇登记到全局 owner 表。
///
/// 不变量检查（对应 bitmap.rs `mutation` 锁合并要防住的并发损坏）：
/// - 步数上限 = 卷簇数 → 检测环；
/// - 同一簇被两条链共享 → 双归属；
/// - next 指向 0（FREE）/保留值/越界 → 断链或越界。
fn walk_chain(
    fat_entries: &[u32],
    start: u32,
    max: usize,
    owners: &mut [u32],
    owner: u32,
) -> Result<(), &'static str> {
    if !(2..max as u32).contains(&start) {
        return Err("chain start out of range");
    }
    let mut cur = start;
    let mut steps: usize = 0;
    loop {
        if steps >= max {
            return Err("cluster chain too long / cycle");
        }
        if owners[cur as usize] != 0 {
            return Err("cluster double ownership");
        }
        owners[cur as usize] = owner;
        steps += 1;
        let next = fat_entries[cur as usize] & 0x0fff_ffff;
        if next >= FAT_RESERVED_END {
            break; // EOC / 保留值：链正常结束
        }
        if next < 2 || next >= max as u32 {
            return Err("chain next out of range");
        }
        cur = next;
    }
    Ok(())
}

/// 重新挂载后扫描簇表：所有非空闲簇必须恰好归属于一条从根目录可达的链。
fn verify_cluster_table(fs: &EasyFileSystem) -> Result<(), &'static str> {
    let dev = &fs.block_device;
    let max = FAT_ENTRY_COUNT;
    let mut fat_entries = Vec::with_capacity(FAT_SECTORS * FAT_BYTES_PER_SECTOR / 4);
    let mut sector = [0u8; FAT_BYTES_PER_SECTOR];
    for sector_offset in 0..FAT_SECTORS {
        dev.read_block(FAT_RESERVED_SECTORS + sector_offset, &mut sector);
        fat_entries.extend(
            sector
                .chunks_exact(4)
                .map(|entry| u32::from_le_bytes([entry[0], entry[1], entry[2], entry[3]])),
        );
    }
    fat_entries.truncate(max);
    let mut owners = vec![0u32; max];

    // 1) 根目录自身链属于伪所有者 1。
    walk_chain(&fat_entries, fs.root_clus, max, &mut owners, 1)?;

    // 2) 根目录内的每个文件链（fat 分配的文件以 fst_clus 进入目录项）。
    let root = fs.root_inode();
    let root_fat = root
        .as_any_ref()
        .downcast_ref::<FatInode>()
        .ok_or("remounted FAT root is not a FatInode")?;
    let inode_lock = root_fat.write();
    let entries = root_fat
        .ls_lock(&inode_lock)
        .map_err(|_| "failed to list remounted FAT root")?;
    let mut file_id: u32 = 2;
    for (_, short_ent) in entries {
        let fst_clus = short_ent.get_first_clus();
        if fst_clus < 2 {
            continue; // 空文件（尚未分配簇）
        }
        walk_chain(&fat_entries, fst_clus, max, &mut owners, file_id)?;
        file_id += 1;
    }
    drop(inode_lock);

    // 3) 全卷扫描：不可达的非零簇项 = 孤儿/泄漏簇（双分配损坏的典型残留）。
    for cluster in 2..max {
        if owners[cluster] != 0 {
            continue;
        }
        let next = fat_entries[cluster] & 0x0fff_ffff;
        if next != 0 {
            return Err("leaked allocated cluster not reachable from root");
        }
    }
    Ok(())
}

/// CPU1/CPU2 双用户探针并发驱动 FAT alloc/free，remount 后校验簇表一致性。
fn cluster_alloc_free_consistency() -> Result<(), &'static str> {
    if crate::smp::configured_cpu_count() < 3 {
        return Err("SKIP: requires CPU1 and CPU2 user probes");
    }
    let raw = new_fat32_device()?;
    let fixture = MountedFatFixture::mount(raw.clone())?;

    // 两个探针使用不同文件名，只共享同一簇表；各文件只创建一次，避免把目录项
    // create/unlink 生命周期混入簇分配测试。真实交错由 MTTCG 调度决定，不以
    // sleep 伪造窗口。
    let writer = build_path_probe(ProbeResult::FatAllocWriter, b"/mnt/fat-smp/w.bin\0")?;
    let freer = build_path_probe(ProbeResult::FatAllocFree, b"/mnt/fat-smp/f.bin\0")?;
    writer.set_initial_cpus_allowed(1usize << 1);
    freer.set_initial_cpus_allowed(1usize << 2);
    let writer_parent = attach_probe_to_runner(&writer)?;
    let freer_parent = attach_probe_to_runner(&freer)?;
    crate::task::publish_task_on(writer.clone(), 1);
    crate::task::publish_task_on(freer.clone(), 2);

    let writer_done = probe_quiesced(&writer, &writer.process, 1, deadline_after(8));
    let freer_done = probe_quiesced(&freer, &freer.process, 2, deadline_after(8));
    let writer_clean = writer_done || stop_probe(&writer, &writer.process, 1);
    let freer_clean = freer_done || stop_probe(&freer, &freer.process, 2);
    let writer_reaped = reap_probe(&writer_parent, &writer);
    let freer_reaped = reap_probe(&freer_parent, &freer);
    if !writer_clean || !freer_clean || !writer_reaped || !freer_reaped {
        return Err("FAT alloc/free probes did not quiesce and reap");
    }
    if wait_status_exit_code(writer.process.exit_code()) != 0
        || wait_status_exit_code(freer.process.exit_code()) != 0
    {
        crate::println!(
            "[fs_fat_smp] writer_exit={} freer_exit={}",
            wait_status_exit_code(writer.process.exit_code()),
            wait_status_exit_code(freer.process.exit_code())
        );
        return Err("FAT alloc/free probe syscall failed");
    }

    // 根目录目录项写回后丢弃全部 mount/fs/inode Arc（触发各 page cache writeback），
    // 在同一块设备上 remount 并从磁盘重新读取簇表做完整性校验。
    fixture.flush()?;
    drop(fixture);
    let remounted = open_fat(raw);
    verify_cluster_table(&remounted)
}
