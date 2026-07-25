#!/bin/sh

set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
classifier=$script_dir/check-la64-regression-log.sh

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

case "${CHECK:-}" in
    make)
        make_source=$script_dir/../os/make/la64.mk

        [ -f "$make_source" ] || fail "missing LA64 make fragment: $make_source"

        require_make_contract() {
            grep -F -- "$1" "$make_source" >/dev/null 2>&1 || \
                fail "LA64 regression Make contract missing: $1"
        }

        if grep -F -- '| tee' "$make_source" >/dev/null 2>&1; then
            fail 'LA64 regression Make contract must not pipe QEMU through tee'
        fi

        require_make_contract 'regression-run: toolchain-preflight'
        require_make_contract 'timeout --foreground 60 qemu-system-loongarch64 \'
        require_make_contract '-smp threads=1 >/tmp/regression-la.log 2>&1; \'
        require_make_contract 'qemu_status=$$?; \'
        require_make_contract 'state=$$(../scripts/check-la64-regression-log.sh /tmp/regression-la.log $$qemu_status); \'
        require_make_contract '"STATE=PASS STATUS=0"'
        require_make_contract '"STATE=BLOCKED_STAGE1_PRE_ENTRY STATUS="*|"STATE=BLOCKED_STAGE1_POST_ENTRY STATUS="*'
        require_make_contract '=== REGRESSION BLOCKED ==='
        require_make_contract '=== REGRESSION FAIL ==='

        echo 'PASS: LA64 regression Make runner source contract'
        exit 0
        ;;
    freshness)
        build_source=$script_dir/../os/build.rs

        [ -f "$build_source" ] || fail "missing build script: $build_source"

        require_build_contract() {
            grep -F -- "$1" "$build_source" >/dev/null 2>&1 || \
                fail "build script contract missing: $1"
        }

        require_build_contract 'println!("cargo:rerun-if-changed=../fs-img-dir/initramfs-regression-la.cpio");'

        echo 'PASS: LA64 regression initramfs Cargo freshness source contract'
        exit 0
        ;;
    pid1)
        pid1_source=$script_dir/../user/src/bin/regression_init.rs

        [ -f "$pid1_source" ] || fail "missing PID1 source: $pid1_source"

        require_pid1_contract() {
            grep -F -- "$1" "$pid1_source" >/dev/null 2>&1 || \
                fail "PID1 source contract missing: $1"
        }

        require_pid1_contract 'let pid = fork();'
        require_pid1_contract 'if pid == 0 {'
        require_pid1_contract 'exec(prog, &args, &envp);'
        require_pid1_contract 'exit(127);'
        require_pid1_contract 'waitpid(pid as usize, &mut status)'
        require_pid1_contract 'if status & 0x7F == 0 {'
        require_pid1_contract '(status >> 8) & 0xFF'
        require_pid1_contract '128 + (status & 0x7F)'
        require_pid1_contract 'println!("[L4 REGRESSION RESULT: PASS]");'
        require_pid1_contract 'println!("[L4 REGRESSION RESULT: FAIL] exit_code={}", exit_code);'
        require_pid1_contract 'shutdown();'

        echo 'PASS: PID1 regression outcome reporting source contract'
        exit 0
        ;;
    trap-slots)
        config_source=$script_dir/../os/src/hal/arch/loongarch64/config.rs
        trap_source=$script_dir/../os/src/hal/arch/loongarch64/kern_stack.rs

        [ -f "$config_source" ] || fail "missing LA64 config source: $config_source"
        [ -f "$trap_source" ] || fail "missing LA64 trap-slot source: $trap_source"

        require_trap_slot_contract() {
            source=$1
            contract=$2
            description=$3
            grep -F -- "$contract" "$source" >/dev/null 2>&1 || \
                fail "LA64 trap-slot $description contract missing: $contract"
        }

        forbid_trap_slot_contract() {
            source=$1
            contract=$2
            description=$3
            if grep -F -- "$contract" "$source" >/dev/null 2>&1; then
                fail "LA64 trap-slot $description contract violated: $contract"
            fi
        }

        require_trap_slot_contract "$config_source" \
            'pub const TRAMPOLINE: usize = SIGNAL_TRAMPOLINE - PAGE_SIZE;' \
            'ordinary trampoline location'
        require_trap_slot_contract "$config_source" \
            'pub const TRAP_CONTEXT_BASE: usize = TRAMPOLINE - KERNEL_STACK_MAX_SLOTS * PAGE_SIZE;' \
            'full trap window sizing'
        require_trap_slot_contract "$config_source" \
            'pub const USR_MMAP_END: usize = TRAP_CONTEXT_BASE;' \
            'mmap boundary'
        forbid_trap_slot_contract "$config_source" \
            'pub const TRAP_CONTEXT_BASE: usize = SIGNAL_TRAMPOLINE - PAGE_SIZE;' \
            'old single-slot layout'
        forbid_trap_slot_contract "$config_source" \
            'pub const TRAMPOLINE: usize = TRAP_CONTEXT_BASE - PAGE_SIZE;' \
            'old trampoline chain'

        require_trap_slot_contract "$trap_source" \
            'pub fn trap_cx_bottom_from_tid(tid: usize) -> usize {' \
            'tid-based trap address helper'
        require_trap_slot_contract "$trap_source" \
            'if !(1..=KERNEL_STACK_MAX_SLOTS).contains(&tid) {' \
            'tid range guard'
        require_trap_slot_contract "$trap_source" \
            'TRAP_CONTEXT_BASE + (tid - 1) * PAGE_SIZE' \
            'full-window tid address'
        forbid_trap_slot_contract "$trap_source" \
            'TRAP_CONTEXT_BASE - (tid - 1) * PAGE_SIZE' \
            'old downward tid address'

        guard_line=$(grep -n -F -- 'if !(1..=KERNEL_STACK_MAX_SLOTS).contains(&tid) {' "$trap_source" | cut -d: -f1 | head -n 1)
        subtract_line=$(grep -n -F -- 'TRAP_CONTEXT_BASE + (tid - 1) * PAGE_SIZE' "$trap_source" | cut -d: -f1 | head -n 1)
        [ -n "$guard_line" ] || fail 'LA64 trap-slot range guard line missing'
        [ -n "$subtract_line" ] || fail 'LA64 trap-slot subtraction line missing'
        [ "$guard_line" -lt "$subtract_line" ] || \
            fail 'LA64 trap-slot range guard must precede slot subtraction'

        echo 'PASS: LA64 full trap-slot window source contract'
        exit 0
        ;;
    trampoline)
        config_source=$script_dir/../os/src/hal/arch/loongarch64/config.rs
        trap_source=$script_dir/../os/src/hal/arch/loongarch64/trap/mod.rs
        address_space_source=$script_dir/../os/src/mm/address_space.rs

        [ -f "$config_source" ] || fail "missing LA64 config source: $config_source"
        [ -f "$trap_source" ] || fail "missing LA64 trap source: $trap_source"
        [ -f "$address_space_source" ] || fail "missing address-space source: $address_space_source"

        require_trampoline_contract() {
            source=$1
            contract=$2
            description=$3
            grep -F -- "$contract" "$source" >/dev/null 2>&1 || \
                fail "LA64 trampoline $description contract missing: $contract"
        }

        forbid_trampoline_contract() {
            source=$1
            contract=$2
            description=$3
            if grep -F -- "$contract" "$source" >/dev/null 2>&1; then
                fail "LA64 trampoline $description contract violated: $contract"
            fi
        }

        require_trampoline_block_contract() {
            source=$1
            function_name=$2
            contract=$3
            description=$4
            block=$(sed -n "/fn $function_name(&mut self) {/,/^    }/p" "$source")
            printf '%s\n' "$block" | grep -F -- "$contract" >/dev/null 2>&1 || \
                fail "LA64 trampoline $description contract missing: $contract"
        }

        forbid_trampoline_block_contract() {
            source=$1
            function_name=$2
            contract=$3
            description=$4
            block=$(sed -n "/fn $function_name(&mut self) {/,/^    }/p" "$source")
            if printf '%s\n' "$block" | grep -F -- "$contract" >/dev/null 2>&1; then
                fail "LA64 trampoline $description contract violated: $contract"
            fi
        }

        forbid_trampoline_contract "$config_source" \
            'pub const TRAMPOLINE: usize = SIGNAL_TRAMPOLINE;' \
            'config separation'
        require_trampoline_contract "$config_source" \
            'pub const SIGNAL_TRAMPOLINE: usize = USR_VIRT_SPACE_END - PAGE_SIZE + 1;' \
            'signal address'
        require_trampoline_contract "$config_source" \
            'pub const TRAMPOLINE: usize = SIGNAL_TRAMPOLINE - PAGE_SIZE;' \
            'ordinary address'
        require_trampoline_contract "$config_source" \
            'pub const TRAP_CONTEXT_BASE: usize = TRAMPOLINE - KERNEL_STACK_MAX_SLOTS * PAGE_SIZE;' \
            'full trap-context window'
        require_trampoline_contract "$config_source" \
            'macro_rules! should_map_trampoline {' \
            'mapping gate'
        awk '
            /macro_rules! should_map_trampoline/ { in_macro = 1; next }
            in_macro && /^[[:space:]]*true[[:space:]]*$/ { found = 1 }
            END { exit(found ? 0 : 1) }
        ' "$config_source" >/dev/null 2>&1 || \
            fail 'LA64 trampoline mapping gate contract missing: should_map_trampoline!() must expand to true'

        require_trampoline_contract "$trap_source" \
            'let restore_va = __restore as usize;' \
            'trap-return restore address'
        forbid_trampoline_contract "$trap_source" \
            'let restore_va = __restore as usize - __alltraps as usize + TRAMPOLINE;' \
            'trap-return restore address'

        require_trampoline_block_contract "$address_space_source" map_trampoline \
            'map_privileged_user_page(' 'ordinary mapping API'
        require_trampoline_block_contract "$address_space_source" map_trampoline \
            'VirtAddr::from(TRAMPOLINE).into(),' 'ordinary mapping address'
        require_trampoline_block_contract "$address_space_source" map_trampoline \
            'PhysAddr::from(strampoline as usize).into(),' 'ordinary mapping target'
        require_trampoline_block_contract "$address_space_source" map_trampoline \
            'MapPermission::R | MapPermission::X,' 'ordinary mapping permissions'
        forbid_trampoline_block_contract "$address_space_source" map_trampoline \
            'MapPermission::R | MapPermission::X | MapPermission::U,' 'ordinary mapping permissions'
        require_trampoline_block_contract "$address_space_source" map_signaltrampoline \
            'map_user_page(' 'signal mapping API'
        require_trampoline_block_contract "$address_space_source" map_signaltrampoline \
            'VirtAddr::from(SIGNAL_TRAMPOLINE).into(),' 'signal mapping address'
        require_trampoline_block_contract "$address_space_source" map_signaltrampoline \
            'PhysAddr::from(ssignaltrampoline as usize).into(),' 'signal mapping target'
        require_trampoline_block_contract "$address_space_source" map_signaltrampoline \
            'MapPermission::R | MapPermission::X | MapPermission::U,' 'signal mapping permissions'

        echo 'PASS: LA64 ordinary/signal trampoline separation source contract'
        exit 0
        ;;
    pt-load-overlap)
        loader_source=$script_dir/../os/src/mm/address_space.rs
        regression_elf=${LA64_REGRESSION_ELF:-$script_dir/../user/target/loongarch64-unknown-linux-gnu/release/regression_init}

        [ -f "$loader_source" ] || fail "missing ELF loader source: $loader_source"
        [ -r "$regression_elf" ] || fail "missing LA64 regression ELF: $regression_elf"

        require_loader_contract() {
            grep -F -- "$1" "$loader_source" >/dev/null 2>&1 || \
                fail "PT_LOAD overlap loader contract missing: $1"
        }

        require_loader_contract 'struct ElfLoadPage'
        require_loader_contract 'fn collect_load_pages('
        require_loader_contract 'fn map_elf_load_segments('
        require_loader_contract 'fn copy_load_segment<F>('
        require_loader_contract 'fn map_load_segment('
        require_loader_contract 'page.map_perm |= segment.map_perm;'
        require_loader_contract 'ppn.get_bytes_array().fill(0);'

        load_layout=$(readelf -lW "$regression_elf")
        printf '%s\n' "$load_layout" | grep -F -- 'LOAD           0x010000 0x0000000120000000' >/dev/null 2>&1 || \
            fail 'missing first shared-page PT_LOAD in LA64 regression ELF'
        printf '%s\n' "$load_layout" | grep -F -- 'LOAD           0x01b0ec 0x000000012000b0ec' >/dev/null 2>&1 || \
            fail 'missing second shared-page PT_LOAD in LA64 regression ELF'
        printf '%s\n' "$load_layout" | grep -F -- 'LOAD           0x020978 0x0000000120010978' >/dev/null 2>&1 || \
            fail 'missing third shared-page PT_LOAD in LA64 regression ELF'

        echo 'PASS: LA64 shared-page PT_LOAD loader contract'
        exit 0
        ;;
    mmap-arena)
        layout_source=$script_dir/../user/src/layout.rs
        regression_source=$script_dir/../user/src/bin/regression/regression_mmap_edge_cases.rs
        mmap_source=$script_dir/../os/src/mm/mmap.rs

        [ -f "$layout_source" ] || fail "missing user layout source: $layout_source"
        [ -f "$regression_source" ] || fail "missing mmap regression source: $regression_source"
        [ -f "$mmap_source" ] || fail "missing kernel mmap source: $mmap_source"

        require_mmap_arena_contract() {
            source=$1
            contract=$2
            description=$3
            grep -F -- "$contract" "$source" >/dev/null 2>&1 || \
                fail "LA64 mmap-arena $description contract missing: $contract"
        }

        require_mmap_arena_contract "$layout_source" \
            'pub const PAGE_SIZE: usize = 0x1000;' 'page size'
        require_mmap_arena_contract "$layout_source" \
            'pub const LA64_USR_VIRT_SPACE_END: usize = (1 << 37) - 1;' 'user address limit'
        require_mmap_arena_contract "$layout_source" \
            'pub const LA64_TRAP_CONTEXT_BASE: usize =' 'trap-context base declaration'
        require_mmap_arena_contract "$layout_source" \
            'LA64_TRAMPOLINE - LA64_KERNEL_STACK_MAX_SLOTS * PAGE_SIZE;' 'trap-context base formula'
        require_mmap_arena_contract "$layout_source" \
            'pub const LA64_MMAP_ARENA_END: usize = LA64_TRAP_CONTEXT_BASE;' 'exclusive mmap arena end'

        require_mmap_arena_contract "$mmap_source" \
            'fn fixed_mmap_intersects_la64_trap_context_window(start: VirtAddr, end: VirtAddr) -> bool {' \
            'LA64 fixed-map reserved-window guard'
        require_mmap_arena_contract "$mmap_source" \
            'let reserved_start = VirtAddr::from(TRAP_CONTEXT_BASE).floor();' \
            'LA64 trap-context bound'
        require_mmap_arena_contract "$mmap_source" \
            'let reserved_end = VirtAddr::from(TRAMPOLINE).ceil();' \
            'LA64 trampoline bound'
        require_mmap_arena_contract "$mmap_source" \
            'request_start < reserved_end && reserved_start < request_end' \
            'LA64 half-open interval intersection'

        fixed_request_contract='flags.contains(MapFlags::MAP_FIXED) || flags.contains(MapFlags::MAP_FIXED_NOREPLACE);'
        [ "$(grep -F -c -- "$fixed_request_contract" "$mmap_source")" -eq 2 ] || \
            fail 'LA64 mmap-arena fixed-request classification must cover mmap and shm mmap'

        require_mmap_guard_before_unmap() {
            block=$1
            name=$2
            guard_line=$(printf '%s\n' "$block" | \
                grep -n -F -- 'if fixed_mmap_intersects_la64_trap_context_window(start_hint, requested_end) {' | \
                cut -d: -f1 | head -n 1)
            unmap_line=$(printf '%s\n' "$block" | \
                grep -n -F -- '.unmap_range(&mut address_space.page_table, start_vpn, end_vpn, true)' | \
                cut -d: -f1 | head -n 1)
            [ -n "$guard_line" ] || fail "LA64 mmap-arena $name reserved-window guard missing"
            [ -n "$unmap_line" ] || fail "LA64 mmap-arena $name destructive unmap missing"
            [ "$guard_line" -lt "$unmap_line" ] || \
                fail "LA64 mmap-arena $name reserved-window guard must precede destructive unmap"
        }

        mmap_fixed_block=$(sed -n '/pub(super) fn do_mmap/,/pub(super) fn do_shm_mmap/p' "$mmap_source")
        shm_fixed_block=$(sed -n '/pub(super) fn do_shm_mmap/,/pub(super) fn do_munmap/p' "$mmap_source")
        require_mmap_guard_before_unmap "$mmap_fixed_block" 'do_mmap'
        require_mmap_guard_before_unmap "$shm_fixed_block" 'do_shm_mmap'

        require_mmap_arena_contract "$regression_source" \
            '#[cfg(target_arch = "loongarch64")]' 'LA64-only subcase'
        require_mmap_arena_contract "$regression_source" \
            'let forbidden_hint = LA64_MMAP_ARENA_END + PAGE_SIZE;' 'slot-2 derived hint'
        require_mmap_arena_contract "$regression_source" \
            'MAP_PRIVATE | MAP_ANONYMOUS,' 'anonymous private mapping'
        require_mmap_arena_contract "$regression_source" \
            'if fallback == forbidden_hint {' 'exact-address RED assertion'
        require_mmap_arena_contract "$regression_source" \
            'if fallback >= LA64_MMAP_ARENA_END {' 'fallback arena assertion'
        require_mmap_arena_contract "$regression_source" \
            'let unmap_ret = sys_munmap(fallback, PAGE_SIZE);' 'fallback-only unmap'
        require_mmap_arena_contract "$regression_source" \
            'let pid = sys_getpid();' 'post-mmap syscall check'

        mmap_arena_subcase=$(awk '
            /LA64-only mmap arena exclusion from the trap-context window/ { in_subcase = 1 }
            /Test 3: mmap MAP_FIXED/ { exit }
            in_subcase { print }
        ' "$regression_source")
        printf '%s\n' "$mmap_arena_subcase" | grep -F -- 'sys_mmap(' >/dev/null 2>&1 || \
            fail 'LA64 mmap-arena subcase must issue mmap'
        if printf '%s\n' "$mmap_arena_subcase" | grep -F -- 'MAP_FIXED' >/dev/null 2>&1; then
            fail 'LA64 mmap-arena subcase must not use MAP_FIXED'
        fi

        echo 'PASS: LA64 mmap arena exclusion source contract'
        exit 0
        ;;
    classifier)
        ;;
    *)
        echo 'RED: CHECK must be freshness, pid1, trap-slots, trampoline, pt-load-overlap, mmap-arena, or classifier' >&2
        exit 1
        ;;
