use crate::bindings::*;
use alloc::boxed::Box;
use alloc::ffi::CString;
use core::ffi::{c_char, c_void};
use core::ptr::null_mut;
use core::slice::{from_raw_parts, from_raw_parts_mut};
use core::str;

/// Device block size.
const EXT4_DEV_BSIZE: u32 = 512;

pub trait KernelDevOp {
    //type DevType: ForeignOwnable + Sized + Send + Sync = ();
    type DevType;

    //fn write(dev: <Self::DevType as ForeignOwnable>::Borrowed<'_>, buf: &[u8]) -> Result<usize, i32>;
    fn write(dev: &mut Self::DevType, buf: &[u8]) -> Result<usize, i32>;
    fn read(dev: &mut Self::DevType, buf: &mut [u8]) -> Result<usize, i32>;
    fn seek(dev: &mut Self::DevType, off: i64, whence: i32) -> Result<i64, i32>;
    fn flush(dev: &mut Self::DevType) -> Result<usize, i32>
    where
        Self: Sized;
}

pub struct Ext4BlockWrapper<K: KernelDevOp> {
    value: Option<Box<ext4_blockdev>>,
    //block_dev: K::DevType,
    name: [u8; 16],
    mount_point: [u8; 32],
    read_only: bool,
    device_registered: bool,
    fs_mounted: bool,
    journal_started: bool,
    writeback_enabled: bool,
    recovered_orphans: u32,
    pd: core::marker::PhantomData<K>,
}

impl<K: KernelDevOp> Ext4BlockWrapper<K> {
    /// Convenience constructor: uses default device name "ext4_fs" and mount point "/".
    /// For multi-filesystem setups, use `new_with_names()` instead.
    pub fn new(block_dev: K::DevType) -> Result<Self, i32> {
        Self::new_with_names(block_dev, "ext4_fs", "/")
    }

    /// Full constructor with custom device name and mount point.
    /// Use this when mounting multiple ext4 filesystems simultaneously to avoid
    /// `ext4_device_register()` EEXIST collisions.
    pub fn new_with_names(
        block_dev: K::DevType,
        dev_name: &str,
        mount_point_str: &str,
    ) -> Result<Self, i32> {
        Self::new_with_names_and_read_only(block_dev, dev_name, mount_point_str, false)
    }

