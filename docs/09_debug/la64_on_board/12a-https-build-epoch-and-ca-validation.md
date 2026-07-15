---
title: "2K1000LA HTTPS：构建时间功能性回退与 CA/主机名正负门禁"
category: debug
status: resolved-with-security-limits
author: MangoCore Team
last_update: 2026-07-15
tags: [postmortem, la64, 2k1000la, https, tls, curl, mbedtls, ca, clock]
code_paths:
  - "scripts/build_curl_runtime_la64.sh"
  - "os/build_initramfs.sh"
  - "user/src/bin/init.rs"
  - "os/initramfs/common/etc/ssl/certs/ca-certificates.crt"
related_docs:
  - "docs/09_debug/la64_on_board/12-glibc-resolver-abi.md"
  - "docs/06_net/test-map.md"
  - "docs/03_fs/init-and-rootfs.md"
evidence:
  - "commit 6b08ed74"
  - "docs/Work_Log.md, 2026-07-13 verified HTTPS entry"
---

# 2K1000LA HTTPS：构建时间功能性回退与 CA/主机名正负门禁

## 0. 一句话结论

板上 DNS 和 HTTP 已经成功，不代表默认 TLS 校验链已经可用。功能验收还依赖：

~~~text
供验证器使用的 wall clock
  + CA bundle
  + TLS 后端
  + DNS/NSS 运行时
  + 默认开启的证书链与主机名校验
~~~

2K1000LA 当时没有可依赖的持久 RTC，早期网络又可能访问不到 NTP；零点或过时
硬编码日期会让正常站点的功能门禁直接失败。提交 <code>6b08ed74</code> 优先尝试
NTP；NTP 失败时把镜像携带的 artifact/build epoch 写入 wall clock，目的仅是避免
零点并让当次构建的公网 HTTPS 正向用例有机会运行。再配合固定版本、固定 SHA 的
curl 8.19.0 + Mbed TLS 3.6.7 和 CA bundle 构建运行时。

<code>SOURCE_DATE_EPOCH</code>/<code>/etc/build-epoch</code> 不是可信当前时间：
它没有真实性、反回滚或“不得晚于当前时刻”的上界。旧镜像可能把真实已过期证书
误判为仍有效，也可能把新证书判为尚未生效；人为设置的未来 epoch 还会拒绝现实中
仍有效的证书。只有 NTP 成功时，本实现才得到网络提供的当前时间；即便如此，这里
也没有证明 NTP 响应经过密码学认证。

最终验收不是 <code>curl -k</code> 或仅完成握手，而是同一镜像同时满足：

- 正向：默认校验访问 <code>https://www.baidu.com/</code> 返回 HTTP 200；
- 反向：<code>https://wrong.host.badssl.com/</code> 因主机名不匹配返回 curl 60。

正反结果共同证明“该镜像、该配置时钟下，CA/主机名验证器在工作”，而非证书检查
被静默关闭；它们不证明 fallback 时钟等于真实当前时间，也不构成完整证书有效期
安全闭环。

---

## 1. 问题边界：HTTP 通与默认 HTTPS 校验可用之间缺什么

前一阶段已经证明：

- DHCP、路由、DNS 正常；
- glibc resolver ABI 已补齐；
- 按域名访问 HTTP 得到 200。

但 HTTPS 比 HTTP 多出以下不可替代的输入：

| 输入 | 缺失时的典型后果 |
|---|---|
| 可用的 wall clock | 验证器无法按其时钟判断证书 notBefore/notAfter |
| 根 CA 集合 | 无法把服务端链锚定到受信根 |
| 主机名 | 无法验证 SAN/CN 与请求域名一致 |
| TLS 实现 | 无法完成协议、签名与链验证 |
| 安全随机数 | 握手可能“能跑”但不具备安全性 |

因此 HTTP 200 只覆盖 DNS、TCP 和应用层明文请求，不能作为 HTTPS 默认校验可用的
证据；同样，给验证器写入任意 epoch 也不能证明时间真实。

---

## 2. 最初症状与错误方向

裸机系统启动时的 wall clock 可能从零、固件遗留值或旧的硬编码日期开始。公网证书通常只在有限时间窗口内有效：

~~~text
notBefore <= now <= notAfter
~~~

