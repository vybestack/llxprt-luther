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
  // Mirrors the shape seen on a real run, scaled down: every changed file is
  // either reviewed or declared excluded, so coverage is complete.
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

test('a reused file counts as covered in the report, not just the verdict', () => {
  // resolveCompleteness treats reused files as resolved. The reported
  // unreviewed list and coverage must agree, or the gate passes while
  // reporting the file as unreviewed and coverage below 1.
  const result = evaluateGate({
    ocrExitCode: 0,
    ocrStatus: 'completed',
    changedFiles: ['src/a.ts', 'src/b.ts'],
    previewText: preview(['src/a.ts', 'src/b.ts'], []),
    reviewedFiles: ['src/a.ts'],
    reusedFiles: ['src/b.ts'],
  });
  assert.strictEqual(result.completeness, 'complete');
  assert.deepStrictEqual(result.unreviewed, []);
  assert.strictEqual(result.coverage.ratio, '1');
});

test('a waiver with a reason excuses the file that actually failed', () => {
  // The complement of the negative case below. Without this, a gate that
  // ignored every waiver would still satisfy that test.
  const result = evaluateGate({
    ocrExitCode: 0,
    ocrStatus: 'completed',
    changedFiles: ['src/a.ts', 'src/b.ts'],
    previewText: preview(['src/a.ts', 'src/b.ts'], []),
    reviewedFiles: ['src/a.ts'],
    failedFiles: ['src/b.ts'],
    waivedFiles: [{ path: 'src/b.ts', reason: 'binary blob, cannot review' }],
  });
  assert.strictEqual(result.completeness, 'complete');
  assert.deepStrictEqual(result.unreviewed, []);
});

test('no changed files is complete rather than a vacuous failure', () => {
  const result = evaluateGate({
    ...base,
    changedFiles: [],
    previewText: preview([], []),
    reviewedFiles: [],
  });
  assert.strictEqual(result.completeness, 'complete');
  assert.deepStrictEqual(result.unreviewed, []);
});

test('a waiver naming a file that did not fail cannot excuse it', () => {
  // Waivers are only valid for failed files. Trusting the raw list would let
  // an arbitrary path mark an unreviewed file as covered.
  const result = evaluateGate({
    ocrExitCode: 0,
    ocrStatus: 'completed',
    changedFiles: ['src/a.ts', 'src/b.ts'],
    previewText: preview(['src/a.ts', 'src/b.ts'], []),
    reviewedFiles: ['src/a.ts'],
    waivedFiles: [{ path: 'src/b.ts', reason: 'not actually failed' }],
  });
  assert.deepStrictEqual(result.unreviewed, ['src/b.ts']);
  assert.strictEqual(result.passed, false);
});

// --- OCR-reported 'skipped' status ---------------------------------------
//
// These assert the gate DECISION (passed), not just the classification. A
// classification test alone would not have caught that 'skipped' passes the
// gate while real files sit unreviewed.

const DOC_EXCLUDES = ['**/*.md', '**/*.markdown', '**/*.txt'];

test('docs-only range passes when the rules exclude every changed file', () => {
  // OCR selected nothing and emitted no preview. Exclusions come from the
  // configured rules, so the changed files resolve and the gate passes.
  const result = evaluateGate({
    ocrExitCode: 0,
    ocrStatus: 'skipped',
    changedFiles: ['docs/a.md', 'notes/b.txt'],
    previewText: '',
    reviewedFiles: [],
    excludeGlobs: DOC_EXCLUDES,
  });
  assert.strictEqual(result.passed, true);
  assert.strictEqual(result.completeness, 'skipped');
  assert.deepStrictEqual(result.selected, []);
  assert.deepStrictEqual(result.unreviewed, []);
});

test('skipped status cannot pass a source file that no rule excludes', () => {
  // The bypass this guards: OCR claims 'skipped' while a source file changed.
  // The status is a third-party claim, so the changed set stays authoritative.
  const result = evaluateGate({
    ocrExitCode: 0,
    ocrStatus: 'skipped',
    changedFiles: ['src/auth.rs'],
    previewText: '',
    reviewedFiles: [],
    excludeGlobs: DOC_EXCLUDES,
  });
  assert.strictEqual(result.passed, false);
  assert.deepStrictEqual(result.unreviewed, ['src/auth.rs']);
});

test('mixed range fails on the unreviewed source file', () => {
  const result = evaluateGate({
    ocrExitCode: 0,
    ocrStatus: 'skipped',
    changedFiles: ['docs/a.md', 'src/auth.rs'],
    previewText: '',
    reviewedFiles: [],
    excludeGlobs: DOC_EXCLUDES,
  });
  assert.strictEqual(result.passed, false);
  assert.deepStrictEqual(result.selected, ['src/auth.rs']);
  assert.deepStrictEqual(result.unreviewed, ['src/auth.rs']);
});

