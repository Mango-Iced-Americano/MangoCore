#!/usr/bin/env bash

set -o pipefail

groups=(basic busybox lua libctest iozone unixbench iperf libcbench lmbench netperf cyclictest ltp)
group_timeout_sec="${GROUP_TIMEOUT_SEC:-300}"
requested_arch="${TEST_ARCH:-rv64}"
blk_mode_global="${TEST_BLK_MODE:-}"
blk_mode_rv="${TEST_BLK_MODE_RV:-}"
blk_mode_la="${TEST_BLK_MODE_LA:-}"

case "${requested_arch}" in
  rv|rv64)
    arch_list=("rv64")
    ;;
  la|la64)
    arch_list=("la64")
    ;;
  both|all)
    arch_list=("la64" "rv64")
    ;;
  *)
    echo "[run_test] unsupported TEST_ARCH=${requested_arch}, expected rv64, la64, or both"
    exit 1
    ;;
esac

resolve_blk_mode() {
  local arch="$1"
  if [[ -n "${blk_mode_global}" ]]; then
    echo "${blk_mode_global}"
    return
  fi
  if [[ "${arch}" == "rv64" ]]; then
    if [[ -n "${blk_mode_rv}" ]]; then
      echo "${blk_mode_rv}"
    else
      echo "virt"
    fi
  else
    if [[ -n "${blk_mode_la}" ]]; then
      echo "${blk_mode_la}"
    else
      echo "mem"
    fi
  fi
}

resolve_run_target() {
  local arch="$1"
  if [[ "${arch}" == "rv64" ]]; then
    echo "rv64-run"
  else
    echo "la64-run"
  fi
}

resolve_result_dir() {
  local arch="$1"
  if [[ "${arch}" == "rv64" ]]; then
    echo "testresult/rv"
  else
    echo "testresult/la"
  fi
}

on_interrupt() {
  echo
  echo "[run_test] interrupted, cleaning child processes..."
  pkill -f "make -C os rv64-run" >/dev/null 2>&1 || true
  pkill -f "make -C os la64-run" >/dev/null 2>&1 || true
  pkill -f "qemu-system-riscv64" >/dev/null 2>&1 || true
  pkill -f "qemu-system-loongarch64" >/dev/null 2>&1 || true
  exit 130
}

trap on_interrupt INT TERM

if ! command -v timeout >/dev/null 2>&1; then
  echo "[run_test] missing command: timeout"
  echo "[run_test] please install coreutils timeout, or run with an environment that provides it"
  exit 1
fi

total_pass_count=0
total_fail_count=0
total_timeout_count=0

validate_group_log() {
  local group="$1"
  local log_file="$2"
  local script_name="${group}_testcode.sh"

  # Hard failure signatures that should never be considered PASS.
  if grep -Eq "No such file or directory|exec failed for" "${log_file}"; then
    return 1
  fi

  # A group is PASS only when both musl and glibc runs finish with exit_code=0.
  if ! grep -Fq "[initproc] done ${script_name} in /musl exit_code=0" "${log_file}"; then
    return 1
  fi
  if ! grep -Fq "[initproc] done ${script_name} in /glibc exit_code=0" "${log_file}"; then
    return 1
  fi

  return 0
}

for arch in "${arch_list[@]}"; do
  blk_mode="$(resolve_blk_mode "${arch}")"
  run_target="$(resolve_run_target "${arch}")"
  result_dir="$(resolve_result_dir "${arch}")"

  mkdir -p "${result_dir}"

  pass_count=0
  fail_count=0
  timeout_count=0

  echo "=== ARCH START arch=${arch} blk_mode=${blk_mode} ==="

  for i in "${!groups[@]}"; do
    g="${groups[$i]}"
    mask=$(printf "0x%03X" $((1 << i)))
    conf="/tmp/os_test_${arch}_${blk_mode}_${g}.conf"
    log="${result_dir}/${g}.log"

    cat > "$conf" <<EOF
mode=run
mask=${mask}
EOF

    echo "=== RUN arch=${arch} blk_mode=${blk_mode} group=${g} mask=${mask} timeout=${group_timeout_sec}s ==="
    if ! make -C os conf-inject CONF_ARCH="${arch}" CONF_BLK_MODE="${blk_mode}" CONF_FILE="$conf"; then
      echo "[run_test] conf inject failed for ${g}"
      fail_count=$((fail_count + 1))
      continue
    fi

    timeout --foreground --signal=TERM --kill-after=20s "${group_timeout_sec}s" \
      make -C os "${run_target}" </dev/null 2>&1 | tee "$log"
    rc=${PIPESTATUS[0]}

    if [[ "$rc" -eq 124 || "$rc" -eq 137 ]]; then
      echo "[run_test] TIMEOUT group=${g}, forced stop and continue"
      timeout_count=$((timeout_count + 1))
      continue
    fi

    if [[ "$rc" -ne 0 ]]; then
      echo "[run_test] FAIL group=${g}, exit_code=${rc}"
      fail_count=$((fail_count + 1))
      continue
    fi

    if ! validate_group_log "${g}" "${log}"; then
      echo "[run_test] FAIL group=${g}, log validation failed"
      fail_count=$((fail_count + 1))
      continue
    fi

    echo "[run_test] PASS group=${g}"
    pass_count=$((pass_count + 1))
  done

  echo "=== SUMMARY arch=${arch} blk_mode=${blk_mode} groups ==="
  echo "PASS=${pass_count} FAIL=${fail_count} TIMEOUT=${timeout_count} TOTAL=${#groups[@]}"

  total_pass_count=$((total_pass_count + pass_count))
  total_fail_count=$((total_fail_count + fail_count))
  total_timeout_count=$((total_timeout_count + timeout_count))
done

echo "=== SUMMARY ALL ARCH ==="
echo "PASS=${total_pass_count} FAIL=${total_fail_count} TIMEOUT=${total_timeout_count} TOTAL=$(( ${#groups[@]} * ${#arch_list[@]} ))"