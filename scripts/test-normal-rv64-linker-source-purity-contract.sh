#!/bin/sh

set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
exec sh "$root/scripts/test-rv64-standard-fdt-boot-contract.sh"
