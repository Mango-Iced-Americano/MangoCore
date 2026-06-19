#!/usr/bin/env python3
"""
MangoCore Drift Analysis Script

Parses QEMU serial output containing drift_window snapshot markers,
computes per-window deltas, detects performance drift anomalies,
and outputs structured analysis (CSV + human-readable report).

Usage:
    python3 scripts/analyze_drift.py <serial_output_file> [--csv-output drift_analysis.csv]
    cat serial_output.log | python3 scripts/analyze_drift.py

Input format (from QEMU serial):
    [initproc] [drift] === drift_window W0 musl pre ===
    ready_len_max=4
    interruptible_len_max=3
    ...
    [initproc] [drift] === drift_window W0 musl pre end ===
    Simple syscall: 9.25013 microseconds
    ...
    [initproc] [drift] === drift_window W0 musl post ===
    ready_len_max=7
    ...
    [initproc] [drift] === drift_window W0 musl post end ===
"""

import sys
import re
import csv
import argparse


WINDOW_RE = re.compile(
    r'\[initproc\] \[drift\] === drift_window (\w+) (\w+) (pre|post) ==='
)
END_RE = re.compile(
    r'\[initproc\] \[drift\] === drift_window \w+ \w+ (?:pre|post) end ==='
)
LMBENCH_RE = re.compile(
    r'Simple syscall:\s*([\d.]+)\s*microseconds'
)
KV_RE = re.compile(r'^([\w.]+)=(\d+)$')


def parse_serial_output(lines):
    """
    Parse serial output lines into structured drift-window data.

    Returns {window_id: {libc: {
        'pre': {str: int}, 'post': {str: int}, 'lmbench_score': float|None
    }}}
    """
    windows = {}

    current_window = None
    current_libc = None
    current_type = None
    in_snapshot = False
    current_kv = {}

    lmbench_pending_window = None
    lmbench_pending_libc = None

    for i, line in enumerate(lines):
        raw = line.rstrip('\n\r')

        m = WINDOW_RE.search(raw)
        if m:
            if current_window is not None and in_snapshot:
                _save_snapshot(windows, current_window, current_libc,
                               current_type, current_kv)

            current_window = m.group(1)
            current_libc = m.group(2)
            current_type = m.group(3)
            in_snapshot = True
            current_kv = {}

            # New window starts: cancel any pending lmbench search from
            # previous window where the lmbench line was never found.
            if current_type == 'pre':
                lmbench_pending_window = None
                lmbench_pending_libc = None
            continue

        m2 = END_RE.search(raw)
        if m2 and in_snapshot:
            _save_snapshot(windows, current_window, current_libc,
                           current_type, current_kv)

            if current_type == 'pre':
                lmbench_pending_window = current_window
                lmbench_pending_libc = current_libc

            in_snapshot = False
            current_kv = {}
            continue

        if in_snapshot:
            kv_m = KV_RE.match(raw)
            if kv_m:
                current_kv[kv_m.group(1)] = int(kv_m.group(2))
            continue

        if lmbench_pending_window is not None:
            lb_m = LMBENCH_RE.search(raw)
            if lb_m:
                wid = lmbench_pending_window
                libc = lmbench_pending_libc
                score = float(lb_m.group(1))
                if wid not in windows:
                    windows[wid] = {}
                if libc not in windows[wid]:
                    windows[wid][libc] = {}
                windows[wid][libc]['lmbench_score'] = score
                lmbench_pending_window = None
                lmbench_pending_libc = None

    if current_window is not None and in_snapshot:
        _save_snapshot(windows, current_window, current_libc,
                       current_type, current_kv)

    return windows


def _save_snapshot(windows, window_id, libc, snap_type, kv):
    if window_id not in windows:
        windows[window_id] = {}
    if libc not in windows[window_id]:
        windows[window_id][libc] = {}
    windows[window_id][libc][snap_type] = dict(kv)


