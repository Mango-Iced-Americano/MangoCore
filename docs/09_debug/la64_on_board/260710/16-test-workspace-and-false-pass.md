---
title: "2K1000LA 测试工作区：只读源、隐藏依赖与 exit 0 假通过复盘"
category: debug
status: resolved
author: MangoCore Team
last_update: 2026-07-15
tags: [postmortem, la64, 2k1000la, test-harness, scratch, fat32, lmbench, false-pass]
code_paths:
  - "user/src/bin/init.rs"
  - "user/src/bin/initproc.rs"
related_docs:
  - "docs/03_fs/2k1000-full-test-disk.md"
  - "docs/03_fs/init-and-rootfs.md"
evidence_commits:
  - "0da6a13e"
  - "3ce82f0a"
  - "bb5b9411"
evidence_records:
  - "docs/Work_Log.md, 2026-07-12 scratch workspace entries"
---

# 2K1000LA 测试工作区：只读源、隐藏依赖与 exit 0 假通过复盘

## 0. 一句话结论

实板测试源位于必须保持只读的分区，但 basic、BusyBox、Lua、lmbench 等用例会在当前目录创建、删除、rename 或 mmap 辅助文件。直接从只读源执行会得到 <code>EROFS</code>；只复制“看得见的主程序”到 P2 <code>/scratch</code> 又会漏掉相对路径、数据文件和绝对 wrapper 依赖。

最隐蔽的故障出现在 lmbench：

- 首版最小 payload 漏了普通文件 <code>lat_sig</code>；
- musl/glibc 外层脚本都退出 0、都有 GROUP END；
- 子项却打印 <code>mmap: Bad file descriptor</code>；
- 因为 <code>lmbench_all lat_sig ... prot lat_sig</code> 把同名文件作为 mmap/page-fault 输入，不只是把 <code>lat_sig</code> 当 applet 名。

修复不是放开测试源写权限，而是把“只读黄金源”和“可写执行实例”分离：按人工审计
结果复制当前已知 workload 的必需 payload 集合到
<code>/scratch/work/&lt;group&gt;-&lt;libc&gt;</code>，复制后逐文件检查，准备失败时
拒绝回退只读源；验收同时检查 exit status、stderr 和关键业务输出。补齐
<code>lat_sig</code> 及绝对 wrapper 后，两个 libc 均出现预期
<code>Protection fault</code>，不再有 Bad FD，并运行到 GROUP END。

---

## 1. 存储约束不是测试故障

上板磁盘的安全边界是：

| 区域 | 角色 | 权限 |
|---|---|---|
| P1/P3 等源分区 | 测试程序、运行时、黄金数据 | 只读 |
| P2 FAT32 | 隔离 scratch | 可写 |
| initramfs ramfs | <code>/bin</code>、<code>/sbin</code>、<code>/lib</code>、<code>/usr</code> 运行时 | staged 模式可写 |

保持源分区只读可以防止：

- 测例改坏下一轮输入；
- 暖复位后状态漂移；
- 测试脚本误删工具链；
- 一个用例污染另一个 libc 的环境。

因此看到 <code>EROFS</code> 时，正确问题不是“怎样让源盘可写”，而是“这个 workload 的可变状态应放在哪里”。

---

## 2. 症状分成两层

### 2.1 显性失败：当前目录不可写

已观察到：

~~~text
lat_select: Could not create temp file ...: Read-only file system
~~~

后续 iozone 也会创建 <code>iozone.tmp</code> 和 <code>iozone.DUMMY.*</code>。这类错误直接指出工作目录与测试写入契约冲突。

### 2.2 隐性失败：外层成功，子测例失败

lmbench 首版 scratch 运行：

~~~text
outer group exit = 0
GROUP END present
stderr: mmap: Bad file descriptor
~~~

这比 EROFS 更危险，因为自动汇总可能只看到退出 0 与 END 标记，从而把不完整 benchmark 记为通过。

