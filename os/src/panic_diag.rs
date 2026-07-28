//! Panic diagnostic dump — prints kernel state at panic time.
//! Called from panic_handler in both lang_items variants.

pub fn dump_panic_context() {
    print_syscall_context();
    print_kernel_memory();
    print_task_info();
    print_backtrace();
}

fn print_syscall_context() {
    println!("--- SYSCTX ---");
    println!("syscall: {}", crate::task::current_syscall_name());
}

fn print_kernel_memory() {
    println!("--- KERNEL MEMORY ---");

    let (free, total, _au, _aa, _w) = crate::mm::heap_stats();
    println!(
        "heap: {}/{} bytes free ({:.1}% used)",
        free,
        total,
        if total > 0 {
            (total - free) as f64 / total as f64 * 100.0
        } else {
            0.0
        }
    );

    let free_frames = crate::mm::unallocated_frames();
    let free_bytes = free_frames * crate::config::PAGE_SIZE;
    println!(
        "physical frames: {} free ({} bytes, {} pages)",
        free_frames, free_bytes, free_frames
    );
}

fn print_task_info() {
    println!("--- TASK ---");

    match crate::task::try_current_task() {
        Ok(Some(task)) => {
            println!("pid: {}  tgid: {}", task.pid(), task.tgid());

            if let Some(inner) = task.try_inner() {
                println!(
                    "status: {:?}  pending_oom_kill: {}",
                    task.task_status(), inner.pending_oom_kill
                );
            } else {
                println!("task inner: <locked>");
            }
            // parent_pid/exe_path require process.inner lock; skip in panic context
        }
        Ok(None) => println!("no current task"),
        Err(()) => println!("task: <CPU-local unavailable or locked>"),
    }
}

fn print_backtrace() {
    println!("--- BACKTRACE ---");

    #[cfg(target_arch = "riscv64")]
    unsafe {
        riscv_backtrace();
    }
    #[cfg(not(target_arch = "riscv64"))]
    {
        println!("backtrace: use addr2line on kernel binary with RA addresses");
    }
}

#[cfg(target_arch = "riscv64")]
unsafe fn riscv_backtrace() {
    let mut fp: usize;
    core::arch::asm!("mv {}, fp", out(reg) fp);
    println!("current fp: {:#x}", fp);

    extern "C" {
        fn boot_stack_top();
        fn boot_stack();
    }
    let stack_top = boot_stack_top as usize;
    let stack_bottom = boot_stack as usize;
    println!("stack range: {:#x} - {:#x}", stack_bottom, stack_top);

    for i in 0..32 {
        if fp < stack_bottom || fp >= stack_top || fp == 0 {
            break;
        }
        let prev_fp = *(fp as *const usize);
        let ra = *((fp as *const usize).add(1));
        let ra2 = *((fp as *const usize).offset(-1));
        println!("#{:02} fp={:#x} ra={:#x} ra(-1)={:#x}", i, fp, ra, ra2);
        if prev_fp == 0 || prev_fp <= fp || prev_fp >= stack_top {
            break;
        }
        fp = prev_fp;
    }
}
