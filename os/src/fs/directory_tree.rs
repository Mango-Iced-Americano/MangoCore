use super::inode::DiskInodeType;
use super::{
    cache::BlockCacheManager,
    dev::{null::Null, tty::Teletype, zero::Zero},
    file_trait::File,
    filesystem::FileSystem,
    layout::OpenFlags,
    Hwclock,
};
use crate::fs::dev::urandom::Urandom;
use crate::fs::fat32::FatOSInode;
use crate::fs::inode;

// ── 旧 VFS trait 定义（待 FAT32 迁移后删除） ─────────────────────────

use crate::fs::cache::BlockCacheManager as BCM;
use crate::fs::BlockDevice;
use downcast_rs::{impl_downcast, DowncastSync};

/// 旧 VFS trait — 仅 FAT32 和目录树仍在使用
/// ext4 已迁移到新的 `FileSystem` trait，FAT32 迁移完成后此 trait 将被删除
pub trait VFS: DowncastSync {
    fn close(&self) -> () {
        todo!();
    }
    fn read(&self) -> alloc::vec::Vec<u8> {
        todo!();
    }
    fn write(&self, _data: alloc::vec::Vec<u8>) -> usize {
        todo!();
    }
    fn get_direcotry(&self) -> ROOT {
        todo!();
    }
    fn alloc_blocks(&self, blocks: usize) -> alloc::vec::Vec<usize>;
    fn get_filesystem_type(&self) -> super::filesystem::FS_Type;
    fn block_size(&self) -> usize;
}
impl_downcast!(sync VFS);

impl VFS {
    pub fn open_fs(
        block_device: alloc::sync::Arc<dyn BlockDevice>,
        index_cache_mgr: alloc::sync::Arc<spin::Mutex<BCM>>,
    ) -> alloc::sync::Arc<Self> {
        let fs_type = super::filesystem::pre_mount();
        match fs_type {
            super::filesystem::FS_Type::Fat32 => {
                super::fat32::EasyFileSystem::open(block_device, index_cache_mgr)
            }
            super::filesystem::FS_Type::Ext4 => {
                super::ext4::ext4fs::Ext4FileSystem::open_ext4rs(block_device, index_cache_mgr)
            }
            super::filesystem::FS_Type::Null => panic!("no filesystem found"),
        }
    }
    pub fn root_osinode(vfs: &alloc::sync::Arc<dyn VFS>) -> alloc::sync::Arc<dyn File> {
        match vfs.get_filesystem_type() {
            super::filesystem::FS_Type::Fat32 => {
                super::fat32::FatOSInode::new(super::fat32::FatInode::root_inode(vfs))
            }
            super::filesystem::FS_Type::Ext4 => {
                use super::ext4::ROOT_INODE;
                use alloc::sync::Arc as A;
                let vfs_concrete = A::downcast::<super::ext4::ext4fs::Ext4FileSystem>(vfs.clone())
                    .unwrap();
                let root_inode = vfs_concrete.get_inode_ref(ROOT_INODE);
                super::ext4::layout::Ext4OSInode::new(root_inode, vfs_concrete)
            }
            super::filesystem::FS_Type::Null => panic!("Null filesystem type does not have a root inode"),
        }
    }
}

/// 对不同类型文件系统文件的封装（仅 FAT32 使用）
pub trait VFSFileContent {}

/// 对不同类型文件系统目录的封装（仅 FAT32 使用）
pub trait VFSDirEnt {}
#[cfg(feature = "oom_handler")]
use crate::mm::tlb_invalidate;
use crate::syscall::errno::*;
use crate::{drivers::BLOCK_DEVICE, fs::filesystem::FS_Type};
use alloc::{
    collections::BTreeMap,
    string::{String, ToString},
    sync::{Arc, Weak},
    vec::Vec,
};
use lazy_static::*;
use spin::{Mutex, MutexGuard, RwLock, RwLockWriteGuard};

lazy_static! {
    // 文件系统实例
    pub static ref FILE_SYSTEM: Arc<dyn VFS> =
        <dyn VFS>::open_fs(BLOCK_DEVICE.clone(), Arc::new(Mutex::new(BlockCacheManager::new())));
    // 目录树根节点
    pub static ref ROOT: Arc<DirectoryTreeNode> = {
        let curr_fs_type = FILE_SYSTEM.get_filesystem_type();
        let inode = DirectoryTreeNode::new(
            // 因为是根节点，所以没有名字（根目录是不是只有‘/’，斜杠左边是不是啥也没有？）
            "".to_string(),
            // 通过获取FILE_SYSTEM的类型来创建目录树的文件系统字段
            Arc::new(FileSystem::new(curr_fs_type)),
            // // 系统Inode，包装了具体文件系统的Inode
            <dyn VFS>::root_osinode(&FILE_SYSTEM),
            // 父节点，因为是根节点所以没有父节点
            Weak::new(),
        );
        inode.add_special_use();
        inode
    };
    pub static ref GLOBAL_BLOCK_SIZE: usize = FILE_SYSTEM.block_size();
    static ref DIRECTORY_VEC: Mutex<(Vec<Weak<DirectoryTreeNode>>, usize)> =
        Mutex::new((Vec::new(), 0));
    static ref PATH_CACHE: Mutex<(String, Weak<DirectoryTreeNode>)> =
        Mutex::new(("".to_string(), Weak::new()));
}

