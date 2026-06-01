//! procfs 文件实现 — /proc/* 的各个文件

pub mod config;
pub mod cpuinfo;
pub mod filesystems;
pub mod meminfo;
pub mod mounts;
pub mod net_dev;
pub mod net_route;
pub mod net_tcp;
pub mod net_udp;
pub mod self_;
pub mod stat;
pub mod sys;
pub mod sysvipc;
pub mod uptime;
pub mod version;

use alloc::sync::Arc;
use crate::fs::vfs::InodeMode;
use crate::utils::error::SyscallErr;

pub fn register_all(root: &Arc<crate::fs::procfs::LockedProcInode>) -> Result<(), SyscallErr> {
    root.add_file("version", InodeMode::from_bits_truncate(0o444), version::version_content, 0)?;
    root.add_file("uptime", InodeMode::from_bits_truncate(0o444), uptime::uptime_content, 0)?;
    root.add_file("meminfo", InodeMode::from_bits_truncate(0o444), meminfo::meminfo_content, 0)?;
    root.add_file("cpuinfo", InodeMode::from_bits_truncate(0o444), cpuinfo::cpuinfo_content, 0)?;
    root.add_file("mounts", InodeMode::from_bits_truncate(0o444), mounts::mounts_content, 0)?;
    root.add_file("stat", InodeMode::from_bits_truncate(0o444), stat::stat_content, 0)?;
    root.add_file("config", InodeMode::from_bits_truncate(0o444), config::config_content, 0)?;
    root.add_file("filesystems", InodeMode::from_bits_truncate(0o444), filesystems::filesystems_content, 0)?;
    let sys_dir = root.add_dir_locked("sys", InodeMode::from_bits_truncate(0o555))?;
    let kernel_dir = sys_dir.add_dir_locked("kernel", InodeMode::from_bits_truncate(0o555))?;
    kernel_dir.add_file(
        "pid_max",
        InodeMode::from_bits_truncate(0o444),
        sys::pid_max_content,
        0,
    )?;
    kernel_dir.add_file(
        "threads-max",
        InodeMode::from_bits_truncate(0o444),
        sys::threads_max_content,
        0,
    )?;
    kernel_dir.add_writable_file_with_write(
        "ns_last_pid",
        InodeMode::from_bits_truncate(0o644),
        sys::ns_last_pid_content,
        sys::ns_last_pid_write,
        0,
    )?;
    kernel_dir.add_writable_file_with_write(
        "core_pattern",
        InodeMode::from_bits_truncate(0o644),
        sys::core_pattern_content,
        sys::core_pattern_write,
        0,
    )?;
    kernel_dir.add_file(
        "tainted",
        InodeMode::from_bits_truncate(0o444),
        sys::tainted_content,
        0,
    )?;
    kernel_dir.add_file(
        "osrelease",
        InodeMode::from_bits_truncate(0o444),
        sys::osrelease_content,
        0,
    )?;
    kernel_dir.add_file(
        "shmmax",
        InodeMode::from_bits_truncate(0o444),
        sys::shmmax_content,
        0,
    )?;
    kernel_dir.add_file(
        "shmall",
        InodeMode::from_bits_truncate(0o444),
        sys::shmall_content,
        0,
    )?;
    kernel_dir.add_file(
        "shmmni",
        InodeMode::from_bits_truncate(0o444),
        sys::shmmni_content,
        0,
    )?;
    kernel_dir.add_writable_file_with_write(
        "msgmax",
        InodeMode::from_bits_truncate(0o644),
        sys::msgmax_content,
        sys::msgmax_write,
        0,
    )?;
    kernel_dir.add_writable_file_with_write(
        "msgmnb",
        InodeMode::from_bits_truncate(0o644),
        sys::msgmnb_content,
        sys::msgmnb_write,
        0,
    )?;
    kernel_dir.add_writable_file_with_write(
        "msgmni",
        InodeMode::from_bits_truncate(0o644),
        sys::msgmni_content,
        sys::msgmni_write,
        0,
    )?;
    kernel_dir.add_writable_file_with_write(
        "msg_next_id",
        InodeMode::from_bits_truncate(0o644),
        sys::msg_next_id_content,
        sys::msg_next_id_write,
        0,
    )?;
    kernel_dir.add_writable_file_with_write(
        "sem",
        InodeMode::from_bits_truncate(0o644),
        sys::sem_content,
        sys::sem_write,
        0,
    )?;
    let user_dir = sys_dir.add_dir_locked("user", InodeMode::from_bits_truncate(0o555))?;
    user_dir.add_writable_file(
        "max_user_namespaces",
        InodeMode::from_bits_truncate(0o644),
        sys::max_user_namespaces_content,
        0,
    )?;
    let vm_dir = sys_dir.add_dir_locked("vm", InodeMode::from_bits_truncate(0o555))?;
    vm_dir.add_writable_file_with_write(
        "overcommit_memory",
        InodeMode::from_bits_truncate(0o644),
        sys::overcommit_memory_content,
        sys::overcommit_memory_write,
        0,
    )?;
    vm_dir.add_writable_file_with_write(
        "overcommit_ratio",
        InodeMode::from_bits_truncate(0o644),
        sys::overcommit_ratio_content,
        sys::overcommit_ratio_write,
        0,
    )?;
    vm_dir.add_writable_file_with_write(
        "max_map_count",
        InodeMode::from_bits_truncate(0o644),
        sys::max_map_count_content,
        sys::max_map_count_write,
        0,
    )?;
    vm_dir.add_writable_file_with_write(
        "min_free_kbytes",
        InodeMode::from_bits_truncate(0o644),
        sys::min_free_kbytes_content,
        sys::min_free_kbytes_write,
        0,
    )?;
    vm_dir.add_writable_file_with_write(
        "panic_on_oom",
        InodeMode::from_bits_truncate(0o644),
        sys::panic_on_oom_content,
        sys::panic_on_oom_write,
        0,
    )?;
    let net_dir = sys_dir.add_dir_locked("net", InodeMode::from_bits_truncate(0o555))?;
    let ipv4_dir = net_dir.add_dir_locked("ipv4", InodeMode::from_bits_truncate(0o555))?;
    let conf_dir = ipv4_dir.add_dir_locked("conf", InodeMode::from_bits_truncate(0o555))?;
    let lo_dir = conf_dir.add_dir_locked("lo", InodeMode::from_bits_truncate(0o555))?;
    let default_dir = conf_dir.add_dir_locked("default", InodeMode::from_bits_truncate(0o555))?;
    default_dir.add_writable_file(
        "tag",
        InodeMode::from_bits_truncate(0o644),
        sys::net_conf_tag_content,
        0,
    )?;
    lo_dir.add_writable_file(
        "tag",
        InodeMode::from_bits_truncate(0o644),
        sys::net_conf_tag_content,
        0,
    )?;
    ipv4_dir.add_writable_file_with_write(
        "ip_forward",
        InodeMode::from_bits_truncate(0o644),
        sys::ip_forward_content,
        sys::ip_forward_write,
        0,
    )?;

    let net_dir = root.add_dir_locked("net", InodeMode::from_bits_truncate(0o555))?;
    net_dir.add_file("dev", InodeMode::from_bits_truncate(0o444), net_dev::net_dev_content, 0)?;
    net_dir.add_file("route", InodeMode::from_bits_truncate(0o444), net_route::net_route_content, 0)?;
    net_dir.add_file("tcp", InodeMode::from_bits_truncate(0o444), net_tcp::net_tcp_content, 0)?;
    net_dir.add_file("udp", InodeMode::from_bits_truncate(0o444), net_udp::net_udp_content, 0)?;

    root.add_dynamic_symlink("self", self_::self_content, 0)?;
    let sysvipc_dir = root.add_dir_locked("sysvipc", InodeMode::from_bits_truncate(0o555))?;
    sysvipc_dir.add_file("shm", InodeMode::from_bits_truncate(0o444), sysvipc::shm_content, 0)?;
    sysvipc_dir.add_file("msg", InodeMode::from_bits_truncate(0o444), sysvipc::msg_content, 0)?;
    sysvipc_dir.add_file("sem", InodeMode::from_bits_truncate(0o444), sysvipc::sem_content, 0)?;

    crate::fs::procfs::pid::setup_pid_hooks(root);

    Ok(())
}
