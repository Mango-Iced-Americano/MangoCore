---
title: "DHCPv4 租约生命周期"
module: "os/src/net/config.rs"
category: net
status: draft
owner: MangoCore Team
last_updated: "2026-07-13"
code_paths:
  - "os/src/net/config.rs"
  - "os/src/net/net_core.rs"
  - "os/src/net/routing.rs"
  - "os/src/fs/procfs/files/net_resolv.rs"
entry_points:
  - "NetInterfaceInner::new()"
  - "take_dhcp_event()"
  - "commit_dhcp_event()"
arch:
  rv64: supported
  la64: supported
related_docs:
  - "docs/06_net/device-stack-and-poll.md"
  - "docs/06_net/routing.md"
  - "docs/07_driver/2k1000-gmac.md"
---

# DHCPv4 租约生命周期

## 两种配置模式

MangoCore 保留两种互不影响的 2K1000LA 网络镜像：

| 模式 | Feature | 地址 | 用途 |
|---|---|---|---|
| Mac 直连 | gmac_2k1000 | 固定 192.168.9.20/24 | U-Boot TFTP、驱动调试 |
| 路由器 LAN | gmac_dhcp | DHCPv4 | 默认路由、DNS、外网测试 |

gmac_dhcp 依赖 gmac_2k1000。没有启用它时，现有静态直连行为保持不变。

## 常驻状态机

启用 gmac_dhcp 后，NetInterfaceInner::new() 创建 DHCP socket，但不在启动
路径阻塞等待，也不删除 socket。其 SocketHandle 保存在 eth0 的 DeviceStack：

~~~text
timer/idle/syscall poll
  -> Interface::poll()
  -> dhcpv4::Socket::poll()
  -> take_dhcp_event()       更新 smoltcp 地址和默认路由
  -> IRQ: 暂存最新事件 / task: 释放 NET_INTERFACE 锁
  -> commit_dhcp_event()     任务上下文更新 net_core、Router 和 DNS
~~~

因此启动时网线未接、TFTP 后换线、租约续期和 DHCP 重新绑定都沿同一运行时轮询
路径推进。Configured 和 Deconfigured 都会输出一条板级控制台日志。

## 三份状态同步

收到 Configured 后必须同时更新：

1. smoltcp Interface 的 IPv4 地址和默认路由；
2. net_core 的 eth0 地址、默认网关和 DNS 服务器快照；
3. 当前网络命名空间 Router 的 connected/default 路由。

Deconfigured 会清除同样三处状态。租约提交在释放 NET_INTERFACE.inner 后进行。
若事件来自定时器中断，则先保存在 DeviceStack，直到下一个任务上下文轮询再提交，
避免中断路径等待 device_list/router 锁。

## DNS 交付

DHCP DNS 服务器保存在 net_core::DNS_SERVERS。/proc/net/resolv.conf 每次读取时
动态生成：

~~~text
nameserver 192.168.1.1
~~~

initproc 将 /etc/resolv.conf 链接到该 procfs 文件，所以续租后 libc/BusyBox
解析器能直接读取新配置。租约尚未到达时暂时保留 QEMU SLIRP 的
nameserver 10.0.2.3 作为回退。

## QEMU 兼容路径

非 2K1000 GMAC 配置仍保留原有启动期 5 秒 DHCP 探测，以维持现有 QEMU 启动和
测例时序。该路径现在也保存 DHCP DNS，但完成或超时后仍删除 DHCP socket，尚不
具备 QEMU 侧续租能力。

## 构建与实板验证

~~~bash
make -C os la64-2k1000-dhcp-shell
make 2k1000-boot IMAGE=kernel-2k1000-dhcp-shell.ui
~~~

启动日志应先出现：

~~~text
[net] eth0 DHCP client started
~~~

接入有 DHCP 服务的路由器 LAN 后应出现 DHCP configured。随后检查：

~~~bash
ifconfig eth0
cat /proc/net/route
cat /etc/resolv.conf
ping -c 4 <default-gateway>
ping -c 4 1.1.1.1
ping -c 4 www.baidu.com
~~~

断开并重新接入 LAN 后，还需确认地址、默认路由和 DNS 能重新出现。

## 已知边界

- 2K1000LA GMAC 当前仍采用轮询收发，尚未接入 IRQ12。
- 用户态 inet_test 中部分 DNS 子测例仍直接使用 QEMU DNS 常量，需在网络测例
  适配阶段改为读取 /etc/resolv.conf。
- 当前只支持 DHCPv4，不支持 IPv6 SLAAC/DHCPv6。
