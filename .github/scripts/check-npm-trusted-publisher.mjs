import { readFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';

export function validatePublisher({ npmVersion, repository, workflowRef, packageRepository }) {
  const version = /^(\d+)\.(\d+)\.(\d+)$/.exec(npmVersion.trim());
  if (!version || Number(version[1]) < 11 ||
      (Number(version[1]) === 11 && Number(version[2]) < 5) ||
      (Number(version[1]) === 11 && Number(version[2]) === 5 && Number(version[3]) < 1)) {
    throw new Error('npm trusted publishing requires npm >=11.5.1 stable');
  }
  if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repository ?? '')) {
    throw new Error('missing GitHub repository identity');
  }
  if (!workflowRef?.startsWith(`${repository}/.github/workflows/publish-sdk.yml@refs/tags/sdk-v`)) {
    throw new Error('npm publication must use the SDK release workflow and tag');
  }
  const expected = `https://github.com/${repository}`;
  const actual = typeof packageRepository === 'string' ? packageRepository.replace(/^git\+/, '').replace(/\.git$/, '') : '';
  if (actual !== expected) {
    throw new Error('package repository.url must match the GitHub OIDC repository');
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const pkg = JSON.parse(readFileSync('sdk/cli/package.json', 'utf8'));
  validatePublisher({ npmVersion: execFileSync('npm', ['--version'], { encoding: 'utf8' }),
    repository: process.env.GITHUB_REPOSITORY, workflowRef: process.env.GITHUB_WORKFLOW_REF,
    packageRepository: pkg.repository?.url });
  console.log('npm OIDC client and package identity prerequisites passed; registry trusted-publisher configuration is verified by npm at publication');
}