esac

if [ ! -x "$classifier" ]; then
    echo "RED: missing executable classifier: $classifier" >&2
    exit 1
fi

work_dir=$(mktemp -d "${TMPDIR:-/tmp}/la64-regression-gate.XXXXXX")
trap 'rm -rf "$work_dir"' EXIT HUP INT TERM

run_case() {
    name=$1
    expected_state=$2
    expected_qemu_status=$3
    fixture=$work_dir/$name.log
    output=$work_dir/$name.out

    shift 3
    printf '%s\n' "$@" > "$fixture"

    if "$classifier" "$fixture" "$expected_qemu_status" > "$output"; then
        actual_exit=0
    else
        actual_exit=$?
    fi

    [ "$actual_exit" -eq 0 ] || fail "$name: classifier exit $actual_exit"
    grep -F "STATE=$expected_state" "$output" >/dev/null 2>&1 || \
        fail "$name: expected STATE=$expected_state"
    grep -F "STATUS=$expected_qemu_status" "$output" >/dev/null 2>&1 || \
        fail "$name: expected STATUS=$expected_qemu_status"
}

run_case pass PASS 0 \
    '[LA64 REGRESSION KERNEL]' \
    '[LA64 REGRESSION PID1]' \
    '[LA64 REGRESSION TERMINAL: PASS]'
run_case test_failure TEST_FAILURE 1 \
    '[LA64 REGRESSION KERNEL]' \
    '[LA64 REGRESSION PID1]' \
    '[LA64 REGRESSION TERMINAL: FAIL]'
run_case stage1_pre_entry BLOCKED_STAGE1_PRE_ENTRY 124 \
    '[LA64 REGRESSION KERNEL]'
run_case stage1_post_entry BLOCKED_STAGE1_POST_ENTRY 124 \
    '[LA64 REGRESSION KERNEL]' \
    '[LA64 REGRESSION PID1]'
run_case entry_failure ENTRY_FAILURE 1 \
    '[kernel] boot reached console'
run_case shutdown_failure SHUTDOWN_FAILURE 1 \
    '[LA64 REGRESSION KERNEL]' \
    '[LA64 REGRESSION PID1]' \
    '[LA64 REGRESSION TERMINAL: PASS]'
run_case timeout_after_pass SHUTDOWN_FAILURE 124 \
    '[LA64 REGRESSION KERNEL]' \
    '[LA64 REGRESSION PID1]' \
    '[LA64 REGRESSION TERMINAL: PASS]'

echo 'PASS: LA64 regression classifier fixture contract'
