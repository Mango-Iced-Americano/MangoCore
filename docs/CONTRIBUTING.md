---
title: "文档贡献规范"
category: overview
status: stable
author: MangoCore Team
last_update: 2026-06-14
tags: [docs, contributing, standards]
---

## 文档语言

中文撰写所有叙述性内容，包括设计说明、架构描述、流程解释、注意事项等。代码标识符、errno 名称、系统调用名称、文件路径、命令行示例、类型名、函数名一律使用英文。错误码用负数形式表示（如 `-EAGAIN` 而非 `-11`），syscall 名称用 `sys_xxx` 格式。

混合语言段落示例：`sys_read` 返回 `EINTR` 时表明调用被信号中断，用户态需重新发起读取。严禁出现中英文混写如"调用 sys_read 来读取数据"，应统一为"调用 `sys_read` 读取数据"。

## 文件组织

所有文档统一存放于 `docs/` 目录，按编号分类子目录：

| 目录 | 主题 |
|------|------|
| `docs/00_overview/` | 项目概览、构建指南、贡献规范 |
| `docs/01_architecture/` | 整体架构、启动流程、HAL 层 |
| `docs/02_syscall/` | 系统调用总表、dispatch 机制 |
| `docs/03_fs/` | 文件系统（ext4/fat32/tmpfs/procfs 等） |
| `docs/04_mm/` | 内存管理（物理内存、虚拟内存、页缓存） |
| `docs/05_process/` | 进程/任务管理、调度、信号、IPC |
| `docs/06_net/` | 网络栈（smoltcp socket、适配层） |
| `docs/07_driver/` | 驱动（virtio 块/网卡、设备框架） |
| `docs/08_testing/` | 测试策略、LTP 指南、配置说明 |
| `docs/09_debug/` | 调试技巧、常见问题、QEMU 用法 |

文件命名统一使用 `lower_kebab_case.md`，如 `page_cache.md`、`syscall_table.md`。每个模块的文档放在对应分类目录下，不跨目录放置。全局性文档（如本文件、Work_Log.md）直接放在 `docs/` 根目录。

## 新文档创建流程

1. 复制模板：`cp docs/_templates/module.md docs/<category>/<your_topic>.md`
2. 填写 YAML header 所有字段（参见下节强制字段清单）
3. 依次填写全部 9 个标准节（Overview / Design Goals / Architecture / Key Data Structures / Execution Flow / Interfaces & APIs / Test Mapping / Known Issues / References）
4. 提交 PR 时 status 标记为 `draft`
5. 经至少一位维护者 review 通过后，status 改为 `stable`

模板中预留的占位符（`<...>`）必须替换为实际内容，不得保留空占位符。确有不适用章节的，在该节填写 "N/A" 并注明原因。

## YAML Header 强制字段

每个文档文件的 YAML header 必须包含以下字段，缺失任何一项的 PR 不予合并：

| 字段 | 类型 | 说明 | 示例 |
|------|------|------|------|
| `title` | string | 文档标题，中文 | `"物理内存管理"` |
| `category` | string | 所属分类，枚举值 | `mm` |
| `status` | string | `draft` / `stable` / `deprecated` | `draft` |
| `author` | string | 作者或维护团队 | `MangoCore Team` |
| `last_update` | string | 最后更新日期，YYYY-MM-DD | `2026-06-14` |
| `tags` | list | 标签列表 | `[mm, page-alloc, frame]` |

category 枚举值：`overview`、`architecture`、`syscall`、`fs`、`mm`、`process`、`net`、`driver`、`testing`、`debug`。

## 内容质量要求

标题层级严格递进：`#` 仅用于文档标题（与 YAML header 之间的隐式标题），正文从 `##` 开始，子节用 `###`，不允许跳级（如 `##` 后直接 `####`）。

所有代码块必须标注语言标签。 Rust 代码用 `rust`，命令行用 `bash`，配置文件用 `text`，C 代码用 `c`。无语言标签的代码块视为格式错误。

