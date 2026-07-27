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
  sessionSlugCandidatesForWorkspace,
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

test('an unresolvable workspace yields its logical slug rather than throwing', () => {
  // Previously this returned '' and the caller reported no evidence. An
  // unresolvable path is still a real path the tool may have keyed on, so the
  // logical form is returned and the caller reports a missing store instead.
  assert.strictEqual(
    sessionSlugForWorkspace('/nonexistent/workspace/path'),
    'nonexistent-workspace-path',
  );
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
  // Compare the two candidates directly rather than against a literal event
  // count, which would break if the fixture gained unrelated events.
  const noisyEvidence = readSessionEvidence(path.join(dir, 'noisy.jsonl'));
  assert.ok(
    selected.eventCount < noisyEvidence.eventCount,
    'the winner must be the one with more reviewed files, not more events',
  );
  assert.ok(
    selected.reviewedFiles.length > noisyEvidence.reviewedFiles.length,
    'the winner must be the one that reviewed more files',
  );
});

test('a renamed file falls back to newPath when filePath is absent', () => {
  // The shared helper always sets both fields, so the fallback branch would
  // otherwise never run and a regression in it would go unnoticed.
  const dir = makeSessionDir();
  const file = writeSession(dir, 'renamed', [
    {
      type: 'review_item_done',
      sessionId: 'renamed',
      oldPath: 'src/old.ts',
      newPath: 'src/new.ts',
    },
  ]);
  assert.deepStrictEqual(readSessionEvidence(file).reviewedFiles, ['src/new.ts']);
});

