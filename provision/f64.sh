#!/bin/sh
# Provision FreeBSD amd64 for row F64. The machine is the RR image of tamnd/rucc-bsd: a FreeBSD
# whose world and kernel rucc built, with the rucc package of tamnd/rucc-bsd installed as cc.
#
#   provision/f64.sh
#
# Run it as root. What it does, and nothing else:
#
#   1. installs with pkg the packages that the minimal configuration under meson and the TAP tests
#      need, and rust, so that rpg can be built on the machine. The image has no bash, so this is
#      a sh script.
#   2. creates the unprivileged user pg, because initdb refuses to run as root.
#
# It changes no system setting.

set -eu

if [ "$(id -u)" != 0 ]; then
	echo "run this as root, it installs packages and creates a user" >&2
	exit 1
fi
if [ "$(uname -s)" != FreeBSD ] || [ "$(uname -m)" != amd64 ]; then
	echo "row F64 is FreeBSD on amd64, and this is $(uname -s) $(uname -m)" >&2
	exit 1
fi

# bison, flex and perl are needed to build from a release tarball, because meson regenerates some
# files. p5-IPC-Run is IPC::Run, which every TAP test uses. GNU make is for the autoconf leg,
# because the base make of FreeBSD is not GNU make.
packages="bison bzip2 ca_root_nss curl flex git gmake meson ninja p5-IPC-Run perl5 pkgconf python3 rsync rust"
echo "installing: $packages"
# shellcheck disable=SC2086 # one word per package
env ASSUME_ALWAYS_YES=yes IGNORE_OSVERSION=yes pkg install -y $packages

if ! id pg > /dev/null 2>&1; then
	pw useradd pg -m -s /bin/sh -c "rpg suites"
fi
cc --version | head -2
perl -MIPC::Run -e 'print "IPC::Run $IPC::Run::VERSION\n"'
echo "row F64 is provisioned"