// 插入一个节点到 DIRECTORY_VEC 中
fn insert_directory_vec(inode: Weak<DirectoryTreeNode>) {
    DIRECTORY_VEC.lock().0.push(inode);
}

// 删除一个节点
// 每次触发 Drop 特征时，都会调用这个函数
fn delete_directory_vec() {
    let mut lock = DIRECTORY_VEC.lock();
    // 增加计数器的值
    lock.1 += 1;
    // 如果计数器的值大于等于 DIRECTORY_VEC中节点数的一半，更新 DIRECTORY_VEC
    if lock.1 >= lock.0.len() / 2 {
        update_directory_vec(&mut lock);
    }
}

// 优化 DIRECTORY_VEC，在计数器达到节点数一半时触发
fn update_directory_vec(lock: &mut MutexGuard<(Vec<Weak<DirectoryTreeNode>>, usize)>) {
    // OOM 回收路径也会调用这里，必须原地压缩，避免在堆紧张时再分配新 Vec。
    lock.0.retain(|inode| inode.upgrade().is_some());
    lock.1 = 0;
}

pub struct DirectoryTreeNode {
    /// 如果这是个目录
    /// 1. cwd 当前工作目录
    /// 2. mount point 挂载点
    /// 3. root node 根节点
    /// 如果这是个文件
    /// 1. 被某些进程执行
    /// 该参数在打开时增加1
    spe_usage: Mutex<usize>,
    pub name: String,
    // 文件系统实例
    pub filesystem: Arc<FileSystem>,
    // 文件
    pub file: Arc<dyn File>,
    // 指向自己的弱引用
    selfptr: Mutex<Weak<Self>>,
    // 指向父节点的弱引用
    father: Mutex<Weak<Self>>,
    // 子节点
    pub children: RwLock<Option<BTreeMap<String, Arc<Self>>>>,
}

// 实现 Drop 特征，当一个 DirectoryTreeNode 被销毁时，会调用 delete_directory_vec 函数
impl Drop for DirectoryTreeNode {
    fn drop(&mut self) {
        delete_directory_vec();
    }
}

impl DirectoryTreeNode {
    pub fn new(
        name: String,
        filesystem: Arc<FileSystem>,
        file: Arc<dyn File>,
        father: Weak<Self>,
    ) -> Arc<Self> {
        let node = Arc::new(DirectoryTreeNode {
            // 初始化为0
            spe_usage: Mutex::new(0),
            name,
            filesystem,
            file,
            selfptr: Mutex::new(Weak::new()),
            father: Mutex::new(father),
            // 子节点初始化为 None
            children: RwLock::new(None),
        });
        *node.selfptr.lock() = Arc::downgrade(&node);
        node.file.info_dirtree_node(Arc::downgrade(&node));
        insert_directory_vec(Arc::downgrade(&node));
        node
    }

    pub fn add_special_use(&self) {
        *self.spe_usage.lock() += 1;
    }

    pub fn sub_special_use(&self) {
        *self.spe_usage.lock() -= 1;
    }

    // 获取当前工作目录，返回一个 String 类型（绝对路径）
    pub fn get_cwd(&self) -> String {
        // 创建一个pathv变量，最多容量为8（个String变量）,
        let mut pathv = Vec::<String>::with_capacity(8);
        // 循环获取父节点，直到根节点为止，并将每一级的节点名称添加到pathv中
        let mut current_inode = self.get_arc();
        loop {
            let lock = current_inode.father.lock();
            let par_inode = match lock.upgrade() {
                Some(inode) => inode.clone(),
                None => break,
            };
            drop(lock);
            pathv.push(current_inode.name.clone());
            current_inode = par_inode;
        }
        pathv.push(current_inode.name.clone());
        pathv.reverse();
        if pathv.len() == 1 {
            "/".to_string()
        } else {
            pathv.join("/")
        }
    }

    // 获取自身的强引用，upgrade() 方法返回一个 Option<Arc<T>> 类型
    fn get_arc(&self) -> Arc<Self> {
        self.selfptr.lock().upgrade().unwrap().clone()
    }

    pub fn father_arc(&self) -> Arc<Self> {
        let lock = self.father.lock().clone();
        lock.upgrade().unwrap()
    }

    /// 解析路径
    /// # 参数
    /// + path: 路径
    /// # 返回值
    /// + 一个 Vec<&str> 类型，存储路径的每一级目录
    /// # 说明
    /// 比如路径是“/lib/a/.././d/c”
    /// 那么存入的内容就是
    /// ["a", "d", "c"]
    pub fn parse_dir_path(path: &str) -> Vec<&str> {
        path.split('/').fold(Vec::with_capacity(8), |mut v, s| {
            match s {
                // 去掉空字符串和当前目录
                "" | "." => {}
                ".." => {
                    if v.last().map_or(true, |s| *s == "..") {
                        v.push(s);
                    } else {
                        v.pop();
                    }
                }
                _ => {
                    v.push(s);
                }
            }
            v
        })
    }

