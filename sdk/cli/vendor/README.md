# Native CLI artifacts

Release packaging places the lock-pinned composed `codex` binary at:

```text
vendor/<node-platform>-<node-arch>/codex[.exe]
```

The binaries are generated release artifacts and are not committed. Each
target also contains `artifact.json` with the upstream SHA, overlay SHA-256,
target, binary name, and binary SHA-256. The launcher verifies this receipt
against the generated `vendor/source.json` before executing packaged artifacts.
Publishing derives `source.json` from the repository source receipt, overlay
manifest, and lock. The launcher build independently records the same canonical
identity in `dist/native-source.json`; startup requires the dist identity,
vendor identity, artifact receipt, and binary digest to agree. Publishing fails
if any selected target is missing or mismatched.
