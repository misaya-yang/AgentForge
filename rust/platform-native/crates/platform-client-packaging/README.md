# Portable native client packaging

`platform-client-packaging` builds independently of the Unix-only server operations tool.
It reuses the canonical `platform-tools/src/packaging.rs` implementation and accepts:

- `package-desktop --matrix`
- `package-desktop --target NAME --build-env`
- `package-desktop --target NAME --binary PATH --output DIRECTORY`
- `package-platformctl --matrix`
- `package-platformctl --target NAME --binary PATH --output DIRECTORY`

Each command also accepts `--repo-root`. Archives, provenance/material checks,
architecture checks and offline executable checks remain the shared contract.
Inputs must be committed and clean. Packaging is not publication or GUI acceptance.

Unix smoke checks use process groups and nonblocking pipes. Windows smoke checks
use a Job Object and `PeekNamedPipe`, retaining the same deadline/output bounds.
Windows does not apply Unix filesystem chmod; archive Unix permission metadata is
still generated and verified. Paths used as material identifiers always use `/`.

Win32 interface references:
- https://learn.microsoft.com/en-us/windows/win32/api/jobapi2/nf-jobapi2-assignprocesstojobobject
- https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-peeknamedpipe
- https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects

No Windows build or client package execution is claimed by source compilation on Linux.