    // 缓存该文件夹下的所有子文件到lock中
    fn cache_all_subfile(
        &self,
        lock: &mut RwLockWriteGuard<Option<BTreeMap<String, Arc<Self>>>>,
    ) -> Result<(), isize> {
        if lock.is_some() {
            return Ok(());
        }
        if !self.file.is_dir() {
            return Err(ENOTDIR);
        }
        let vec = match self.file.open_subfile() {
            Ok(vec) => vec,
            Err(errno) => return Err(errno),
        };
        let mut map = BTreeMap::new();
        for (name, file) in vec {
            let key = name.clone();
            let value = Self::new(
                key.clone(),
                self.filesystem.clone(),
                file.clone(),
                Arc::downgrade(&self.get_arc()),
            );
            map.insert(key, value);
        }
        **lock = Some(map);
        Ok(())
    }

    // 尝试获取子文件
    pub fn try_to_open_subfile(
        &self,
        name: &str,
        lock: &mut RwLockWriteGuard<Option<BTreeMap<String, Arc<Self>>>>,
    ) -> Result<Arc<Self>, isize> {
        // 查缓存 — 注意缓存可能因 shrink 缺失条目
        if let Some(ref map) = **lock {
            if let Some(child) = map.get(&name.to_string()) {
                return Ok(child.clone());
            }
        }
        // 缓存 miss：从磁盘只读目标文件，追加到缓存（不替换已有条目，
        // 否则会丢弃已缓存条目持有的未回写脏数据）
        let vec = self.file.open_subfile().map_err(|e| {
            if e == crate::syscall::errno::ENOENT { crate::syscall::errno::ENOENT }
            else { crate::syscall::errno::EIO }
        })?;
        let target_name = name.to_string();
        for (fname, file) in vec {
            // 跳过已存在的条目
            if let Some(ref map) = **lock {
                if map.contains_key(&fname) {
                    continue;
                }
            }
            let new_child = Self::new(
                fname.clone(),
                self.filesystem.clone(),
                file,
                Arc::downgrade(&self.get_arc()),
            );
            if fname == target_name {
                let result = new_child.clone();
                if let Some(ref mut map) = **lock {
                    map.insert(fname, new_child);
                } else {
                    let mut map = BTreeMap::new();
                    map.insert(fname, new_child);
                    **lock = Some(map);
                }
                return Ok(result);
            }
            if let Some(ref mut map) = **lock {
                map.insert(fname, new_child);
            } else {
                let mut map = BTreeMap::new();
                map.insert(fname, new_child);
                **lock = Some(map);
            }
        }
        Err(ENOENT)
    }

    // 通过一个动态数组 components 来进入某个目录
    pub fn cd_comp(&self, components: &Vec<&str>) -> Result<Arc<Self>, isize> {
        let mut current_inode = self.get_arc();
        for component in components {
            if !current_inode.file.is_dir() {
                return Err(ENOTDIR);
            }
            if *component == ".." {
                let lock = current_inode.father.lock();
                let par_inode = lock.upgrade();
                match par_inode {
                    Some(par_inode) => {
                        drop(lock);
                        current_inode = par_inode;
                    }
                    None => {}
                }
                continue;
            }
            let mut lock = current_inode.children.write();
            match current_inode.try_to_open_subfile(component, &mut lock) {
                Ok(child_inode) => {
                    let child_inode = child_inode.clone();
                    drop(lock);
                    current_inode = child_inode.clone()
                }
                Err(errno) => return Err(errno),
            }
            // 跟随中间路径组件中的符号链接
            // 例如 lib64 → /lib ，这样后续组件才能在目标目录中查找
            let mut link_depth = 0;
            while current_inode.file.get_file_type() == DiskInodeType::Link {
                if link_depth >= 8 {
                    return Err(ELOOP);
                }
                let target_path = current_inode.file.read_link();
                let start_inode = if target_path.starts_with('/') {
                    ROOT.clone()
                } else {
                    current_inode.father_arc()
                };
                let comps = Self::parse_dir_path(&target_path);
                let mut current = start_inode;
                for comp in comps.iter() {
                    if *comp == ".." {
                        let maybe_parent = current.father.lock().upgrade();
                        if let Some(par) = maybe_parent {
                            current = par;
                        }
                        continue;
                    }
                    let mut child_lock = current.children.write();
                    match current.try_to_open_subfile(comp, &mut child_lock) {
                        Ok(child) => {
                            let child = child.clone();
                            drop(child_lock);
                            current = child;
                        }
                        Err(errno) => return Err(errno),
                    }
                }
                current_inode = current;
                link_depth += 1;
            }
        }
        Ok(current_inode)
    }
    // 调用 cd_comp 方法，通过一个字符串 path 来进入某个目录
    // 其中 path 会调用 parse_dir_path 方法来解析
    pub fn cd_path(&self, path: &str) -> Result<Arc<Self>, isize> {
        let components = Self::parse_dir_path(path);
        let inode = if path.starts_with("/") {
            &**ROOT
        } else {
            &self
        };
        inode.cd_comp(&components)
    }

    // // 创建一个子文件，文件名和文件类型由参数提供
    // // file_type: 文件是常规文件还是目录
    pub fn create(&self, name: &str, file_type: DiskInodeType) -> Result<Arc<dyn File>, isize> {
        // if name == "" || !self.file.is_dir() {
        //     debug_assert!(false);
        // }
        self.file.create(name, file_type)
    }
    // // 判断路径是否存在
    // pub fn path_exists(&self, path: &str) -> bool {
    //     // 解析路径
    //     let components = Self::parse_dir_path(path);