def compute_deltas(windows):
    """
    For each (window, libc) compute deltas = post - pre per counter.

    Returns {(window_id, libc): {
        'lmbench_score': float|None,
        'delta': {str: int},
        'pre': {str: int},
        'post': {str: int},
    }}
    """
    results = {}

    for wid, libc_dict in windows.items():
        for libc, data in libc_dict.items():
            pre = data.get('pre', {})
            post = data.get('post', {})

            all_keys = set(pre.keys()) | set(post.keys())
            deltas = {}
            for k in all_keys:
                p = pre.get(k, 0)
                q = post.get(k, 0)
                delta = q - p
                if delta != 0:
                    deltas[k] = delta
                else:
                    deltas[k] = 0

            results[(wid, libc)] = {
                'lmbench_score': data.get('lmbench_score'),
                'delta': deltas,
                'pre': pre,
                'post': post,
            }

    return results


def compute_derived(deltas_data):
    """
    Add derived/ratio metrics on top of raw deltas.

    Returns {(wid, libc): {
        'getppid_avg_ticks': float, 'syscall_avg_ticks': float,
        'fast_path_ratio': float, 'ctxsw_per_getppid': float,
        'tlb_per_getppid': float, 'reclaim_per_getppid': float,
        'heap_growth': int, 'delta': {str: int},
    }}
    """
    derived = {}

    for key, data in deltas_data.items():
        d = data['delta']

        getppid_total = d.get('syscall_getppid_total', 0)
        syscall_total = d.get('syscall_total', 0)
        fast_path = d.get('fast_path_calls', 0)
        fair_pick = d.get('fair_pick_calls', 0)
        ctx_switch = d.get('context_switch_total', 0)
        tlb_flushes = d.get('tlb_flushes', 0)
        reclaim_scanned = d.get('reclaim_pages_scanned_total', 0)

        getppid_cost_ticks = d.get('getppid_cost_ticks_total', 0)
        syscall_cost_ticks = d.get('syscall_cost_ticks_total', 0)

        derived[key] = {
            'lmbench_score': data['lmbench_score'],
            'getppid_avg_ticks': (
                getppid_cost_ticks / max(getppid_total, 1)
            ),
            'syscall_avg_ticks': (
                syscall_cost_ticks / max(syscall_total, 1)
            ),
            'fast_path_ratio': (
                fast_path / max(fast_path + fair_pick, 1)
            ),
            'ctxsw_per_getppid': (
                ctx_switch / max(getppid_total, 1)
            ),
            'tlb_per_getppid': (
                tlb_flushes / max(getppid_total, 1)
            ),
            'reclaim_per_getppid': (
                reclaim_scanned / max(getppid_total, 1)
            ),
            'heap_growth': d.get('heap_current_bytes', 0),
            'delta': d,
        }

    return derived


