"""Synthetic export fixtures exercise validation, never claim a compiler ran."""
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from scripts.harness import agent_runtime_source_candidate as candidate
from scripts.harness import agent_runtime_supply_chain as supply


def dump(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, sort_keys=True))


def blob(data):
    return hashlib.sha1(b'blob ' + str(len(data)).encode() + b'\0' + data).hexdigest()


class ComposedReceiptTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.package = self.root / 'package'
        self.upstream = self.root / 'upstream'
        self.composed = self.root / 'composed'
        self.repo = self.root / 'repo'
        self.export = self.root / 'export'
        self.schema = self.root / 'schema'
        self.output = self.root / 'output'
        for path in (self.upstream, self.repo, self.export, self.schema):
            path.mkdir()
        names = ['ai-platform-agent-runtime', 'ai-platform-capability-worker',
                 'ai-platform-capability-contract', 'ai-platform-office']
        self.entries = {}
        self.objects = {}
        for relative, data in {'LICENSE': b'Apache-2.0\n', 'NOTICE': b'OpenAI attribution\n',
                               'codex-rs/rust-toolchain.toml': b'[toolchain]\nchannel = "1.95.0"\n'}.items():
            self.entries[relative] = ('100644', blob(data))
            self.objects[blob(data)] = data
            for base in (self.upstream, self.composed):
                path = base / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(data)
        payload = self.package / 'payload/kernel-rs'
        payload.mkdir(parents=True)
        (payload / 'Cargo.toml').write_text('[workspace]\n')
        lock = '\n'.join(f'[[package]]\nname = "{name}"\nversion = "0.162.1"\n' for name in names)
        (payload / 'Cargo.lock').write_text(lock)
        for name in names:
            path = payload / name / 'Cargo.toml'
            path.parent.mkdir()
            path.write_text(f'[package]\nname = "{name}"\nversion = "0.162.1"\n')
        records = []
        for path in sorted(p for p in payload.rglob('*') if p.is_file()):
            relative = str(path.relative_to(payload))
            target = self.composed / 'codex-rs' / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(path.read_bytes())
            records.append({'path': relative, 'file_mode': '100644', 'target_upstream_blob': None,
                'target_preimage_sha256': None, 'proposed_sha256': candidate.digest(path),
                'original_replay_status': 'synthetic'})
        self.lock_sha = candidate.digest(payload / 'Cargo.lock')
        self.metadata = {'workspace_root': str(self.composed / 'codex-rs'), 'workspace_members': names,
            'packages': [{'id': name, 'name': name, 'version': '0.162.1', 'source': None,
                'manifest_path': str(self.composed / 'codex-rs' / name / 'Cargo.toml')} for name in names],
            'resolve': {'nodes': [{'id': name, 'dependencies': []} for name in names]}}
        evidence = self.package / 'evidence'
        dump(evidence / 'candidate-source.json', {'target': 'a' * 40, 'target_tree': 'b' * 40,
            'records': [r for r in records if r['path'] != 'Cargo.lock']})
        candidate_hash = candidate.digest(evidence / 'candidate-source.json')
        dump(evidence / 'build-receipt.json', {'target_commit': 'a' * 40,
            'resolved_lock_sha256': self.lock_sha, 'source_identity': {'candidate_source_sha256': candidate_hash},
            'contract_unit_tests': {'passed': 13, 'failed': 0}})
        for filename, operation, name in [('worker-check.json', 'check', names[1]),
                ('runtime-check-retry.json', 'check', names[0]), ('contract-tests.json', 'test', names[2])]:
            dump(evidence / filename, {'exit_code': 0, 'stop_reason': None, 'command':
                ['cargo', '+1.95.0', operation, '--locked', '-p', name] + (['--all-targets'] if operation == 'check' else [])})
        dump(evidence / 'resolved-metadata.json', self.metadata)
        dump(self.package / 'live-preimages.json', {'records': []})
        unit = {'schema': 'ai-platform/runtime-source-unit/v1', 'target': 'a' * 40, 'target_tree': 'b' * 40,
            'records': records, 'source_file_count': len(records), 'candidate_start_allowed': False,
            'generated_release_identity': False, 'release_lock_refresh_ready': False,
            'resolved_lock_sha256': self.lock_sha, 'input_candidate_manifest_sha256': candidate_hash,
            'evidence': {str(p.relative_to(self.package)): candidate.digest(p) for p in evidence.iterdir()},
            'live_preimages_sha256': candidate.digest(self.package / 'live-preimages.json')}
        dump(self.package / 'source-unit.json', unit)
        migration = self.repo / supply.CAPABILITY_WORKER_SCHEMA_REL
        migration.parent.mkdir(parents=True)
        migration.write_text('-- synthetic migration\n')
        dump(self.schema / 'protocol.json', {'type': 'object'})
        dump(self.export / 'cargo-metadata.json', self.metadata)
        (self.export / 'Cargo.lock').write_text(lock)
        (self.export / 'cargo-version.txt').write_text('cargo 1.95.0 (synthetic)\n')
        (self.export / 'rustc-version.txt').write_text('rustc 1.95.0 (synthetic)\n')
        dump(self.export / 'schema-command.json', {'cwd': str(self.composed / 'codex-rs'), 'exit_code': 0, 'stop_reason': None, 'command':
            ['cargo', '+1.95.0', 'run', '--locked', '--offline', '-p', 'codex-app-server-protocol',
             '--example', 'ai_platform_export_schema', '--', str(self.schema)]})
        dump(self.export / 'metadata-command.json', {'cwd': str(self.composed / 'codex-rs'), 'exit_code': 0, 'stop_reason': None,
            'command': ['cargo', '+1.95.0', 'metadata', '--locked', '--offline', '--format-version', '1'],
            'stdout_sha256': candidate.digest(self.export / 'cargo-metadata.json')})
        self.export_manifest = {'schema': 'ai-platform/composed-source-export/v1',
            'source_unit_sha256_before': candidate.digest(self.package / 'source-unit.json'),
            'source_unit_sha256_after': candidate.digest(self.package / 'source-unit.json'),
            'target_commit': 'a' * 40, 'resolved_lock_sha256': self.lock_sha,
            'schema_bundle': supply.bundle_identity(self.schema), 'files': {}}
        for key, name in [('cargo_metadata', 'cargo-metadata.json'), ('cargo_lock', 'Cargo.lock'),
                ('cargo_version', 'cargo-version.txt'), ('rustc_version', 'rustc-version.txt'),
                ('schema_command_receipt', 'schema-command.json'), ('metadata_command_receipt', 'metadata-command.json')]:
            self.export_manifest['files'][key] = {'path': name, 'sha256': candidate.digest(self.export / name)}
        self.save_export()
        self.git_patch = patch.object(candidate, 'git', side_effect=self.git)
        self.git_patch.start()
        self.addCleanup(self.git_patch.stop)
        self.run_patch = patch.object(supply, '_run', side_effect=self.run_git)
        self.run_patch.start()
        self.addCleanup(self.run_patch.stop)

    def save_export(self):
        dump(self.export / 'export.json', self.export_manifest)

    def change_export_file(self, key, value):
        record = self.export_manifest['files'][key]
        dump(self.export / record['path'], value)
        record['sha256'] = candidate.digest(self.export / record['path'])
        self.save_export()

    def git(self, upstream, *args):
        if args[0] == 'rev-parse':
            return ('b' * 40 + '\n').encode()
        if args[0] == 'ls-tree':
            return b''.join(f'{mode} blob {sha}\t{path}\0'.encode() for path, (mode, sha) in self.entries.items())
        if args[0] == 'cat-file':
            return self.objects[args[-1]]
        raise AssertionError(args)

    def run_git(self, command, *, cwd):
        if command[1] == 'status':
            return ''
        if command[1:3] == ['rev-parse', 'HEAD']:
            return 'a' * 40
        if command[1:3] == ['rev-parse', 'HEAD^{tree}']:
            return 'b' * 40
        if command[1] == 'remote':
            return 'https://github.com/openai/codex.git'
        raise AssertionError(command)

    def generate(self):
        supply.generate_composed_receipt(package=self.package, upstream=self.upstream,
            composed=self.composed, repo_root=self.repo, export_evidence=self.export / 'export.json',
            schema_dir=self.schema, output=self.output)

    def test_valid_export_generates_bound_sboms_without_runtime_claim(self):
        self.generate()
        receipt = json.loads((self.output / 'source-receipt.json').read_text())
        self.assertFalse(receipt['evidence_provenance']['runtime_compiled'])
        self.assertTrue(receipt['evidence_provenance']['schema_generated_by_compiled_protocol_library'])
        self.assertEqual(receipt['toolchain']['cargo_lock_sha256'], self.lock_sha)
        self.assertEqual(receipt['sbom']['sha256'], candidate.digest(self.output / 'sbom.cdx.json'))
        self.assertEqual(receipt['overlay']['manifest_sha256'], candidate.digest(self.output / 'manifest.json'))
        self.assertFalse((self.output / 'lock.json').exists())

    def test_output_cannot_overwrite(self):
        self.output.mkdir()
        with self.assertRaisesRegex(supply.ContractError, 'must not exist'):
            self.generate()

    def test_output_cannot_mutate_source(self):
        self.output = self.composed / 'new-receipt'
        with self.assertRaisesRegex(supply.ContractError, 'external'):
            self.generate()

    def test_dirty_upstream_rejected(self):
        with patch.object(supply, '_run', return_value=' M Cargo.toml'):
            with self.assertRaisesRegex(supply.ContractError, 'must be clean'):
                self.generate()

    def test_wrong_upstream_head_rejected(self):
        original = self.run_git
        def wrong(command, *, cwd):
            return 'c' * 40 if command[1:3] == ['rev-parse', 'HEAD'] else original(command, cwd=cwd)
        with patch.object(supply, '_run', side_effect=wrong):
            with self.assertRaisesRegex(supply.ContractError, 'HEAD'):
                self.generate()

    def test_extra_composed_source_rejected(self):
        (self.composed / 'build.rs').write_text('')
        with self.assertRaisesRegex(supply.ContractError, 'inventory mismatch'):
            self.generate()

    def test_wrong_composed_mode_rejected(self):
        (self.composed / 'codex-rs/Cargo.toml').chmod(0o755)
        with self.assertRaisesRegex(supply.ContractError, 'mode mismatch'):
            self.generate()

    def test_changed_schema_rejected(self):
        dump(self.schema / 'protocol.json', {'type': 'string'})
        with self.assertRaisesRegex(supply.ContractError, 'schema bundle differs'):
            self.generate()

    def test_failed_schema_command_rejected(self):
        value = json.loads((self.export / 'schema-command.json').read_text())
        value['exit_code'] = 101
        self.change_export_file('schema_command_receipt', value)
        with self.assertRaisesRegex(supply.ContractError, 'did not succeed'):
            self.generate()

    def test_filtered_metadata_rejected(self):
        value = json.loads((self.export / 'metadata-command.json').read_text())
        value['command'] += ['--filter-platform', 'x86_64-unknown-linux-gnu']
        self.change_export_file('metadata_command_receipt', value)
        with self.assertRaisesRegex(supply.ContractError, 'unfiltered'):
            self.generate()

    def test_metadata_output_not_bound_rejected(self):
        value = json.loads((self.export / 'metadata-command.json').read_text())
        value['stdout_sha256'] = '0' * 64
        self.change_export_file('metadata_command_receipt', value)
        with self.assertRaisesRegex(supply.ContractError, 'output is not hash-bound'):
            self.generate()

    def test_export_lock_tamper_rejected(self):
        (self.export / 'Cargo.lock').write_text('changed')
        with self.assertRaisesRegex(supply.ContractError, 'export file changed'):
            self.generate()

    def test_source_changed_after_export_rejected(self):
        self.export_manifest['source_unit_sha256_after'] = '0' * 64
        self.save_export()
        with self.assertRaisesRegex(supply.ContractError, 'before and after'):
            self.generate()

    def test_source_only_metadata_rejected(self):
        self.metadata['workspace_root'] = str(self.upstream / 'codex-rs')
        self.change_export_file('cargo_metadata', self.metadata)
        value = json.loads((self.export / 'metadata-command.json').read_text())
        value['stdout_sha256'] = candidate.digest(self.export / 'cargo-metadata.json')
        self.change_export_file('metadata_command_receipt', value)
        with self.assertRaisesRegex(supply.ContractError, 'another workspace'):
            self.generate()

    def test_metadata_package_absent_from_lock_rejected(self):
        self.metadata['packages'][0]['version'] = '9.9.9'
        self.change_export_file('cargo_metadata', self.metadata)
        value = json.loads((self.export / 'metadata-command.json').read_text())
        value['stdout_sha256'] = candidate.digest(self.export / 'cargo-metadata.json')
        self.change_export_file('metadata_command_receipt', value)
        with self.assertRaisesRegex(supply.ContractError, 'not present in exact resolved lock'):
            self.generate()

    def test_wrong_schema_command_workspace_rejected(self):
        value = json.loads((self.export / 'schema-command.json').read_text())
        value['cwd'] = str(self.upstream / 'codex-rs')
        self.change_export_file('schema_command_receipt', value)
        with self.assertRaisesRegex(supply.ContractError, 'another working directory'):
            self.generate()

    def test_wrong_toolchain_rejected(self):
        path = self.export / 'rustc-version.txt'
        path.write_text('rustc 1.94.0 (synthetic)')
        self.export_manifest['files']['rustc_version']['sha256'] = candidate.digest(path)
        self.save_export()
        with self.assertRaisesRegex(supply.ContractError, 'toolchain versions'):
            self.generate()

    def test_schema_symlink_rejected(self):
        (self.schema / 'extra.json').symlink_to(self.schema / 'protocol.json')
        with self.assertRaisesRegex(supply.ContractError, 'cannot contain symlinks'):
            self.generate()


if __name__ == '__main__':
    unittest.main(verbosity=2)
