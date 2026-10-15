#!/usr/bin/env python3
"""Offline, read-only source-candidate verification; never grants release admission."""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import subprocess
import sys


class InvalidSource(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise InvalidSource(message)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def safe_path(root, relative):
    path = PurePosixPath(relative)
    require(not path.is_absolute() and '..' not in path.parts and str(path) == relative,
            f'unsafe relative path: {relative}')
    result = root / relative
    require(result.resolve().is_relative_to(root.resolve()), f'escaping path: {relative}')
    require(not result.is_symlink(), f'symlink forbidden: {relative}')
    return result


def verify_payload(package, unit):
    require(unit['schema'] == 'ai-platform/runtime-source-unit/v1', 'unsupported source schema')
    for key in ('candidate_start_allowed', 'generated_release_identity', 'release_lock_refresh_ready'):
        require(unit.get(key) is False, f'source-only safety flag changed: {key}')
    paths = [r['path'] for r in unit['records']]
    require(len(paths) == len(set(paths)) == unit['source_file_count'], 'duplicate/count mismatch')
    payload = package / 'payload/kernel-rs'
    actual = {str(p.relative_to(payload)) for p in payload.rglob('*') if p.is_file() or p.is_symlink()}
    require(actual == set(paths), 'undeclared or missing payload files')
    for record in unit['records']:
        p = safe_path(payload, record['path'])
        require(digest(p) == record['proposed_sha256'], f'payload digest mismatch: {record["path"]}')
        mode = '100755' if p.stat().st_mode & 0o111 else '100644'
        require(mode == record['file_mode'], f'payload executable mode mismatch: {record["path"]}')
    require(digest(payload / 'Cargo.lock') == unit['resolved_lock_sha256'], 'resolved lock mismatch')
    require(digest(package / 'live-preimages.json') == unit['live_preimages_sha256'], 'preimages changed')
    for relative, expected in unit['evidence'].items():
        require(digest(safe_path(package, relative)) == expected, f'evidence changed: {relative}')
    candidate = json.loads((package / 'evidence/candidate-source.json').read_text())
    require(digest(package / 'evidence/candidate-source.json') == unit['input_candidate_manifest_sha256'],
            'candidate manifest identity mismatch')
    require(candidate['target'] == unit['target'] and candidate['target_tree'] == unit['target_tree'],
            'candidate target mismatch')
    source_records = {r['path']: r for r in unit['records'] if r['path'] != 'Cargo.lock'}
    require(source_records == {r['path']: r for r in candidate['records']}, 'candidate record mismatch')
    receipt = json.loads((package / 'evidence/build-receipt.json').read_text())
    require(receipt['resolved_lock_sha256'] == unit['resolved_lock_sha256'], 'compiler lock mismatch')
    require(receipt['source_identity']['candidate_source_sha256'] == unit['input_candidate_manifest_sha256'],
            'compiler source identity mismatch')
    require(receipt['target_commit'] == unit['target'], 'compiler target mismatch')
    for filename, operation, crate in (
        ('worker-check.json', 'check', 'ai-platform-capability-worker'),
        ('runtime-check-retry.json', 'check', 'ai-platform-agent-runtime'),
        ('contract-tests.json', 'test', 'ai-platform-capability-contract'),
    ):
        result = json.loads((package / 'evidence' / filename).read_text())
        command = result['command']
        require(result['exit_code'] == 0 and result.get('stop_reason') is None,
                f'unsuccessful evidence: {filename}')
        require(command[1:] == ['+1.95.0', operation, '--locked', '-p', crate] +
                (['--all-targets'] if operation == 'check' else []), f'wrong check command: {filename}')
    require(receipt['contract_unit_tests']['passed'] == 13 and receipt['contract_unit_tests']['failed'] == 0,
            'contract test scope mismatch')
    metadata = json.loads((package / 'evidence/resolved-metadata.json').read_text())
    names = {p['name'] for p in metadata['packages'] if p['id'] in metadata['workspace_members']}
    require({'ai-platform-agent-runtime', 'ai-platform-capability-worker',
             'ai-platform-capability-contract', 'ai-platform-office'} <= names,
            'resolved metadata lacks platform workspace crates')


def git(upstream, *args):
    return subprocess.check_output(['git', '-C', str(upstream), *args])


def verify_upstream(package, unit, upstream, composed=None):
    require(git(upstream, 'rev-parse', unit['target'] + '^{tree}').decode().strip() == unit['target_tree'],
            'upstream target tree mismatch')
    entries = {}
    for row in git(upstream, 'ls-tree', '-rz', '--full-tree', unit['target']).split(b'\0'):
        if not row:
            continue
        info, path = row.split(b'\t', 1)
        mode, kind, blob = info.decode().split()
        require(kind == 'blob', f'unsupported upstream gitlink/object kind: {path.decode()} ({kind}); explicit policy required')
        entries[path.decode()] = (mode, blob)
    for record in unit['records']:
        entry = entries.get('codex-rs/' + record['path'])
        require((entry[1] if entry else None) == record['target_upstream_blob'],
                f'upstream blob mismatch: {record["path"]}')
        if entry:
            data = git(upstream, 'cat-file', 'blob', entry[1])
            require(hashlib.sha256(data).hexdigest() == record['target_preimage_sha256'],
                    f'upstream preimage mismatch: {record["path"]}')
    if composed is not None:
        return verify_composed(unit, entries, composed)
    return 0


def composed_inventory(root):
    """Inventory every entry except root .git metadata; no source/build ignores."""
    paths = set()
    def fail_walk(error):
        raise error
    for parent, dirs, files in os.walk(root, followlinks=False, onerror=fail_walk):
        parent = Path(parent)
        if parent == root:
            dirs[:] = [name for name in dirs if name != '.git']
            files = [name for name in files if name != '.git']
        # os.walk lists symlinked directories in dirs; retain their identity but
        # never follow them into an undeclared source tree.
        for name in list(dirs):
            path = parent / name
            if path.is_symlink():
                paths.add(str(path.relative_to(root)))
                dirs.remove(name)
        for name in files:
            path = parent / name
            require(path.is_file() or path.is_symlink(), f'non-regular composed entry: {path.relative_to(root)}')
            paths.add(str(path.relative_to(root)))
    return paths


def verify_composed(unit, entries, composed):
    overlay = {'codex-rs/' + r['path']: r for r in unit['records']}
    expected = set(entries) | set(overlay)
    actual = composed_inventory(composed)
    require(actual == expected,
            f'composed inventory mismatch; undeclared={sorted(actual - expected)}; missing={sorted(expected - actual)}')
    for relative in sorted(expected):
        if relative not in overlay and entries[relative][0] == '120000':
            p = composed / relative
            require(p.parent.resolve().is_relative_to(composed.resolve()) and p.is_symlink(),
                    f'upstream symlink missing or unsafe parent: {relative}')
        else:
            p = safe_path(composed, relative)
            require(p.is_file(), f'composed file missing: {relative}')
        expected_mode = overlay[relative]['file_mode'] if relative in overlay else entries[relative][0]
        actual_mode = '120000' if p.is_symlink() else ('100755' if p.stat().st_mode & 0o111 else '100644')
        require(actual_mode == expected_mode, f'composed executable/file mode mismatch: {relative}')
        if relative in overlay:
            require(digest(p) == overlay[relative]['proposed_sha256'], f'composed overlay mismatch: {relative}')
        else:
            data = os.readlink(p).encode() if p.is_symlink() else p.read_bytes()
            blob = hashlib.sha1(b'blob ' + str(len(data)).encode() + b'\0' + data).hexdigest()
            require(blob == entries[relative][1], f'composed upstream mismatch: {relative}')
    return len(set(entries) | set(overlay))


def verify_live_preimages(package, repo):
    preimages = json.loads((package / 'live-preimages.json').read_text())
    for record in preimages['records']:
        p = safe_path(repo, record['path'])
        actual = digest(p) if p.is_file() else None
        require(actual == record['sha256'], f'live preimage changed; reconcile, never overwrite: {record["path"]}')
        if p.exists():
            require(oct(p.stat().st_mode & 0o777) == record['mode'], f'live mode changed: {record["path"]}')
    prefix = 'rust/agent-runtime-overlay/kernel-rs/'
    expected = {r['path'] for r in preimages['records'] if r['path'].startswith(prefix) and r['sha256'] is not None}
    actual = {str(p.relative_to(repo)) for p in (repo / prefix).rglob('*') if p.is_file() or p.is_symlink()}
    require(expected == actual, 'live overlay inventory changed; reconcile additions/deletions')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--package', type=Path, default=Path(__file__).resolve().parent)
    parser.add_argument('--upstream', type=Path, required=True, help='Existing Git checkout; never fetches')
    parser.add_argument('--repo-root', type=Path, required=True)
    parser.add_argument('--composed-source', type=Path)
    args = parser.parse_args()
    try:
        unit = json.loads((args.package / 'source-unit.json').read_text())
        verify_payload(args.package, unit)
        compared = verify_upstream(args.package, unit, args.upstream, args.composed_source)
        verify_live_preimages(args.package, args.repo_root)
        print(json.dumps({'source_validation': 'passed', 'source_files': unit['source_file_count'],
                          'composed_files_compared': compared, 'release_admission': False,
                          'live_pin_changed': False, 'candidate_start_allowed': False}, indent=2))
        return 0
    except (InvalidSource, OSError, ValueError, KeyError, subprocess.CalledProcessError) as exc:
        print(json.dumps({'source_validation': 'failed', 'error': str(exc), 'release_admission': False}), file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