    //     // 如果路径以 `/` 开头，表示从根目录开始查找
    //     let inode = if path.starts_with("/") {
    //         &**ROOT
    //     } else {
    //         &self
    //     };

    //     // 解析路径
    //     match inode.cd_comp(&components) {
    //         Ok(parent_inode) => {
    //             // 获取路径最后一个组件
    //             let last_comp = components.last();
    //             if let Some(last_comp) = last_comp {
    //                 // 检查最后一个组件是否存在
    //                 let lock = parent_inode.children.read();
    //                 if let Some(child) = lock.as_ref().and_then(|m| m.get(*last_comp)) {
    //                     // 文件或目录存在
    //                     return true;
    //                 }
    //             }
    //             false // 最后一个组件不存在
    //         },
    //         Err(_) => false, // 路径无效
    //     }
    // }

    // 模拟文件系统的 open 调用
    pub fn open(
        &self,
        path: &str,
        flags: OpenFlags,
        special_use: bool,
    ) -> Result<Arc<dyn File>, isize> {
        log::info!("[open]: cwd: {}, path: {}", self.get_cwd(), path);
        // println!("open file in dtn: cwd: {} name: {}",self.get_cwd(), path );

        // 重定向链接库
        let path = match path {
            /* "/lib/ld-linux-riscv64-lp64.so.1" => "/musl/lib/libc.so",
            "/lib/ld-linux-riscv64-lp64d.so.1" => "/musl/lib/libc.so",
            "/lib/ld-musl-riscv64.so.1" | "/lib/ld-musl-riscv64-sf.so.1" => "/musl/lib/libc.so",
            "/lib64/ld-linux-loongarch-lp64d.so.1" => "/glibc/lib/ld-linux-loongarch-lp64d.so.1",
            "libm.so.6" => "/glibc/lib/libm.so.6",
            "/lib64/ld-musl-loongarch-lp64d.so.1" => "/musl/lib/libc.so",
            "/usr/lib/tls_get_new-dtv_dso.so" => "./libtls_get_new-dtv_dso.so", */
            _ => path,
        };

        // 获取目录树根节点
        let mut inode = if path.starts_with("/") {
            &**ROOT
        } else {
            &self
        };
        log::info!("[open]: origin file type {:?}", inode.file.get_file_type());
        // 获取路径缓存
        let mut path_cache_lock = PATH_CACHE.lock();
        // 如果路径以 '/' 开头，且路径等于缓存路径，且缓存路径的弱引用存在
        let mut inode = if path.starts_with('/')
            && path == path_cache_lock.0
            && path_cache_lock.1.upgrade().is_some()
        {
            // 获取缓存路径的弱引用
            path_cache_lock.1.upgrade().unwrap()
        } else {
            // 解析路径
            let mut components = Self::parse_dir_path(path);
            // 获取目录栈的栈顶，也就是父目录或者文件本身
            let last_comp = components.pop();
            // 从剩余的路径中获取父目录节点
            let inode = match inode.cd_comp(&components) {
                Ok(inode) => inode,
                Err(errno) => return Err(errno),
            };
            // 跟随中间路径组件中的符号链接（例如 lib64 → /lib）
            // 如果父目录 inode 本身是软链接，需要解析到实际目录
            // 然后再用这个实际目录去查找最后一个路径组件
            let inode = {
                let mut resolved = inode;
                let mut link_depth = 0;
                while resolved.file.get_file_type() == DiskInodeType::Link {
                    if link_depth >= 8 {
                        return Err(ELOOP);
                    }
                    let target_path = resolved.file.read_link();
                    let start_inode = if target_path.starts_with('/') {
                        ROOT.clone()
                    } else {
                        resolved.father_arc()
                    };
                    let comps = Self::parse_dir_path(&target_path);
                    let mut current = start_inode;
                    for comp in comps.iter() {
                        if *comp == ".." {
                            let maybe_parent = current.father.lock().upgrade();
                            if let Some(par) = maybe_parent {
                                current = par;
                            }
                            continue;
                        }
                        let mut lock = current.children.write();
                        match current.try_to_open_subfile(comp, &mut lock) {
                            Ok(child) => {
                                let child = child.clone();
                                drop(lock);
                                current = child;
                            }
                            Err(errno) => return Err(errno),
                        }
                    }
                    resolved = current;
                    link_depth += 1;
                }
                resolved
            };
            // 若最后一个组件存在，则进行处理
            if let Some(last_comp) = last_comp {
                let mut lock = inode.children.write();
                match inode.try_to_open_subfile(last_comp, &mut lock) {
                    Ok(inode) => {
                        if flags.contains(OpenFlags::O_CREAT | OpenFlags::O_EXCL) {
                            return Err(EEXIST);
                        }
                        inode
                    }
                    Err(ENOENT) => {
                        if !flags.contains(OpenFlags::O_CREAT) {
                            return Err(ENOENT);
                        }
                        // println!("last_comp:{:?}", last_comp);
                        let new_file = match inode.create(last_comp, DiskInodeType::File) {
                            Ok(file) => file,
                            Err(errno) => return Err(errno),
                        };
                        let key = (*last_comp).to_string();
                        let value = Self::new(
                            key.clone(),
                            inode.filesystem.clone(),
                            new_file,
                            Arc::downgrade(&inode.get_arc()),
                        );
                        let new_inode = value.clone();
                        lock.as_mut().unwrap().insert(key, value);
                        new_inode
                    }
                    Err(errno) => {
                        return Err(errno);
                    }
                }
            } else {
                inode
            }
        };

        // 软链接追踪逻辑
        let mut final_inode = inode.clone();
        let mut link_depth = 0;

        while final_inode.file.get_file_type() == DiskInodeType::Link {
            if link_depth >= 8 {
                return Err(ELOOP);
            }

            let target_path = final_inode.file.read_link();
            log::info!("[open]: link target path: {}", target_path);

            // 决定起始查找起点，绝对路径从root,相对路径从父目录
            let start_inode = if target_path.starts_with('/') {
                ROOT.clone()
            } else {
                final_inode.father_arc()
            };

            // 解析链接目标路径
            let components = Self::parse_dir_path(&target_path);
            let mut current_inode = start_inode;

            for comp in components.iter() {
                if *comp == ".." {
                    let maybe_parent = {
                        let guard = current_inode.father.lock();
                        guard.upgrade()
                    };

                    if let Some(par) = maybe_parent {
                        current_inode = par;
                    }
                    continue;
                }

                let mut lock = current_inode.children.write();
                match current_inode.try_to_open_subfile(comp, &mut lock) {
                    Ok(child_inode) => {
                        let child_inode = child_inode.clone();
                        drop(lock);
                        current_inode = child_inode;
                    }
                    Err(errno) => return Err(errno),
                }
            }
            final_inode = current_inode;
            link_depth += 1;
        }
        let final_file_type = final_inode.file.get_file_type();
        inode = final_inode;

        log::info!(
            "[open] final file type of {} is {:?} ",
            path,
            final_file_type,
        );
        if flags.contains(OpenFlags::O_TRUNC) {
            match inode.file.truncate_size(0) {
                Ok(_) => {}
                Err(errno) => return Err(errno),
            }
        }

        if inode.file.is_file()
            && *inode.spe_usage.lock() > 0
            && (flags.contains(OpenFlags::O_WRONLY) || flags.contains(OpenFlags::O_RDWR))
        {
            return Err(ETXTBSY);
        }

        if inode.file.is_dir()
            && (flags.contains(OpenFlags::O_WRONLY) || flags.contains(OpenFlags::O_RDWR))
        {
            return Err(EISDIR);
        }

        if !inode.file.is_dir() && flags.contains(OpenFlags::O_DIRECTORY) {
            return Err(ENOTDIR);
        }

        if special_use {
            *inode.spe_usage.lock() += 1;
        }

        if path.starts_with('/') && path != path_cache_lock.0 {
            *path_cache_lock = (path.to_string(), Arc::downgrade(&inode.get_arc()));
        }

        Ok(inode.file.open(flags, special_use))
    }