禁止口语化表述。不使用"这个其实就是"、"说白了"、"说白了就是"等口语化表达。技术文档用陈述句、被动句或祈使句，保持客观严谨。

所有引用的文件路径必须是仓库中真实存在的路径。引用代码时标注文件路径和行号范围，如 `os/src/mm/page_alloc.rs:120-145`。不确定路径正确性的，先用 `ls` 或 `git ls-files` 确认。

图片统一存放于 `docs/assets/{module}/` 目录，引用格式为 `![描述文本](./assets/{module}/xxx.png)`。图片使用 PNG 格式，单图不超过 500KB。

## 测试映射强制要求

每个模块文档的第 7 节必须为 Test Mapping，包含两张表格。

第一张表映射特性到测试用例：

| 特性 | Syscall / API | LTP 用例 | OSCOMP 分组 | 状态 |
|------|--------------|----------|-------------|------|
| `<特性>` | `sys_xxx` | `<testcase>` | `<group>` | pass |
| `<特性>` | `sys_xxx` | `<testcase>` | `<group>` | partial |

状态列使用枚举值：`pass`（全量通过）、`partial`（部分通过）、`fail`（未通过）、`not_run`（未运行）。LTP 用例列引用 Linux Test Project 标准用例名，OSCOMP 分组列引用竞赛测试框架的 group 名。

第二张表记录 LTP 跳过清单：

| 用例 | 跳过原因 | 跟踪 Issue |
|------|----------|------------|
| `<testcase>` | `<原因>` | `<issue 链接>` |

## 同步更新规则

修改系统调用语义时，必须同步更新 `docs/02_syscall/` 下对应文档的接口描述表和该模块的 Test Mapping 状态列。

新增系统调用时，更新 `docs/02_syscall/syscall_table.md`（如已移除则更新所在模块的 API 表），同时在相关模块文档的 Test Mapping 中添加新条目。

修复 bug 时，在受影响模块文档的 Known Issues 节中更新问题状态。已修复的 issue 添加修复日期和 commit hash。

修改架构设计时，更新 `docs/01_architecture/` 下对应架构文档，以及所有受影响的子模块文档。

所有代码修改必须同步更新 `docs/Work_Log.md`，按 mango-workflow skill 规则格式记录。更新内容包括修改的文件列表、验证结果（编译 + 运行）、相关 commit hash。

## 禁止事项

禁止将长日志输出粘贴到文档正文。超过 20 行的日志、trace、dump 应保存到 `docs/logs/` 目录的独立文件中，文档内仅引用文件名和关键摘要。

禁止未经 review 的 status 变更。`draft` 到 `stable` 必须有至少一位维护者在 PR 中明确 approve。`stable` 到 `deprecated` 需要 issue 讨论记录。

禁止删除标准 9 节结构中的任何一节。确定不适用某节时在该节填写 "N/A" 并注明原因，而不是删除该节标题。

禁止在文档中使用硬编码的绝对路径。所有路径引用使用相对于仓库根目录的相对路径，并以 `os/src/` 或 `docs/` 开头。`/home/user/`、`/root/` 等路径不得出现。

## 审查清单

PR 审查人逐项核对，全部勾选后方可合并：

- [ ] YAML header 包含全部 6 个强制字段，格式正确
- [ ] `status` 字段为 `draft`（新文档）或符合 review 记录
- [ ] 标题层级严格递进，无跳级
- [ ] 所有代码块标注了语言标签
- [ ] 无口语化表达
- [ ] 所有文件路径是仓库真实路径
- [ ] 第 7 节 Test Mapping 包含两张完整表格
- [ ] 状态列使用枚举值：pass / partial / fail / not_run
- [ ] 代码修改对应的 `docs/Work_Log.md` 已更新
- [ ] 修改系统调用语义时相关文档已同步
- [ ] 无长日志直接粘贴（均已引用 `docs/logs/`）
- [ ] 无硬编码的绝对路径
- [ ] 双架构编译验证通过（`make rv64-kernel-build-only` + `make la64-kernel-build-only`）