test('an existing but empty session file yields no evidence', () => {
  const dir = makeSessionDir();
  const file = path.join(dir, 'empty.jsonl');
  fs.writeFileSync(file, '');
  const evidence = readSessionEvidence(file);
  assert.deepStrictEqual(evidence.reviewedFiles, []);
  assert.strictEqual(evidence.eventCount, 0);
  assert.strictEqual(evidence.ended, false);
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

test('a slug is a separator-free key', () => {
  const dir = makeSessionDir();
  const slug = sessionSlugForWorkspace(dir);
  assert.ok(!slug.startsWith('-'), 'slug must not retain a leading separator');
  assert.ok(!slug.includes('/'), 'slug must not contain path separators');
  assert.ok(!slug.includes('\\'), 'slug must not contain backslash separators');
});

// --- store slug resolution ------------------------------------------------
//
// The tool derives its store directory from its own working directory via Go's
// os.Getwd, which honours $PWD only when $PWD names the same directory as the
// physical cwd. So the slug depends on how the tool's process was spawned, and
// either form is reachable. Measured against 1.7.16 from a symlinked workspace:
// with a symlinked $PWD the tool found no sessions where the physical path held
// three, and with $PWD unset or non-aliasing it found all three.
//
// The earlier implementation resolved symlinks and asserted that the resolved
// path "is what the store keys on". These two tests fail that implementation,
// which is the point: a fix handling only one form is the same defect mirrored.

// A symlinked workspace plus an empty store root, so a test can create exactly
// one of the two slug directories and assert which one is chosen.
// Creating a symlink on Windows needs elevated privileges or developer mode, so
// these tests would fail there for a reason unrelated to what they check. CI is
// ubuntu-only today, so this is a guard against a future runner rather than an
// observed failure.
const symlinksAvailable = process.platform !== 'win32';

// The store's slug transformation, written out here rather than imported so a
// mistake in the module under test cannot agree with itself. Takes the path as
// given: callers that need a resolved path resolve it themselves, so which
// form is being asserted stays visible at the call site, which is the whole
// subject of these tests.
const slugOf = (value) => value.replace(/^\//, '').replace(/[/\\]/g, '-');

function makeSymlinkedWorkspace(suffix) {
  // Registered for the global cleanup before the symlink is attempted: if that
  // throws, the returned cleanup never runs and the directories would leak.
  const workspace = fs.mkdtempSync(path.join(os.tmpdir(), 'ocr-ws-'));
  tempDirs.push(workspace);
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ocr-root-'));
  tempDirs.push(root);

  const link = path.join(os.tmpdir(), `ocr-link-${Date.now()}-${suffix}`);
  fs.symlinkSync(workspace, link);
  tempDirs.push(link);

  return {
    link,
    root,
    logical: slugOf(path.resolve(link)),
    physical: slugOf(fs.realpathSync(link)),
  };
}

test('a store written under the physical slug is found through a symlink', { skip: !symlinksAvailable }, () => {
  const ws = makeSymlinkedWorkspace('a');
  assert.notStrictEqual(ws.logical, ws.physical, 'the symlink must alias a different path');
  // The workspace is reached through the symlink, but the tool wrote its store
  // under the physical name. Deriving the logical slug alone misses it.
  fs.mkdirSync(path.join(ws.root, ws.physical), { recursive: true });
  assert.strictEqual(sessionSlugForWorkspace(ws.link, ws.root), ws.physical);
});

test('a store written under the logical slug is found when the path resolves elsewhere', { skip: !symlinksAvailable }, () => {
  const ws = makeSymlinkedWorkspace('b');
  // Only the logical form exists, which is what happens when the tool ran with
  // a symlinked $PWD that aliased its cwd. Resolving symlinks misses it.
  fs.mkdirSync(path.join(ws.root, ws.logical), { recursive: true });
  assert.strictEqual(sessionSlugForWorkspace(ws.link, ws.root), ws.logical);
});

test('both slug forms are offered as candidates for a symlinked workspace', { skip: !symlinksAvailable }, () => {
  const ws = makeSymlinkedWorkspace('c');
  const candidates = sessionSlugCandidatesForWorkspace(ws.link);
  assert.ok(candidates.includes(ws.logical), 'the logical form must be a candidate');
  assert.ok(candidates.includes(ws.physical), 'the physical form must be a candidate');
});

test('a path with no symlink yields one deduplicated candidate', () => {
  // The two derivations agree here, and a caller checking a list must not probe
  // the same store directory twice or report two candidates where there is one.
  const dir = makeSessionDir();
  const candidates = sessionSlugCandidatesForWorkspace(fs.realpathSync(dir));
  assert.strictEqual(candidates.length, 1, `expected a single candidate, got ${candidates}`);
});

test('a nonexistent path still yields its logical candidate', () => {
  // realpathSync throws here. The logical form is still the store the tool would
  // have keyed on, so returning nothing would report no evidence for a review
  // that may have completed.
  const candidates = sessionSlugCandidatesForWorkspace('/nonexistent/workspace/path');
  assert.deepStrictEqual(candidates, ['nonexistent-workspace-path']);
});

test('a broken symlink yields its logical candidate without throwing', { skip: !symlinksAvailable }, () => {
  const missing = path.join(os.tmpdir(), `ocr-missing-${Date.now()}`);
  const link = path.join(os.tmpdir(), `ocr-broken-${Date.now()}`);
  fs.symlinkSync(missing, link);
  tempDirs.push(link);
  const candidates = sessionSlugCandidatesForWorkspace(link);
  assert.deepStrictEqual(candidates, [
    slugOf(path.resolve(link)),
  ]);
});

// --- the workspace is below the repository root ---------------------------
//
// The writer keys on the repository root; the reader is handed whatever path
// its caller supplies. Those coincide for every caller today, which is why no
// test above covers the case where they do not.

function makeRepoWithSubdir() {
  const repo = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'ocr-repo-')));
  tempDirs.push(repo);
  fs.mkdirSync(path.join(repo, '.git'));
  const nested = path.join(repo, 'packages', 'thing');
  fs.mkdirSync(nested, { recursive: true });
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ocr-root-'));
  tempDirs.push(root);
  return { repo, nested, root, repoSlug: slugOf(repo), nestedSlug: slugOf(nested) };
}

test('a workspace below the repository root offers the root slug too', () => {
  const { nested, repoSlug, nestedSlug } = makeRepoWithSubdir();
  const candidates = sessionSlugCandidatesForWorkspace(nested);
  assert.ok(
    candidates.includes(nestedSlug),
    'the given path stays a candidate: a store may exist under it',
  );
  assert.ok(
    candidates.includes(repoSlug),
    'the repository root is where the writer keys its store',
  );
});

test('a store written by the writer is found from a subdirectory', () => {
  const { nested, root, repoSlug } = makeRepoWithSubdir();
  // Only the root store exists, which is what a review invoked anywhere in
  // the repository produces.
  fs.mkdirSync(path.join(root, repoSlug));
  assert.strictEqual(sessionSlugForWorkspace(nested, root), repoSlug);
});

test('a worktree root is not mistaken for the repository enclosing it', () => {
  const { repo } = makeRepoWithSubdir();
  const worktree = path.join(repo, 'wt');
  fs.mkdirSync(worktree);
  // In a linked worktree `.git` is a file, not a directory. Treating only
  // directories as roots would walk past this one and key on `repo`.
  fs.writeFileSync(path.join(worktree, '.git'), 'gitdir: /elsewhere/.git/worktrees/wt\n');
  // Assert on a NESTED path, not on the worktree itself: the worktree's own
  // slug is already present as the given path, so asserting on it would pass
  // whatever the walk-up decided. Only a subdirectory forces the walk to
  // choose, and choosing `repo` over `worktree` is the failure being excluded.
  const inside = path.join(worktree, 'src');
  fs.mkdirSync(inside);
  const candidates = sessionSlugCandidatesForWorkspace(inside);
  assert.ok(
    candidates.includes(slugOf(worktree)),
    'the worktree root is its own repository root, and `.git` there is a file',
  );
  assert.ok(
    !candidates.includes(slugOf(repo)),
    'the walk must stop at the worktree, not continue to the repository around it',
  );
});