def detect_anomalies(derived, windows):
    """
    Detect performance drift anomalies per the Oracle decision tree.

    Returns list of (severity, category, description, window_id).
    """
    anomalies = []

    by_libc = {}
    for (wid, libc), data in derived.items():
        wnum = int(re.sub(r'\D', '', wid) or 0)
        by_libc.setdefault(libc, []).append((wnum, wid, data))

    for libc, entries in by_libc.items():
        entries.sort(key=lambda x: x[0])

        getppid_avgs = [
            (wnum, wid, data['getppid_avg_ticks'])
            for wnum, wid, data in entries
        ]

        # getppid_cost drift: last 3 windows monotonically increasing
        # AND last > first * 1.15
        if len(getppid_avgs) >= 3:
            last3 = getppid_avgs[-3:]
            increasing = all(
                last3[i + 1][2] >= last3[i][2] for i in range(2)
            )
            first_val = last3[0][2]
            last_val = last3[-1][2]
            if increasing and first_val > 0 and last_val > first_val * 1.15:
                values_str = ', '.join(f'{v:.2f}' for _, _, v in last3)
                anomalies.append((
                    'HIGH',
                    'getppid_cost_drift',
                    f'getppid avg ticks monotonically increasing over last 3'
                    f' windows [{libc}]: {values_str} '
                    f'(last {last_val:.2f} > first*1.15 = {first_val * 1.15:.2f})',
                    last3[-1][1],
                ))

        if len(getppid_avgs) >= 2:
            first_val = getppid_avgs[0][2]
            last_val = getppid_avgs[-1][2]
            if first_val > 0 and last_val > first_val * 1.5:
                anomalies.append((
                    'MEDIUM',
                    'getppid_cost_drift',
                    f'getppid avg ticks [{libc}] {last_val:.2f} is '
                    f'{last_val / first_val:.1f}x the first window '
                    f'({first_val:.2f})',
                    getppid_avgs[-1][1],
                ))

        for wnum, wid, data in entries:
            # Scheduler degradation: fast_path dropping or fair_pick active
            if data['fast_path_ratio'] < 0.99:
                anomalies.append((
                    'HIGH',
                    'scheduler_degradation',
                    f'fast_path_ratio={data["fast_path_ratio"]:.6f} '
                    f'(< 0.99) [{libc}/{wid}]',
                    wid,
                ))

            fair_pick = data['delta'].get('fair_pick_calls', 0)
            if fair_pick > 0:
                anomalies.append((
                    'MEDIUM',
                    'scheduler_degradation',
                    f'fair_pick_calls={fair_pick} (> 0) [{libc}/{wid}]',
                    wid,
                ))

            # Timer bloat: stale wake tasks or excessive timer wheel depth
            pre_ktimer = (
                windows.get(wid, {})
                .get(libc, {})
                .get('pre', {})
                .get('ktimer_len_max', 0)
            )
            post_ktimer = (
                windows.get(wid, {})
                .get(libc, {})
                .get('post', {})
                .get('ktimer_len_max', 0)
            )
            max_ktimer = max(pre_ktimer, post_ktimer)
            if max_ktimer > 100:
                anomalies.append((
                    'MEDIUM',
                    'timer_bloat',
                    f'ktimer_len_max={max_ktimer} (> 100) [{libc}/{wid}]',
                    wid,
                ))

            stale = data['delta'].get('ktimer_stale_waketask', 0)
            if stale > 0:
                anomalies.append((
                    'HIGH',
                    'timer_bloat',
                    f'ktimer_stale_waketask={stale} (> 0) [{libc}/{wid}]',
                    wid,
                ))

            # Reclaim during null syscall indicates memory pressure
            reclaim = data['delta'].get('reclaim_pages_scanned_total', 0)
            if reclaim > 0:
                anomalies.append((
                    'MEDIUM',
                    'reclaim_interference',
                    f'reclaim_pages_scanned_total={reclaim} '
                    f'[{libc}/{wid}] (page reclaim during null syscall)',
                    wid,
                ))

            # Null syscall should never trigger TLB flush
            tlb = data['delta'].get('tlb_flushes', 0)
            if tlb > 0:
                anomalies.append((
                    'HIGH',
                    'tlb_anomaly',
                    f'tlb_flushes={tlb} [{libc}/{wid}] '
                    f'(TLB flush during null syscall!)',
                    wid,
                ))

            hg = data['heap_growth']
            if hg > 0:
                anomalies.append((
                    'LOW',
                    'heap_growth',
                    f'heap_current_bytes grew by {hg} bytes [{libc}/{wid}]',
                    wid,
                ))

        # Monotonically increasing heap across windows
        heap_post_vals = []
        for wnum, wid, data in entries:
            post = windows.get(wid, {}).get(libc, {}).get('post', {})
            heap_post_vals.append(
                (wnum, wid, post.get('heap_current_bytes', 0))
            )

        if len(heap_post_vals) >= 3:
            if all(
                heap_post_vals[i + 1][2] >= heap_post_vals[i][2]
                for i in range(len(heap_post_vals) - 1)
            ):
                vals = ', '.join(str(v[2]) for v in heap_post_vals)
                anomalies.append((
                    'MEDIUM',
                    'heap_growth',
                    f'heap_current_bytes monotonically increasing [{libc}]: '
                    f'{vals}',
                    heap_post_vals[-1][1],
                ))

        # Monotonically decreasing free frames = leak
        free_vals = []
        for wnum, wid, data in entries:
            post = windows.get(wid, {}).get(libc, {}).get('post', {})
            free_vals.append(
                (wnum, wid, post.get('free_frames', 0))
            )

        if len(free_vals) >= 3:
            if all(
                free_vals[i + 1][2] <= free_vals[i][2]
                for i in range(len(free_vals) - 1)
            ):
                vals = ', '.join(str(v[2]) for v in free_vals)
                anomalies.append((
                    'HIGH',
                    'resource_leak',
                    f'free_frames monotonically decreasing [{libc}]: {vals}',
                    free_vals[-1][1],
                ))

        # Zombie accumulation signals process-reaping issues
        zombie_vals = []
        for wnum, wid, data in entries:
            post = windows.get(wid, {}).get(libc, {}).get('post', {})
            zombie_vals.append(
                (wnum, wid, post.get('total_zombies', 0))
            )

        if len(zombie_vals) >= 2:
            first_z = zombie_vals[0][2]
            last_z = zombie_vals[-1][2]
            if first_z > 0 and last_z > first_z:
                anomalies.append((
                    'MEDIUM',
                    'resource_leak',
                    f'total_zombies [{libc}] increased from {first_z} '
                    f'to {last_z}',
                    zombie_vals[-1][1],
                ))

    return anomalies