    /// Full constructor with custom names and explicit mount access mode.
    ///
    /// A read-only mount must be communicated to lwext4 itself, not only to
    /// the backing device: otherwise mount-time journal recovery may still
    /// issue writes before the VFS has a chance to enforce `MS_RDONLY`.
    pub fn new_with_names_and_read_only(
        block_dev: K::DevType,
        dev_name: &str,
        mount_point_str: &str,
        read_only: bool,
    ) -> Result<Self, i32> {
        // note this ownership
        let devt_user = Box::into_raw(Box::new(block_dev)) as *mut c_void;

        // Block size buffer
        let bbuf = Box::new([0u8; EXT4_DEV_BSIZE as usize]);

        let ext4bdif: ext4_blockdev_iface = ext4_blockdev_iface {
            open: Some(Self::dev_open),
            bread: Some(Self::dev_bread),
            bwrite: Some(Self::dev_bwrite),
            close: Some(Self::dev_close),
            flush: Some(Self::dev_flush),
            lock: None,
            unlock: None,
            ph_bsize: EXT4_DEV_BSIZE,
            ph_bcnt: 0,
            ph_bbuf: Box::into_raw(bbuf) as *mut u8,
            ph_refctr: 0,
            bread_ctr: 0,
            bwrite_ctr: 0,
            p_user: devt_user,
        };

        let ext4dev = ext4_blockdev {
            bdif: Box::into_raw(Box::new(ext4bdif)),
            part_offset: 0,
            part_size: 0 * EXT4_DEV_BSIZE as u64,
            // lwext4 binds this to its mount-point-owned cache during mount.
            // Allocating a placeholder here only leaks it when C overwrites
            // the pointer.
            bc: null_mut(),
            lg_bsize: 0,
            lg_bcnt: 0,
            cache_write_back: 0,
            fs: null_mut(),
            journal: null_mut(),
        };

        // Use caller-provided device name (must fit in [u8; 16])
        let c_name = CString::new(dev_name).expect("CString::new dev_name failed");
        let c_name = c_name.as_bytes_with_nul();

        // Use caller-provided mount point (must fit in [u8; 32])
        let c_mountpoint = CString::new(mount_point_str).unwrap();
        let c_mountpoint = c_mountpoint.as_bytes_with_nul();

        let mut name: [u8; 16] = [0; 16];
        let mut mount_point: [u8; 32] = [0; 32];
        name[..c_name.len().min(16)].copy_from_slice(&c_name[..c_name.len().min(16)]);
        mount_point[..c_mountpoint.len().min(32)]
            .copy_from_slice(&c_mountpoint[..c_mountpoint.len().min(32)]);

        let mut ext4bd = Self {
            value: Some(Box::new(ext4dev)),
            name,
            mount_point,
            read_only,
            device_registered: false,
            fs_mounted: false,
            journal_started: false,
            writeback_enabled: false,
            recovered_orphans: 0,
            pd: core::marker::PhantomData,
        };

        info!("New an Ext4 Block Device: {}", dev_name);
        ext4bd.ext4_set_debug();

        unsafe {
            ext4bd.lwext4_mount().map_err(|e| {
                error!("Failed to mount the ext4 file system, perhaps the disk is not an EXT4 file system.");
                e
            })?;
        }

        ext4bd.lwext4_dir_ls();
        ext4bd.print_lwext4_mp_stats();
        ext4bd.print_lwext4_block_stats();

        Ok(ext4bd)
    }
    pub extern "C" fn dev_open(bdev: *mut ext4_blockdev) -> ::core::ffi::c_int {
        unsafe {
        let p_user = (*(*bdev).bdif).p_user;
        debug!("OPEN Ext4 block device p_user={:#x}", p_user as usize);
        // DevType: Disk
        if p_user as usize == 0 {
            error!("Invalid null pointer of p_user");
            return EIO as _;
        }
        //let mut devt = Box::from_raw(p_user as *mut K::DevType);
        let devt = unsafe { &mut *(p_user as *mut K::DevType) };

        // buffering at Disk
        // setbuf(dev_file, buffer);

        let seek_off = K::seek(devt, 0, SEEK_END as i32);
        let cur = match seek_off {
            Ok(v) => v,
            Err(e) => {
                error!("dev_open to K::seek failed: {:?}", e);
                return EFAULT as _;
            }
        };

        (*bdev).part_offset = 0;
        (*bdev).part_size = cur as u64; //ftello()
        (*(*bdev).bdif).ph_bcnt = (*bdev).part_size / (*(*bdev).bdif).ph_bsize as u64;
        EOK as _
        }
    }
    pub extern "C" fn dev_bread(
        bdev: *mut ext4_blockdev,
        buf: *mut ::core::ffi::c_void,
        blk_id: u64,
        blk_cnt: u32,
    ) -> ::core::ffi::c_int {
        unsafe {
        debug!("READ Ext4 block id: {}, count: {}", blk_id, blk_cnt);
        let devt = unsafe { &mut *((*(*bdev).bdif).p_user as *mut K::DevType) };

        let seek_off = K::seek(
            devt,
            (blk_id * ((*(*bdev).bdif).ph_bsize as u64)) as i64,
            SEEK_SET as i32,
        );
        match seek_off {
            Ok(v) => v,
            Err(_e) => return EIO as _,
        };

        if blk_cnt == 0 {
            return EOK as _;
        }

        let buf_len = ((*(*bdev).bdif).ph_bsize * blk_cnt * 1) as usize;
        let buffer = unsafe { from_raw_parts_mut(buf as *mut u8, buf_len) };

        let read_cnt = K::read(devt, buffer);
        match read_cnt {
            Ok(v) => v,
            Err(_e) => return EIO as _,
        };

        EOK as _
        }
    }
    pub extern "C" fn dev_bwrite(
        bdev: *mut ext4_blockdev,
        buf: *const ::core::ffi::c_void,
        blk_id: u64,
        blk_cnt: u32,
    ) -> ::core::ffi::c_int {
        unsafe {
        debug!("WRITE Ext4 block id: {}, count: {}", blk_id, blk_cnt);

        let devt = unsafe { &mut *((*(*bdev).bdif).p_user as *mut K::DevType) };
        //let mut devt = unsafe { K::DevType::borrow_mut((*(*bdev).bdif).p_user) };
        //let mut devt = unsafe { K::DevType::from_foreign((*(*bdev).bdif).p_user) };
        //let mut devt = Box::from_raw((*(*bdev).bdif).p_user as *mut K::DevType);

        let seek_off = K::seek(
            devt,
            (blk_id * ((*(*bdev).bdif).ph_bsize as u64)) as i64,
            SEEK_SET as i32,
        );
        match seek_off {
            Ok(v) => v,
            Err(_e) => return EIO as _,
        };

        if blk_cnt == 0 {
            return EOK as _;
        }

        let buf_len = ((*(*bdev).bdif).ph_bsize * blk_cnt * 1) as usize;
        let buffer = unsafe { from_raw_parts(buf as *const u8, buf_len) };
        if let Err(e) = K::write(devt, buffer) {
            let errno = if e < 0 { e.saturating_neg() } else { e };
            return if errno == 0 { EIO as _ } else { errno as _ };
        }

        // drop_cache();
        // sync

        EOK as _
        }
    }
    pub extern "C" fn dev_close(_bdev: *mut ext4_blockdev) -> ::core::ffi::c_int {
        debug!("CLOSE Ext4 block device");
        //fclose(dev_file);
        EOK as _
    }

