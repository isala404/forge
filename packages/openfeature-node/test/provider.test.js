'use strict';

const assert = require('node:assert/strict');
const test = require('node:test');
const { ErrorCode, StandardResolutionReasons } = require('@openfeature/server-sdk');
const { ForgeProvider, telemetryHook } = require('../index.js');

test('resolves typed details without mutating context or installing hooks', async () => {
  const calls = [];
  const forge = {
    async flagDetails(key, defaultJson, targetingKey) {
      calls.push({ key, defaultJson, targetingKey });
      return { valueJson: '"dark"', valueType: 'string', variant: 'theme-v1', reason: 'static', errorCode: null };
    },
  };
  const provider = new ForgeProvider(forge);
  const context = Object.freeze({ targetingKey: 'user-1', tenant: 'acme' });
  const result = await provider.resolveStringEvaluation('theme', 'light', context);
  assert.deepEqual(result, { value: 'dark', reason: StandardResolutionReasons.STATIC, variant: 'theme-v1' });
  assert.deepEqual(calls, [{ key: 'theme', defaultJson: '"light"', targetingKey: 'user-1' }]);
  assert.equal(provider.hooks, undefined);
  assert.equal(context.tenant, 'acme');
});

test('returns standard missing and type mismatch details', async () => {
  const missing = new ForgeProvider({
    async flagDetails() {
      return { valueJson: 'false', valueType: 'boolean', variant: null, reason: 'default_missing', errorCode: null };
    },
  });
  assert.equal((await missing.resolveBooleanEvaluation('missing', false)).errorCode, ErrorCode.FLAG_NOT_FOUND);

  const mismatch = new ForgeProvider({
    async flagDetails() {
      return { valueJson: '"wrong"', valueType: 'string', variant: null, reason: 'static', errorCode: null };
    },
  });
  assert.equal((await mismatch.resolveBooleanEvaluation('flag', false)).errorCode, ErrorCode.TYPE_MISMATCH);
  assert.ok(telemetryHook());
});
