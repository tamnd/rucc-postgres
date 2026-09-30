#!/usr/bin/env bash
# Provision a Mac for row M64, macOS arm64 with Apple clang as the reference.
#
#   provision/m64.sh
#
# Run it as the user who will run the suites, not with sudo: Homebrew refuses root, and on macOS
# the suites run as that user. It needs Homebrew and the Xcode command line tools already there.
#
# What it does:
#
#   1. installs meson, ninja, bison, flex, GNU make, pkgconf and cpanminus with Homebrew. The bison
#      macOS ships is 2.3, too old for Postgres, and Homebrew's is keg only, so it has to go first
#      on PATH; the script prints the line that does that. GNU make 4 comes as gmake, which rpg
#      runs on macOS because the suites use make -O and the system's make 3.81 does not have it.
#   2. installs IPC::Run into ~/perl5 with cpanm, for the TAP tests, and prints the PERL5LIB line.
#
# It changes no system setting. Crash reports go to ~/Library/Logs/DiagnosticReports as usual.

set -euo pipefail

if [ "$(id -u)" = 0 ]; then
	echo "run this as the user who will run the suites, not as root" >&2
	exit 1
fi
if [ "$(uname -s)" != Darwin ] || [ "$(uname -m)" != arm64 ]; then
	echo "row M64 is macOS on arm64, and this is $(uname -s) $(uname -m)" >&2
	exit 1
fi
if ! command -v brew >/dev/null; then
	echo "Homebrew is needed; see https://brew.sh" >&2
	exit 1
fi
if ! xcode-select -p >/dev/null 2>&1; then
	echo "the Xcode command line tools are needed; run xcode-select --install" >&2
	exit 1
fi

brew install meson ninja bison flex make pkgconf cpanminus

prefix=$(brew --prefix)
cpanm --local-lib "$HOME/perl5" --notest IPC::Run
PERL5LIB="$HOME/perl5/lib/perl5${PERL5LIB:+:$PERL5LIB}" perl -MIPC::Run -e 'print "IPC::Run $IPC::Run::VERSION\n"'

clang --version | head -1
echo
echo "add these to the shell profile of this user:"
echo "  export PATH=\"$prefix/opt/bison/bin:$prefix/opt/flex/bin:\$PATH\""
echo "  export PERL5LIB=\"\$HOME/perl5/lib/perl5\${PERL5LIB:+:\$PERL5LIB}\""
echo "row M64 is provisioned"
