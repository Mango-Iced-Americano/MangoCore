//! /proc/sys/* — LTP 环境探测所需的最小兼容节点。

use alloc::format;
use alloc::string::{String, ToString};
use crate::fs::procfs::proc_read_str;
use crate::utils::error::SyscallErr;
use lazy_static::lazy_static;
use spin::Mutex;

lazy_static! {
    static ref CORE_PATTERN: Mutex<String> = Mutex::new(String::from("core\n"));
}

pub fn pid_max_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    proc_read_str(offset, len, buf, "32768\n")
}

pub fn threads_max_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::config::SYSTEM_TASK_LIMIT);
    proc_read_str(offset, len, buf, &value)
}

pub fn ns_last_pid_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::task::ns_last_pid());
    proc_read_str(offset, len, buf, &value)
}

pub fn ns_last_pid_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let text = core::str::from_utf8(buf).map_err(|_| SyscallErr::EINVAL)?;
    let value = text
        .trim()
        .parse::<usize>()
        .map_err(|_| SyscallErr::EINVAL)?;
    crate::task::set_ns_last_pid(value);
    Ok(buf.len())
}

pub fn core_pattern_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let pattern = CORE_PATTERN.lock();
    proc_read_str(offset, len, buf, &pattern)
}

pub fn core_pattern_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let text = core::str::from_utf8(buf).map_err(|_| SyscallErr::EINVAL)?;
    *CORE_PATTERN.lock() = text.to_string();
    Ok(buf.len())
}

pub fn tainted_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    proc_read_str(offset, len, buf, "0\n")
}

pub fn max_user_namespaces_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    proc_read_str(offset, len, buf, "0\n")
}

pub fn pipe_max_size_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::fs::dev::pipe::pipe_max_size());
    proc_read_str(offset, len, buf, &value)
}

pub fn pipe_max_size_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let value = parse_usize_sysctl(buf)?;
    if !crate::fs::dev::pipe::set_pipe_max_size(value) {
        return Err(SyscallErr::EINVAL);
    }
    Ok(buf.len())
}

pub fn pipe_user_pages_soft_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::fs::dev::pipe::pipe_user_pages_soft());
    proc_read_str(offset, len, buf, &value)
}

pub fn pipe_user_pages_hard_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::fs::dev::pipe::pipe_user_pages_hard());
    proc_read_str(offset, len, buf, &value)
}

pub fn mqueue_queues_max_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::syscall::posix_mq_queues_max());
    proc_read_str(offset, len, buf, &value)
}

pub fn mqueue_queues_max_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let value = parse_usize_sysctl(buf)?;
    if !crate::syscall::set_posix_mq_queues_max(value) {
        return Err(SyscallErr::EINVAL);
    }
    Ok(buf.len())
}

pub fn mqueue_msg_max_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::syscall::posix_mq_msg_max());
    proc_read_str(offset, len, buf, &value)
}

pub fn mqueue_msg_max_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let value = parse_usize_sysctl(buf)?;
    if !crate::syscall::set_posix_mq_msg_max(value) {
        return Err(SyscallErr::EINVAL);
    }
    Ok(buf.len())
}

pub fn mqueue_msgsize_max_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::syscall::posix_mq_msgsize_max());
    proc_read_str(offset, len, buf, &value)
}

pub fn mqueue_msgsize_max_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let value = parse_usize_sysctl(buf)?;
    if !crate::syscall::set_posix_mq_msgsize_max(value) {
        return Err(SyscallErr::EINVAL);
    }
    Ok(buf.len())
}

pub fn mqueue_msg_default_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::syscall::posix_mq_msg_default());
    proc_read_str(offset, len, buf, &value)
}

pub fn mqueue_msg_default_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let value = parse_usize_sysctl(buf)?;
    if !crate::syscall::set_posix_mq_msg_default(value) {
        return Err(SyscallErr::EINVAL);
    }
    Ok(buf.len())
}

pub fn mqueue_msgsize_default_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::syscall::posix_mq_msgsize_default());
    proc_read_str(offset, len, buf, &value)
}

pub fn mqueue_msgsize_default_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let value = parse_usize_sysctl(buf)?;
    if !crate::syscall::set_posix_mq_msgsize_default(value) {
        return Err(SyscallErr::EINVAL);
    }
    Ok(buf.len())
}

pub fn overcommit_memory_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::mm::overcommit_memory());
    proc_read_str(offset, len, buf, &value)
}