---

## 3. 调试时间线

### 3.1 basic：先验证工作区机制

第一批只迁移 basic：

~~~text
/musl 或 /glibc（只读源）
  ↓ cp
/scratch/work/basic-musl
/scratch/work/basic-glibc
  ↓
从工作副本 chdir + exec
~~~

验收确认两个 libc 的 <code>getcwd</code> 分别位于：

- <code>/scratch/work/basic-musl/basic</code>；
- <code>/scratch/work/basic-glibc/basic</code>。

两组全部运行到 END，且覆盖测试内创建/删除文件、sync 和下一组继续执行。

### 3.2 复制实现第一次失败：BusyBox shell 函数变量不是局部变量

首版尝试用 shell 函数递归复制目录。BusyBox shell 中函数变量默认是全局的；递归进入子目录后覆盖父调用保存的 source/destination：

~~~text
copy_tree(parent)
  ├─ src=parent
  └─ copy_tree(child)
       └─ src=child  ← 覆盖 parent 的 src
回到 parent 后继续使用的 src 已经不是 parent
~~~

结果不是立即报“递归算法错”，而是最终工作区缺少 <code>run-all.sh</code>。

处理方式：

- 不再维护自定义递归 shell copy；
- 恢复 BusyBox <code>cp -R</code>；
- 对 FAT32 不支持的 chmod/set_metadata 告警只做已知范围的 stderr 抑制；
- 用复制命令退出值和关键文件存在性检查决定成功。

这里的原则是：可以抑制已知无害噪声，不能省略后置条件。

### 3.3 扩到 BusyBox/Lua：按组定义 payload

不同组需要的不是同一个“大目录复制”：

- BusyBox：主程序、测试脚本、命令列表；
- Lua：busybox、lua、驱动脚本和九个 lua 文件；
- basic：完整 basic 子目录、组脚本和 busybox。

每组准备命令都采用：

~~~text
rm -rf old_workdir || exit 1
mkdir -p workdir    || exit 1
copy every payload  || exit 1
check key files     || exit 1
sync                || exit 1
~~~

当 scratch 已启用而准备函数返回失败，runner 打印 FATAL 并拒绝从只读源继续。这避免“准备失败 → 悄悄走旧路径 → 又报 EROFS”的假回退。

### 3.4 lmbench 首版：主程序齐了，语义依赖没齐

首版 lmbench payload 包含：

- <code>busybox</code>；
- <code>lmbench_testcode.sh</code>；
- <code>lmbench_all</code>；
- <code>hello</code>。

看起来主脚本和 multi-call binary 都在，但两组均输出：

~~~text
mmap: Bad file descriptor
~~~

外层仍 exit 0。

### 3.5 追到隐藏文件 lat_sig

检查实际命令而不是文件名直觉后，发现保护故障测试的调用形态：

~~~text
lmbench_all lat_sig ... prot lat_sig
~~~

这里两个 <code>lat_sig</code> 角色不同：

1. 前者选择 <code>lmbench_all</code> 中的 applet；
2. 末尾普通路径 <code>lat_sig</code> 是被 mmap 的输入文件。

当工作区没有末尾文件时，open 失败，后续 mmap 接收到无效 fd，于是打印 Bad FD。benchmark wrapper 没有把该子项失败传播为组退出失败，所以外层仍为 0。

修复后明确复制并检查 <code>lat_sig</code>。

### 3.6 追到绝对 wrapper 回调

lmbench 的 fork+exec 项不是只在当前目录寻找程序，<code>hello</code> wrapper 会回调绝对路径：

~~~text
/code/lmbench_src/bin/build/lmbench_all
~~~

若只复制工作区而不更新该路径，musl/glibc 可能仍调用旧源、错误 libc 或不存在的链接。

准备每个 libc 工作区时都：

