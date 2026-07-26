'use strict';

const test = require('node:test');
const assert = require('node:assert');

const { evaluateGate } = require('./ocr-gate');

const ESC = '\u001b';
const RESET = `${ESC}[0m`;

function preview(reviewed, excluded) {
  const lines = [];
  lines.push(`${ESC}[1mWill review (${reviewed.length}):${RESET}`);
  for (const file of reviewed) {
    lines.push(`  ${ESC}[32m[A]${RESET}  ${file}   ${ESC}[32m+1 ${RESET} ${ESC}[31m-0 ${RESET}`);
  }
  lines.push(`${ESC}[1mExcluded from review (${excluded.length}):${RESET}`);
  for (const file of excluded) {
    lines.push(`  ${ESC}[33m[M]${RESET}  ${file}   ${ESC}[2m(unsupported_ext)${RESET}`);
  }
  return lines.join('\n');
}

const base = {
  ocrExitCode: 0,
  ocrStatus: 'success',
};

test('every selected file reviewed is complete', () => {
  const result = evaluateGate({
    ...base,
    changedFiles: ['a.ts', 'b.ts'],
    previewText: preview(['a.ts', 'b.ts'], []),
    reviewedFiles: ['a.ts', 'b.ts'],
  });
  assert.strictEqual(result.completeness, 'complete');
  assert.strictEqual(result.passed, true);
  assert.deepStrictEqual(result.unreviewed, []);
});

test('declared exclusions are resolved rather than counted as missing', () => {
  // The real v30 shape: 30 changed, 28 reviewed, 2 declared unsupported_ext.
  const result = evaluateGate({
    ...base,
    changedFiles: ['a.ts', 'b.ts', 'notes.md'],
    previewText: preview(['a.ts', 'b.ts'], ['notes.md']),
    reviewedFiles: ['a.ts', 'b.ts'],
  });
  assert.strictEqual(result.completeness, 'complete');
  assert.deepStrictEqual(result.unreviewed, []);
  assert.deepStrictEqual(
    result.excluded.map((entry) => entry.path),
    ['notes.md'],
  );
});

test('a changed file that was neither reviewed nor excluded fails closed', () => {
  const result = evaluateGate({
    ...base,
    changedFiles: ['a.ts', 'b.ts', 'c.ts'],
    previewText: preview(['a.ts', 'b.ts'], []),
    reviewedFiles: ['a.ts', 'b.ts'],
  });
  assert.strictEqual(result.completeness, 'partial');
  assert.strictEqual(result.passed, false);
  assert.deepStrictEqual(result.unreviewed, ['c.ts']);
});

test('a preview-declared review that was not completed remains unresolved', () => {
  // Only the preview may declare an exclusion; an unreviewed file that the
  // preview said it would review remains unresolved.
  const result = evaluateGate({
    ...base,
    changedFiles: ['a.ts', 'skipped.md'],
    previewText: preview(['a.ts', 'skipped.md'], []),
    reviewedFiles: ['a.ts'],
  });
  assert.strictEqual(result.completeness, 'partial');
  assert.deepStrictEqual(result.unreviewed, ['skipped.md']);
});

test('a missing or unparseable preview does not excuse coverage', () => {
  for (const previewText of ['', 'garbage', null]) {
    const result = evaluateGate({
      ...base,
      changedFiles: ['a.ts', 'b.ts'],
      previewText,
      reviewedFiles: ['a.ts'],
    });
    assert.strictEqual(result.completeness, 'partial');
    assert.deepStrictEqual(result.unreviewed, ['b.ts']);
  }
});

test('a success status cannot override missing coverage', () => {
  const result = evaluateGate({
    ...base,
    ocrStatus: 'success',
    changedFiles: ['a.ts', 'b.ts'],
    previewText: preview(['a.ts', 'b.ts'], []),
    reviewedFiles: [],
  });
  assert.strictEqual(result.completeness, 'partial');
  assert.strictEqual(result.passed, false);
});