若 <code>now</code> 落在 1970 年或过期硬编码日期，TLS 库正确的行为就是拒绝证书。此时网络完全正常，错误仍会表现为 HTTPS 失败。

容易出现三种错误处理：

1. 使用 <code>-k</code>；
2. 在 TLS 测试中使用 NoVerify；
3. 每次启动强制写死一个“看起来较新”的日期。

前两者绕过校验，第三个会随时间再次失效，也不具备可复现构建语义。artifact epoch
改善了可复现性和功能可用性，但没有把构建元数据升级为当前时间真值。

---

## 3. 调试追溯

### 3.1 先把 HTTPS 故障拆成四个检查点

~~~text
DNS 成功？
  ↓
TCP 443 能连接？
  ↓
TLS 握手能完成？
  ↓
证书链、按配置时钟计算的有效期、主机名能通过默认验证？
~~~

已有 HTTP/DNS 结果覆盖前两层的主要前置条件；<code>inet_test tls</code> 的 NoVerify 路径只能作为握手诊断，不能覆盖最后一层。

### 3.2 识别时间源是板端特有断点

QEMU user network 与板端互联网共享环境并不完全相同：

- QEMU 可较容易访问外部时间服务；
- 实板早期启动阶段 NTP 可能因网络策略、DNS 或 UDP 可达性失败；
- 板端不能因为 NTP 失败无限阻塞启动；
- 也不能回退到已经失效的硬编码日期。

因此方案必须同时满足：

- NTP 成功时采用网络当前时间；
- NTP 失败时用 artifact epoch 避免零点/固定旧日期，作为功能性 fallback；
- 输入损坏时 fail visibly，不把垃圾 epoch 写入时钟。

这里不能把 NTP 失败分支称为“时间下界”：构建系统只校验 epoch 是数字，init 只校验
不早于 2024-01-01，没有验证来源、镜像是否回滚，也没有未来上界。

### 3.3 先在 QEMU 做正反门禁，再搬到板上

验证顺序为：

1. Docker 串行完成双架构 release kernel build；
2. 构建 <code>la64-qemu-curl-shell</code>；
3. QEMU 默认校验访问正确站点；
4. QEMU 访问错误主机名站点，必须失败；
5. 构建同一 HTTPS 运行时的 2K1000LA uImage；
6. 校验镜像 SHA、CRC、TFTP 长度与 U-Boot <code>iminfo</code>；
7. 板上在 NTP 不可达、build epoch 功能性回退条件下重复正反用例。

这使“TLS 运行时打包错误”和“实板网络/时钟差异”分开暴露。

---

## 4. 底层原理一：build epoch 是 artifact metadata，不是当前时间

### 4.1 构建期生成

<code>os/build_initramfs.sh</code>：

1. 优先读取 <code>SOURCE_DATE_EPOCH</code>；
2. 未设置时使用构建主机当前 UTC epoch；
3. 拒绝空值或任何非数字；
4. 写入 <code>/etc/build-epoch</code>。

<code>SOURCE_DATE_EPOCH</code> 使发布构建可复现；未设置时记录构建主机时间，使日常
镜像通常比零点/旧硬编码日期更接近验收时刻。但两者都只是构建输入，不带真实性证明。

### 4.2 启动期校验

<code>init</code> 读取最多 32 字节并逐位解析：

- 使用 checked multiply/add 防整数溢出；
- 只接受十进制数字和尾随空白；
- 必须至少有一位数字；
- epoch 必须不早于 <code>1704067200</code>，即 2024-01-01 UTC。

无效时不会再写一个硬编码日期，而是打印原因并保留内核现有时钟。

### 4.3 网络时间和 artifact fallback 的优先级

~~~text
BusyBox ntpd，最多 2 次，每次限时 3000 ms
  ├─ 成功：保留 NTP 提供的网络当前时间
  └─ 全失败：读取并设置 /etc/build-epoch
                 └─ 文件无效：不覆盖当前时钟
~~~

build epoch 不能保证设备当前时刻，也不能保证“镜像不可能在自己构建之前运行”：
<code>SOURCE_DATE_EPOCH</code> 可被显式设置，镜像也可被回滚。它还不能：

