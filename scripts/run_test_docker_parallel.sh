#!/usr/bin/env bash

set -euo pipefail

printf '%s\n' 'ERROR: run_test_docker_parallel.sh is deprecated; run python3 scripts/run_full_test.py --serial inside Docker instead.' >&2
exit 64

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd -- "${SCRIPT_DIR}/.." && pwd)

DOCKER_IMAGE=${DOCKER_IMAGE:-zhouzhouyi/os-contest:20260104}
WORK_BASE=${PARALLEL_WORKDIR:-"${REPO_ROOT}/.parallel-test"}
RUN_ID=${PARALLEL_RUN_ID:-$(date +%Y%m%d-%H%M%S)}
RESULT_ROOT=${PARALLEL_RESULT_DIR:-"${REPO_ROOT}/testresult/docker-parallel/${RUN_ID}"}
PARALLEL_BUILD=${PARALLEL_BUILD:-0}
PARALLEL_IMAGE_MODE=${PARALLEL_IMAGE_MODE:-bind}
MODE=${MODE:-release}
GROUP_TIMEOUT_SEC=${GROUP_TIMEOUT_SEC:-300}

safe_run_id=${RUN_ID//[^a-zA-Z0-9_.-]/-}
container_names=(
  "mangocore-rv64-${safe_run_id}"
  "mangocore-la64-${safe_run_id}"
)

usage() {
  cat <<'EOF'
Run rv64 and la64 test groups concurrently in two isolated Docker containers.

Environment variables:
  TEST_GROUPS               Test groups forwarded to run_test.sh, e.g. basic,ltp.
  GROUP_TIMEOUT_SEC         Per-group timeout in seconds. Default: 300.
  TEST_BLK_MODE             Global block mode forwarded to run_test.sh.
  TEST_BLK_MODE_RV          rv64 block mode. Default inside run_test.sh: virt.
  TEST_BLK_MODE_LA          la64 block mode. Default inside run_test.sh: virt_pci.
  DOCKER_IMAGE              Docker image. Default: zhouzhouyi/os-contest:20260104.
  PARALLEL_WORKDIR          Host work directory. Default: .parallel-test.
  PARALLEL_RESULT_DIR       Result directory. Default: testresult/docker-parallel/<timestamp>.
  PARALLEL_BUILD            1 to build each arch inside its container before tests. Default: 0.
  PARALLEL_IMAGE_MODE       bind to use root sdcard images directly, copy for isolated
                            per-arch image copies. Default: bind.
  MODE, LOG, RV_TOOLCHAIN, LA_TOOLCHAIN
                            Forwarded to make/run_test.sh when set.

Examples:
  bash scripts/run_test_docker_parallel.sh
  TEST_GROUPS=basic,busybox GROUP_TIMEOUT_SEC=300 bash scripts/run_test_docker_parallel.sh
  PARALLEL_IMAGE_MODE=copy TEST_GROUPS=basic bash scripts/run_test_docker_parallel.sh
  PARALLEL_BUILD=1 GROUP_TIMEOUT_SEC=1800 bash scripts/run_test_docker_parallel.sh
EOF
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

on_interrupt() {
  echo
  echo "[parallel] interrupted, stopping containers..."
  if [[ "${#container_names[@]}" -gt 0 ]]; then
    docker rm -f "${container_names[@]}" >/dev/null 2>&1 || true
  fi
  exit 130
}

trap on_interrupt INT TERM

require_command() {
  local cmd="$1"
  if ! command -v "${cmd}" >/dev/null 2>&1; then
    echo "[parallel] missing command: ${cmd}"
    exit 1
  fi
}

copy_file_cow() {
  local src="$1"
  local dst="$2"
  local tmp="${dst}.tmp.$$"

  mkdir -p "$(dirname -- "${dst}")"
  rm -f "${tmp}"

  if [[ "$(uname -s)" == "Darwin" ]]; then
    cp -c "${src}" "${tmp}" 2>/dev/null || cp -p "${src}" "${tmp}"
  else
    cp --reflink=auto -p "${src}" "${tmp}" 2>/dev/null || cp -p "${src}" "${tmp}"
  fi

  mv -f "${tmp}" "${dst}"
}

sync_source_tree() {
  local dst="$1"
  local excludes=(
    "--exclude=/.git/"
    "--exclude=/.parallel-test/"
    "--exclude=/fs-img-dir/"
    "--exclude=/logs/"
    "--exclude=/testresult/"
    "--exclude=/sdcard-rv.img"
    "--exclude=/sdcard-la.img"
    "--exclude=/kernel-rv"
    "--exclude=/kernel-la"
    "--exclude=/packets.pcap"
    "--exclude=/os/qemu.log"
    "--exclude=**/target/"
  )

  case "${WORK_BASE}/" in
    "${REPO_ROOT}/"*)
      local rel="${WORK_BASE#"${REPO_ROOT}/"}"
      local top="${rel%%/*}"
      if [[ -n "${top}" ]]; then
        excludes+=("--exclude=/${top}/")
      fi
      ;;
  esac

  mkdir -p "${dst}"
  rsync -a --delete "${excludes[@]}" "${REPO_ROOT}/" "${dst}/"
}

prepare_arch_workdir() {
  local arch="$1"
  local workdir="${WORK_BASE}/${arch}"
  local image_name
  local kernel_name

  if [[ "${arch}" == "rv64" ]]; then
    image_name="sdcard-rv.img"
    kernel_name="kernel-rv"
  else
    image_name="sdcard-la.img"
    kernel_name="kernel-la"
  fi

  echo "[parallel] sync arch=${arch} workdir=${workdir}"
  sync_source_tree "${workdir}"

  if [[ ! -f "${REPO_ROOT}/${image_name}" ]]; then
    echo "[parallel] missing ${image_name}; run make testsuits-download and extract images first"
    exit 1
  fi

  if [[ "${PARALLEL_IMAGE_MODE}" == "copy" ]]; then
    echo "[parallel] copy image arch=${arch} image=${image_name}"
    copy_file_cow "${REPO_ROOT}/${image_name}" "${workdir}/${image_name}"
  else
    rm -f "${workdir:?}/${image_name}"
    ln -s "/repo/${image_name}" "${workdir}/${image_name}"
    echo "[parallel] bind image arch=${arch} image=${REPO_ROOT}/${image_name}"
  fi

  if [[ "${PARALLEL_BUILD}" == "1" ]]; then
    rm -f "${workdir:?}/${kernel_name}"
  elif [[ -f "${REPO_ROOT}/${kernel_name}" ]]; then
    if [[ "${PARALLEL_IMAGE_MODE}" == "copy" ]]; then
      echo "[parallel] copy kernel arch=${arch} kernel=${kernel_name}"
      copy_file_cow "${REPO_ROOT}/${kernel_name}" "${workdir}/${kernel_name}"
    else
      rm -f "${workdir:?}/${kernel_name}"
      ln -s "/repo/${kernel_name}" "${workdir}/${kernel_name}"
      echo "[parallel] bind kernel arch=${arch} kernel=${REPO_ROOT}/${kernel_name}"
    fi
  else
    echo "[parallel] missing ${kernel_name}; run make all first, or set PARALLEL_BUILD=1"
    exit 1
  fi
}

docker_run_arch() {
  local arch="$1"
  local container_name="$2"
  local workdir="${WORK_BASE}/${arch}"
  local arch_result_dir="${RESULT_ROOT}/${arch}"
  local console_log="${arch_result_dir}/console.log"
  local build_target
  local user_board
  local build_blk_mode
  local inner_cmd
  local docker_args

  if [[ "${arch}" == "rv64" ]]; then
    build_target="rv64-kernel-build-only"
    user_board="rvqemu"
    build_blk_mode="${TEST_BLK_MODE_RV:-virt}"
  else
    build_target="la64-kernel-build-only"
    user_board="laqemu"
    build_blk_mode="${TEST_BLK_MODE_LA:-virt_pci}"
  fi

  if [[ -n "${TEST_BLK_MODE:-}" ]]; then
    build_blk_mode="${TEST_BLK_MODE}"
  fi

  if [[ "${PARALLEL_BUILD}" == "1" ]]; then
    inner_cmd="make -C user rust-user BOARD=${user_board} MODE=${MODE} && make -C os ${build_target} BLK_MODE=${build_blk_mode} MODE=${MODE} && bash run_test.sh"
  else
    inner_cmd="bash run_test.sh"
  fi

  mkdir -p "${arch_result_dir}"
  docker rm -f "${container_name}" >/dev/null 2>&1 || true

  docker_args=(
    run --rm
    --name "${container_name}"
    --privileged
    --network host
    -v "${workdir}:/app"
    -w /app
    -e "MODE=${MODE}"
    -e "TEST_ARCH=${arch}"
    -e "GROUP_TIMEOUT_SEC=${GROUP_TIMEOUT_SEC}"
    -e "TEST_GROUPS=${TEST_GROUPS:-}"
    -e "TEST_BLK_MODE=${TEST_BLK_MODE:-}"
    -e "TEST_BLK_MODE_RV=${TEST_BLK_MODE_RV:-}"
    -e "TEST_BLK_MODE_LA=${TEST_BLK_MODE_LA:-}"
  )

  if [[ "${PARALLEL_IMAGE_MODE}" == "bind" ]]; then
    docker_args+=(-v "${REPO_ROOT}:/repo")
  fi

  if [[ -n "${LOG:-}" ]]; then
    docker_args+=(-e "LOG=${LOG}")
  fi
  if [[ -n "${RV_TOOLCHAIN:-}" ]]; then
    docker_args+=(-e "RV_TOOLCHAIN=${RV_TOOLCHAIN}")
  fi
  if [[ -n "${LA_TOOLCHAIN:-}" ]]; then
    docker_args+=(-e "LA_TOOLCHAIN=${LA_TOOLCHAIN}")
  fi
  if [[ -e /dev/loop-control ]]; then
    docker_args+=(--device /dev/loop-control:/dev/loop-control)
  fi
  if [[ -e /dev/loop0 ]]; then
    docker_args+=(--device /dev/loop0:/dev/loop0)
  fi

  docker_args+=("${DOCKER_IMAGE}" bash -lc "${inner_cmd}")

  echo "[parallel] start arch=${arch} container=${container_name}"
  set +e
  docker "${docker_args[@]}" 2>&1 \
    | awk -v prefix="[${arch}] " '{ print prefix $0; fflush(); }' \
    | tee "${console_log}"
  local rc=${PIPESTATUS[0]}
  set -e

  if [[ -d "${workdir}/testresult" ]]; then
    rsync -a "${workdir}/testresult/" "${arch_result_dir}/testresult/"
  fi

  echo "[parallel] done arch=${arch} exit=${rc} log=${console_log}"
  return "${rc}"
}

require_command docker
require_command rsync
require_command awk

case "${PARALLEL_IMAGE_MODE}" in
  bind|copy) ;;
  *)
    echo "[parallel] unsupported PARALLEL_IMAGE_MODE=${PARALLEL_IMAGE_MODE}, expected bind or copy"
    exit 1
    ;;
esac

if [[ "${WORK_BASE}" == "${REPO_ROOT}" ]]; then
  echo "[parallel] PARALLEL_WORKDIR must not be the repository root"
  exit 1
fi

mkdir -p "${WORK_BASE}" "${RESULT_ROOT}"

echo "[parallel] image=${DOCKER_IMAGE}"
echo "[parallel] result_dir=${RESULT_ROOT}"
echo "[parallel] build_before_test=${PARALLEL_BUILD}"
echo "[parallel] image_mode=${PARALLEL_IMAGE_MODE}"

prepare_arch_workdir rv64
prepare_arch_workdir la64

docker_run_arch rv64 "${container_names[0]}" &
rv_pid=$!
docker_run_arch la64 "${container_names[1]}" &
la_pid=$!

rv_rc=0
la_rc=0
wait "${rv_pid}" || rv_rc=$?
wait "${la_pid}" || la_rc=$?

echo "=== DOCKER PARALLEL SUMMARY ==="
echo "rv64_exit=${rv_rc}"
echo "la64_exit=${la_rc}"
echo "result_dir=${RESULT_ROOT}"

if [[ "${rv_rc}" -ne 0 || "${la_rc}" -ne 0 ]]; then
  exit 1
fi
