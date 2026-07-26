'use strict';

const test = require('node:test');
const assert = require('node:assert');

const { parsePreview, stripAnsi, stripChurn, stripExclusionReason } = require('./ocr-preview');

const ESC = '\u001b';
const BOLD = `${ESC}[1m`;
const GREEN = `${ESC}[32m`;
const YELLOW = `${ESC}[33m`;
const DIM = `${ESC}[2m`;
const RESET = `${ESC}[0m`;

// Mirrors real OCR output, including ANSI codes and both row shapes.
const REAL_PREVIEW = [
  '',
  `Preview: 30 file(s) changed  |  ${GREEN}+2042${RESET}  ${ESC}[31m-144${RESET}`,
  '',
  `${BOLD}Will review (28):${RESET}`,
  `  ${GREEN}[A]${RESET}  .github/scripts/ocr-reviewed-range.js      ${GREEN}+220 ${RESET} ${ESC}[31m-0   ${RESET}`,
  `  ${YELLOW}[M]${RESET}  .github/workflows/ocr-pr-review.yml        ${GREEN}+140 ${RESET} ${ESC}[31m-35  ${RESET}`,
  '',
  `${BOLD}Excluded from review (2):${RESET}`,
  `  ${YELLOW}[M]${RESET}  workflow/docs/guides/local-ocr-review.md   ${DIM}(unsupported_ext)${RESET}`,
  `  ${GREEN}[A]${RESET}  workflow/project-plans/issue-138/plan.md   ${DIM}(unsupported_ext)${RESET}`,
  '',
].join('\n');

test('parses both section row shapes from real ANSI output', () => {
  const result = parsePreview(REAL_PREVIEW);
  assert.deepStrictEqual(result.reviewed, [
    '.github/scripts/ocr-reviewed-range.js',
    '.github/workflows/ocr-pr-review.yml',
  ]);
  assert.deepStrictEqual(result.excludedPaths, [
    'workflow/docs/guides/local-ocr-review.md',
    'workflow/project-plans/issue-138/plan.md',
  ]);
});

test('exclusion reasons are captured and never leak into paths', () => {
  const result = parsePreview(REAL_PREVIEW);
  for (const entry of result.excluded) {
    assert.strictEqual(entry.reason, 'unsupported_ext');
    assert.ok(!entry.path.includes('('), 'path must not retain the reason');
    assert.ok(!entry.path.includes('unsupported'), 'path must not retain the reason');
  }
});

test('paths containing spaces survive parsing', () => {
  const preview = [
    `${BOLD}Will review (1):${RESET}`,
    `  ${GREEN}[A]${RESET}  docs/my notes/design doc.ts   ${GREEN}+5 ${RESET} ${ESC}[31m-1 ${RESET}`,
    `${BOLD}Excluded from review (1):${RESET}`,
    `  ${GREEN}[A]${RESET}  docs/my notes/read me.md   ${DIM}(unsupported_ext)${RESET}`,
  ].join('\n');
  const result = parsePreview(preview);
  assert.deepStrictEqual(result.reviewed, ['docs/my notes/design doc.ts']);
  assert.deepStrictEqual(result.excludedPaths, ['docs/my notes/read me.md']);
});

test('an excluded row is not parsed with the reviewed row shape', () => {
  // The defect this guards: applying the churn-stripping rule to an excluded
  // row leaves the reason attached to the path.
  const preview = [
    `${BOLD}Excluded from review (1):${RESET}`,
    '  [M]  workflow/docs/guides/local-ocr-review.md   (unsupported_ext)',
  ].join('\n');
  assert.deepStrictEqual(parsePreview(preview).excludedPaths, [
    'workflow/docs/guides/local-ocr-review.md',
  ]);
});

test('rows are attributed to the section that contains them', () => {
  const result = parsePreview(REAL_PREVIEW);
  for (const reviewedPath of result.reviewed) {
    assert.ok(
      !result.excludedPaths.includes(reviewedPath),
      'a path cannot be both reviewed and excluded',
    );
  }
});

test('rows outside any known section are ignored', () => {
  const preview = [
    '  [A]  stray/before/any/section.ts  +1  -0',
    `${BOLD}Unknown section (1):${RESET}`,
    '  [A]  unknown/section/file.ts  +1  -0',
    `${BOLD}Will review (1):${RESET}`,
    '  [A]  real/file.ts  +1  -0',
  ].join('\n');
  const result = parsePreview(preview);
  assert.deepStrictEqual(result.reviewed, ['real/file.ts']);
  assert.deepStrictEqual(result.excludedPaths, []);
});

test('empty, missing, and headerless input yield empty sets', () => {
  for (const input of ['', null, undefined, 'no sections here']) {
    const result = parsePreview(input);
    assert.deepStrictEqual(result.reviewed, []);
    assert.deepStrictEqual(result.excludedPaths, []);
  }
});

test('an empty section produces no rows', () => {
  const preview = `${BOLD}Will review (0):${RESET}\n`;
  assert.deepStrictEqual(parsePreview(preview).reviewed, []);
});

test('duplicate rows are deduplicated', () => {
  const preview = [
    `${BOLD}Will review (2):${RESET}`,
    '  [A]  same/file.ts  +1  -0',
    '  [A]  same/file.ts  +1  -0',
  ].join('\n');
  assert.deepStrictEqual(parsePreview(preview).reviewed, ['same/file.ts']);
});

test('helpers strip ANSI, churn, and reasons independently', () => {
  assert.strictEqual(stripAnsi(`${GREEN}x${RESET}`), 'x');
  assert.strictEqual(stripChurn('a/b.ts   +12  -3'), 'a/b.ts');
  assert.strictEqual(stripChurn('a/b.ts'), 'a/b.ts');
  assert.deepStrictEqual(stripExclusionReason('a/b.md  (unsupported_ext)'), {
    path: 'a/b.md',
    reason: 'unsupported_ext',
  });
  assert.deepStrictEqual(stripExclusionReason('a/b.md'), { path: 'a/b.md', reason: '' });
});