- 反映设备断电后的真实经过时间；
- 修复一张多年未更新镜像中的过期 CA/证书；
- 判断真实当前时刻已经过期、但 artifact 时刻仍在有效期内的证书；
- 阻止未来 epoch 把现实中有效证书判成过期；
- 替代长期 RTC、经过认证的网络时间或安全时间协议；
- 防御攻击者回滚整个镜像。

所以本文只称其为“功能性 artifact fallback”，不称“可信时间”“当前时间”或
“证书有效期安全下界”。

---

## 5. 底层原理二：CA bundle 和主机名验证缺一不可

### 5.1 TLS 链验证

服务端通常发送叶子证书和中间证书。客户端沿签发关系验证到本地受信根：

~~~text
leaf(www.baidu.com)
  → intermediate CA
  → root CA in /etc/ssl/certs/ca-certificates.crt
~~~

若没有 CA bundle，即使握手密码学计算正确，也没有本地信任锚。

### 5.2 主机名验证

链可信只说明“某受信 CA 签发了这张证书”，还必须确认 SAN/CN 覆盖请求域名。否则攻击者拿另一域名的合法证书也可能被接受。

这就是为什么负向用例使用 <code>wrong.host.badssl.com</code>：

- 站点可连接；
- TLS 服务存在；
- 故意提供与请求主机名不匹配的证书；
- 正确客户端必须拒绝并返回 curl 60。

正向成功 + 负向拒绝，比单个 HTTPS 200 更能证明验证路径没有被关闭。

---

## 6. 底层原理三：构建产物必须可追溯

运行时构建脚本固定：

- curl 8.19.0；
- Mbed TLS 3.6.7；
- 两份上游源码 URL；
- 对应 SHA-256；
- 静态链接 Mbed TLS；
- CA 路径 <code>/etc/ssl/certs/ca-certificates.crt</code>；
- glibc 与 <code>libnss_dns.so.2</code>、<code>libnss_files.so.2</code>。

这解决两类常见“开发机能跑、initramfs 不能跑”：

1. TLS 动态库未打包；
2. glibc 在域名解析时找不到 NSS 模块。

固定源码摘要还使后续复盘能够确认实际使用的 TLS/curl 源码，而不是同名浮动下载。

---

## 7. 功能性故障定位与门禁证明

### 7.1 事实分类

| 类型 | 内容 |
|---|---|
| 已有事实 | DNS 与 HTTP 已成功 |
| 板端事实 | NTP 不可达，启动日志进入 build epoch 功能性回退 |
| 源码事实 | 旧方案依赖过时硬编码时间；新方案生成/校验 epoch |
| QEMU 正向 | 默认校验 HTTPS，HTTP 200，2443 B，rc=0 |
| QEMU 反向 | wrong.host 因 CN/hostname 不匹配，curl 60 |
| 板端正向 | 设置 artifact epoch 后默认校验，HTTP 200，2443 B，rc=0 |
| 板端反向 | wrong.host 返回 curl 60 |

### 7.2 这些结果能证明什么

若只是 TCP/TLS 握手通，而证书验证被关闭，则错误主机名也会成功。实际错误主机名稳定失败，说明主机名验证在执行。

若板端失败点仍只在 DNS 或 TCP，则改变验证器时钟输入与 TLS 运行时不会解释同一
网络上正向 HTTPS 从失败到成功。实际 NTP 失败后，设置 artifact epoch 并带齐
CA/TLS/NSS 运行时，正向用例通过。这定位了当时功能链缺口。

但这一干预没有拿真实当前时间与 fallback 值做可信比对，因而只能得到：

> 解析与 TCP 前置链已通；当时默认 HTTPS 功能门禁缺少一个可工作的 wall-clock
> 输入和完整验证运行时。artifact epoch 让该镜像的正向站点通过，错误主机名仍被
> 拒绝；这不是对真实证书有效期的安全证明。

---

## 8. 明确拒绝的“假修复”

### 8.1 curl -k

<code>-k</code> 同时削弱链验证/主机身份保证，只证明服务器能说 TLS，不证明连接对象可信。

### 8.2 inet_test NoVerify

NoVerify 适合回答“协议实现能否完成握手”，不能作为 CA 验收。历史 <code>inet_test tls</code> 仍只被列为诊断项。

### 8.3 永久硬编码日期

硬编码日期会再次过期，并可能把实际时钟倒退。build epoch 随镜像更新，且可由
<code>SOURCE_DATE_EPOCH</code> 控制，构建语义更清楚；但旧/回滚/未来 epoch 的
安全问题仍然存在。

