---
title: "smoltcp 适配层、路由层与设备层"
module: "net"
category: net
status: deprecated
owner: MangoCore Team
last_updated: 2026-06-29
code_paths: []
entry_points: []
arch:
  rv64: supported
  la64: supported
tests:
  ltp: []
  oscomp: []
related_docs:
  - "docs/06_net/device-stack-and-poll.md"
  - "docs/06_net/device-adapter.md"
  - "docs/06_net/net-core-iface.md"
  - "docs/06_net/routing.md"
  - "docs/06_net/neighbour.md"
  - "docs/06_net/dhcp.md"
---

## 本文档已废弃

本文档的内容已拆分为 6 篇专题文档，每篇聚焦一个独立主题：

| 文档 | 主题 |
|------|------|
| [device-stack-and-poll.md](device-stack-and-poll.md) | NetInterface、DeviceStack、polling、socket handle 管理 |
| [device-adapter.md](device-adapter.md) | IfaceDevice、SmoltcpDeviceAdapter、NullNetDevice |
| [net-core-iface.md](net-core-iface.md) | Iface trait、IfaceCommon、设备注册中心、ioctl |
| [routing.md](routing.md) | RouteSocketHandle、SocketBinding、FIB、route_output |
| [neighbour.md](neighbour.md) | NEIGHBOUR_TABLE、ARP 捕获 |
| [dhcp.md](dhcp.md) | DHCP 初始化流程 |

请根据需要跳转到上述文档阅读对应内容。本文档仅作历史引用，不再更新。
