use super::common::*;

pub fn sys_mount(
    source: *const u8,
    target: *const u8,
    filesystemtype: *const u8,
    mountflags_raw: usize,
    data: *const u8,
) -> isize {
    // Permission check: mount requires root (CAP_SYS_ADMIN)
    let task = current_task().unwrap();
    let is_root = task.acquire_inner_lock().euid == 0;
    if !is_root {
        return EPERM;
    }
    if target.is_null() {
        return EINVAL;
    }
    let token = current_user_token();
    let target = match user_cstring(token, target) {
        Ok(target) => target,
        Err(errno) => return errno,
    };

    // Validate path length before lookup (Linux: ENAMETOOLONG before ENOENT)
    if let Err(errno) = validate_path_len(&target) {
        return errno;
    }

    // Parse mountflags early — needed for flag routing
    let mountflags = match MountFlags::from_bits(mountflags_raw) {
        Some(f) => f,
        None => return EINVAL,
    };

    // Resolve target path (support CWD-relative)
    let (lookup_inode, lookup_path) = {
        let task = current_task().unwrap();
        let fs_ref = task.process.fs();
        let fs = fs_ref.lock();
        if target.starts_with('/') {
            let root: Arc<dyn vfs::IndexNode> = crate::fs::vfs_root().mountpoint_root_inode();
            (root, target)
        } else {
            let cwd_inode: Arc<dyn vfs::IndexNode> = fs.working_inode.inode.clone();
            let path = normalize_cwd(&fs.working_path, &target);
            (cwd_inode, path)
        }
    };

    // Look up the target inode — must be a directory
    let target_inode = match vfs_lookup(&lookup_inode, &lookup_path, false) {
        Ok(inode) => inode,
        Err(errno) => {
            error!("[sys_mount] vfs_lookup failed for '{}': errno={}", lookup_path, errno);
            return errno;
        }
    };
    let md = match target_inode.metadata() {
        Ok(md) => md,
        Err(e) => return -(e as isize),
    };
    if md.file_type != FileType::Dir {
        return ENOTDIR;
    }
    let inode_id = md.inode_id;

    // ── Flag routing — must happen BEFORE any RamFS creation ──

    let propagation_type_flags = MountFlags::MS_SHARED
        | MountFlags::MS_PRIVATE | MountFlags::MS_SLAVE | MountFlags::MS_UNBINDABLE;
    let prop_type_flag = mountflags & propagation_type_flags;

    // Propagation-type-change commands (e.g., mount --make-shared /mnt)
    // MS_REC is allowed as modifier, but only when there is exactly one
    // propagation-type flag AND no MS_MOVE/MS_REMOUNT.
    // MS_BIND + single propagation flag is allowed (bind, then override).
    let bind_prop_override: Option<vfs::propagation::PropagationType> = if mountflags.intersects(MountFlags::MS_BIND) && !prop_type_flag.is_empty() {
        if prop_type_flag.bits().count_ones() != 1 {
            return EINVAL;
        }
        if prop_type_flag.contains(MountFlags::MS_SHARED) {
            Some(vfs::propagation::PropagationType::Shared)
        } else if prop_type_flag.contains(MountFlags::MS_PRIVATE) {
            Some(vfs::propagation::PropagationType::Private)
        } else if prop_type_flag.contains(MountFlags::MS_SLAVE) {
            Some(vfs::propagation::PropagationType::Slave)
        } else {
            Some(vfs::propagation::PropagationType::Unbindable)
        }
    } else {
        None
    };
    let bind_prop_override_recursive = if bind_prop_override.is_some() {
        mountflags.contains(MountFlags::MS_REC)
    } else {
        false
    };

    if !prop_type_flag.is_empty() {
        if mountflags.intersects(MountFlags::MS_MOVE | MountFlags::MS_REMOUNT)
            || (prop_type_flag.bits().count_ones() != 1 && bind_prop_override.is_none())
        {
            return EINVAL;
        }
        // Pure propagation-type-change (no BIND): apply to existing mount
        if bind_prop_override.is_none() {
            let is_recursive = mountflags.contains(MountFlags::MS_REC);
            let target_mnt_inode = match target_inode.as_any_ref().downcast_ref::<vfs::MountFSInode>() {
                Some(m) => m,
                None => return EINVAL,
            };
            let prop_type = if prop_type_flag.contains(MountFlags::MS_SHARED) {
                vfs::propagation::PropagationType::Shared
            } else if prop_type_flag.contains(MountFlags::MS_PRIVATE) {
                vfs::propagation::PropagationType::Private
            } else if prop_type_flag.contains(MountFlags::MS_SLAVE) {
                vfs::propagation::PropagationType::Slave
            } else {
                vfs::propagation::PropagationType::Unbindable
            };
            let mnt = target_mnt_inode.mount_fs.clone();
            if prop_type == vfs::propagation::PropagationType::Slave {
                let parent_prop = target_mnt_inode.mount_fs.propagation();
                let master_gid = parent_prop.peer_group_id();
                mnt.propagation().set_master_group_id(master_gid);
            }
            vfs::propagation::set_propagation_type(&mnt, prop_type);
            // registration now handled inside set_propagation_type
            if is_recursive {
                set_propagation_recursive(&mnt, prop_type);
            }
            return SUCCESS;
        }
    }

    if mountflags.intersects(MountFlags::MS_BIND) {
        let mnt_fs = match do_bind_mount(source, token, &lookup_inode, &lookup_path, target_inode, mountflags) {
            Ok(fs) => fs,
            Err(errno) => return errno,
        };
        // Apply explicit propagation override if specified (e.g., --bind --make-slave)
        if let Some(prop_type) = bind_prop_override {
            apply_propagation_change(&mnt_fs, prop_type, bind_prop_override_recursive);
        }
        return SUCCESS;
    }

    if mountflags.intersects(MountFlags::MS_MOVE) {
        let source_path = match user_cstring(token, source) {
            Ok(s) => s,
            Err(errno) => return errno,
        };
        let (src_lookup_inode, src_lookup_path) = {
            let task = current_task().unwrap();
            let fs_ref = task.process.fs();
            let fs = fs_ref.lock();
            if source_path.starts_with('/') {
                let root: Arc<dyn vfs::IndexNode> = crate::fs::vfs_root().mountpoint_root_inode();
                (root, source_path)
            } else {
                let cwd: Arc<dyn vfs::IndexNode> = fs.working_inode.inode.clone();
                let path = normalize_cwd(&fs.working_path, &source_path);
                (cwd, path)
            }
        };
        let src_inode = match vfs_lookup(&src_lookup_inode, &src_lookup_path, false) {
            Ok(inode) => inode,
            Err(errno) => {
                error!("[sys_mount] MS_MOVE source lookup failed: {}", errno);
                return errno;
            }
        };
        let src_mnt = match src_inode
            .as_any_ref()
            .downcast_ref::<vfs::MountFSInode>()
            .map(|m| m.mount_fs.clone())
        {
            Some(m) => m,
            None => return EINVAL,
        };

        let old_mp = match src_mnt.self_mountpoint() {
            Some(mp) => mp,
            None => return EINVAL,
        };
        let old_mp_id = match old_mp.inner_inode.metadata() {
            Ok(md) => md.inode_id,
            Err(e) => return -(e as isize),
        };
        let old_parent_mnt = old_mp.mount_fs.clone();

        let target_mnt_inode = match target_inode
            .as_any_ref()
            .downcast_ref::<vfs::MountFSInode>()
        {
            Some(m) => m,
            None => return EINVAL,
        };
        let new_parent_mnt = target_mnt_inode.mount_fs.clone();

        // Reject MS_MOVE from a shared parent: Linux forbids detaching
        // a mount from a shared tree without move-propagation support.
        if old_parent_mnt.propagation().is_shared() {
            return EINVAL;
        }

        // Reject MS_MOVE to a shared parent when the subtree contains
        // unbindable mounts: they cannot be propagated to peers.
        if new_parent_mnt.propagation().is_shared() && subtree_has_unbindable(&src_mnt) {
            return EINVAL;
        }

        // Prevent moving a mount under its own subtree (would create a cycle).
        // Walk parent chain from target: if any ancestor is src_mnt, reject.
        {
            let mut cur = Arc::clone(&new_parent_mnt);
            let mut depth: u32 = 0;
            loop {
                if Arc::ptr_eq(&cur, &src_mnt) {
                    return EINVAL;
                }
                depth += 1;
                if depth > 64 {
                    return EINVAL;
                }
                // Walk up via self_mountpoint
                let next = match cur.self_mountpoint() {
                    Some(mp) => mp.mount_fs.clone(),
                    None => break,
                };
                if Arc::ptr_eq(&next, &cur) {
                    break;
                }
                cur = next;
            }
        }

        // Save old state for rollback if new-parent add fails
        let old_path = src_mnt.mount_path();
        let old_backref = old_mp.clone();

        old_parent_mnt.remove_mount(old_mp_id);

        vfs::mount::MOUNT_LIST.remove_fs(&src_mnt);

        if let Err(e) = new_parent_mnt.add_mount(inode_id, src_mnt.clone()) {
            // Rollback: restore old parent (best-effort, must never panic)
            log::error!(
                "[sys_mount] MS_MOVE add_mount to '{}' failed (errno={}); restoring old parent",
                lookup_path, e as isize,
            );
            if let Err(rollback_err) = old_parent_mnt.add_mount(old_mp_id, src_mnt.clone()) {
                log::error!(
                    "[sys_mount] MS_MOVE rollback failed: add_mount back to old parent errno={}",
                    rollback_err as isize
                );
            } else {
                if let Some(ref old_path) = old_path {
                    vfs::mount::MOUNT_LIST.insert(old_path.as_str(), src_mnt.clone(), Some(old_mp_id));
                }
                src_mnt.set_self_mountpoint(Some(old_backref));
                src_mnt.set_mount_path(old_path);
            }
            return -(e as isize);
        }

        // Success: update to new parent
        let new_backref =
            vfs::MountFSInode::new(target_mnt_inode.inner_inode.clone(), new_parent_mnt.clone());
        src_mnt.set_self_mountpoint(Some(new_backref));

        let old_prefix = old_path.clone();
        let new_prefix = lookup_path.clone();
        src_mnt.set_mount_path(Some(new_prefix.clone()));
        vfs::mount::MOUNT_LIST.insert(new_prefix.as_str(), src_mnt.clone(), Some(inode_id));

        // MS_MOVE must also update mount_path of all descendants.
        // Without this, child mounts retain old paths (e.g., "parent2/a")
        // making them unreachable via umount and causing cleanup loops.
        {
            let mut queue: Vec<Arc<vfs::MountFS>> = {
                let mps = src_mnt.mountpoints.lock();
                mps.values().cloned().collect()
            };
            let mut seen: Vec<usize> = alloc::vec![Arc::as_ptr(&src_mnt) as usize];
            while let Some(child) = queue.pop() {
                let ptr = Arc::as_ptr(&child) as usize;
                if seen.contains(&ptr) || seen.len() > 64 {
                    continue;
                }
                seen.push(ptr);
                if let Some(ref old_child_path) = old_prefix {
                    if let Some(ref cur_path) = child.mount_path() {
                        if let Some(suffix) = cur_path.strip_prefix(old_child_path.as_str()) {
                            let new_child_path = if suffix.is_empty() {
                                new_prefix.clone()
                            } else if suffix.starts_with('/') {
                                alloc::format!("{}{}", new_prefix, suffix)
                            } else {
                                alloc::format!("{}/{}", new_prefix, suffix)
                            };
                            vfs::mount::MOUNT_LIST.remove(cur_path.as_str());
                            vfs::mount::MOUNT_LIST.insert(
                                new_child_path.as_str(), child.clone(), None,
                            );
                            child.set_mount_path(Some(new_child_path));
                        }
                    }
                }
                {
                    let mps = child.mountpoints.lock();
                    for gc in mps.values() {
                        queue.push(gc.clone());
                    }
                }
            }
        }

        // Propagate moved mount tree to new parent's peers.
        // DragonOS: mount into shared parent makes the moved root shared
        // in the parent's peer group. Ensure src_mnt has a non-zero peer
        // group before propagation so clones are created as shared.
        if new_parent_mnt.propagation().is_shared() {
            let src_peer = src_mnt.propagation().peer_group_id();
            if src_peer == 0 {
                vfs::propagation::set_propagation_type(
                    &src_mnt,
                    vfs::propagation::PropagationType::Shared,
                );
            }
            let snapshot = collect_rbind_snapshot(
                src_mnt.clone(),
                src_mnt.mountpoint_root_inode(),
            );
            let child_name = new_prefix.rsplit('/').next().unwrap_or("");
            vfs::propagation::propagate_mount(
                &new_parent_mnt, inode_id, &src_mnt, child_name,
            );
            if !snapshot.is_empty() {
                for peer in vfs::propagation::get_peers(&new_parent_mnt) {
                    let peer_clone = {
                        let mps = peer.mountpoints.lock();
                        mps.get(&inode_id).cloned()
                    };
                    if let Some(clone) = peer_clone {
                        let _ = apply_rbind_snapshot(
                            &snapshot, src_mnt.clone(), clone, &new_prefix,
                        );
                    }
                }
            }
        }

        return SUCCESS;
    }

    if mountflags.intersects(MountFlags::MS_REMOUNT) {
        // Must target a mount root — reject silently falling back to vfs_root()
        let Some(mnt_inode) = target_inode
            .as_any_ref()
            .downcast_ref::<vfs::MountFSInode>()
        else {
            return EINVAL;
        };
        if !mnt_inode.is_mountpoint_root() {
            return EINVAL;
        }

        // User-specified flags with MS_REMOUNT stripped
        let user_flags = vfs::MountFlags::from_bits_truncate(
            (mountflags.bits() & !MountFlags::MS_REMOUNT.bits()) as u32,
        );

        // ── Linux remount flag merge semantics ──
        //
        // Most modifiable flags (non-atime) are replaced by the user-supplied
        // value.  Atime-policy flags (NOATIME, NODIRATIME, RELATIME) are
        // preserved unless the user explicitly provides an atime policy in
        // the remount request.  STRICTATIME is accepted as a remount signal
        // that explicitly clears all atime-policy bits (it is not stored as
        // a persistent flag itself).
        let old_flags = mnt_inode.mount_fs.mount_flags();

        let non_atime_mod = vfs::MountFlags::RDONLY
            | vfs::MountFlags::NOSUID
            | vfs::MountFlags::NODEV
            | vfs::MountFlags::NOEXEC
            | vfs::MountFlags::SYNCHRONOUS
            | vfs::MountFlags::MANDLOCK
            | vfs::MountFlags::DIRSYNC
            | vfs::MountFlags::NOSYMFOLLOW;

        // Atime mask for detecting whether the user provided any atime policy.
        let atime_all = vfs::MountFlags::NOATIME
            | vfs::MountFlags::NODIRATIME
            | vfs::MountFlags::RELATIME
            | vfs::MountFlags::STRICTATIME;

        // Replace non-atime modifiable flags with user's request
        let mut new_flags = (old_flags & !non_atime_mod) | (user_flags & non_atime_mod);

        // Atime: preserve old unless user explicitly specified any atime policy.
        // `normalize_request` handles STRICTATIME (clear all), NOATIME→NODIRATIME
        // implication, NODIRATIME-alone default, and no-flag default (RELATIME).
        if user_flags.intersects(atime_all) {
            new_flags = (new_flags & !atime_all) | vfs::normalize_request(user_flags);
        } else {
            // No atime policy requested: only old canonical atime bits are
            // preserved; non-atime bits (e.g. NOSYMFOLLOW) come from user's
            // explicit request or are cleared — matching Linux remount semantics.
            new_flags = (new_flags & !atime_all)
                | (vfs::canonicalize_state(old_flags) & atime_all);
        }

        mnt_inode.mount_fs.set_mount_flags(new_flags);
        return SUCCESS;
    }

    if mountflags.intersects(MountFlags::MS_REC) {
        // MS_REC is a modifier, not a standalone operation
        return EINVAL;
    }

    // ── Normal mount path ──

    // filesystemtype is required for normal mounts (already checked NULL at entry for bind,
    // but normal mounts need it)
    if filesystemtype.is_null() {
        return EINVAL;
    }

    // Reject mounting over an already-mounted target (Linux: EBUSY)
    // Must be checked after all special-case routing (MS_BIND/MS_MOVE/MS_REMOUNT/propagation)
    // so that those paths are unaffected.
    if let Some(mnt_inode) = target_inode.as_any_ref().downcast_ref::<vfs::MountFSInode>() {
        // If the target inode is itself a mount root (vfs_lookup followed an overlay),
        // this path is already a mountpoint.
        if mnt_inode.is_mountpoint_root() {
            if !mountflags.contains(MountFlags::MS_REMOUNT) {
                return EBUSY;
            }
        }
        // Also check if something is already mounted AT this inode (child mount)
        let inode_id = match mnt_inode.inner_inode.metadata() {
            Ok(md) => md.inode_id,
            Err(e) => return -(e as isize),
        };
        let mountpoints = mnt_inode.mount_fs.mountpoints.lock();
        if mountpoints.contains_key(&inode_id) {
            if !mountflags.contains(MountFlags::MS_REMOUNT) {
                return EBUSY;
            }
        }
    }

    let filesystemtype = match user_cstring(token, filesystemtype) {
        Ok(filesystemtype) => filesystemtype,
        Err(errno) => return errno,
    };

    // For block-backed filesystems, NULL source → EINVAL (not ENOENT via empty string lookup)
    let is_block_based = matches!(
        filesystemtype.as_str(),
        "ext2" | "ext3" | "ext4" | "vfat" | "fat32" | "exfat" | "btrfs" | "xfs" | "ntfs"
    );
    let source = if source.is_null() {
        if is_block_based {
            return EINVAL;
        }
        String::new()
    } else {
        match user_cstring(token, source) {
            Ok(source) => source,
            Err(errno) => return errno,
        }
    };

    info!(
        "[sys_mount] source: {}, target: {}, filesystemtype: {}, mountflags: {:?}, data: {:?}",
        source, lookup_path, filesystemtype, mountflags, data
    );

    if matches!(filesystemtype.as_str(), "cgroup" | "cgroup2") {
        return ENODEV;
    }

    // Use mount_subtree_inner to go through the shared-parent propagation
    // path. The raw MountFS::new() + add_mount() path would bypass child
    // peer group allocation and mount event propagation.
    let target_mfs_inode = match target_inode.as_any_ref().downcast_ref::<vfs::MountFSInode>() {
        Some(m) => m,
        None => return EINVAL,
    };

    let new_fs: Arc<dyn vfs::FileSystem> = match filesystemtype.as_str() {
        "devtmpfs" => crate::fs::dev::DEV_FS.clone(),
        "tmpfs" => crate::fs::tmpfs::TmpFS::new_with_options(4096 * 4096), // ~16MB default
        "sysfs" => {
            let s = crate::fs::sysfs::SysFS::new();
            crate::fs::sysfs::files::register_all(s.root())
                .expect("sysfs: failed to register root entries");
            s
        }
        "proc" => {
            let p = crate::fs::procfs::ProcFS::new();
            crate::fs::procfs::files::register_all(p.root())
                .expect("procfs: failed to register root entries");
            p
        }
        _ => {
            match filesystemtype.as_str() {
                "ext2" | "ext3" | "ext4" | "vfat" | "fat32" => {
                    // 1. Resolve source device path → BlockDevice
                    let dev_inode = match vfs_lookup(&lookup_inode, &source, false) {
                        Ok(i) => i,
                        Err(errno) => return errno,
                    };
                    // Unwrap through MountFS if the inode is a mount-point wrapper
                    let dev_inode = match dev_inode
                        .as_any_ref()
                        .downcast_ref::<vfs::MountFSInode>()
                    {
                        Some(mfsi) => mfsi.inner_inode.clone(),
                        None => dev_inode,
                    };
                    let bdi = match dev_inode.as_any_ref()
                        .downcast_ref::<crate::fs::dev::block::BlockDevInode>()
                    {
                        Some(b) => b,
                        None => return -(SyscallErr::ENOTBLK as isize),
                    };
                    let blk_dev = &bdi.inner;

                    // 2. Detect both filesystem type and its native block
                    // size.  Dynamic mounts must use the same device adapter
                    // path as boot-time mounts (notably for 512-byte FAT
                    // sectors on a 4 KiB platform block device).
                    let detected = match crate::fs::detect_fs_layout(blk_dev) {
                        Some(detected) => detected,
                        None => return -(SyscallErr::EINVAL as isize),
                    };

                    // 3. Validate FS type matches user request
                    let is_ext = matches!(filesystemtype.as_str(), "ext2" | "ext3" | "ext4");
                    match (detected.fs_type, is_ext) {
                        (crate::fs::FS_Type::Ext4, true) => {}
                        (crate::fs::FS_Type::Fat32, false) => {}
                        _ => return -(SyscallErr::EINVAL as isize),
                    }

                    // 4. Adapt native I/O granularity and enforce MS_RDONLY
                    // at the physical block-device boundary before opening
                    // the selected backend.
                    let fs_device = crate::fs::adapt_filesystem_device(
                        blk_dev.clone(),
                        detected,
                        mountflags.contains(MountFlags::MS_RDONLY),
                    );
                    let new_fs: Arc<dyn vfs::FileSystem> = match detected.fs_type {
                        crate::fs::FS_Type::Ext4 => {
                            match crate::fs::ext4_backend::open(
                                fs_device,
                                mountflags.contains(MountFlags::MS_RDONLY),
                            ) {
                                Ok(fs) => fs,
                                Err(e) => return -(e as isize),
                            }
                        }
                        crate::fs::FS_Type::Fat32 => {
                            crate::fs::fat32::EasyFileSystem::open(fs_device)
                        }
                        _ => return -(SyscallErr::EINVAL as isize),
                    };

                    // 5. Insert into mount tree
                    let root_inode = new_fs.root_inode();
                    let mnt_flags = vfs::normalize_request(
                        vfs::MountFlags::from_bits_truncate(mountflags.bits() as u32),
                    );
                    let lifecycle = vfs::BackendLifecycle::new(new_fs);
                    let mnt = match target_mfs_inode.mount_subtree_inner(
                        lifecycle, root_inode, mnt_flags, Some(lookup_path.clone()), true,
                    ) {
                        Ok(m) => m,
                        Err(e) => return -(e as isize),
                    };
                    let _ = mnt;
                    return SUCCESS;
                }
                "exfat" | "btrfs" | "xfs" | "ntfs" => {
                    return -(SyscallErr::ENODEV as isize)
                }
                _ => return -(SyscallErr::ENODEV as isize),
            }
        }
    };
    let root_inode = new_fs.root_inode();
    let mnt_flags = vfs::normalize_request(
        vfs::MountFlags::from_bits_truncate(mountflags.bits() as u32),
    );

    let lifecycle = vfs::BackendLifecycle::new(new_fs);
    let mnt = match target_mfs_inode.mount_subtree_inner(
        lifecycle, root_inode, mnt_flags, Some(lookup_path.clone()), true,
    ) {
        Ok(m) => m,
        Err(e) => return -(e as isize),
    };

    // Dynamic pseudo-fs need dentry cache disabled so hooks fire on every access
    match filesystemtype.as_str() {
        "sysfs" | "proc" => mnt.no_dentry_cache.store(true, core::sync::atomic::Ordering::Relaxed),
        _ => {}
    }

    SUCCESS
}