test('a nonzero or untrustworthy exit code fails', () => {
  const full = {
    changedFiles: ['a.ts'],
    previewText: preview(['a.ts'], []),
    reviewedFiles: ['a.ts'],
  };
  assert.strictEqual(evaluateGate({ ...full, ocrExitCode: 1, ocrStatus: 'success' }).completeness, 'failed');
  assert.strictEqual(evaluateGate({ ...full, ocrExitCode: null, ocrStatus: 'success' }).completeness, 'failed');
});

test('an unrecognized status degrades to partial', () => {
  const result = evaluateGate({
    ocrExitCode: 0,
    ocrStatus: 'completed_with_errors',
    changedFiles: ['a.ts'],
    previewText: preview(['a.ts'], []),
    reviewedFiles: ['a.ts'],
  });
  assert.strictEqual(result.completeness, 'partial');
});

test('a skipped run is reported as skipped and passes', () => {
  // The changed/reviewed inputs are deliberately non-empty and would otherwise
  // resolve to 'partial'. Keeping them proves the skip is decided before the
  // coverage comparison, which empty inputs could not distinguish.
  const result = evaluateGate({
    ...base,
    skipped: true,
    changedFiles: ['a.ts'],
    previewText: preview(['a.ts'], []),
    reviewedFiles: [],
  });
  assert.strictEqual(result.completeness, 'skipped');
  assert.strictEqual(result.passed, true);
});

test('an all-excluded range is complete with no selection', () => {
  const result = evaluateGate({
    ...base,
    changedFiles: ['notes.md', 'readme.md'],
    previewText: preview([], ['notes.md', 'readme.md']),
    reviewedFiles: [],
  });
  assert.strictEqual(result.completeness, 'complete');
  assert.deepStrictEqual(result.selected, []);
});

test('coverage reports the proven fraction of the selection', () => {
  const result = evaluateGate({
    ...base,
    changedFiles: ['a.ts', 'b.ts', 'c.ts', 'd.ts'],
    previewText: preview(['a.ts', 'b.ts', 'c.ts', 'd.ts'], []),
    reviewedFiles: ['a.ts', 'b.ts'],
  });
  assert.strictEqual(result.coverage.selected, 4);
  assert.strictEqual(result.coverage.completed, 2);
  assert.strictEqual(result.coverage.ratio, '0.5');
});

test('paths with spaces are reconciled correctly end to end', () => {
  const result = evaluateGate({
    ...base,
    changedFiles: ['src/my dir/a b.ts', 'docs/read me.md'],
    previewText: preview(['src/my dir/a b.ts'], ['docs/read me.md']),
    reviewedFiles: ['src/my dir/a b.ts'],
  });
  assert.strictEqual(result.completeness, 'complete');
  assert.deepStrictEqual(result.unreviewed, []);
});

test('untrimmed changed paths still match reviewed evidence', () => {
  // Reviewed evidence is normalized, so changed paths must be too. Comparing
  // a trimmed set against untrimmed input would report a reviewed file as
  // unreviewed and fail the gate on a formatting artifact.
  const result = evaluateGate({
    ocrExitCode: 0,
    ocrStatus: 'completed',
    changedFiles: ['  src/a.ts  ', 'src/b.ts'],
    previewText: preview(['src/a.ts', 'src/b.ts'], []),
    reviewedFiles: ['src/a.ts', '  src/b.ts'],
  });
  assert.strictEqual(result.completeness, 'complete');
  assert.deepStrictEqual(result.unreviewed, []);
});

test('an untrimmed declared exclusion still excuses a file', () => {
  const result = evaluateGate({
    ocrExitCode: 0,
    ocrStatus: 'completed',
    changedFiles: ['src/a.ts', '  docs/x.md  '],
    previewText: preview(['src/a.ts'], ['docs/x.md']),
    reviewedFiles: ['src/a.ts'],
  });
  assert.strictEqual(result.completeness, 'complete');
  assert.deepStrictEqual(result.unreviewed, []);
});