    // 创建一个文件夹
    pub fn mkdir(&self, path: &str) -> Result<(), isize> {
        let inode = if path.starts_with("/") {
            &**ROOT
        } else {
            &self
        };

        let mut components = Self::parse_dir_path(path);
        let last_comp = components.pop();
        let inode = match inode.cd_comp(&components) {
            Ok(inode) => inode,
            Err(errno) => return Err(errno),
        };

        if let Some(last_comp) = last_comp {
            let mut lock = inode.children.write();
            match inode.try_to_open_subfile(last_comp, &mut lock) {
                Ok(_) => {
                    return Err(EEXIST);
                }
                Err(ENOENT) => {
                    let new_file = match inode.create(last_comp, DiskInodeType::Directory) {
                        Ok(file) => file,
                        Err(errno) => return Err(errno),
                    };
                    let key = (*last_comp).to_string();
                    let value = Self::new(
                        key.clone(),
                        inode.filesystem.clone(),
                        new_file,
                        Arc::downgrade(&inode.get_arc()),
                    );
                    let new_inode = value.clone();
                    lock.as_mut().unwrap().insert(key, value);
                    new_inode
                }
                Err(errno) => {
                    return Err(errno);
                }
            }
        } else {
            return Err(EEXIST);
        };

        Ok(())
    }

    // 删除一个文件夹或文件
    pub fn delete(&self, path: &str, delete_directory: bool) -> Result<(), isize> {
        if path.split('/').last().map_or(true, |x| x == ".") {
            return Err(EINVAL);
        }

        let inode = if path.starts_with("/") {
            &**ROOT
        } else {
            &self
        };

        let components = Self::parse_dir_path(path);
        let last_comp = *components.last().unwrap();
        let inode = match inode.cd_comp(&components) {
            Ok(inode) => inode,
            Err(errno) => return Err(errno),
        };

        if *inode.spe_usage.lock() > 0 {
            return Err(EBUSY);
        }

        if !delete_directory && inode.file.is_dir() {
            return Err(EISDIR);
        }

        if delete_directory && !inode.file.is_dir() {
            return Err(ENOTDIR);
        }

        match {
            // 首先获取父节点的 Weak 引用并释放锁
            let father_weak = inode.father.lock().clone();
            father_weak.upgrade()
        } {
            Some(par_inode) => {
                let mut lock = par_inode.children.write();
                match inode.file.unlink(true) {
                    Ok(_) => {
                        let key = last_comp.to_string();
                        lock.as_mut().unwrap().remove(&key);
                    }
                    Err(errno) => return Err(errno),
                }
            }
            None => return Err(EACCES),
        }
        Ok(())
    }