pub fn overcommit_memory_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let value = parse_usize_sysctl(buf)?;
    if !crate::mm::set_overcommit_memory(value) {
        return Err(SyscallErr::EINVAL);
    }
    Ok(buf.len())
}

pub fn overcommit_ratio_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::mm::overcommit_ratio());
    proc_read_str(offset, len, buf, &value)
}

pub fn overcommit_ratio_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let value = parse_usize_sysctl(buf)?;
    crate::mm::set_overcommit_ratio(value);
    Ok(buf.len())
}

pub fn max_map_count_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::mm::max_map_count());
    proc_read_str(offset, len, buf, &value)
}

pub fn max_map_count_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let value = parse_usize_sysctl(buf)?;
    if !crate::mm::set_max_map_count(value) {
        return Err(SyscallErr::EINVAL);
    }
    Ok(buf.len())
}

pub fn min_free_kbytes_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::mm::min_free_kbytes());
    proc_read_str(offset, len, buf, &value)
}

pub fn min_free_kbytes_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let value = parse_usize_sysctl(buf)?;
    crate::mm::set_min_free_kbytes(value);
    Ok(buf.len())
}

pub fn panic_on_oom_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::mm::panic_on_oom());
    proc_read_str(offset, len, buf, &value)
}

pub fn panic_on_oom_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let value = parse_usize_sysctl(buf)?;
    crate::mm::set_panic_on_oom(value);
    Ok(buf.len())
}

pub fn osrelease_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    proc_read_str(offset, len, buf, "5.10.0-mangocore\n")
}

pub fn shmmax_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::syscall::sysv_shmmax());
    proc_read_str(offset, len, buf, &value)
}

pub fn shmall_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::syscall::sysv_shmall());
    proc_read_str(offset, len, buf, &value)
}

pub fn shmmni_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::syscall::sysv_shmmni());
    proc_read_str(offset, len, buf, &value)
}

pub fn msgmax_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::syscall::sysv_msgmax());
    proc_read_str(offset, len, buf, &value)
}

pub fn msgmax_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let value = parse_usize_sysctl(buf)?;
    if !crate::syscall::set_sysv_msgmax(value) {
        return Err(SyscallErr::EINVAL);
    }
    Ok(buf.len())
}

pub fn msgmnb_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::syscall::sysv_msgmnb());
    proc_read_str(offset, len, buf, &value)
}

pub fn msgmnb_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let value = parse_usize_sysctl(buf)?;
    if !crate::syscall::set_sysv_msgmnb(value) {
        return Err(SyscallErr::EINVAL);
    }
    Ok(buf.len())
}

pub fn msgmni_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::syscall::sysv_msgmni());
    proc_read_str(offset, len, buf, &value)
}

pub fn msgmni_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let value = parse_usize_sysctl(buf)?;
    if !crate::syscall::set_sysv_msgmni(value) {
        return Err(SyscallErr::EINVAL);
    }
    Ok(buf.len())
}

pub fn msg_next_id_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::syscall::sysv_msg_next_id());
    proc_read_str(offset, len, buf, &value)
}

pub fn msg_next_id_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let text = core::str::from_utf8(buf).map_err(|_| SyscallErr::EINVAL)?;
    let value = text
        .trim()
        .parse::<i32>()
        .map_err(|_| SyscallErr::EINVAL)?;
    if !crate::syscall::set_sysv_msg_next_id(value) {
        return Err(SyscallErr::EINVAL);
    }
    Ok(buf.len())
}

pub fn sem_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let (semmsl, semmns, semopm, semmni) = crate::syscall::sysv_sem_limits();
    let value = format!("{semmsl}\t{semmns}\t{semopm}\t{semmni}\n");
    proc_read_str(offset, len, buf, &value)
}

pub fn sem_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let (semmsl, semmns, semopm, semmni) = parse_four_usize_sysctl(buf)?;
    if !crate::syscall::set_sysv_sem_limits(semmsl, semmns, semopm, semmni) {
        return Err(SyscallErr::EINVAL);
    }
    Ok(buf.len())
}

fn parse_usize_sysctl(buf: &[u8]) -> Result<usize, SyscallErr> {
    let text = core::str::from_utf8(buf).map_err(|_| SyscallErr::EINVAL)?;
    text.trim()
        .parse::<usize>()
        .map_err(|_| SyscallErr::EINVAL)
}