### 8.4 只跑一个正确站点

若程序意外关闭校验，正确站点依然会返回 200。没有故意错误证书/主机名的负向用例，就不能证明拒绝路径。

---

## 9. 验证矩阵

| 环境 | 条件/用例 | 结果 |
|---|---|---|
| Docker | rv64 release kernel build | 成功 |
| Docker | la64 release kernel build | 成功 |
| QEMU | DHCP/NAT、curl 特性 | 报告 Mbed TLS 3.6.7、HTTPS/SSL |
| QEMU | baidu，默认验证 | HTTP 200，2443 B，rc=0 |
| QEMU | wrong.host.badssl | curl 60 |
| 构建产物 | uImage | 16173912 B；load/entry <code>0x90000000</code> |
| 传输链 | SHA/CRC/iminfo | SHA <code>0e8b…c5aa</code>，CRC <code>26e477c0</code>，均匹配 |
| 2K1000LA | DHCP | <code>192.168.2.2/24</code> |
| 2K1000LA | NTP | 不可达，进入 build epoch 功能性回退 |
| 2K1000LA | baidu，默认验证 | HTTP 200，2443 B，rc=0 |
| 2K1000LA | wrong.host.badssl | curl 60 |

镜像摘要完整值可在 <code>docs/Work_Log.md</code> 的 2026-07-13 条目核对。仓库没有为本轮单独归档一份串口原始日志，本文不虚构日志路径。

---

## 10. 安全边界：当时“功能通过”不等于可放密钥

在 <code>6b08ed74</code> 这个历史检查点：

- <code>/dev/urandom</code> 仍返回零；
- <code>getrandom</code> 仍是时间/地址播种的 xorshift；
- Work Log 明确禁止在板上放置真实 API 密钥。

因此该提交证明的是“在当次 artifact epoch 与测试站点下，默认 CA/主机名验证功能
门禁可运行”，不是“真实当前时间可信”或“密码学随机源已达生产安全”。可信熵与
CSPRNG 是后续独立工作，在后来的 <code>5d2f16ef</code> 中推进，不能倒灌成
<code>6b08ed74</code> 当时已有的能力。

另外，本轮没有覆盖：

- OCSP/CRL 撤销检查；
- 镜像回滚保护；
- build epoch 来源真实性与未来上界；
- fallback 时钟和真实当前时间的一致性；
- 长期 RTC 漂移；
- CA bundle 更新机制；
- TLS 压力与并发连接；
- 所有公网证书链组合。

---

## 11. 可复用验收模板

裸机 HTTPS 上板应至少记录：

1. DNS 与 TCP 前置证据；
2. 验证器所用 UTC 值及来源，并区分当前时间源与 artifact fallback；
3. NTP 失败时的明确回退路径、回滚风险和未来值上界；
4. TLS 库、curl 版本和源码摘要；
5. CA bundle 的真实镜像路径；
6. 默认验证的正向站点；
7. 错误主机名或不可信链的负向站点；
8. 随机数安全状态；
9. 镜像摘要与板端传输一致性。

其中第 6 项与第 7 项必须成对出现：

~~~text
该接受的接受 + 该拒绝的拒绝
        才能证明验证器工作
~~~

---

## 12. 最终证据链

~~~text
DNS / HTTP 已成功
  ↓
HTTPS 默认验证仍依赖一个 wall-clock 输入 + CA + hostname
  ↓
板端 NTP 不可达，零点/旧硬编码日期使功能门禁不可用
  ↓
构建期生成并校验 /etc/build-epoch
启动期 NTP 优先，失败后使用 artifact epoch 做功能性 fallback
  ↓
fallback 无真实性、反回滚或未来上界
不能代表真实当前时间
  ↓
固定 curl 8.19.0 + Mbed TLS 3.6.7 + CA + NSS
  ↓
QEMU：正确站点 200，错误 hostname curl 60
  ↓
同一运行时上板：NTP 失败进入 epoch 回退
正确站点 200，错误 hostname curl 60
  ↓
证明该配置时钟下 CA/主机名正负门禁工作
不证明真实证书有效期或时间安全闭环
~~~

对应修复提交：<code>6b08ed74 feat(board): enable verified HTTPS curl</code>。