    // 重命名一个文件夹或文件
    pub fn rename(old_path: &str, new_path: &str) -> Result<(), isize> {
        assert!(old_path.starts_with('/'));
        assert!(new_path.starts_with('/'));

        let mut old_comps = Self::parse_dir_path(old_path);
        let mut new_comps = Self::parse_dir_path(new_path);

        if old_comps == new_comps {
            return Ok(());
        }

        if new_comps.starts_with(&old_comps) {
            return Err(EINVAL);
        }
        // We gurantee that last component isn't empty
        let old_last_comp = old_comps.pop().unwrap();
        let new_last_comp = new_comps.pop().unwrap();

        let old_par_inode = match ROOT.cd_comp(&old_comps) {
            Ok(inode) => inode,
            Err(errno) => return Err(errno),
        };
        let new_par_inode = match ROOT.cd_comp(&new_comps) {
            Ok(inode) => inode,
            Err(errno) => return Err(errno),
        };
        type ChildLockType<'a> =
            RwLockWriteGuard<'a, Option<BTreeMap<String, Arc<DirectoryTreeNode>>>>;

        let old_lock: Arc<Mutex<ChildLockType<'_>>>;
        let new_lock: Arc<Mutex<ChildLockType<'_>>>;

        // Be careful about the lock ordering
        if old_comps == new_comps {
            old_lock = Arc::new(Mutex::new(old_par_inode.children.write()));
            new_lock = old_lock.clone();
        } else if old_comps < new_comps {
            old_lock = Arc::new(Mutex::new(old_par_inode.children.write()));
            new_lock = Arc::new(Mutex::new(new_par_inode.children.write()));
        } else {
            new_lock = Arc::new(Mutex::new(new_par_inode.children.write()));
            old_lock = Arc::new(Mutex::new(old_par_inode.children.write()));
        }

        let old_inode =
            match old_par_inode.try_to_open_subfile(old_last_comp, &mut (*old_lock.lock())) {
                Ok(inode) => inode,
                Err(errno) => return Err(errno),
            };

        if *old_inode.spe_usage.lock() > 0 {
            return Err(EBUSY);
        }

        if old_inode.filesystem.fs_id != new_par_inode.filesystem.fs_id {
            return Err(EXDEV);
        }
        let old_key = old_last_comp.to_string();
        let new_key = new_last_comp.to_string();
        match new_par_inode.try_to_open_subfile(new_last_comp, &mut (*new_lock.lock())) {
            Ok(new_inode) => {
                if new_inode.file.is_dir() && !old_inode.file.is_dir() {
                    return Err(EISDIR);
                }
                if old_inode.file.is_dir() && !new_inode.file.is_dir() {
                    return Err(ENOTDIR);
                }
                if *new_inode.spe_usage.lock() > 0 {
                    return Err(EBUSY);
                }
                // delete
                match new_par_inode.file.unlink(true) {
                    Ok(_) => {
                        new_lock.lock().as_mut().unwrap().remove(&new_key);
                    }
                    Err(errno) => return Err(errno),
                }
            }
            Err(ENOENT) => {}
            Err(errno) => return Err(errno),
        }

        let value = old_lock.lock().as_mut().unwrap().remove(&old_key).unwrap();
        match old_inode.file.unlink(false) {
            Ok(_) => {}
            Err(errno) => return Err(errno),
        };
        match old_inode.filesystem.fs_type {
            FS_Type::Fat32 => {
                let old_file = old_inode.file.downcast_ref::<FatOSInode>().unwrap();
                let new_par_file = new_par_inode.file.downcast_ref::<FatOSInode>().unwrap();
                new_par_file.link_child(old_last_comp, old_file)?;
            }
            FS_Type::Ext4 => {
                use crate::fs::ext4::layout::Ext4OSInode;
                let old_file = old_inode.file.downcast_ref::<Ext4OSInode>().unwrap();
                let new_par_file = new_par_inode.file.downcast_ref::<Ext4OSInode>().unwrap();
                new_par_file.link_child(old_last_comp, old_file)?;
            }
            FS_Type::Null => return Err(EACCES),
        }
        *value.father.lock() = Arc::downgrade(&new_par_inode.get_arc());
        new_lock.lock().as_mut().unwrap().insert(new_key, value);

        Ok(())
    }

    // 创建一个符号链接
    pub fn symlink(&self, target: &str, linkpath: &str) -> Result<(), isize> {
        // 1. 解析要创建链接的父目录
        let inode = if linkpath.starts_with("/") {
            &**ROOT
        } else {
            &self
        };
        let mut components = Self::parse_dir_path(linkpath);
        let link_name = components.pop().ok_or(crate::syscall::errno::ENOENT)?;

        let parent_inode = inode.cd_comp(&components)?;

        // 2. 检查该名字是否已存在
        let mut lock = parent_inode.children.write();
        if parent_inode
            .try_to_open_subfile(link_name, &mut lock)
            .is_ok()
        {
            return Err(crate::syscall::errno::EEXIST); // 文件已存在
        }

        // 3. 在底层创建文件，类型指定为 Link
        let new_file = parent_inode.create(link_name, DiskInodeType::Link)?;

        // 4. 将目标路径（target）写入这个新文件
        new_file.write_link(target)?;

        // 5. 更新 VFS 缓存
        let key = link_name.to_string();
        let value = Self::new(
            key.clone(),
            parent_inode.filesystem.clone(),
            new_file,
            Arc::downgrade(&parent_inode.get_arc()),
        );
        lock.as_mut().unwrap().insert(key, value);

        Ok(())
    }
}