test('skipped status does not pass when files were actually reviewed', () => {
  // Self-contradictory: a run reporting 'skipped' after reviewing something is
  // not a clean no-selection run, so it must not be trusted.
  const result = evaluateGate({
    ocrExitCode: 0,
    ocrStatus: 'skipped',
    changedFiles: ['src/a.rs', 'src/b.rs'],
    previewText: '',
    reviewedFiles: ['src/a.rs'],
    excludeGlobs: DOC_EXCLUDES,
  });
  assert.strictEqual(result.passed, false);
});

test('missing exclusion rules keep every changed file selected', () => {
  // Fail closed: without rules nothing is excluded, so prose stays unresolved
  // rather than being silently passed.
  const result = evaluateGate({
    ocrExitCode: 0,
    ocrStatus: 'skipped',
    changedFiles: ['docs/a.md'],
    previewText: '',
    reviewedFiles: [],
  });
  assert.strictEqual(result.passed, false);
  assert.deepStrictEqual(result.unreviewed, ['docs/a.md']);
});

test('rule-derived exclusions do not resolve a failed file', () => {
  // A file that failed review is unresolved even though a rule would have
  // excluded a sibling; failures are never masked by exclusions.
  const result = evaluateGate({
    ocrExitCode: 0,
    ocrStatus: 'success',
    changedFiles: ['docs/a.md', 'src/auth.rs'],
    previewText: '',
    reviewedFiles: [],
    failedFiles: ['src/auth.rs'],
    excludeGlobs: DOC_EXCLUDES,
  });
  assert.strictEqual(result.passed, false);
});

test('exclusion globs match only their intended paths', () => {
  // A malformed or overbroad pattern must not become a catch-all.
  const result = evaluateGate({
    ocrExitCode: 0,
    ocrStatus: 'skipped',
    changedFiles: ['src/markdown_parser.rs', 'a.md', 'deep/nested/b.md'],
    previewText: '',
    reviewedFiles: [],
    excludeGlobs: DOC_EXCLUDES,
  });
  // Both .md files match, including one at the repository root.
  assert.deepStrictEqual(result.selected, ['src/markdown_parser.rs']);
  assert.strictEqual(result.passed, false);
});

test('trailing whitespace in a filename cannot borrow an extension rule', () => {
  // `evil.rs.md ` does not end in `.md`. OCR sees the real path and does not
  // exclude it, so neither may the gate -- trimming here would drop a source
  // file from review entirely.
  const result = evaluateGate({
    ocrExitCode: 0,
    ocrStatus: 'skipped',
    changedFiles: ['src/evil.rs.md '],
    previewText: '',
    reviewedFiles: [],
    excludeGlobs: DOC_EXCLUDES,
  });
  assert.strictEqual(result.passed, false);
  assert.deepStrictEqual(result.unreviewed, ['src/evil.rs.md ']);
});

test('unsupported exclusion syntax excludes nothing', () => {
  // Rather than approximating another matcher's semantics, any pattern that is
  // not exactly `**/*.ext` is refused. A literal comma must never become
  // alternation, which would have excluded arbitrary source paths.
  const result = evaluateGate({
    ocrExitCode: 0,
    ocrStatus: 'skipped',
    changedFiles: ['src/auth.rs', 'evil-src/auth.rs'],
    previewText: '',
    reviewedFiles: [],
    excludeGlobs: ['docs,src/**', '**/target/**', '{a,b}.rs', 'src/**/*.rs'],
  });
  assert.strictEqual(result.passed, false);
  assert.deepStrictEqual(result.unreviewed, ['src/auth.rs', 'evil-src/auth.rs']);
});

test('extension exclusions are case-insensitive', () => {
  // OCR lowercases patterns and paths, so a case-sensitive gate would leave
  // README.MD selected and permanently unreviewable.
  const result = evaluateGate({
    ocrExitCode: 0,
    ocrStatus: 'skipped',
    changedFiles: ['README.MD', 'docs/Guide.Md'],
    previewText: '',
    reviewedFiles: [],
    excludeGlobs: DOC_EXCLUDES,
  });
  assert.strictEqual(result.passed, true);
});

test('a dotfile named like an extension is not excluded', () => {
  const result = evaluateGate({
    ocrExitCode: 0,
    ocrStatus: 'skipped',
    changedFiles: ['.md', 'src/.txt'],
    previewText: '',
    reviewedFiles: [],
    excludeGlobs: DOC_EXCLUDES,
  });
  assert.strictEqual(result.passed, false);
  assert.deepStrictEqual(result.unreviewed, ['.md', 'src/.txt']);
});

test('reported exclusions account for the rule-derived decision', () => {
  // The logged count must agree with the verdict; reporting excluded=0 while
  // rule exclusions caused the pass would be actively misleading.
  const result = evaluateGate({
    ocrExitCode: 0,
    ocrStatus: 'skipped',
    changedFiles: ['docs/a.md'],
    previewText: '',
    reviewedFiles: [],
    excludeGlobs: DOC_EXCLUDES,
  });
  assert.strictEqual(result.passed, true);
  assert.deepStrictEqual(result.excluded, [
    { path: 'docs/a.md', reason: 'excluded_by_configured_rule' },
  ]);
});
