#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd -- "${SCRIPT_DIR}/.." && pwd)
ROLE_TOOL="${REPO_ROOT}/scripts/image_roles.py"

ARCH=${ARCH:-la64}
BLK_MODE=${BLK_MODE:-mem}
CONF_FILE=${CONF_FILE:-"${REPO_ROOT}/os_test.conf"}
IMAGE_PATH=${IMAGE_PATH:-}
DERIVED_IMAGE_PATH=${DERIVED_IMAGE_PATH:-}
AUTO_REBUILD_MEM=${AUTO_REBUILD_MEM:-1}
MODE=${MODE:-release}
LOG=${LOG:-error}

usage() {
    cat <<'EOF'
Inject /os_test.conf into the target image.

Environment variables:
  ARCH              la64|rv64 (default: la64)
  BLK_MODE          mem|virt|virt_pci|sata (default: mem)
  CONF_FILE         path to config file (default: ../os_test.conf)
   IMAGE_PATH         override a mutable target image path
   DERIVED_IMAGE_PATH named derived image for virt/virt_pci injection
  AUTO_REBUILD_MEM  1 to rebuild kernel automatically for mem mode (default: 1)
  MODE              release|debug for auto rebuild (default: release)
  LOG               log level for auto rebuild (default: error)

Examples:
   ARCH=la64 BLK_MODE=mem CONF_FILE=os_test.conf scripts/inject_os_test_conf.sh
   ARCH=rv64 BLK_MODE=virt CONF_FILE=os_test.conf scripts/inject_os_test_conf.sh
   DERIVED_IMAGE_PATH=build/development/la64/sdcard-la-derived.img CONF_FILE=os_test.conf scripts/inject_os_test_conf.sh
EOF
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    usage
    exit 0
fi

case "${ARCH}" in
    la|la64) ARCH="la64" ;;
    rv|rv64) ARCH="rv64" ;;
    *)
        echo "ERROR: unsupported ARCH='${ARCH}', expected la64 or rv64"
        exit 1
        ;;
esac

if [[ ! -f "${CONF_FILE}" ]]; then
    echo "ERROR: CONF_FILE not found: ${CONF_FILE}"
    exit 1
fi

if [[ -z "${IMAGE_PATH}" ]]; then
    case "${BLK_MODE}" in
        mem)
            if [[ "${ARCH}" == "la64" ]]; then
                IMAGE_PATH="${REPO_ROOT}/fs-img-dir/rootfs-ubifs-ze.img"
            else
                IMAGE_PATH="${REPO_ROOT}/fs-img-dir/rootfs-rv.img"
            fi
            ;;
        *)
            source_image=$(python3 "${ROLE_TOOL}" official --repo-root "${REPO_ROOT}" --arch "${ARCH}")
            if [[ -z "${DERIVED_IMAGE_PATH}" ]]; then
                DERIVED_IMAGE_PATH=$(python3 "${ROLE_TOOL}" derived --repo-root "${REPO_ROOT}" --arch "${ARCH}")
            fi
            source_image=$(python3 "${ROLE_TOOL}" validate-official --repo-root "${REPO_ROOT}" --arch "${ARCH}" --path "${source_image}")
            DERIVED_IMAGE_PATH=$(python3 "${ROLE_TOOL}" validate-derived --repo-root "${REPO_ROOT}" --arch "${ARCH}" --path "${DERIVED_IMAGE_PATH}")
            DERIVED_IMAGE_PATH=$(python3 "${ROLE_TOOL}" validate-mutable --repo-root "${REPO_ROOT}" --arch "${ARCH}" --path "${DERIVED_IMAGE_PATH}")
            mkdir -p "$(dirname -- "${DERIVED_IMAGE_PATH}")"
            cp --reflink=auto "${source_image}" "${DERIVED_IMAGE_PATH}"
            IMAGE_PATH="${DERIVED_IMAGE_PATH}"
            ;;
    esac
fi

if [[ "${BLK_MODE}" != "mem" ]]; then
    IMAGE_PATH=$(python3 "${ROLE_TOOL}" validate-derived --repo-root "${REPO_ROOT}" --arch "${ARCH}" --path "${IMAGE_PATH}")
fi
IMAGE_PATH=$(python3 "${ROLE_TOOL}" validate-mutable --repo-root "${REPO_ROOT}" --arch "${ARCH}" --path "${IMAGE_PATH}")

if [[ ! -f "${IMAGE_PATH}" ]]; then
    echo "ERROR: target image not found: ${IMAGE_PATH}"
    exit 1
fi

if ! command -v debugfs >/dev/null 2>&1; then
    echo "ERROR: debugfs not found in PATH"
    exit 1
fi

CONF_FILE_ABS=$(realpath -e -- "${CONF_FILE}")
IMAGE_PATH_ABS=$(realpath -e -- "${IMAGE_PATH}")

echo "[conf-inject] arch=${ARCH} blk_mode=${BLK_MODE}"
echo "[conf-inject] conf=${CONF_FILE_ABS}"
echo "[conf-inject] image=${IMAGE_PATH_ABS}"

cmd_file=$(mktemp)
trap 'rm -f "${cmd_file}"' EXIT

cat >"${cmd_file}" <<EOF
rm /os_test.conf
write ${CONF_FILE_ABS} /os_test.conf
stat /os_test.conf
EOF

# Fix stale ext4 metadata checksums before opening with debugfs.
# lwext4 does not update metadata_csum on bitmap writes, so
# debugfs rejects the image on first open.
e2fsck -fy "${IMAGE_PATH_ABS}" 2>&1 || true

debugfs_output=$(debugfs -w -f "${cmd_file}" "${IMAGE_PATH_ABS}" 2>&1) || {
    echo "${debugfs_output}"
    echo "ERROR: debugfs failed while updating ${IMAGE_PATH_ABS}"
    exit 1
}
echo "${debugfs_output}"

if echo "${debugfs_output}" | grep -E "Permission denied while trying to open|Filesystem not open|No such file or directory while trying to open" >/dev/null; then
    echo "ERROR: debugfs could not access image ${IMAGE_PATH_ABS}"
    exit 1
fi

echo "[conf-inject] injected /os_test.conf into ${IMAGE_PATH_ABS}"

if [[ "${BLK_MODE}" == "mem" && "${AUTO_REBUILD_MEM}" == "1" ]]; then
    echo "[conf-inject] mem mode detected, rebuilding kernel so embedded rootfs takes effect..."
    pushd "${REPO_ROOT}/os" >/dev/null
    if [[ "${ARCH}" == "la64" ]]; then
# la64.mk has no "build" target; rebuild kernel and refresh ../kernel-la explicitly.
make -f make/la64.mk kernel mv BLK_MODE=mem MODE="${MODE}" LOG="${LOG}"
    else
        make -f make/rv64.mk build BLK_MODE=mem MODE="${MODE}" LOG="${LOG}"
    fi
    popd >/dev/null
fi

echo "[conf-inject] done"