fn parse_four_usize_sysctl(buf: &[u8]) -> Result<(usize, usize, usize, usize), SyscallErr> {
    let text = core::str::from_utf8(buf).map_err(|_| SyscallErr::EINVAL)?;
    let mut fields = text.split_whitespace();
    let semmsl = fields
        .next()
        .ok_or(SyscallErr::EINVAL)?
        .parse::<usize>()
        .map_err(|_| SyscallErr::EINVAL)?;
    let semmns = fields
        .next()
        .ok_or(SyscallErr::EINVAL)?
        .parse::<usize>()
        .map_err(|_| SyscallErr::EINVAL)?;
    let semopm = fields
        .next()
        .ok_or(SyscallErr::EINVAL)?
        .parse::<usize>()
        .map_err(|_| SyscallErr::EINVAL)?;
    let semmni = fields
        .next()
        .ok_or(SyscallErr::EINVAL)?
        .parse::<usize>()
        .map_err(|_| SyscallErr::EINVAL)?;
    Ok((semmsl, semmns, semopm, semmni))
}

pub fn net_conf_tag_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    proc_read_str(offset, len, buf, "0\n")
}

pub fn ip_forward_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    proc_read_str(offset, len, buf, "0\n")
}

pub fn ip_forward_write(
    _extra: usize,
    _offset: usize,
    _buf: &[u8],
) -> Result<usize, SyscallErr> {
    Err(SyscallErr::EPERM)
}

pub fn disable_ipv6_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    proc_read_str(offset, len, buf, "0\n")
}

pub fn accept_dad_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    proc_read_str(offset, len, buf, "0\n")
}

pub fn net_snmp_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    // Must match net-tools parsesnmp() expectations: Ip, Icmp, Tcp, Udp, UdpLite sections
    // with header fields matching snmp mib standard names.
    let content = concat!(
        "Ip: Forwarding DefaultTTL InReceives InHdrErrors InAddrErrors ForwDatagrams InUnknownProtos InDiscards InDelivers OutRequests OutDiscards OutNoRoutes ReasmTimeout ReasmReqds ReasmOKs ReasmFails FragOKs FragFails FragCreates\n",
        "Ip: 2 64 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n",
        "Icmp: InMsgs InErrors InCsumErrors InDestUnreachs InTimeExcds InParmProbs InSrcQuenchs InRedirects InEchos InEchoReps InTimestamps InTimestampReps InAddrMasks InAddrMaskReps OutMsgs OutErrors OutDestUnreachs OutTimeExcds OutParmProbs OutSrcQuenchs OutRedirects OutEchos OutEchoReps OutTimestamps OutTimestampReps OutAddrMasks OutAddrMaskReps\n",
        "Icmp: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n",
        "Tcp: RtoAlgorithm RtoMin RtoMax MaxConn ActiveOpens PassiveOpens AttemptFails EstabResets CurrEstab InSegs OutSegs RetransSegs InErrs OutRsts InCsumErrors\n",
        "Tcp: 1 200 120000 -1 0 0 0 0 0 0 0 0 0 0 0\n",
        "Udp: InDatagrams NoPorts InErrors OutDatagrams RcvbufErrors SndbufErrors InCsumErrors IgnoredMulti\n",
        "Udp: 0 0 0 0 0 0 0 0\n",
        "UdpLite: InDatagrams NoPorts InErrors OutDatagrams RcvbufErrors SndbufErrors InCsumErrors\n",
        "UdpLite: 0 0 0 0 0 0 0\n",
    );
    proc_read_str(offset, len, buf, content)
}

pub fn net_netstat_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    // netstat -s also reads /proc/net/netstat; includes multicast counters for -gn.
    // TcpExt: 69 fields; IpExt: 16 fields (with InMcastPkts/OutMcastPkts).
    let content = concat!(
        "TcpExt: SyncookiesSent SyncookiesRecv SyncookiesFailed EmbryonicRsts PruneCalled RcvPruned OfoPruned OutOfWindowIcmps LockDroppedIcmps ArpFilter TW TWRecycled TWKilled PAWSPassive PAWSActive PAWSEstab DelayedACKs DelayedACKLocked DelayedACKLost ListenOverflows ListenDropped TCPPrequeued TCPDirectCopyFromBacklog TCPDirectCopyFromPrequeue TCPHPHits TCPHPHitsToUser TCPPureAcks TCPHPAcks TCPRenoRecovery TCPSackRecovery TCPSACKReneging TCPFACKReorder TCPSACKReorder TCPRenoReorder TCPTSReorder TCPFullUndo TCPPartialUndo TCPLossUndo TCPLoss TCPLostRetransmit TCPRenoFailures TCPSackFailures TCPLossFailures TCPFastRetrans TCPForwardRetrans TCPSlowStartRetrans TCPTimeouts TCPLossProbes TCPLossProbeRecovery TCPRenoRecoveryFail TCPSackRecoveryFail TCPSchedulerFailed TCPRcvCollapsed TCPDSACKOldSent TCPDSACKOldRecv TCPDSACKUndo TCPDSACKIgnoredNoUndo TCPDSACKIgnoredOld TCPDSACKOfoRecv TCPDSACKRecv TCPDSACKOfoSent TCPAbortOnData TCPAbortOnClose TCPAbortOnMemory TCPAbortOnTimeout TCPAbortOnLinger TCPAbortFailed TCPMemoryPressures TCPSACKDiscard\n",
        "TcpExt: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n",
        "IpExt: InNoRoutes InTruncatedPkts InMcastPkts OutMcastPkts InBcastPkts OutBcastPkts InOctets OutOctets InMcastOctets OutMcastOctets InBcastOctets OutBcastOctets InCsumErrors InNoECTPkts InECT0Pkts InCEPkts\n",
        "IpExt: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n",
    );
    proc_read_str(offset, len, buf, content)
}

