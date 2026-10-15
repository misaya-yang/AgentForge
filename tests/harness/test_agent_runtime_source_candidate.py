import copy
import json
import shutil
from pathlib import Path
import tempfile
import unittest

from scripts.harness import agent_runtime_source_candidate as gate


def fixture(package):
    """Synthetic source/evidence identities only; no external build fixture required."""
    payload = package / 'payload/kernel-rs'
    evidence = package / 'evidence'
    payload.mkdir(parents=True)
    evidence.mkdir()
    (payload / 'Cargo.toml').write_text('[workspace]\n')
    (payload / 'Cargo.lock').write_text('# synthetic resolved lock\n')
    (package / 'live-preimages.json').write_text('{"records": []}\n')
    records = [
        {'path': name, 'file_mode': '100644', 'proposed_sha256': gate.digest(payload / name),
         'target_upstream_blob': None, 'target_preimage_sha256': None,
         'original_replay_status': 'synthetic_test_fixture'}
        for name in ['Cargo.lock', 'Cargo.toml']
    ]
    def write(name, value):
        (evidence / name).write_text(json.dumps(value))
    write('candidate-source.json', {'target': 'a' * 40, 'target_tree': 'b' * 40,
                                   'records': [records[1].copy()]})
    candidate_hash = gate.digest(evidence / 'candidate-source.json')
    write('build-receipt.json', {'target_commit': 'a' * 40,
          'resolved_lock_sha256': gate.digest(payload / 'Cargo.lock'),
          'source_identity': {'candidate_source_sha256': candidate_hash},
          'contract_unit_tests': {'passed': 13, 'failed': 0}})
    for filename, operation, crate in (
        ('worker-check.json', 'check', 'ai-platform-capability-worker'),
        ('runtime-check-retry.json', 'check', 'ai-platform-agent-runtime'),
        ('contract-tests.json', 'test', 'ai-platform-capability-contract'),
    ):
        write(filename, {'exit_code': 0, 'stop_reason': None, 'command':
              ['cargo', '+1.95.0', operation, '--locked', '-p', crate] +
              (['--all-targets'] if operation == 'check' else [])})
    names = ['ai-platform-agent-runtime', 'ai-platform-capability-worker',
             'ai-platform-capability-contract', 'ai-platform-office']
    write('resolved-metadata.json', {'workspace_members': names,
          'packages': [{'id': name, 'name': name} for name in names]})
    return {'schema': 'ai-platform/runtime-source-unit/v1',
            'target': 'a' * 40, 'target_tree': 'b' * 40,
            'records': records, 'source_file_count': 2,
            'candidate_start_allowed': False, 'generated_release_identity': False,
            'release_lock_refresh_ready': False,
            'resolved_lock_sha256': gate.digest(payload / 'Cargo.lock'),
            'live_preimages_sha256': gate.digest(package / 'live-preimages.json'),
            'input_candidate_manifest_sha256': candidate_hash,
            'evidence': {str(p.relative_to(package)): gate.digest(p) for p in evidence.iterdir()}}


class SourceUnitTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.package = Path(self.tmp.name) / 'package'
        self.unit = fixture(self.package)

    def check(self):
        gate.verify_payload(self.package, self.unit)

    def test_valid_source(self):
        self.check()

    def test_payload_mutation(self):
        p = self.package / 'payload/kernel-rs/Cargo.toml'
        p.write_text(p.read_text() + '\n# changed\n')
        with self.assertRaisesRegex(gate.InvalidSource, 'payload digest'):
            self.check()

    def test_wrong_resolved_lock(self):
        self.unit['resolved_lock_sha256'] = '0' * 64
        with self.assertRaisesRegex(gate.InvalidSource, 'resolved lock'):
            self.check()

    def test_undeclared_file(self):
        (self.package / 'payload/kernel-rs/unreviewed.rs').write_text('')
        with self.assertRaisesRegex(gate.InvalidSource, 'undeclared'):
            self.check()

    def test_duplicate_record(self):
        self.unit['records'].append(copy.deepcopy(self.unit['records'][0]))
        with self.assertRaisesRegex(gate.InvalidSource, 'duplicate'):
            self.check()

    def test_evidence_tamper(self):
        (self.package / 'evidence/worker-check.json').write_text('{}')
        with self.assertRaisesRegex(gate.InvalidSource, 'evidence changed'):
            self.check()

    def test_executable_mode(self):
        (self.package / 'payload/kernel-rs/Cargo.toml').chmod(0o755)
        with self.assertRaisesRegex(gate.InvalidSource, 'mode mismatch'):
            self.check()

    def test_startup_flag(self):
        self.unit['candidate_start_allowed'] = True
        with self.assertRaisesRegex(gate.InvalidSource, 'safety flag'):
            self.check()

    def test_release_flag(self):
        self.unit['generated_release_identity'] = True
        with self.assertRaisesRegex(gate.InvalidSource, 'safety flag'):
            self.check()

    def test_path_escape(self):
        with self.assertRaisesRegex(gate.InvalidSource, 'unsafe relative'):
            gate.safe_path(self.package, '../escape')

    def test_symlink_payload(self):
        p = self.package / 'payload/kernel-rs/Cargo.toml'
        p.unlink()
        p.symlink_to('/dev/null')
        with self.assertRaisesRegex(gate.InvalidSource, 'escaping path|symlink'):
            self.check()

    def test_live_preimage_changed(self):
        repo = Path(self.tmp.name) / 'repo'
        repo.mkdir()
        (repo / 'a').write_text('changed')
        (self.package / 'live-preimages.json').write_text(json.dumps({'records': [
            {'path': 'a', 'sha256': '0' * 64, 'mode': '0o644'}]}))
        with self.assertRaisesRegex(gate.InvalidSource, 'never overwrite'):
            gate.verify_live_preimages(self.package, repo)

    def test_candidate_record_rebinding(self):
        self.unit['records'][1]['original_replay_status'] = 'invented'
        with self.assertRaisesRegex(gate.InvalidSource, 'candidate record mismatch'):
            self.check()


    def composed_fixture(self):
        root = Path(self.tmp.name) / 'composed'
        shutil.copytree(self.package / 'payload/kernel-rs', root / 'codex-rs')
        return root

    def test_extra_composed_source_rejected(self):
        root = self.composed_fixture()
        (root / 'codex-rs/build.rs').write_text('fn main() {}')
        with self.assertRaisesRegex(gate.InvalidSource, 'inventory mismatch.*undeclared'):
            gate.verify_composed(self.unit, {}, root)

    def test_composed_executable_mode_rejected(self):
        root = self.composed_fixture()
        (root / 'codex-rs/Cargo.toml').chmod(0o755)
        with self.assertRaisesRegex(gate.InvalidSource, 'composed executable/file mode'):
            gate.verify_composed(self.unit, {}, root)

    def test_root_git_metadata_only_exemption(self):
        root = self.composed_fixture()
        (root / '.git').mkdir()
        (root / '.git/HEAD').write_text('local metadata')
        self.assertEqual(gate.verify_composed(self.unit, {}, root), self.unit['source_file_count'])
        (root / 'codex-rs/.git').mkdir()
        (root / 'codex-rs/.git/injected.rs').write_text('')
        with self.assertRaisesRegex(gate.InvalidSource, 'inventory mismatch'):
            gate.verify_composed(self.unit, {}, root)

    def test_extra_composed_symlink_directory_rejected(self):
        root = self.composed_fixture()
        (root / 'codex-rs/injected').symlink_to(self.package, target_is_directory=True)
        with self.assertRaisesRegex(gate.InvalidSource, 'inventory mismatch'):
            gate.verify_composed(self.unit, {}, root)

    def test_upstream_executable_mode_rejected(self):
        root = self.composed_fixture()
        upstream = root / 'script.sh'
        upstream.write_bytes(b'#!/bin/sh\n')
        data = upstream.read_bytes()
        import hashlib
        blob = hashlib.sha1(b'blob ' + str(len(data)).encode() + b'\0' + data).hexdigest()
        with self.assertRaisesRegex(gate.InvalidSource, 'composed executable/file mode'):
            gate.verify_composed(self.unit, {'script.sh': ('100755', blob)}, root)


if __name__ == '__main__':
    unittest.main(verbosity=2)
