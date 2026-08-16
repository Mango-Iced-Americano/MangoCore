macro_rules! writable_data_inode_mutations {
    () => {
        fn write_at(
            &self,
            offset: usize,
            len: usize,
            buffer: &[u8],
            data: spin::MutexGuard<crate::fs::vfs::FilePrivateData>,
        ) -> Result<usize, crate::utils::error::SyscallErr> {
            drop(data);
            let fs = self.fs_arc()?;
            match self.file_type {
                crate::fs::vfs::FileType::Dir => {
                    return Err(crate::utils::error::SyscallErr::EISDIR)
                }
                crate::fs::vfs::FileType::File => {}
                _ => return Err(crate::utils::error::SyscallErr::EINVAL),
            }
            let actual = len.min(buffer.len());
            if actual == 0 {
                return Ok(0);
            }
            let end = offset
                .checked_add(actual)
                .ok_or(crate::utils::error::SyscallErr::EFBIG)?;
            let old_size = self
                .lifetime
                .logical_size
                .load(core::sync::atomic::Ordering::Acquire);
            let cache = self.regular_page_cache(&fs)?;
            self.lifetime.publish_pending_write_end(end);
            let written = match cache.write_kernel(offset, &buffer[..actual], old_size) {
                Ok(written) => written,
                Err(error) => {
                    self.lifetime.clear_pending_write_end(end);
                    return Err(error);
                }
            };
            let written_end = match offset.checked_add(written) {
                Some(end) => end,
                None => {
                    self.lifetime.clear_pending_write_end(end);
                    return Err(crate::utils::error::SyscallErr::EFBIG);
                }
            };
            self.lifetime
                .logical_size
                .fetch_max(written_end, core::sync::atomic::Ordering::AcqRel);
            self.lifetime.clear_pending_write_end(end);
            self.lifetime
                .size_generation
                .fetch_add(1, core::sync::atomic::Ordering::AcqRel);
            self.lifetime.retain_dirty_page_cache(&cache);
            Ok(written)
        }

        fn write_at_user(
            &self,
            offset: usize,
            len: usize,
            source: &crate::mm::UserBuffer,
        ) -> Result<usize, crate::utils::error::SyscallErr> {
            let fs = self.fs_arc()?;
            match self.file_type {
                crate::fs::vfs::FileType::Dir => {
                    return Err(crate::utils::error::SyscallErr::EISDIR)
                }
                crate::fs::vfs::FileType::File => {}
                _ => return Err(crate::utils::error::SyscallErr::EINVAL),
            }
            if len > source.len() {
                return Err(crate::utils::error::SyscallErr::EFAULT);
            }
            if len == 0 {
                return Ok(0);
            }
            let cache = self.regular_page_cache(&fs)?;
            let old_size = self
                .lifetime
                .logical_size
                .load(core::sync::atomic::Ordering::Acquire);
            let requested_end = offset
                .checked_add(len)
                .ok_or(crate::utils::error::SyscallErr::EFBIG)?;
            self.lifetime.publish_pending_write_end(requested_end);
            let written = match cache.write_at_user(offset, len, source, old_size) {
                Ok(written) => written,
                Err(error) => {
                    self.lifetime.clear_pending_write_end(requested_end);
                    return Err(error);
                }
            };
            let written_end = match offset.checked_add(written) {
                Some(end) => end,
                None => {
                    self.lifetime.clear_pending_write_end(requested_end);
                    return Err(crate::utils::error::SyscallErr::EFBIG);
                }
            };
            self.lifetime
                .logical_size
                .fetch_max(written_end, core::sync::atomic::Ordering::AcqRel);
            self.lifetime.clear_pending_write_end(requested_end);
            self.lifetime
                .size_generation
                .fetch_add(1, core::sync::atomic::Ordering::AcqRel);
            self.lifetime.retain_dirty_page_cache(&cache);
            Ok(written)
        }

        fn write_direct(
            &self,
            offset: usize,
            len: usize,
            buffer: &[u8],
            data: spin::MutexGuard<crate::fs::vfs::FilePrivateData>,
        ) -> Result<usize, crate::utils::error::SyscallErr> {
            self.write_at(offset, len, buffer, data)
        }

        fn write_sync(
            &self,
            offset: usize,
            buffer: &[u8],
        ) -> Result<usize, crate::utils::error::SyscallErr> {
            let private = spin::Mutex::new(crate::fs::vfs::FilePrivateData::Unused);
            let written = self.write_at(offset, buffer.len(), buffer, private.lock())?;
            self.sync()?;
            Ok(written)
        }

        fn resize(&self, len: usize) -> Result<(), crate::utils::error::SyscallErr> {
            let fs = self.fs_arc()?;
            match self.file_type {
                crate::fs::vfs::FileType::Dir => {
                    return Err(crate::utils::error::SyscallErr::EISDIR)
                }
                crate::fs::vfs::FileType::File => {}
                _ => return Err(crate::utils::error::SyscallErr::EINVAL),
            }
            let inode_id = u32::try_from(self.key.inode_id())
                .map_err(|_| crate::utils::error::SyscallErr::EFBIG)?;
            let old_size = self
                .lifetime
                .logical_size
                .load(core::sync::atomic::Ordering::Acquire);
            if let Some(cache) = self.page_cache() {
                cache.writeback_all_before_io_gate()?;
                cache.with_io_gate(|| {
                    if len < old_size {
                        cache.truncate_with_io_gate_held_and_backend(len, || {
                            fs.run_metadata_operation(|| {
                                fs.inner().truncate_inode(inode_id, len as u64)
                            })
                        })?;
                    } else {
                        fs.run_metadata_operation(|| {
                            fs.inner().truncate_inode(inode_id, len as u64)
                        })?;
                    }
                    Ok::<(), crate::utils::error::SyscallErr>(())
                })?;
            } else {
                fs.run_metadata_operation(|| fs.inner().truncate_inode(inode_id, len as u64))?;
            }
            self.lifetime
                .logical_size
                .store(len, core::sync::atomic::Ordering::Release);
            self.lifetime
                .pending_write_end
                .store(0, core::sync::atomic::Ordering::Release);
            self.lifetime
                .size_generation
                .fetch_add(1, core::sync::atomic::Ordering::AcqRel);
            Ok(())
        }

        fn sync(&self) -> Result<(), crate::utils::error::SyscallErr> {
            let fs = self.fs_arc()?;
            let id = u32::try_from(self.key.inode_id())
                .map_err(|_| crate::utils::error::SyscallErr::EFBIG)?;
            let sync_metadata = || {
                let generation = self
                    .lifetime
                    .size_generation
                    .load(core::sync::atomic::Ordering::Acquire);
                let timestamps = self.lifetime.dirty_timestamps();
                if generation != 0 {
                    let size = self
                        .lifetime
                        .logical_size
                        .load(core::sync::atomic::Ordering::Acquire);
                    fs.run_metadata_operation(|| {
                        fs.inner().commit_inode_size(id, size as u64, None)
                    })?;
                }
                let committed_timestamps = if timestamps.is_some() {
                    fs.commit_lifetime_timestamps(id, &self.lifetime)?
                } else {
                    None
                };

                // The generation and timestamp snapshots remain dirty until
                // every preceding data/metadata write crosses a successful
                // durability boundary. A failed flush must be retryable.
                fs.flush_device()?;
                if generation != 0
                    && self
                        .lifetime
                        .size_generation
                        .compare_exchange(
                            generation,
                            0,
                            core::sync::atomic::Ordering::AcqRel,
                            core::sync::atomic::Ordering::Acquire,
                        )
                        .is_ok()
                {
                    self.lifetime.release_dirty_page_cache();
                }
                if let Some(timestamps) = committed_timestamps {
                    self.lifetime.finish_timestamp_commit(timestamps);
                }
                Ok(())
            };
            if let Some(cache) = self.page_cache() {
                cache.writeback_all_before_io_gate()?;
                return cache.with_io_gate(sync_metadata);
            }
            sync_metadata()
        }

        fn datasync(&self) -> Result<(), crate::utils::error::SyscallErr> {
            self.sync()
        }

        fn discard_write_at(
            &self,
            _offset: usize,
            _len: usize,
            _data: spin::MutexGuard<crate::fs::vfs::FilePrivateData>,
        ) -> Result<usize, crate::utils::error::SyscallErr> {
            let _fs = self.fs_arc()?;
            Err(crate::utils::error::SyscallErr::EOPNOTSUPP)
        }
    };
}

pub(crate) use writable_data_inode_mutations;
