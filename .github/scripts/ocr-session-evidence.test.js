'use strict';

const test = require('node:test');
const assert = require('node:assert');
const fs = require('fs');
const os = require('os');
const path = require('path');

const {
  readSessionEvidence,
  selectReviewSession,
  sessionSlugForWorkspace,
} = require('./ocr-session-evidence');

// Temp dirs are tracked and removed after the run so repeated executions do
// not accumulate directories under the system temp path.
const tempDirs = [];

function makeSessionDir() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'ocr-evidence-'));
  tempDirs.push(dir);
  return dir;
}

test.after(() => {
  for (const dir of tempDirs) {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

function writeSession(dir, id, events) {
  const file = path.join(dir, `${id}.jsonl`);
  fs.writeFileSync(file, events.map((e) => JSON.stringify(e)).join('\n') + '\n');
  return file;
}

function reviewEvent(sessionId, filePath) {
  return { type: 'review_item_done', sessionId, filePath, newPath: filePath };
}

test('reads reviewed paths from the camelCase filePath field', () => {
  const dir = makeSessionDir();
  const file = writeSession(dir, 's1', [
    { type: 'session_start', sessionId: 's1' },
    reviewEvent('s1', 'src/a.ts'),
    reviewEvent('s1', 'src/b.ts'),
    { type: 'session_end', sessionId: 's1' },
  ]);
  const evidence = readSessionEvidence(file);
  assert.deepStrictEqual(evidence.reviewedFiles.sort(), ['src/a.ts', 'src/b.ts']);
  assert.strictEqual(evidence.sessionId, 's1');
  assert.strictEqual(evidence.ended, true);
});

test('duplicate review events for one path are deduplicated', () => {
  const dir = makeSessionDir();
  const file = writeSession(dir, 's1', [
    reviewEvent('s1', 'src/a.ts'),
    reviewEvent('s1', 'src/a.ts'),
  ]);
  assert.deepStrictEqual(readSessionEvidence(file).reviewedFiles, ['src/a.ts']);
});

test('a truncated trailing line does not discard earlier evidence', () => {
  const dir = makeSessionDir();
  const file = path.join(dir, 's1.jsonl');
  fs.writeFileSync(
    file,
    JSON.stringify(reviewEvent('s1', 'src/a.ts')) + '\n{"type":"review_item_do',
  );
  assert.deepStrictEqual(readSessionEvidence(file).reviewedFiles, ['src/a.ts']);
});

test('an unterminated session is reported as not ended', () => {
  const dir = makeSessionDir();
  const file = writeSession(dir, 's1', [reviewEvent('s1', 'src/a.ts')]);
  assert.strictEqual(readSessionEvidence(file).ended, false);
});

test('a missing session file yields empty evidence rather than throwing', () => {
  const evidence = readSessionEvidence('/nonexistent/session.jsonl');
  assert.deepStrictEqual(evidence.reviewedFiles, []);
  assert.strictEqual(evidence.eventCount, 0);
  // Every field must be the empty/false form, so a missing file can never be
  // mistaken for a completed session.
  assert.strictEqual(evidence.sessionId, '');
  assert.strictEqual(evidence.ended, false);
});

test('an unresolvable workspace yields an empty slug rather than throwing', () => {
  assert.strictEqual(sessionSlugForWorkspace('/nonexistent/workspace/path'), '');
});

test('selection ignores empty probe sessions and picks the one with evidence', () => {
  const dir = makeSessionDir();
  writeSession(dir, 'probe-a', []);
  writeSession(dir, 'probe-b', [{ type: 'session_start', sessionId: 'probe-b' }]);
  writeSession(dir, 'real', [reviewEvent('real', 'src/a.ts')]);
  const selected = selectReviewSession(dir);
  assert.strictEqual(selected.sessionId, 'real');
  assert.deepStrictEqual(selected.reviewedFiles, ['src/a.ts']);
});

test('selection is not count-based, so extra empty sessions are harmless', () => {
  const dir = makeSessionDir();
  for (let i = 0; i < 5; i += 1) {
    writeSession(dir, `probe-${i}`, []);
  }
  writeSession(dir, 'real', [reviewEvent('real', 'src/a.ts')]);
  assert.strictEqual(selectReviewSession(dir).sessionId, 'real');
});

test('selection ranks by reviewed files, not by total event volume', () => {
  const dir = makeSessionDir();
  // Empty sessions cannot discriminate: a naive total-event ranking picks the
  // same winner. This session has far more events yet reviewed fewer files, so
  // it must lose to the one that actually reviewed more.
  const noisy = [{ type: 'session_start', sessionId: 'noisy' }];
  for (let i = 0; i < 20; i += 1) {
    noisy.push({ type: 'tool_call', sessionId: 'noisy', index: i });
  }
  noisy.push(reviewEvent('noisy', 'src/only.ts'));
  writeSession(dir, 'noisy', noisy);
  writeSession(dir, 'thorough', [
    reviewEvent('thorough', 'src/a.ts'),
    reviewEvent('thorough', 'src/b.ts'),
  ]);

  const selected = selectReviewSession(dir);
  assert.strictEqual(selected.sessionId, 'thorough');
  assert.ok(
    selected.eventCount < 21,
    'the winner must be the one with more reviewed files, not more events',
  );
});

test('an explicit session id must match exactly', () => {
  const dir = makeSessionDir();
  writeSession(dir, 'wanted', [reviewEvent('wanted', 'src/a.ts')]);
  writeSession(dir, 'other', [reviewEvent('other', 'src/b.ts')]);
  assert.strictEqual(selectReviewSession(dir, 'wanted').sessionId, 'wanted');
  assert.strictEqual(selectReviewSession(dir, 'absent'), null);
});

test('ambiguous equal-evidence candidates fail closed', () => {
  const dir = makeSessionDir();
  writeSession(dir, 'one', [reviewEvent('one', 'src/a.ts')]);
  writeSession(dir, 'two', [reviewEvent('two', 'src/b.ts')]);
  assert.strictEqual(selectReviewSession(dir), null);
});

test('no session containing evidence yields null rather than a false positive', () => {
  const dir = makeSessionDir();
  writeSession(dir, 'probe', []);
  assert.strictEqual(selectReviewSession(dir), null);
  assert.strictEqual(selectReviewSession('/nonexistent/dir'), null);
});

test('the slug resolves symlinks so /tmp and /private/tmp agree', () => {
  const dir = makeSessionDir();
  const slug = sessionSlugForWorkspace(dir);
  assert.ok(!slug.startsWith('-'), 'slug must not retain a leading separator');
  assert.ok(!slug.includes('/'), 'slug must not contain path separators');
  assert.ok(!slug.includes('\\'), 'slug must not contain backslash separators');
  // The resolved path is what the store keys on.
  assert.strictEqual(slug, fs.realpathSync(dir).replace(/^\//, '').replace(/\//g, '-'));
});