    pub extern "C" fn dev_flush(bdev: *mut ext4_blockdev) -> ::core::ffi::c_int {
        unsafe {
            if bdev.is_null() || (*bdev).bdif.is_null() {
                return EIO as _;
            }
            let p_user = (*(*bdev).bdif).p_user;
            if p_user.is_null() {
                return EIO as _;
            }
            let devt = &mut *(p_user as *mut K::DevType);
            match K::flush(devt) {
                Ok(_) => EOK as _,
                Err(error) => {
                    let errno = if error < 0 {
                        error.saturating_neg()
                    } else {
                        error
                    };
                    if errno == 0 { EIO as _ } else { errno as _ }
                }
            }
        }
    }

    pub unsafe fn lwext4_mount(&mut self) -> Result<usize, i32> {
        let c_name = &self.name as *const _ as *const c_char;
        let c_mountpoint = &self.mount_point as *const _ as *const c_char;

        if self.device_registered || self.fs_mounted {
            error!("lwext4_mount called on an active block wrapper");
            return Err(EIO as i32);
        }

        let value = self.value.as_deref_mut().ok_or(EIO as i32)?;
        let r = ext4_device_register(value, c_name);
        if r != EOK as i32 {
            error!("ext4_device_register: rc = {:?}\n", r);
            return Err(r);
        }
        self.device_registered = true;

        let r = ext4_mount(c_name, c_mountpoint, self.read_only);
        if r != EOK as i32 {
            error!("ext4_mount: rc = {:?}\n", r);
            return Err(r);
        }
        self.fs_mounted = true;

        if !self.read_only {
            let r = ext4_recover(c_mountpoint);
            if (r != EOK as i32) && (r != ENOTSUP as i32) {
                error!("ext4_recover: rc = {:?}\n", r);
                return Err(r);
            }

            let r = ext4_journal_start(c_mountpoint);
            if r != EOK as i32 {
                error!("ext4_journal_start: rc = {:?}\n", r);
                return Err(r);
            }
            self.journal_started = true;

            let mut recovered_orphans = 0u32;
            let r = ext4_orphan_cleanup(c_mountpoint, &mut recovered_orphans);
            if r != EOK as i32 {
                error!("ext4_orphan_cleanup: rc = {:?}\n", r);
                return Err(r);
            }
            self.recovered_orphans = recovered_orphans;
            if recovered_orphans != 0 {
                info!("lwext4 recovered {} persistent orphan(s)", recovered_orphans);
            }

            let r = ext4_cache_write_back(c_mountpoint, true);
            if r != EOK as i32 {
                error!("ext4_cache_write_back(enable): rc = {:?}\n", r);
                return Err(r);
            }
            self.writeback_enabled = true;
        }

        info!("lwext4 mount Okay (read_only={})", self.read_only);
        Ok(0)
    }

    pub fn recovered_orphans(&self) -> u32 {
        self.recovered_orphans
    }

    /// Call this when block device is being uninstalled
    pub fn lwext4_umount(&mut self) -> Result<usize, i32> {
        let c_name = &self.name as *const _ as *const c_char;
        let c_mountpoint = &self.mount_point as *const _ as *const c_char;

        unsafe {
            if self.writeback_enabled {
                let r = ext4_cache_write_back(c_mountpoint, false);
                if r != EOK as i32 {
                    error!("ext4_cache_write_back(disable): fail {}", r);
                    return Err(r);
                }
                self.writeback_enabled = false;
            }

            if self.journal_started {
                let r = ext4_journal_stop(c_mountpoint);
                if r != EOK as i32 {
                    error!("ext4_journal_stop: fail {}", r);
                    return Err(r);
                }
                self.journal_started = false;
            }

            if self.fs_mounted {
                let r = ext4_umount(c_mountpoint);
                if r != EOK as i32 {
                    error!("ext4_umount: fail {}", r);
                    return Err(r);
                }
                self.fs_mounted = false;
            }

            if self.device_registered {
                let r = ext4_device_unregister(c_name);
                if r != EOK as i32 {
                    error!("ext4_device_unregister: fail {}", r);
                    return Err(r);
                }
                self.device_registered = false;
            }
        }

        info!("lwext4 umount Okay");
        Ok(0)
    }

