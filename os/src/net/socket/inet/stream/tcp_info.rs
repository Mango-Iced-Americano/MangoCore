/// Linux struct tcp_info (from /usr/include/linux/tcp.h)
/// 用于 getsockopt(TCP_INFO)，netperf 等程序通过 tcpi_state 判断连接状态。
/// 所有字段必须填充或置零，否则未初始化的内存会误导用户程序。
#[repr(C)]
pub struct TcpInfo {
    tcpi_state: u8,
    tcpi_ca_state: u8,
    tcpi_retransmits: u8,
    tcpi_probes: u8,
    tcpi_backoff: u8,
    tcpi_options: u8,
    tcpi_snd_wscale: u8,
    tcpi_rcv_wscale: u8,

    tcpi_rto: u32,
    tcpi_ato: u32,
    tcpi_snd_mss: u32,
    tcpi_rcv_mss: u32,

    tcpi_unacked: u32,
    tcpi_sacked: u32,
    tcpi_lost: u32,
    tcpi_retrans: u32,
    tcpi_fackets: u32,

    /* Times */
    tcpi_last_data_sent: u32,
    tcpi_last_ack_sent: u32,
    tcpi_last_data_recv: u32,
    tcpi_last_ack_recv: u32,

    /* Metrics */
    tcpi_pmtu: u32,
    tcpi_rcv_ssthresh: u32,
    tcpi_rtt: u32,
    tcpi_rttvar: u32,
    tcpi_snd_ssthresh: u32,
    tcpi_snd_cwnd: u32,
    tcpi_advmss: u32,
    tcpi_reordering: u32,

    tcpi_rcv_rtt: u32,
    tcpi_rcv_space: u32,

    tcpi_total_retrans: u32,

    tcpi_pacing_rate: u64,
    tcpi_max_pacing_rate: u64,
    tcpi_bytes_acked: u64,
    tcpi_bytes_received: u64,
    tcpi_segs_out: u32,
    tcpi_segs_in: u32,

    tcpi_notsent_bytes: u32,
    tcpi_min_rtt: u32,
    tcpi_data_segs_in: u32,
    tcpi_data_segs_out: u32,
    tcpi_delivery_rate: u64,

    tcpi_busy_time: u64,
    tcpi_rwnd_limited: u64,
    tcpi_sndbuf_limited: u64,

    tcpi_delivered: u32,
    tcpi_delivered_ce: u32,

    tcpi_bytes_sent: u64,
    tcpi_bytes_retrans: u64,
    tcpi_dsack_dups: u32,
    tcpi_reord_seen: u32,

    tcpi_rcv_ooopack: u32,
    tcpi_snd_wnd: u32,
}

impl TcpInfo {
    pub fn new(state: u8, mss: u32) -> Self {
        Self {
            tcpi_state: state,
            tcpi_ca_state: 0,
            tcpi_retransmits: 0,
            tcpi_probes: 0,
            tcpi_backoff: 0,
            tcpi_options: 0,
            tcpi_snd_wscale: 0,
            tcpi_rcv_wscale: 0,

            tcpi_rto: 0,
            tcpi_ato: 0,
            tcpi_snd_mss: mss,
            tcpi_rcv_mss: mss,

            tcpi_unacked: 0,
            tcpi_sacked: 0,
            tcpi_lost: 0,
            tcpi_retrans: 0,
            tcpi_fackets: 0,

            tcpi_last_data_sent: 0,
            tcpi_last_ack_sent: 0,
            tcpi_last_data_recv: 0,
            tcpi_last_ack_recv: 0,

            tcpi_pmtu: 0,
            tcpi_rcv_ssthresh: 0,
            tcpi_rtt: 0,
            tcpi_rttvar: 0,
            tcpi_snd_ssthresh: 0,
            tcpi_snd_cwnd: 0,
            tcpi_advmss: mss,
            tcpi_reordering: 0,

            tcpi_rcv_rtt: 0,
            tcpi_rcv_space: 0,

            tcpi_total_retrans: 0,

            tcpi_pacing_rate: 0,
            tcpi_max_pacing_rate: 0,
            tcpi_bytes_acked: 0,
            tcpi_bytes_received: 0,
            tcpi_segs_out: 0,
            tcpi_segs_in: 0,

            tcpi_notsent_bytes: 0,
            tcpi_min_rtt: 0,
            tcpi_data_segs_in: 0,
            tcpi_data_segs_out: 0,
            tcpi_delivery_rate: 0,

            tcpi_busy_time: 0,
            tcpi_rwnd_limited: 0,
            tcpi_sndbuf_limited: 0,

            tcpi_delivered: 0,
            tcpi_delivered_ce: 0,

            tcpi_bytes_sent: 0,
            tcpi_bytes_retrans: 0,
            tcpi_dsack_dups: 0,
            tcpi_reord_seen: 0,

            tcpi_rcv_ooopack: 0,
            tcpi_snd_wnd: 0,
        }
    }
}