test('a workspace outside any repository yields no root candidate', () => {
  const outside = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'ocr-bare-')));
  tempDirs.push(outside);
  const candidates = sessionSlugCandidatesForWorkspace(outside);
  // os.tmpdir() is not inside a repository, so the walk reaches the filesystem
  // root and stops. Reaching it must not add a slug for `/` or throw.
  assert.deepStrictEqual(candidates, [slugOf(outside)]);
});

// --- failed review events -------------------------------------------------
//
// OCR records a per-file failure as its own event type. These were ignored
// entirely, so failedFiles was always empty in production: a run that failed
// every file looked exactly like one that reviewed nothing.

function failedEvent(sessionId, filePath, extra) {
  return { type: 'review_item_failed', sessionId, filePath, ...(extra || {}) };
}

test('failed review events are recorded as failed files', () => {
  const dir = makeSessionDir();
  const file = writeSession(dir, 's-failed', [
    reviewEvent('s-failed', 'src/a.rs'),
    failedEvent('s-failed', 'src/b.rs'),
  ]);
  const evidence = readSessionEvidence(file);
  assert.deepStrictEqual(evidence.reviewedFiles, ['src/a.rs']);
  assert.deepStrictEqual(evidence.failedFiles, ['src/b.rs']);
});

test('a failed event falls back to newPath when filePath is absent', () => {
  const dir = makeSessionDir();
  const file = writeSession(dir, 's-newpath', [
    { type: 'review_item_failed', sessionId: 's-newpath', newPath: 'src/renamed.rs' },
  ]);
  assert.deepStrictEqual(readSessionEvidence(file).failedFiles, ['src/renamed.rs']);
});

test('a session holding only failures is still selected as evidence', () => {
  // Requiring a completed file discarded these sessions, so the gate saw no
  // evidence at all and a wholly failed run could look like a clean skip.
  const dir = makeSessionDir();
  writeSession(dir, 's-onlyfailed', [
    failedEvent('s-onlyfailed', 'src/a.rs'),
    { type: 'session_end', sessionId: 's-onlyfailed' },
  ]);
  const session = selectReviewSession(dir);
  assert.notStrictEqual(session, null);
  assert.deepStrictEqual(session.reviewedFiles, []);
  assert.deepStrictEqual(session.failedFiles, ['src/a.rs']);
});

test('a failed-only session is found by explicit session id', () => {
  const dir = makeSessionDir();
  writeSession(dir, 's-target', [failedEvent('s-target', 'src/a.rs')]);
  writeSession(dir, 's-other', [reviewEvent('s-other', 'src/b.rs')]);
  const session = selectReviewSession(dir, 's-target');
  assert.strictEqual(session.sessionId, 's-target');
  assert.deepStrictEqual(session.failedFiles, ['src/a.rs']);
});

test('ranking counts failed evidence, not just reviewed evidence', () => {
  const dir = makeSessionDir();
  writeSession(dir, 's-small', [reviewEvent('s-small', 'src/a.rs')]);
  writeSession(dir, 's-big', [
    failedEvent('s-big', 'src/b.rs'),
    failedEvent('s-big', 'src/c.rs'),
  ]);
  assert.strictEqual(selectReviewSession(dir).sessionId, 's-big');
});

test('equal evidence weight remains ambiguous and fails closed', () => {
  const dir = makeSessionDir();
  writeSession(dir, 's-one', [reviewEvent('s-one', 'src/a.rs')]);
  writeSession(dir, 's-two', [failedEvent('s-two', 'src/b.rs')]);
  assert.strictEqual(selectReviewSession(dir), null);
});

test('a path with both terminal events is counted once', () => {
  // Summing the two lists would double-count and could manufacture a false
  // tie against a genuinely larger session.
  const dir = makeSessionDir();
  writeSession(dir, 's-dup', [
    reviewEvent('s-dup', 'src/a.rs'),
    failedEvent('s-dup', 'src/a.rs'),
  ]);
  writeSession(dir, 's-real', [
    reviewEvent('s-real', 'src/x.rs'),
    reviewEvent('s-real', 'src/y.rs'),
  ]);
  assert.strictEqual(selectReviewSession(dir).sessionId, 's-real');
});
