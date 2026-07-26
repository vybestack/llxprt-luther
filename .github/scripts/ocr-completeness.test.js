'use strict';

const test = require('node:test');
const assert = require('node:assert');

const { resolveCompleteness, computeCoverage, normalizePaths } = require('./ocr-completeness');

const baseComplete = {
  ocrExitCode: 0,
  ocrStatus: 'success',
  selectedFiles: ['a.ts', 'b.ts'],
  completedFiles: ['a.ts', 'b.ts'],
  failedFiles: [],
  reusedFiles: [],
  waivedFiles: [],
};

test('full coverage with a recognized status is complete', () => {
  assert.strictEqual(resolveCompleteness(baseComplete), 'complete');
  assert.strictEqual(
    resolveCompleteness({ ...baseComplete, ocrStatus: 'completed' }),
    'complete',
  );
});

test('skipped short-circuits before any other classification', () => {
  assert.strictEqual(
    resolveCompleteness({ ...baseComplete, skipped: true, ocrExitCode: 9 }),
    'skipped',
  );
});

test('untrustworthy exit codes fail closed rather than coercing to zero', () => {
  for (const ocrExitCode of [undefined, null, NaN, -1, 1.5, '0']) {
    assert.strictEqual(
      resolveCompleteness({ ...baseComplete, ocrExitCode }),
      'failed',
      `exit code ${String(ocrExitCode)} must be failed`,
    );
  }
  assert.strictEqual(resolveCompleteness({ ...baseComplete, ocrExitCode: 1 }), 'failed');
});

test('unrecognized, empty, or missing status yields partial', () => {
  for (const ocrStatus of ['', 'unknown', 'completed_with_errors', undefined, null, 7]) {
    assert.strictEqual(
      resolveCompleteness({ ...baseComplete, ocrStatus }),
      'partial',
      `status ${String(ocrStatus)} must be partial`,
    );
  }
});

test('a selected file that was never resolved yields partial', () => {
  assert.strictEqual(
    resolveCompleteness({ ...baseComplete, completedFiles: ['a.ts'] }),
    'partial',
  );
});

test('classification is set-based, so matching counts do not imply coverage', () => {
  // Two selected, two completed, but the paths differ. A count-based check
  // would see 2 >= 2 and wrongly report complete.
  assert.strictEqual(
    resolveCompleteness({
      ...baseComplete,
      selectedFiles: ['a.ts', 'b.ts'],
      completedFiles: ['a.ts', 'c.ts'],
    }),
    'partial',
  );
});

test('a resolved set larger than the selection cannot cover a missing file', () => {
  // Three resolved paths against two selected: any size comparison passes,
  // yet 'b.ts' was never reviewed. Only per-path membership catches this.
  assert.strictEqual(
    resolveCompleteness({
      ...baseComplete,
      selectedFiles: ['a.ts', 'b.ts'],
      completedFiles: ['a.ts'],
      reusedFiles: ['x.ts', 'y.ts'],
    }),
    'partial',
  );
});

test('reused files count as resolved', () => {
  assert.strictEqual(
    resolveCompleteness({
      ...baseComplete,
      completedFiles: ['a.ts'],
      reusedFiles: ['b.ts'],
    }),
    'complete',
  );
});

test('a waiver requires a reason and a matching failure', () => {
  const withFailure = {
    ...baseComplete,
    completedFiles: ['a.ts'],
    failedFiles: ['b.ts'],
  };
  // No reason.
  assert.strictEqual(
    resolveCompleteness({ ...withFailure, waivedFiles: [{ path: 'b.ts' }] }),
    'partial',
  );
  // Bare string carries no reason.
  assert.strictEqual(
    resolveCompleteness({ ...withFailure, waivedFiles: ['b.ts'] }),
    'partial',
  );
  // Waiver for a file that never failed is irrelevant.
  assert.strictEqual(
    resolveCompleteness({
      ...withFailure,
      waivedFiles: [{ path: 'zzz.ts', reason: 'unrelated' }],
    }),
    'partial',
  );
  // Valid waiver resolves the failure.
  assert.strictEqual(
    resolveCompleteness({
      ...withFailure,
      waivedFiles: [{ path: 'b.ts', reason: 'generated file' }],
    }),
    'complete',
  );
});

test('an unresolved failure yields partial', () => {
  assert.strictEqual(
    resolveCompleteness({
      ...baseComplete,
      selectedFiles: ['a.ts', 'b.ts'],
      completedFiles: ['a.ts', 'b.ts'],
      failedFiles: ['c.ts'],
    }),
    'partial',
  );
});

test('a file both completed and failed is resolved because completion wins', () => {
  assert.strictEqual(
    resolveCompleteness({ ...baseComplete, failedFiles: ['b.ts'] }),
    'complete',
  );
});

test('an inflated completed set cannot mask a missing selected file', () => {
  assert.strictEqual(
    resolveCompleteness({
      ...baseComplete,
      selectedFiles: ['a.ts'],
      completedFiles: ['a.ts', 'extra.ts'],
    }),
    'partial',
  );
});

test('paths are trimmed and deduplicated before comparison', () => {
  assert.strictEqual(
    resolveCompleteness({
      ...baseComplete,
      selectedFiles: [' a.ts ', 'a.ts', 'b.ts'],
      completedFiles: ['a.ts', ' b.ts'],
    }),
    'complete',
  );
});

test('an empty selection with a success status is complete', () => {
  assert.strictEqual(
    resolveCompleteness({ ...baseComplete, selectedFiles: [], completedFiles: [] }),
    'complete',
  );
});

test('non-array path inputs degrade to empty rather than throwing', () => {
  assert.deepStrictEqual(normalizePaths(null), []);
  assert.deepStrictEqual(normalizePaths('a.ts'), []);
  assert.deepStrictEqual(normalizePaths(undefined), []);
  assert.strictEqual(
    resolveCompleteness({ ...baseComplete, selectedFiles: null, completedFiles: null }),
    'complete',
  );
});

test('missing params do not throw and fail closed', () => {
  assert.strictEqual(resolveCompleteness(), 'failed');
  assert.strictEqual(resolveCompleteness({}), 'failed');
});

test('coverage ratio reports selection progress', () => {
  assert.deepStrictEqual(computeCoverage({ selected: 4, completed: 2 }), {
    completed: 2,
    selected: 4,
    ratio: '0.5',
  });
  // An empty selection is fully covered rather than dividing by zero.
  assert.deepStrictEqual(computeCoverage({ selected: 0, completed: 0 }), {
    completed: 0,
    selected: 0,
    ratio: '1',
  });
});