pub fn net_snmp6_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    // /proc/net/snmp6 uses "key value" pairs per line, parsed by process6_fd().
    // Must include Ip6*, Icmp6*, and Udp6* entries matching snmp6tabs[].
    // Using flat hex (no colons) for IPv6 addresses to match parser expectations.
    let content = concat!(
        "Ip6InReceives 0\n",
        "Ip6InHdrErrors 0\n",
        "Ip6InTooBigErrors 0\n",
        "Ip6InNoRoutes 0\n",
        "Ip6InAddrErrors 0\n",
        "Ip6InUnknownProtos 0\n",
        "Ip6InTruncatedPkts 0\n",
        "Ip6InDiscards 0\n",
        "Ip6InDelivers 0\n",
        "Ip6OutForwDatagrams 0\n",
        "Ip6OutRequests 0\n",
        "Ip6OutDiscards 0\n",
        "Ip6OutNoRoutes 0\n",
        "Ip6ReasmTimeout 0\n",
        "Ip6ReasmReqds 0\n",
        "Ip6ReasmOKs 0\n",
        "Ip6ReasmFails 0\n",
        "Ip6FragOKs 0\n",
        "Ip6FragFails 0\n",
        "Ip6FragCreates 0\n",
        "Ip6InMcastPkts 0\n",
        "Ip6OutMcastPkts 0\n",
        "Icmp6InMsgs 0\n",
        "Icmp6InErrors 0\n",
        "Icmp6OutMsgs 0\n",
        "Icmp6OutErrors 0\n",
        "Icmp6InDestUnreachs 0\n",
        "Icmp6InPktTooBigs 0\n",
        "Icmp6InTimeExcds 0\n",
        "Icmp6InParmProblems 0\n",
        "Icmp6InEchos 0\n",
        "Icmp6InEchoReplies 0\n",
        "Icmp6InGroupMembQueries 0\n",
        "Icmp6InGroupMembResponses 0\n",
        "Icmp6InGroupMembReductions 0\n",
        "Icmp6InRouterSolicits 0\n",
        "Icmp6InRouterAdvertisements 0\n",
        "Icmp6InNeighborSolicits 0\n",
        "Icmp6InNeighborAdvertisements 0\n",
        "Icmp6InRedirects 0\n",
        "Icmp6OutDestUnreachs 0\n",
        "Icmp6OutPktTooBigs 0\n",
        "Icmp6OutTimeExcds 0\n",
        "Icmp6OutParmProblems 0\n",
        "Icmp6OutEchos 0\n",
        "Icmp6OutEchoReplies 0\n",
        "Icmp6OutGroupMembQueries 0\n",
        "Icmp6OutGroupMembResponses 0\n",
        "Icmp6OutGroupMembReductions 0\n",
        "Icmp6OutRouterSolicits 0\n",
        "Icmp6OutRouterAdvertisements 0\n",
        "Icmp6OutNeighborSolicits 0\n",
        "Icmp6OutNeighborAdvertisements 0\n",
        "Icmp6OutRedirects 0\n",
        "Udp6InDatagrams 0\n",
        "Udp6NoPorts 0\n",
        "Udp6InErrors 0\n",
        "Udp6OutDatagrams 0\n",
        "Udp6RcvbufErrors 0\n",
        "Udp6SndbufErrors 0\n",
        "Udp6InCsumErrors 0\n",
    );
    proc_read_str(offset, len, buf, content)
}