// 用于处理OOM的情况，被 mm 模块调用
// 会调用 tlb_invalidate 函数，在 arch/la64中实现
// 会调用 update_directory_vec 函数
#[cfg(feature = "oom_handler")]
pub fn oom() -> usize {
    tlb_invalidate();
    const MAX_FAIL_TIME: usize = 3;
    let mut fail_time = 0;
    log::warn!("[oom] start oom");
    let mut lock = DIRECTORY_VEC.lock();
    update_directory_vec(&mut lock);
    // 先执行 VFS 剪枝，回收仅作缓存的目录节点（strong_count == 2 且 spe_usage == 0）
    // 这能释放大量内核堆内存
    drop(lock);
    shrink();
    let mut lock = DIRECTORY_VEC.lock();
    loop {
        let mut dropped = 0;
        for weak_inode in &lock.0 {
            if let Some(inode) = weak_inode.upgrade() {
                dropped += inode.file.oom();
            }
        }
        if dropped > 0 {
            log::warn!("[oom] recycle pages: {}", dropped);
            return dropped;
        }
        fail_time += 1;
        if fail_time >= MAX_FAIL_TIME {
            return dropped;
        }
    }
}

/// VFS 剪枝器（Shrinker）：剔除 strong_count == 2 且 spe_usage == 0 的缓存目录节点。
///
/// strong_count == 2 意味着唯一的强引用来自父节点的 `children` BTreeMap
/// 以及 upgrade() 产生的临时 Arc。这些节点没有被任何进程的 FD、cwd 或 mmap 持有。
/// 可以安全移除，释放它们占用的内核堆内存（String、BTreeMap 节点等）。
///
/// 为避免在堆紧张时分配新 Vec，该函数原地遍历 DIRECTORY_VEC。
pub fn shrink() {
    log::info!("[vfs-shrink] start shrinking directory tree nodes");
    // OOM 场景下调用栈可能很深，批次大小控制在 64（~1KB 栈）减小栈溢出风险。
    // 若仍有待回收节点，oom() 的循环会再次调用本函数，小步快跑更安全。
    const BATCH_SIZE: usize = 64;
    let mut to_prune: [Option<Weak<DirectoryTreeNode>>; BATCH_SIZE] =
        core::array::from_fn(|_| None);
    let mut to_prune_count = 0usize;

    {
        // 阶段一：扫描全局树节点数组（加锁）
        let mut lock = DIRECTORY_VEC.lock();
        update_directory_vec(&mut lock);

        for weak_node in lock.0.iter() {
            if to_prune_count >= BATCH_SIZE {
                break;
            }
            if let Some(node) = weak_node.upgrade() {
                // 不剪枝根目录的直接子节点 — 它们是真实文件系统上的目录
                // （/musl, /glibc, /proc, /dev, /tmp 等），不是路径遍历产生的缓存。
                let is_root_child = {
                    node.father.lock().upgrade()
                        .map_or(false, |father| father.father.lock().upgrade().is_none())
                };
                if is_root_child {
                    continue;
                }
                // count == 2：只有 parent.children 和 upgrade() 产生的临时 Arc 持有
                // spe_usage == 0：当前没有作为 CWD 或被打开的文件正在使用
                if Arc::strong_count(&node) == 2 && *node.spe_usage.lock() == 0 {
                    to_prune[to_prune_count] = Some(weak_node.clone());
                    to_prune_count += 1;
                }
            }
        }
    } // [关键] 扫描完毕，释放 DIRECTORY_VEC 锁

    let mut pruned = 0usize;

    // 阶段二：安全执行剪枝操作。
    // 关键锁顺序：先获取 father（短暂持有，仅取 Arc），释放 father 锁，
    // 再获取 parent.children。避免与自上而下（children→child）路径形成 ABBA 死锁。
    for entry in to_prune.iter().take(to_prune_count) {
        if let Some(weak) = entry {
            if let Some(node) = weak.upgrade() {
                // Double check，因为锁释放期间状态可能发生变化
                if *node.spe_usage.lock() != 0 {
                    continue;
                }
                // 关键：不在持有 node.father 锁的情况下去拿 parent.children 锁
                let parent_arc = node.father.lock().upgrade();
                // node.father 的 MutexGuard 已释放

                if let Some(parent) = parent_arc {
                    let mut children_lock = parent.children.write();
                    // <= 3：父目录 map + 当前 node + 可能的短暂引用（非 spe_usage 路径）
                    if Arc::strong_count(&node) <= 3 && *node.spe_usage.lock() == 0 {
                        if let Some(ref mut map) = *children_lock {
                            if map.remove(&node.name).is_some() {
                                pruned += 1;
                            }
                            if map.is_empty() {
                                *children_lock = None;
                            }
                        }
                    }
                }
                // node (Arc) 在此释放；若为最后一个强引用，触发 Drop →
                // delete_directory_vec()，此时不持有任何锁，可安全获取 DIRECTORY_VEC。
            }
        }
    }

    if pruned > 0 {
        log::warn!("[vfs-shrink] pruned {} unused directory tree nodes", pruned);
    }
}

