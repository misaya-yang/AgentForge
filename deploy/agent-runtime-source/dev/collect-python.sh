#!/bin/sh
# Only an explicit already-local Python3.14 locked-package donor is admissible.
set -eu
cmp /proof/python-catalog.json /opt/agentforge/python-packages/catalog.json
OPENBLAS_NUM_THREADS=1 OMP_NUM_THREADS=1 python3 -I -S -B -c 'import sys,json,importlib.metadata as m; assert sys.version_info[:2]==(3,14); c=json.load(open("/proof/python-catalog.json")); sys.path[:0]=["/opt/agentforge/python-packages/"+p for p in c["packages"]]; import numpy,pandas,scipy,matplotlib,PIL,openpyxl; assert all(m.version(p)==v["version"] for p,v in c["packages"].items())'
# Reuse the same system library resolver; actual binaries must link on this donor.
/bin/sh /collect-rootfs.sh worker
mkdir -p /rootfs/usr/local/lib /rootfs/opt/agentforge /rootfs/var/lib/agentforge-sandbox/jobs
cp -aL /usr/local/lib/python3.14 /rootfs/usr/local/lib/
cp -aL /opt/agentforge/python-packages /rootfs/opt/agentforge/
# The donor must already have removed package installers/ensurepip/site-packages.
test ! -d /usr/local/lib/python3.14/ensurepip
for tool in pip pip3 uv cargo rustc; do if command -v "$tool"; then exit 1; fi; done
# All Python extension shared objects have extra ELF dependency closures.
find /usr/local/lib/python3.14 /opt/agentforge/python-packages -type f -name '*.so*' > /python-elf-files
printf '%s\n' /usr/local/bin/python3.14 /usr/bin/unshare >> /python-elf-files
while IFS= read -r file; do
 cp -L --parents "$file" /rootfs
 report=$(ldd "$file" 2>&1) || { printf '%s\n' "$report" >&2; exit 1; }
 if printf '%s\n' "$report" | grep -q 'not found'; then exit 1; fi
 printf '%s\n' "$report" | awk '{for(i=1;i<=NF;i++) if($i ~ /^\//) print $i}' | while IFS= read -r lib; do
  case "$lib" in /lib/*|/lib64/*|/usr/lib/*|/usr/local/lib/*|/opt/agentforge/python-packages/*) cp -L --parents "$lib" /rootfs;; *) exit 1;; esac
 done
done < /python-elf-files
ln -s python3.14 /rootfs/usr/local/bin/python3
chown -R 10001:10001 /rootfs/var/lib/agentforge-sandbox
chmod 0700 /rootfs/var/lib/agentforge-sandbox/jobs