    pub fn lwext4_dir_ls(&self) {
        let path = &self.mount_point;
        let mut sss: [u8; 255] = [0; 255];
        let mut d: ext4_dir = unsafe { core::mem::zeroed() };

        let entry_to_str = |entry_type| match entry_type {
            EXT4_DE_UNKNOWN => "[unk] ",
            EXT4_DE_REG_FILE => "[fil] ",
            EXT4_DE_DIR => "[dir] ",
            EXT4_DE_CHRDEV => "[cha] ",
            EXT4_DE_BLKDEV => "[blk] ",
            EXT4_DE_FIFO => "[fif] ",
            EXT4_DE_SOCK => "[soc] ",
            EXT4_DE_SYMLINK => "[sym] ",
            _ => "[???] ",
        };

        info!("ls {}", str::from_utf8(path).unwrap());
        unsafe {
            ext4_dir_open(&mut d, path as *const _ as *const c_char);
            let mut de = ext4_dir_entry_next(&mut d);
            while !de.is_null() {
                let dentry = &(*de);
                sss.copy_from_slice(&dentry.name);
                sss[dentry.name_length as usize] = 0;

                info!(
                    "  {}{}",
                    entry_to_str(dentry.inode_type as u32),
                    str::from_utf8(&sss).unwrap()
                );
                de = ext4_dir_entry_next(&mut d);
            }
            ext4_dir_close(&mut d);
        }
        info!("");
    }

    pub fn ext4_set_debug(&self) {
        unsafe {
            ext4_dmask_set(DEBUG_ALL);
        }
    }

    pub fn print_lwext4_mp_stats(&self) {
        //struct ext4_mount_stats stats;
        let mut stats: ext4_mount_stats = unsafe { core::mem::zeroed() };

        let c_mountpoint = &self.mount_point as *const _ as *const c_char;

        unsafe {
            ext4_mount_point_stats(c_mountpoint, &mut stats);
        }

        info!("********************");
        info!("ext4_mount_point_stats");
        info!("inodes_count = {:x?}", stats.inodes_count);
        info!("free_inodes_count = {:x?}", stats.free_inodes_count);
        info!("blocks_count = {:x?}", stats.blocks_count);
        info!("free_blocks_count = {:x?}", stats.free_blocks_count);
        info!("block_size = {:x?}", stats.block_size);
        info!("block_group_count = {:x?}", stats.block_group_count);
        info!("blocks_per_group= {:x?}", stats.blocks_per_group);
        info!("inodes_per_group = {:x?}", stats.inodes_per_group);

        let vol_name = unsafe { core::ffi::CStr::from_ptr(&stats.volume_name as _) };
        info!("volume_name = {:?}", vol_name);
        info!("********************\n");
    }

    pub fn print_lwext4_block_stats(&self) {
        let Some(ext4dev) = self.value.as_ref() else {
            return;
        };
        //if ext4dev.is_null { return; }

        info!("********************");
        info!("ext4 blockdev stats");
        unsafe {
            info!("bdev->bread_ctr = {:?}", (*ext4dev.bdif).bread_ctr);
            info!("bdev->bwrite_ctr = {:?}", (*ext4dev.bdif).bwrite_ctr);

            info!("bcache->ref_blocks = {:?}", (*ext4dev.bc).ref_blocks);
            info!(
                "bcache->max_ref_blocks = {:?}",
                (*ext4dev.bc).max_ref_blocks
            );
            info!("bcache->lru_ctr = {:?}", (*ext4dev.bc).lru_ctr);
        }
        info!("********************\n");
    }
}

impl<K: KernelDevOp> Drop for Ext4BlockWrapper<K> {
    fn drop(&mut self) {
        info!("Drop struct Ext4BlockWrapper");
        let detached = self.lwext4_umount().is_ok()
            && !self.device_registered
            && !self.fs_mounted
            && !self.journal_started
            && !self.writeback_enabled;

        let Some(mut value) = self.value.take() else {
            return;
        };

        if !detached {
            // C may still retain pointers into `value` or its p_user object.
            // Leaking on a fatal teardown error is safer than leaving a
            // dangling pointer in lwext4's global registry.
            error!("lwext4 teardown incomplete; leaking registered block device safely");
            core::mem::forget(value);
            return;
        }

        unsafe {
            let bdif = value.bdif;
            if !bdif.is_null() {
                let p_user = (*bdif).p_user;
                let ph_bbuf = (*bdif).ph_bbuf;
                (*bdif).p_user = null_mut();
                (*bdif).ph_bbuf = null_mut();
                value.bdif = null_mut();

                if !p_user.is_null() {
                    drop(Box::from_raw(p_user as *mut K::DevType));
                }
                if !ph_bbuf.is_null() {
                    drop(Box::from_raw(
                        ph_bbuf as *mut [u8; EXT4_DEV_BSIZE as usize]
                    ));
                }
                drop(Box::from_raw(bdif));
            }
        }
    }
}
