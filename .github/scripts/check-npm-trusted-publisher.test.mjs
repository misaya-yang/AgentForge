import test from 'node:test';
import assert from 'node:assert/strict';
import { validatePublisher } from './check-npm-trusted-publisher.mjs';
const valid = { npmVersion: '11.5.1', repository: 'misaya-yang/AgentForge',
  workflowRef: 'misaya-yang/AgentForge/.github/workflows/publish-sdk.yml@refs/tags/sdk-v1.5.0',
  packageRepository: 'git+https://github.com/misaya-yang/AgentForge.git' };
test('official minimum and newer stable npm clients accepted', () => {
  validatePublisher(valid);
  validatePublisher({ ...valid, npmVersion: '12.0.0' });
});
test('old prerelease and malformed npm clients rejected', () => {
  for (const npmVersion of ['10.99.0', '11.4.99', '11.5.0', '11.5.1-beta.1', 'garbage']) {
    assert.throws(() => validatePublisher({ ...valid, npmVersion }));
  }
});
test('wrong repository workflow branch and missing identity rejected', () => {
  for (const override of [{ packageRepository: 'git+https://github.com/misaya-yang/AI--Platfform.git' },
    { packageRepository: undefined }, { repository: '' },
    { workflowRef: valid.workflowRef.replace('publish-sdk.yml', 'ci.yml') },
    { workflowRef: valid.workflowRef.replace('refs/tags/sdk-v1.5.0', 'refs/heads/main') }]) {
    assert.throws(() => validatePublisher({ ...valid, ...override }));
  }
});
