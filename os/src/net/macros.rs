/// 为 socket 类型统一生成 `impl File for $ty` 代码块。
/// 所有不适用 socket 的方法统一返回正确错误码，read/write 委托给 try_recv/try_send，
/// r_ready/w_ready/hang_up 委托给 socket_r_ready/socket_w_ready/socket_hang_up。
///
/// 注意：调用此宏前需要在当前文件中引入以下依赖：
///   use $crate::fs::file_trait::File;
///   use $crate::fs::layout::*;
///   use $crate::fs::cache::PageCache;
///   use $crate::fs::directory_tree::DirectoryTreeNode;
///   use $crate::fs::dirent::Dirent;
///   use $crate::mm::UserBuffer;
///   use $crate::utils::error::{GeneralRet, SyscallErr, SyscallRet};
///   use alloc::sync::{Arc, Weak};
///   use alloc::string::String;
///   use alloc::vec::Vec;
///   use spin::Mutex;
macro_rules! impl_file_for_socket {
    ($ty:ty) => {
        impl File for $ty {
            fn deep_clone(&self) -> Arc<dyn File> {
                self.deep_clone_socket()
            }

            fn readable(&self) -> bool {
                true
            }

            fn writable(&self) -> bool {
                true
            }

            fn read(&self, _offset: Option<&mut usize>, buf: &mut [u8]) -> usize {
                match self.try_recv(buf) {
                    Ok(n) => n as usize,
                    Err(e) => e.as_errno_ret(),
                }
            }

            fn write(&self, _offset: Option<&mut usize>, buf: &[u8]) -> usize {
                match self.try_send(buf) {
                    Ok(n) => n as usize,
                    Err(e) => e.as_errno_ret(),
                }
            }

            fn r_ready(&self) -> bool {
                self.socket_r_ready()
            }

            fn w_ready(&self) -> bool {
                self.socket_w_ready()
            }

            fn read_user(&self, _offset: Option<usize>, buf: UserBuffer) -> usize {
                let mut data = vec![0u8; buf.len];
                match self.try_recv(&mut data) {
                    Ok(s) => {
                        let mut offset = 0usize;
                        let mut remain = s as usize;
                        for b in buf.buffers.into_iter() {
                            let copy_len = remain.min(b.len());
                            b[..copy_len].copy_from_slice(&data[offset..offset + copy_len]);
                            offset += copy_len;
                            remain -= copy_len;
                            if remain == 0 {
                                break;
                            }
                        }
                        s as usize
                    }
                    Err(e) => e.as_errno_ret(),
                }
            }

            fn write_user(&self, _offset: Option<usize>, buf: UserBuffer) -> usize {
                let mut data = vec![0u8; buf.len];
                let mut offset = 0;
                for b in buf.buffers.into_iter() {
                    data[offset..offset + b.len()].copy_from_slice(&b);
                    offset += b.len();
                }
                self.write(None, &data)
            }

            fn get_size(&self) -> usize {
                0
            }

            fn get_stat(&self) -> Stat {
                unsafe { core::mem::zeroed() }
            }

            fn get_file_type(&self) -> DiskInodeType {
                DiskInodeType::File
            }

            fn is_dir(&self) -> bool {
                false
            }

            fn is_file(&self) -> bool {
                true
            }

            fn info_dirtree_node(&self, _dirnode_ptr: Weak<DirectoryTreeNode>) {}

            fn get_dirtree_node(&self) -> Option<Arc<DirectoryTreeNode>> {
                None
            }

            fn open(&self, _flags: OpenFlags, _special_use: bool) -> Arc<dyn File> {
                panic!("socket open should not be called");
            }

            fn open_subfile(&self) -> Result<Vec<(String, Arc<dyn File>)>, isize> {
                Err(-($crate::syscall::errno::EISDIR as isize))
            }

            fn create(
                &self,
                _name: &str,
                _file_type: DiskInodeType,
            ) -> Result<Arc<dyn File>, isize> {
                Err(-($crate::syscall::errno::EISDIR as isize))
            }

            fn link_child(&self, _name: &str, _child: &Self) -> Result<(), isize> {
                Err(-($crate::syscall::errno::EISDIR as isize))
            }

            fn unlink(&self, _delete: bool) -> Result<(), isize> {
                Err(-($crate::syscall::errno::EISDIR as isize))
            }

            fn get_dirent(&self, _count: usize) -> Vec<Dirent> {
                Vec::new()
            }

            fn lseek(&self, _offset: isize, _whence: SeekWhence) -> Result<usize, isize> {
                Err(-($crate::syscall::errno::ESPIPE as isize))
            }

            fn modify_size(&self, _diff: isize) -> Result<(), isize> {
                Err(-($crate::syscall::errno::EPERM as isize))
            }

            fn truncate_size(&self, _new_size: usize) -> Result<(), isize> {
                Err(-($crate::syscall::errno::EPERM as isize))
            }

            fn set_timestamp(
                &self,
                _ctime: Option<usize>,
                _atime: Option<usize>,
                _mtime: Option<usize>,
            ) {
            }

            fn get_single_cache(&self, _offset: usize) -> Result<Arc<Mutex<PageCache>>, ()> {
                Err(())
            }

            fn get_all_caches(&self) -> Result<Vec<Arc<Mutex<PageCache>>>, ()> {
                Err(())
            }

            fn oom(&self) -> usize {
                0
            }

            fn hang_up(&self) -> bool {
                self.socket_hang_up()
            }

            fn ioctl(&self, _cmd: u32, _argp: usize) -> isize {
                $crate::syscall::errno::ENOTTY
            }

            fn fcntl(&self, _cmd: u32, _arg: u32) -> isize {
                0
            }
        }
    };
}

pub(crate) use impl_file_for_socket;