1. 建立 <code>/code/lmbench_src/bin/build</code>；
2. 删除旧 <code>lmbench_all</code> 链接；
3. 链到本轮 <code>/scratch/work/lmbench-&lt;libc&gt;/lmbench_all</code>；
4. 任何一步失败即拒绝运行。

---

## 4. 底层原理一：payload 必须按真实调用审计，不能只凭文件名印象

本轮人工审计的输入类别可表示为：

~~~text
audited_payload(test) =
  executable
  ∪ scripts sourced/executed
  ∪ relative data files
  ∪ writable current-directory state
  ∪ dynamic loader/libraries
  ∪ absolute wrapper callbacks
  ∪ symlink targets
~~~

这不是通用依赖发现算法，也不保证得到数学意义上的完整/最小闭包。代码只维护各组
显式文件清单与后置检查；升级脚本或 workload 后仍可能出现新的隐藏输入。

只运行 <code>ldd</code> 或只复制 ELF，最多覆盖动态库依赖，无法发现：

- shell 脚本中的相对文件；
- multi-call applet 的数据参数；
- 运行时生成/rename 的临时文件；
- 硬编码 <code>/code/...</code> 回调。

<code>lat_sig</code> 就是“名字像程序、实际又作为数据文件”的典型隐藏依赖。

---

## 5. 底层原理二：为什么 GROUP END 与 exit 0 都不充分

测试栈至少有四层：

~~~text
initproc scheduler
  → group shell script
    → benchmark driver
      → individual benchmark command
~~~

外层 exit 0 只说明最外层脚本最后执行的命令成功，除非脚本显式使用 fail-fast 并传播每个子状态。GROUP END 更弱：它只证明控制流走到了打印标记的位置，超时场景甚至可能由 initproc 补打。

因此：

| 信号 | 能证明 | 不能证明 |
|---|---|---|
| GROUP END | 调度边界结束 | 每个子项成功 |
| group exit 0 | shell 最终状态为 0 | 中间命令没有失败 |
| 无 EROFS | 工作目录可写 | 当前已知 payload 已全部覆盖 |
| 有 benchmark 数值 | 某项产生输出 | 所有关键项覆盖 |
| stderr 无 Bad FD | 未见这一类 fd 故障 | 全部语义正确 |

本次假通过正是“最外层成功掩盖内层失败”。

---

## 6. 根因证明

### 6.1 工作区问题

| 事实 | 结论 |
|---|---|
| 只读源上 <code>lat_select</code> 创建临时文件报 EROFS | workload 需要可写 cwd |
| 复制到 P2 后 EROFS 消失 | 不是测试本身要求修改黄金源 |
| 自定义递归 copy 后缺 <code>run-all.sh</code> | 复制算法破坏 payload |
| <code>cp -R</code> + 后置检查后 basic 完整到 END | 工作区机制成立 |

### 6.2 lmbench 假通过问题

| 事实 | 结论 |
|---|---|
| 两组 exit 0，但有 <code>mmap: Bad file descriptor</code> | 外层状态不足 |
| 首版排查识别出普通 <code>lat_sig</code> 缺口 | 建立具体假设 |
| 命令末尾把 <code>lat_sig</code> 作为 mmap 输入 | 解释 Bad FD 机制 |
| 补文件后出现预期 <code>Protection fault</code>，Bad FD 消失 | 假设被干预验证 |
| musl 108s、glibc 216s，关键指标到 GROUP END | 完整度回归 |

<code>Protection fault</code> 在该用例中是被测信号路径的预期输出，不是新 crash；它替代 Bad FD 才说明测试真正走到了保护故障阶段。

---

## 7. 最终实现的不变量

### 7.1 源与实例分离

~~~text
只读 source = 可复现输入
可写 workdir = 一次运行的状态
~~~

### 7.2 准备阶段 fail closed

若 <code>/scratch</code> 存在且该组声明必须使用 scratch：

