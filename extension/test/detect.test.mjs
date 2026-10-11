// Runs the shared cases under Node's test runner: `node --test extension/test/*.test.mjs`.
import test from 'node:test';
import assert from 'node:assert/strict';
import { cases } from './cases.mjs';

const t = { equal: assert.equal, deepEqual: assert.deepEqual, ok: assert.ok };
for (const [name, fn] of cases) test(name, () => fn(t));
