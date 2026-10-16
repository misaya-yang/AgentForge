# Optional staged secret guard

`pre-commit` is a native Bash/Git/grep replacement for the existing known-secret
check. It reads **staged index contents**, rather than unstaged working-tree files,
for added, copied and modified paths. NUL-delimited filenames are supported.

The guard rejects the three existing known-secret prefix patterns without printing
the matching contents. It reports only the staged path. The retained historical
exception is `docs/plans/six_month/bug/*`. Deleted paths are not scanned; `grep -I`
excludes binary content. Inspection errors fail the hook. This is a narrow legacy
pattern guard, not a comprehensive credential detector, history scan or guarantee
that an archive is safe. Never stage real credentials, including in excluded paths.

## Opt-in installation

The hook is checked in but **is not installed automatically**. From the repository
root, inspect an existing hook configuration before choosing whether to replace it:

```sh
git config --show-origin --get core.hooksPath
```

If you choose to use this repository's hooks, install for this repository only:

```sh
git config --local core.hooksPath .githooks
```

This changes which hooks Git runs for this checkout. It does not merge an existing
hook directory; preserve and deliberately restore any previous local setting.
If there was no previous local setting, undo this opt-in with:

```sh
git config --local --unset core.hooksPath
```

The script requires Bash, Git, grep and mktemp and must retain its executable bit.
Installing the hook is separate from running tests, configuring global Git hooks,
committing, or publishing. None of those actions is performed by this README.
