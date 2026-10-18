#!/bin/sh
# Executed against immutable existing slim Runtime donor. No apt/fetch/compiler.
set -eu
role=${1:?role required}
case "$role" in runtime|worker) ;; *) exit 2;; esac
test ! -e /rootfs
mkdir -p /rootfs/bin /rootfs/etc /rootfs/usr/local/bin /rootfs/tmp /rootfs/workspace /rootfs/run/agentforge-sandbox /rootfs/var/lib/ai-platform-agent-runtime /rootfs/runtime-home
chmod 1777 /rootfs/tmp
copy_file() { test -f "$1"; cp -L --parents "$1" /rootfs; }
copy_elf() {
 copy_file "$1"
 # Several canonical tools (gunzip and Git helpers) are POSIX scripts.
 if test "$(od -An -tx1 -N4 "$1" | tr -d ' \n')" != 7f454c46; then return; fi
 report=$(ldd "$1" 2>&1) || { printf '%s\n' "$report" >&2; exit 1; }
 if printf '%s\n' "$report" | grep -q 'not found'; then exit 1; fi
 printf '%s\n' "$report" | awk '{for(i=1;i<=NF;i++) if($i ~ /^\//) print $i}' | while IFS= read -r lib; do
  case "$lib" in /lib/*|/lib64/*|/usr/lib/*) ;; *) exit 1;; esac
  resolved=$(readlink -f "$lib")
  case "$resolved" in /lib/*|/lib64/*|/usr/lib/*) ;; *) exit 1;; esac
  copy_file "$lib"
 done
}
for file in /bin/sh /usr/bin/curl /usr/bin/openssl /usr/bin/mkdir /usr/bin/find /usr/bin/id /usr/bin/test /usr/bin/cat /usr/bin/sha256sum; do copy_elf "$file"; done
if test "$role" = runtime; then
 # Fixed shell/tool surface from the canonical runtime dependency contract.
 copy_elf /bin/bash
 for name in perl bash git rg bwrap unshare setsid env stat timeout rm chmod chown install mktemp ln ls cp mv readlink head tail wc sort uniq cut tr touch basename dirname realpath tee date sleep xargs grep sed awk diff cmp patch tar gzip gunzip bzip2 xz unzip ssh ssh-keygen scp ps pgrep; do copy_elf "$(command -v "$name")"; done
 # Git transports are dlopen/exec dependencies, beyond the binary's ldd graph.
 test -d /usr/lib/git-core
 mkdir -p /rootfs/usr/lib/git-core
 for file in /usr/lib/git-core/*; do
  if test -f "$file"; then
   if head -c 4 "$file" | grep -q 'ELF'; then copy_elf "$file"; else copy_file "$file"; fi
  fi
 done
 for directory in /usr/lib/*/perl-base /usr/lib/*/perl /usr/share/perl /usr/share/perl5; do
  if test -d "$directory"; then mkdir -p "/rootfs$(dirname "$directory")"; cp -aL "$directory" "/rootfs$directory"; fi
 done
 if test -d /usr/share/git-core; then mkdir -p /rootfs/usr/share; cp -aL /usr/share/git-core /rootfs/usr/share/; fi
fi
# Resolve freshly built application's actual ELF requirements against this donor.
mkdir -p /rootfs/usr/share/agentforge/dev
: > /rootfs/usr/share/agentforge/dev/elf-linkage.txt
for executable in /payload/bin/*; do
 printf '%s\n' "$executable" >> /rootfs/usr/share/agentforge/dev/elf-linkage.txt
 report=$(ldd "$executable" 2>&1) || { printf '%s\n' "$report" >&2; exit 1; }
 printf '%s\n' "$report" >> /rootfs/usr/share/agentforge/dev/elf-linkage.txt
 if printf '%s\n' "$report" | grep -q 'not found'; then exit 1; fi
 printf '%s\n' "$report" | awk '{for(i=1;i<=NF;i++) if($i ~ /^\//) print $i}' | while IFS= read -r lib; do
  case "$lib" in /lib/*|/lib64/*|/usr/lib/*) copy_file "$lib";; *) exit 1;; esac
 done
done
mkdir -p /rootfs/etc/ssl /rootfs/usr/lib/ssl
cp -aL /etc/ssl/certs /rootfs/etc/ssl/
copy_file /etc/ssl/openssl.cnf
ln -s /etc/ssl/openssl.cnf /rootfs/usr/lib/ssl/openssl.cnf
ln -s /etc/ssl/certs /rootfs/usr/lib/ssl/certs
ln -s /etc/ssl/certs/ca-certificates.crt /rootfs/usr/lib/ssl/cert.pem
for dir in /usr/lib/*/ossl-modules; do if test -d "$dir"; then mkdir -p "/rootfs$(dirname "$dir")"; cp -aL "$dir" "/rootfs$dir"; fi; done
printf 'agent-runtime:x:10001:10001:AgentForge:/var/lib/ai-platform-agent-runtime:/bin/bash\n' > /rootfs/etc/passwd
printf 'agent-runtime:x:10001:\n' > /rootfs/etc/group
printf 'hosts: files dns\n' > /rootfs/etc/nsswitch.conf
chown 10001:10001 /rootfs/workspace /rootfs/run/agentforge-sandbox /rootfs/var/lib/ai-platform-agent-runtime /rootfs/runtime-home
chmod 0700 /rootfs/workspace /rootfs/run/agentforge-sandbox /rootfs/var/lib/ai-platform-agent-runtime /rootfs/runtime-home
# Never include donor applications, old identity labels, toolchains or private PKI.
test ! -e /rootfs/etc/ssl/private
