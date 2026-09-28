#!/usr/bin/env bash
# Provision a machine for row L64, Linux x86-64 with gcc-16 as the reference.
#
#   sudo provision/l64.sh [--toolchain-ppa] [--core-pattern]
#
# See linux.sh for what it does.
set -euo pipefail
RPG_EXPECT_ARCH=x86_64 RPG_ROW=L64 exec "$(dirname "$0")/linux.sh" "$@"
