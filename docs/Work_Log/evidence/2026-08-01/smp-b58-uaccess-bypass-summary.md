# B58 uaccess 原始物理视图绕过路径收口证据

## 范围与结论

B57 只证明了 fixed-size 对象/数组 copy 的“翻译与实际访问同一 VM 锁域”。
B58 继续删除能够绕过该边界的生产路径，但不在本节点原地重写遍布 FS/网络的
`UserBuffer`。这样可先把风险收敛到一个核心，再于下一节点原子替换其数据模型和
返回值语义，避免新旧两套抽象并行扩散。

本节点完成后：

- 生产源码不再引用 `trans_ref!`/`trans_refmut!`；两个宏已删除；
- `translated_str()` 不再读取锁外物理页 slice；
- clone3、uname/prctl、mremap/mincore、getrandom 不再直接构造物理页 `UserBuffer`；
- bind/connect/sendto 只在内核所有的 sockaddr 快照上解析地址；
- 未使用的 raw-pointer sockaddr parser 已删除；
- UserBuffer 未使用的 Index/IndexMut/IntoIterator 面已删除。

## 关键设计裁决

### 1. 预 fault 不等于固定映射

`fault_in_user_range()` 只用于必须在外部副作用前尽早报错的 ABI，例如创建 pidfd、生成
随机数或收集 mincore 结果。它逐页 fault-in 和验证，但不返回 PA/slice。另一 CPU
可在预检查完成后立即 CoW、mprotect 或 munmap，因此真正读写仍必须走
`copy_from/to_user`。

### 2. parser 只消费内核所有快照

sockaddr 最大 512 字节。`read_sockaddr()` 在 VM 锁内复制这段数据到 `Vec<u8>`，然后
`Endpoint::from_sockaddr()` 才解析它。字符串使用一个 4 KiB scratch：锁内 copy，锁外扫描
NUL 和扩容 `String`。两者都避免把 allocator/parser 带入 VM 临界区。

### 3. 阻塞路径不保存用户页视图

recvfrom 的阻塞路径在 WaitQueue 期间只持有内核 `Vec<u8>`，唤醒后再用
`copy_to_user_array()` 写回。旧注释把它称为 `trans_refmut!` workaround，宏删除后已改为
真实的寿命和锁序约束。

### 4. getrandom 的部分完成

getrandom 先预 fault，再用 256 字节内核 chunk 生成并写回。对大于 256 字节的请求，
若已完成部分 chunk 后后续操作失败，返回已完成字节数，而不是在用户内存已改变时
伪装成全量错误。Linux man-pages 明确要求调用者检查返回值，且大请求可部分返回；
Linux `drivers/char/random.c::get_random_bytes_user()` 也在 copy 失败前已写入部分时返回进度。

官方/一手参考：

- Linux man-pages `getrandom(2)`:
  <https://www.man7.org/linux/man-pages/man2/getrandom.2.html>
- Linux kernel `drivers/char/random.c`:
  <https://github.com/torvalds/linux/blob/master/drivers/char/random.c>
- Rust Reference, undefined behavior / aliasing:
  <https://doc.rust-lang.org/stable/reference/behavior-considered-undefined.html>
- Rust-for-Linux `UserSlice`:
  <https://rust.docs.kernel.org/6.14/src/kernel/uaccess.rs.html>

## DeepSeek 本地协作与人工裁决

- `smp-b58-userbuffer-design`: 只读统计 UserBuffer 调用面，确认构造时 fault-in、锁外长期使用
  物理页切片是 SMP 竞态。
- 未采纳其“先加一个未使用 VA 内部结构”的分批；这会暂时形成两套数据模型，
  不会提升生产安全性。
- 纠正了审查中的三个事实错误：`UserBuffer[Index]` 当时仍被 clone3 使用；TCP
  self-connected 路径仍有队列锁域细节；UserIoVec 与 UserBuffer 必须同一节点迁移。
- `smp-b58-build-gate`: 在首次冻结源码上串行 RV64/LA64 `CORE_NUM=8` kernel build，
  分别 126.420s/132.876s，exit 0。随后的注释收口与未使用 parser 删除纳入最终
  preliminary 内嵌 build，首次 build 只作编译反馈，不作最终指纹证据。
- `smp-b58-final-gate`: 在受限 Docker 网关中串行运行两个最终门禁。DeepSeek 对既有
  `busybox kill 10`/`test_brk` 原因的描述是未经日志证实的推测，本证据不采纳；
  只核对原始 judge JSON 中的计数与失败集合。

## 最终验证

冻结代码 diff SHA-256：

```text
ffe33257bdf0831793e37aede2e97f954570f046d0112c9af49259fdc75d3711
```

| 门禁 | 结果 | 耗时 | 精确失败集合 |
|------|------|------|----------------|
| RV64 `CORE_NUM=8 mask=0x003` | 312/314，exit 0 | 357.267s | musl/glibc 各一个 `busybox kill 10` |
| LA64 `CORE_NUM=8 mask=0x003` | 308/314，exit 0 | 372.638s | musl/glibc 的 `test_brk` 各1/3，以及各一个 `busybox kill 10` |

两个 preliminary recipe 都使用最终源码先执行对应架构 `make kernel`，然后启动 8 核
QEMU。runner 的 source before/after 均为上述 diff 指纹，`mutation_detected=false`；无 panic、
fatal trap、timeout 或缺失 group marker。RV64/LA64 计数和 B57 基线精确一致。

`git diff --check` 通过。`make lint`、定向 clone3/getrandom/socket 竞态测试和普通 UserBuffer
并发 unmap/CoW 测试未运行；最后一项必须等 VA-backed UserBuffer 完成后才有正确测试对象。

## 后续边界

下一节点需将 `UserBuffer`/`UserIoVec` 原子替换为“当前 token + 用户 VA 区间”，并将
`read/write/read_at/write_at/clear/fill_at` 改为可传播首页 `EFAULT` 与后续页部分进度的
`Result<usize, isize>`。不应用隐藏 last-error 字段或无条件把失败变成“0 字节成功”。