~~~text
prepare success → 只从 workdir 运行
prepare failure → FATAL，停止该组
绝不回退 → /musl 或 /glibc 只读源
~~~

### 7.3 每组显式 payload 与后置检查

lmbench 最终至少检查：

- <code>busybox</code>；
- <code>lmbench_testcode.sh</code>；
- <code>lmbench_all</code>；
- <code>hello</code>；
- <code>lat_sig</code>；
- 指向当前 libc 工作区的绝对 wrapper 链接。

---

## 8. 验证矩阵

| 阶段 | musl | glibc | 关键判据 |
|---|---|---|---|
| basic scratch | 完整 END，exit 0 | 完整 END，exit 0 | getcwd 在各自 workdir |
| BusyBox scratch | 组内命令成功 | 组内命令成功 | 可写操作/rename |
| Lua scratch | 9 项 success | 9 项 success | 18 个子项总计 |
| lmbench 首版 | exit 0 + Bad FD | exit 0 + Bad FD | 判为失败/不完整 |
| lmbench 补依赖 | 108s 到 END | 216s 到 END | Protection fault；无 Bad FD |

每轮修改后按项目规则在 Docker 中串行完成 RV64、LA64 kernel build，并构建 2K1000 scratch 镜像。历史 basic 阶段 QEMU 因缺少 <code>disk-la.img</code> 未启动；本文不把编译通过写成 QEMU 通过。

仓库当前没有为 2026-07-12 这组实板操作保存独立原始串口日志，实测结果追溯到三个提交与 Work Log，不列无关 testresult 文件充当证据。

---

## 9. 修复边界

已经解决：

- 已迁移组不再从只读源直接写；
- 工作区复制失败不静默回退；
- basic/BusyBox/Lua/lmbench 当前已审计的必需 payload 可运行；
- lmbench <code>lat_sig</code> 隐藏数据文件和绝对 wrapper 已覆盖；
- 验收不再只看 exit 0。

仍需持续执行：

- 每新增/升级测试版本重新审计 payload；当前实现不会自动发现依赖；
- 相对路径、绝对路径和动态加载依赖都可能变化；
- FAT32 不具备 Unix 权限语义，不能把 chmod 告警一概当成功；
- 单个 benchmark 的数值合理性仍需独立基线；
- exit 0 的传播问题最好在上游脚本中逐项修复，而非永远靠日志关键词。

---

## 10. 可复用检查清单

迁移任何测试组前回答：

1. 当前目录会创建、删除、rename、chmod、mmap 哪些文件？
2. 主脚本还执行或 source 哪些脚本？
3. multi-call binary 是否把普通路径当数据？
4. 是否存在硬编码绝对 wrapper？
5. 动态 loader、libc、NSS 等运行时在哪里？
6. copy 命令是否真的复制 symlink target？
7. 准备失败是否会误回退只读源？
8. 哪些 stderr 是预期，哪些必须判失败？
9. 哪些关键业务标记必须出现？
10. 外层 exit 是否能代表全部子项？

---

## 11. 最终证据链

~~~text
只读源运行 → lat_select EROFS
  ↓
P2 /scratch 建工作副本
  ↓
自定义递归 copy 因 BusyBox 全局函数变量漏 run-all.sh
  ↓
改 cp -R + exit 检查 + 关键文件后置检查
  ↓
basic / BusyBox / Lua 工作区成功
  ↓
lmbench 两组 exit 0，却有 mmap: Bad file descriptor
  ↓
追到命令末尾 lat_sig 是 mmap 输入文件
并追到 /code/.../lmbench_all 绝对回调
  ↓
补齐 payload 和 libc-specific 链接
  ↓
Protection fault 出现、Bad FD 消失、108s/216s 到 GROUP END
  ↓
根因闭环：执行实例缺依赖 + 外层状态掩盖子项失败
~~~

对应提交：<code>0da6a13e</code>、<code>3ce82f0a</code>、<code>bb5b9411</code>。
