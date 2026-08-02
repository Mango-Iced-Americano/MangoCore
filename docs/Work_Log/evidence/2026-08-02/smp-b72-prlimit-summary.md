# B72 prlimit 成对事务冻结证据

## 变更边界

- `prlimit()` 先 copyin 完整新值，再解析当前目标与资源并做无锁范围校验。
- owner 锁内一次完成旧 soft/hard pair 快照、hard-limit 提权复核和新 pair 提交。
- NOFILE 的两个 setter 共用一次 fd-table guard；其他已实现限制本批仍使用 `task.inner`。
- 日志和 old-limit copyout 均在锁外；copyout `EFAULT` 不回滚已提交的新值。

本批没有迁移 rlimit owner。TCB/FdTable 仍是过渡状态；Linux 式进程级共享、group CPU
accounting，以及 `CLONE_FILES` 跨进程共享时 NOFILE 与 fd-table 生命周期分离均未验收。

## 冻结指纹

- 基线 HEAD：`9179cde5d69cf7e48463b7b712c01b90f35205c6`
- `os/src/syscall/process/ids.rs` SHA-256：
  `ad8fec0b5b79302b1b46b4b4d7f1d50c23ccb239938d5bbadcdfc1b1b71560f9`
- 验证前 tracked diff SHA-256：
  `86a739ab4752389d6cbe15ddc0f663d56ab79e96ecd334a905d1938446a75e87`
- 四个 DeepSeek child job 的 source before/after 指纹完全相同，`mutation_detected=false`。

## Docker 验证

| 架构 | 项目 | 结果 | 时间 |
|---|---|---:|---:|
| RV64 | `CORE_NUM=8` kernel build | PASS，exit 0 | 130.733s |
| LA64 | `CORE_NUM=8` kernel build | PASS，exit 0 | 133.802s |
| RV64 | 8 核 rlimit focused gate | musl 9/9、glibc 9/9 | 151.578s |
| LA64 | 8 核 rlimit focused gate | musl 9/9、glibc 9/9 | 166.339s |

两种 QEMU 日志均打印 `configured=8 ... online_mask=0xff`，没有 panic、timeout、TFAIL、
TBROK 或 TCONF。focused 集合为 `getrlimit01..03` 与 `setrlimit01..06`；覆盖全资源读取一致性、
NOFILE/FSIZE/NPROC/CORE 设置、EINVAL/EPERM/EFAULT，以及 CPU soft/hard 信号路径。

## 证据边界

- 普通 LTP 未并发执行两个 `prlimit(NOFILE)`，精确双线程交错为 **NOT RUN**。
- torn pair 的排除依据是所有合法 NOFILE 读写都受同一个 fd-table owner 锁保护，不将 8 核运行
  本身误写成动态并发证明。
- 进程级 owner 迁移、线程组共享、group CPU accounting、跨进程 `CLONE_FILES` 语义均为后续节点。
- 本批按风险复用 B69 的同祖先初赛门禁，没有重复执行 `mask=0x003`。

## 协作裁决

DeepSeek 执行了双架构构建和 QEMU 日志汇总。GPT/Codex 复核原始日志后修正了“日志未出现
online mask”和“8 核运行已证明 NOFILE 并发一致性”两项过度结论，并以源码 owner/锁序和
明确的 NOT RUN 边界作为最终裁决。
