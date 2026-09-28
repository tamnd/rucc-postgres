#!/usr/bin/env bash
# Provision an Ubuntu or Debian machine for the Linux rows. Called by l64.sh and la64.sh, which
# say which architecture they expect; run one of those rather than this.
#
# What it does, and nothing else:
#
#   1. installs the packages the minimal configuration and the TAP tests need, from the
#      distribution's own archive;
#   2. checks that the row's reference compiler, gcc-16, is on the path, and only adds the
#      ubuntu-toolchain-r/test PPA to get it when asked with --toolchain-ppa;
#   3. creates the unprivileged user `pg`, because initdb refuses to run as root;
#   4. only when asked with --core-pattern, points kernel.core_pattern at /var/tmp/cores so a
#      backend that crashes under a test leaves a core behind. This is a machine wide setting that
#      replaces apport's, which is why it is not the default.
#
# It installs no Rust. Build rpg with whatever cargo the machine has and copy the two binaries,
# rpg and rpg-cc, somewhere the pg user can run them.

set -euo pipefail

expect_arch=${RPG_EXPECT_ARCH:?run l64.sh or la64.sh rather than this script}
row=${RPG_ROW:?run l64.sh or la64.sh rather than this script}

toolchain_ppa=0
core_pattern=0
for arg in "$@"; do
	case $arg in
	--toolchain-ppa) toolchain_ppa=1 ;;
	--core-pattern) core_pattern=1 ;;
	-h | --help)
		sed -n '2,20p' "$0"
		exit 0
		;;
	*)
		echo "unknown option $arg" >&2
		exit 2
		;;
	esac
done

if [ "$(id -u)" != 0 ]; then
	echo "run this as root, it installs packages and creates a user" >&2
	exit 1
fi
arch=$(uname -m)
if [ "$arch" != "$expect_arch" ]; then
	echo "row $row is $expect_arch, but this machine is $arch" >&2
	exit 1
fi
if ! command -v apt-get >/dev/null; then
	echo "this script knows apt only; install the packages listed in it by hand" >&2
	exit 1
fi

# bison, flex and perl are needed to build from a release tarball as much as from git, because
# meson regenerates some files. libipc-run-perl is IPC::Run, which every TAP test uses. The
# minimal configuration turns off ICU, readline, zlib, OpenSSL and the rest, so no -dev library
# packages are needed.
packages=(
	bison
	bzip2
	ca-certificates
	curl
	flex
	git
	libipc-run-perl
	make
	meson
	ninja-build
	perl
	pkg-config
	python3
	rsync
	tar
)
echo "installing: ${packages[*]}"
export DEBIAN_FRONTEND=noninteractive
apt-get update -q
apt-get install -y -q --no-install-recommends "${packages[@]}"

if ! command -v gcc-16 >/dev/null; then
	if [ "$toolchain_ppa" = 1 ]; then
		apt-get install -y -q --no-install-recommends software-properties-common
		add-apt-repository -y ppa:ubuntu-toolchain-r/test
		apt-get update -q
		apt-get install -y -q --no-install-recommends gcc-16
	else
		echo "warning: gcc-16, the reference compiler of row $row, is not on the path." >&2
		echo "         rerun with --toolchain-ppa to add ppa:ubuntu-toolchain-r/test and install it." >&2
	fi
fi

if id pg >/dev/null 2>&1; then
	echo "user pg exists"
else
	useradd --create-home --shell /bin/bash --comment "rucc-postgres test runs" pg
	echo "created user pg"
fi

if [ "$core_pattern" = 1 ]; then
	install -d -m 1777 /var/tmp/cores
	echo "kernel.core_pattern = /var/tmp/cores/core.%e.%p" >/etc/sysctl.d/60-rucc-postgres-cores.conf
	sysctl -q -p /etc/sysctl.d/60-rucc-postgres-cores.conf
	echo "core_pattern is now $(cat /proc/sys/kernel/core_pattern)"
	echo "note: apport rewrites core_pattern when it starts; disable it (systemctl disable --now apport) to keep this across reboots"
else
	echo "core_pattern left as it was: $(cat /proc/sys/kernel/core_pattern)"
fi

perl -MIPC::Run -e 'print "IPC::Run $IPC::Run::VERSION\n"'
for tool in meson ninja bison flex; do
	printf '%s %s\n' "$tool" "$("$tool" --version 2>&1 | head -1)"
done
if command -v gcc-16 >/dev/null; then
	gcc-16 --version | head -1
fi
echo "row $row is provisioned"
