#!/bin/sh
# Executed only in the already-local compiler DONOR build stage. Nothing compiles.
set -eu
mkdir -p /runtime/bin /runtime/usr/bin /runtime/etc /runtime/tmp /runtime/app /runtime/usr/local/bin
chmod 1777 /runtime/tmp
copy_file() {
    test -f "$1"
    cp -L --parents "$1" /runtime
}
copy_library() {
    case "$1" in /lib/*|/lib64/*|/usr/lib/*) ;; *) printf 'unexpected non-system ELF dependency: %s\n' "$1" >&2; exit 1 ;; esac
    resolved=$(readlink -f "$1")
    case "$resolved" in /lib/*|/lib64/*|/usr/lib/*) ;; *) printf 'escaped system ELF dependency: %s\n' "$resolved" >&2; exit 1 ;; esac
    copy_file "$1"
}
copy_elf() {
    copy_file "$1"
    report=$(ldd "$1" 2>&1) || { printf '%s\n' "$report" >&2; exit 1; }
    if printf '%s\n' "$report" | grep -q 'not found'; then exit 1; fi
    printf '%s\n' "$report" | awk '{ for(i=1;i<=NF;i++) if ($i ~ /^\//) print $i }' | while IFS= read -r library; do copy_library "$library"; done
}
# Small fixed runtime surface. No compiler, Cargo, Rustup, registry or donor app.
for program in /bin/sh /usr/bin/curl /usr/bin/openssl /usr/bin/mkdir /usr/bin/chmod /usr/bin/test /usr/bin/sha256sum /usr/bin/cat /usr/bin/id /usr/bin/ls /usr/bin/pdftotext /usr/bin/pdfinfo /usr/bin/pdftoppm /usr/bin/pdftocairo /usr/bin/pdfimages /usr/bin/pdftohtml /usr/bin/pdftops /usr/bin/pdfseparate /usr/bin/pdfunite; do
    copy_elf "$program"
done
# Validate selected app dependencies against this exact compatible donor too.
for executable in /payload/bin/*; do
    report=$(ldd "$executable" 2>&1) || { printf '%s\n' "$report" >&2; exit 1; }
    if printf '%s\n' "$report" | grep -q 'not found'; then exit 1; fi
    printf '%s\n' "$report" | awk '{ for(i=1;i<=NF;i++) if ($i ~ /^\//) print $i }' | while IFS= read -r library; do copy_library "$library"; done
done
for directory in /etc/ssl/certs /etc/fonts /usr/share/poppler /usr/share/fontconfig /usr/share/fonts; do
    if test -d "$directory"; then mkdir -p "/runtime$(dirname "$directory")"; cp -aL "$directory" "/runtime$directory"; fi
done
# Public OpenSSL config/default CA links are required by credential generation.
# Never copy /etc/ssl/private from the donor.
copy_file /etc/ssl/openssl.cnf
mkdir -p /runtime/usr/lib/ssl
ln -s /etc/ssl/openssl.cnf /runtime/usr/lib/ssl/openssl.cnf
ln -s /etc/ssl/certs /runtime/usr/lib/ssl/certs
ln -s /etc/ssl/certs/ca-certificates.crt /runtime/usr/lib/ssl/cert.pem
# OpenSSL provider modules can be dlopen'd, outside ldd's graph.
for directory in /usr/lib/*/ossl-modules; do
    if test -d "$directory"; then mkdir -p "/runtime$(dirname "$directory")"; cp -aL "$directory" "/runtime$directory"; fi
done
printf 'gateway:x:1000:1000:Gateway:/app:/bin/sh\n' > /runtime/etc/passwd
printf 'gateway:x:1000:\n' > /runtime/etc/group
printf 'hosts: files dns\n' > /runtime/etc/nsswitch.conf
mkdir -p /runtime/app/data/images /runtime/app/data/files /runtime/app/logs /runtime/app/uploads /runtime/app/data/pdf-scratch /runtime/app/config /runtime/app/agent-memory /runtime/app/deploy/release /runtime/run/qdrant
chown -R 1000:1000 /runtime/app
chmod 700 /runtime/app/agent-memory
# Verify exact app bytes before Docker copies them into the final stage.
cd /payload
sha256sum --check --strict binaries.sha256
