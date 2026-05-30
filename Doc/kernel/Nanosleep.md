# Nanosleep

Nanosleep 是一种精确到纳秒级别的睡眠系统调用，允许用户进程挂起自身执行一段指定的时间，用于实现延时、轮询节流、定时调度等功能。

## NPUcore 的 nanosleep 实现

我们在 NPUcore 内核中支持使用 `sys_clock_nanosleep` 接口实现精确睡眠。该接口支持 CLOCK_REALTIME 和 CLOCK_MONOTONIC 等基础时钟（暂未完全支持全部 Linux 时钟），并允许通过 flags 指定是否为中断唤醒等行为。

目前我们只支持进程自身的主动挂起，不支持中断唤醒和剩余时间返回。

## 接口定义

```rust
pub fn sys_clock_nanosleep(
    clk_id: usize,
    flags: u32,
    rqtp: *const TimeSpec,
    rmtp: *mut TimeSpec,
) -> isize
```

- `clk_id`: 时钟源，一般为 `0` 表示 CLOCK_REALTIME。
- `flags`: 目前未使用，可用于将来支持 TIMER_ABSTIME 等标志。
- `rqtp`: 需要睡眠的时间指针，类型为 `TimeSpec`。
- `rmtp`: 睡眠被中断时返回剩余时间（当前未实现，传入将被忽略）。

返回值：

- 成功返回 `SUCCESS`。
- 参数非法或翻译失败返回负的错误码。

## 实现方式

我们通过内核调度器支持的 **阻塞睡眠队列** 机制实现 nanosleep。具体流程如下：

1. 内核接收到 `sys_clock_nanosleep` 请求；
2. 从用户空间读取 `TimeSpec` 结构；
3. 将当前进程加入定时阻塞队列，设置超时时间；
4. 调度器自动在超时时间后唤醒该进程；
5. 如果中途被唤醒（尚未支持），将返回错误码并设置 `rmtp`。

目前实现中，我们主要支持基于 **CLOCK_MONOTONIC** 的相对时间睡眠，使用系统时间戳加上目标纳秒数实现计时控制。

## 数据结构

```rust
pub struct TimeSpec {
    pub tv_sec: usize,
    pub tv_nsec: usize,
}
```

所有 `nanosleep` 的请求都会先被转换为目标唤醒时间（单位为内核时钟节拍或纳秒），并插入到调度器管理的定时器队列中。

## 注意事项

- 当前实现不支持被信号中断的中断返回（不支持 `rmtp`）；
- 所有时间都以内核启动时间为参考点，非绝对时间；
- 多线程环境下，若线程调用 nanosleep，将仅挂起该线程；
- 若传入非法指针或超大时间，系统将拒绝处理并返回错误码。