def write_csv(derived, filepath):
    if not derived:
        print("No data to write to CSV.", file=sys.stderr)
        return

    all_delta_keys = sorted({
        k for data in derived.values() for k in data['delta']
    })

    fieldnames = [
        'window',
        'libc',
        'lmbench_score_us',
        'getppid_avg_ticks',
        'syscall_avg_ticks',
        'fast_path_ratio',
        'ctxsw_per_getppid',
        'tlb_per_getppid',
        'reclaim_per_getppid',
        'heap_growth_bytes',
    ] + all_delta_keys

    with open(filepath, 'w', newline='') as f:
        writer = csv.DictWriter(f, fieldnames=fieldnames)
        writer.writeheader()

        def _sort_key(item):
            (wid, libc), _ = item
            wnum = int(re.sub(r'\D', '', wid) or 0)
            return (wnum, libc)

        for (wid, libc), data in sorted(derived.items(), key=_sort_key):
            row = {
                'window': wid,
                'libc': libc,
                'lmbench_score_us': (
                    f'{data["lmbench_score"]:.5f}'
                    if data['lmbench_score'] is not None
                    else ''
                ),
                'getppid_avg_ticks': round(data['getppid_avg_ticks'], 2),
                'syscall_avg_ticks': round(data['syscall_avg_ticks'], 2),
                'fast_path_ratio': round(data['fast_path_ratio'], 6),
                'ctxsw_per_getppid': round(data['ctxsw_per_getppid'], 4),
                'tlb_per_getppid': round(data['tlb_per_getppid'], 4),
                'reclaim_per_getppid': round(data['reclaim_per_getppid'], 4),
                'heap_growth_bytes': data['heap_growth'],
            }
            for k in all_delta_keys:
                row[k] = data['delta'].get(k, 0)
            writer.writerow(row)


