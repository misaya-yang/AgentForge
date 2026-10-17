#!/bin/sh
# Offline image contract. No database, provider, package download or service start.
set -eu
fail() { echo "ERROR: runtime-image: $*" >&2; exit 1; }
[ "$(id -u)" = 10001 ] && [ "$(id -g)" = 10001 ] || fail 'service must use uid/gid 10001'
. /etc/os-release
[ "$ID" = debian ] && [ "$VERSION_ID" = 13 ] || fail 'Debian 13 is required'
for directory in /workspace /var/lib/ai-platform-agent-runtime /var/lib/ai-platform-agent-runtime/runtime-home; do
    [ "$(stat -c '%u:%g' "$directory")" = 10001:10001 ] || fail "wrong owner: $directory"
    [ -w "$directory" ] && [ -x "$directory" ] || fail "unusable directory: $directory"
done
[ "${HOME:-}" = /var/lib/ai-platform-agent-runtime ] || fail 'wrong HOME'
[ "${SHELL:-}" = /bin/bash ] || fail 'wrong shell'
[ -s /etc/ssl/certs/ca-certificates.crt ] || fail 'missing TLS root certificates'
for command in sh bash git ssh curl rg bwrap awk grep sed find diff patch tar gzip bzip2 xz unzip ps timeout setsid unshare env mkdir cat stat ldd openssl; do
    command -v "$command" >/dev/null 2>&1 || fail "missing command: $command"
done
for command in rustc cargo rustup cc gcc g++ c++ clang clang++ ld make; do
    if command -v "$command" >/dev/null 2>&1; then fail "compiler/build tool present: $command"; fi
done
for directory in /usr/local/cargo /usr/local/rustup /root/.cargo /root/.rustup; do
    [ ! -e "$directory" ] || fail "toolchain tree present: $directory"
done
# gcc-N-base contains runtime metadata, not a compiler, and is allowed.
if dpkg-query -W -f='${db:Status-Abbrev} ${binary:Package}\n' | awk '
    $1 == "ii" && $2 ~ /^(rustc|cargo|rustup|gcc(-[0-9]+)?|g\+\+(-[0-9]+)?|clang(-[0-9]+)?|build-essential|llvm(-[0-9]+)?)(:[a-z0-9]+)?$/ {found=1}
    END {exit !found}'; then
    fail 'compiler package installed'
fi
scratch="$(mktemp -d /tmp/runtime-image-check.XXXXXX)"
trap 'rm -rf "$scratch"' EXIT HUP INT TERM
ldd /usr/local/bin/ai-platform-agent-runtime > "$scratch/ldd"
if grep -q 'not found' "$scratch/ldd"; then fail 'unresolved Runtime dynamic library'; fi
/usr/local/bin/ai-platform-agent-runtime --version >/dev/null
curl --version >/dev/null
ssh -V 2>/dev/null
bwrap --version >/dev/null
bash -c 'test "$((2 + 3))" = 5'
printf 'runtime-image-probe\n' > "$scratch/probe"
rg -q '^runtime-image-probe$' "$scratch/probe"
git -C "$scratch" init --quiet
git -C "$scratch" -c user.name=image-check -c user.email=image-check@invalid add probe
git -C "$scratch" -c user.name=image-check -c user.email=image-check@invalid commit --quiet -m image-check
[ -z "$(git -C "$scratch" status --porcelain)" ] || fail 'git command probe failed'
echo 'runtime-image-check: PASS (Debian13, nonroot, tools, ELF, no compiler)'
