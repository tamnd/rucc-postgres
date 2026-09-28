#!/usr/bin/env bash
# Provision a machine for row LA64, Linux AArch64 with gcc-16 as the reference.
#
#   sudo provision/la64.sh [--toolchain-ppa] [--core-pattern]
#
# See linux.sh for what it does.
set -euo pipefail
RPG_EXPECT_ARCH=aarch64 RPG_ROW=LA64 exec "$(dirname "$0")/linux.sh" "$@"