def print_report(derived, anomalies):
    print("# MangoCore Drift Analysis Report")
    print()

    print("## LMBench Scores (null syscall latency)")
    print()
    print("| Window | Libc | Score (μs) | getppid_avg (ticks) | fast_path_ratio |")
    print("|--------|------|-----------|---------------------|-----------------|")

    libc_order = list(dict.fromkeys(
        lc for _, lc in sorted(
            derived.keys(), key=lambda x: (int(re.sub(r'\D', '', x[0]) or 0), x[1])
        )
    ))

    for libc in libc_order:
        for (wid, lc), data in sorted(derived.items(), key=lambda x: int(re.sub(r'\D', '', x[0][0]) or 0)):
            if lc != libc:
                continue
            score_str = (
                f'{data["lmbench_score"]:.5f}'
                if data['lmbench_score'] is not None
                else 'N/A'
            )
            print(
                f"| {wid} | {lc} | {score_str} | "
                f"{data['getppid_avg_ticks']:.2f} | "
                f"{data['fast_path_ratio']:.6f} |"
            )
    print()

    print("## Anomalies Detected")
    print()

    if not anomalies:
        print("**No anomalies detected.** All windows within expected bounds.")
        print()
    else:
        severity_rank = {'HIGH': 0, 'MEDIUM': 1, 'LOW': 2}
        anomalies.sort(key=lambda a: (
            severity_rank.get(a[0], 99),
            a[3],
        ))

        for severity, category, desc, window in anomalies:
            icon = {'HIGH': '🔴', 'MEDIUM': '🟡', 'LOW': '🟢'}
            badge = icon.get(severity, '⚪')
            print(f"### {badge} [{severity}] {category}")
            print(f"- **Window**: {window}")
            print(f"- **Details**: {desc}")
            print()

    print("## Recommendations")
    print()

    rec_map = {
        'getppid_cost_drift': (
            'Instrument getppid() with per-call cycle counters and check '
            'for cache-TLB effects in the fast path.'
        ),
        'scheduler_degradation': (
            'Add runqueue balance counters and trace sched_yield wakeup '
            'storms. Check for unfair scheduling in the fast-path bypass.'
        ),
        'timer_bloat': (
            'Instrument timer wheel compaction with sub-window histograms. '
            'Check for stale timer accumulation patterns.'
        ),
        'tlb_anomaly': (
            'Add per-CPU TLB flush source tracing. A null syscall should '
            'never touch page tables \u2014 likely culprit is getppid accessing '
            'a remote process field that crosses a page boundary.'
        ),
        'resource_leak': (
            'Add allocation tracking per syscall category. Check for missing '
            'frees or refcount leaks in the getppid code path.'
        ),
        'heap_growth': (
            'Profile heap allocations during getppid with a per-syscall '
            'allocation counter. Look for temporary allocations not freed '
            'before returning.'
        ),
        'reclaim_interference': (
            'Add reclaim backtrace to identify which allocation triggers '
            'page reclaim during a null syscall.'
        ),
    }

    high_sev = [a for a in anomalies if a[0] == 'HIGH']
    med_sev = [a for a in anomalies if a[0] == 'MEDIUM']

    if high_sev:
        top_cat = high_sev[0][1]
        print(f'**Priority**: Investigate **{top_cat}** '
              f'({len(high_sev)} high severity finding(s))')
        print()
        print(f'**Suggested next step**: {rec_map.get(top_cat, "Investigate the HIGH severity anomalies first.")}')
    elif med_sev:
        print('**Priority**: Investigate medium severity anomalies in the next iteration.')
        print()
        print('**Suggested next step**: Add targeted instrumentation per the anomaly descriptions above.')
    else:
        print('**Priority**: No anomalies detected. System appears stable.')
        print()
        print('**Suggested next step**: Consider running with higher load '
              '(e.g. multiple concurrent lat_syscall instances) to expose '
              'subtle regressions.')

    print()
    print('---')
    print('*Report generated by analyze_drift.py*')


def main():
    parser = argparse.ArgumentParser(
        description=(
            'Analyze MangoCore drift debugging output from QEMU serial. '
            'Parses drift_window markers, computes deltas, detects anomalies.'
        ),
    )
    parser.add_argument(
        'input', nargs='?',
        help='Input file (QEMU serial output). Reads from stdin if omitted.',
    )
    parser.add_argument(
        '--csv-output', default='drift_analysis.csv',
        help='CSV output path (default: drift_analysis.csv)',
    )
    args = parser.parse_args()

    if args.input:
        with open(args.input, 'r') as f:
            lines = f.readlines()
    else:
        lines = sys.stdin.readlines()

    if not lines:
        print("Error: empty input.", file=sys.stderr)
        sys.exit(1)

    windows = parse_serial_output(lines)
    if not windows:
        print(
            "Error: No drift_window markers found in input. "
            "Check that the serial output contains [drift] markers.",
            file=sys.stderr,
        )
        sys.exit(1)

    total_snapshots = sum(
        1 for ld in windows.values()
        for d in ld.values()
        if d.get('pre') and d.get('post')
    )
    print(
        f'Parsed {len(windows)} window(s), {total_snapshots} complete '
        f'(pre+post) snapshot(s).',
        file=sys.stderr,
    )

    deltas_data = compute_deltas(windows)
    derived = compute_derived(deltas_data)

    anomalies = detect_anomalies(derived, windows)

    if derived:
        write_csv(derived, args.csv_output)
        print(f"CSV written to {args.csv_output}", file=sys.stderr)

    print_report(derived, anomalies)


if __name__ == '__main__':
    main()