/// 返回目录树中存活的节点数（诊断用）
pub fn directory_node_count() -> usize {
    let lock = DIRECTORY_VEC.lock();
    lock.0.iter().filter(|w| w.upgrade().is_some()).count()
}

// 初始化文件系统
pub fn init_fs() {
    init_device_directory();
    init_tmp_directory();
    init_proc_directory();
}
#[allow(unused)]
// 初始化设备目录
fn init_device_directory() {
    ROOT.mkdir("/dev");

    let dev_inode = match ROOT.cd_path("/dev") {
        Ok(inode) => inode,
        Err(_) => panic!("dev directory doesn't exist"),
    };

    println!("[kernel] /dev init Successfully!");

    dev_inode.mkdir("shm");
    dev_inode.mkdir("misc");

    println!("[kernel] shm and misc init Successfully!");

    let null_dev = DirectoryTreeNode::new(
        "null".to_string(),
        Arc::new(FileSystem::new(FS_Type::Null)),
        Arc::new(Null {}),
        Arc::downgrade(&dev_inode.get_arc()),
    );
    println!("[kernel] null_dev init successfully!");
    let zero_dev = DirectoryTreeNode::new(
        "zero".to_string(),
        Arc::new(FileSystem::new(FS_Type::Null)),
        Arc::new(Zero {}),
        Arc::downgrade(&dev_inode.get_arc()),
    );
    println!("[kernel] zero_dev init successfully!");
    let urandom_dev = DirectoryTreeNode::new(
        "urandom".to_string(),
        Arc::new(FileSystem::new(FS_Type::Null)),
        Arc::new(Urandom {}),
        Arc::downgrade(&dev_inode.get_arc()),
    );
    println!("[kernel] urandom_dev init successfully!");
    let tty_dev = DirectoryTreeNode::new(
        "tty".to_string(),
        Arc::new(FileSystem::new(FS_Type::Null)),
        Arc::new(Teletype::new()),
        Arc::downgrade(&dev_inode.get_arc()),
    );

    println!("[kernel] tty_dev init successfully!");
    let mut lock = dev_inode.children.write();
    lock.as_mut().unwrap().insert("null".to_string(), null_dev);
    lock.as_mut().unwrap().insert("zero".to_string(), zero_dev);
    lock.as_mut().unwrap().insert("tty".to_string(), tty_dev);
    lock.as_mut()
        .unwrap()
        .insert("urandom".to_string(), urandom_dev);
    drop(lock);

    let misc_inode = match dev_inode.cd_path("./misc") {
        Ok(inode) => inode,
        Err(_) => panic!("misc directory doesn't exist"),
    };
    let hwclock_dev = DirectoryTreeNode::new(
        "rtc".to_string(),
        Arc::new(FileSystem::new(FS_Type::Null)),
        Arc::new(Hwclock {}),
        Arc::downgrade(&misc_inode.get_arc()),
    );
    let mut lock = misc_inode.children.write();
    misc_inode.cache_all_subfile(&mut lock);
    lock.as_mut()
        .unwrap()
        .insert("rtc".to_string(), hwclock_dev);
    drop(lock);
}
// 初始化临时文件目录
fn init_tmp_directory() {
    match ROOT.mkdir("/tmp") {
        _ => {}
    }
    println!("[kernel] init_tmp_directory successfully!");
}
// 初始化进程目录
fn init_proc_directory() {
    match ROOT.mkdir("/proc") {
        _ => {}
    }
    println!("[kernel] init_proc_directory successfully!");
    match ROOT.open(
        "/proc/meminfo",
        OpenFlags::O_CREAT | OpenFlags::O_RDWR | OpenFlags::O_TRUNC,
        false,
    ) {
        Ok(meminfo) => {
            let mut offset = 0usize;
            let data = b"MemTotal:        786432 kB\n\
MemFree:         700000 kB\n\
MemAvailable:   700000 kB\n\
Buffers:              0 kB\n\
Cached:               0 kB\n\
SwapCached:           0 kB\n\
Active:               0 kB\n\
Inactive:             0 kB\n\
SwapTotal:            0 kB\n\
SwapFree:             0 kB\n\
Dirty:                0 kB\n\
Writeback:            0 kB\n\
AnonPages:            0 kB\n\
Mapped:               0 kB\n\
Shmem:                0 kB\n\
Slab:                 0 kB\n\
SReclaimable:         0 kB\n\
SUnreclaim:           0 kB\n\
KernelStack:          0 kB\n\
PageTables:           0 kB\n\
CommitLimit:     786432 kB\n\
Committed_AS:         0 kB\n";
            meminfo.write(Some(&mut offset), data);
        }
        _ => {}
    }
    println!("[kernel] init_proc_meminfo_directory successfully!");
    match ROOT.open("/proc/mounts", OpenFlags::O_CREAT, false) {
        _ => {}
    }
    println!("[kernel] init_proc_mounts_directory successfully!");
}
