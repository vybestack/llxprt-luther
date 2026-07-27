'use strict';

const fs = require('fs');
const path = require('path');

// Durable session evidence reader.
//
// The OCR CLI's human-facing output is not a stable contract: `session show`
// advertises a --json flag that it does not honor (verified against 1.7.16),
// and preview listings mix row formats between sections. The session JSONL
// written under the session store IS stable and machine-readable, so it is the
// only source this module reads.
//
// Field names below were verified against real session data rather than
// assumed: completed review events are `{"type":"review_item_done"}` carrying
// a camelCase `filePath`.

const REVIEW_ITEM_DONE = 'review_item_done';
// OCR records a per-file failure as its own event. Ignoring it made every
// failure invisible to the gate, so a run that failed files looked identical to
// one that reviewed nothing.
const REVIEW_ITEM_FAILED = 'review_item_failed';
const SESSION_END = 'session_end';

/**
 * Convert an absolute path into a session-store slug.
 *
 * The store replaces separators with dashes and drops the leading one. This
 * performs no resolution: which path to hand it is the caller's decision, and
 * getting that decision wrong is the defect this module previously carried.
 */
function slugForPath(absolutePath) {
  return absolutePath.replace(/^\//, '').replace(/[/\\]/g, '-');
}

/**
 * Every slug the OCR session store might have used for this workspace, most
 * likely first.
 *
 * The tool derives its store directory from its own working directory via Go's
 * `os.Getwd`, which honours `$PWD` only when `$PWD` names the same directory as
 * the physical cwd, and otherwise falls back to the physical path. So the slug
 * depends on how the process was spawned, not on the path alone. Measured
 * against 1.7.16 from `/tmp/ocrlink`, a symlink to a repo holding three
 * sessions:
 *
 *     PWD=/tmp/ocrlink (shell cd)  -> no sessions   logical alias honoured
 *     PWD unset                    -> 3 sessions    physical fallback
 *     PWD=/tmp, /no/such/dir, ''   -> 3 sessions    not an alias, rejected
 *
 * Deriving a single slug therefore cannot be correct: whichever one this module
 * picked, the other is reachable. Both are returned and the caller uses the
 * store that exists. The earlier version resolved symlinks and documented that
 * the store "keys off the resolved (symlink-free) path", which is the opposite
 * of the aliasing case - a stated justification for the wrong behaviour, which
 * is how it survived.
 *
 * An unresolvable workspace still yields the logical form rather than throwing,
 * so a caller cannot be crashed by an inaccessible workspace.
 */
function sessionSlugCandidatesForWorkspace(workspacePath) {
  const candidates = [];
  const add = (value) => {
    const slug = slugForPath(value);
    if (slug && !candidates.includes(slug)) {
      candidates.push(slug);
    }
  };

  add(path.resolve(workspacePath));
  try {
    add(fs.realpathSync(workspacePath));
  } catch (error) {
    // Not fatal: the logical form is still a valid candidate, and a store
    // written under it is still findable. Warn rather than return nothing,
    // because returning nothing here reports "no evidence" for a review that
    // may well have completed.
    console.warn(
      `ocr-session-evidence: could not resolve workspace ${workspacePath}: ${error.message}`,
    );
  }
  return candidates;
}

/**
 * The slug for this workspace, preferring a store directory that exists.
 *
 * Retained for callers that want a single answer. When no candidate store
 * exists the first candidate is returned so the caller reports a missing store
 * for a real path rather than an empty string.
 */
function sessionSlugForWorkspace(workspacePath, sessionRoot) {
  const candidates = sessionSlugCandidatesForWorkspace(workspacePath);
  if (candidates.length === 0) {
    return '';
  }
  if (sessionRoot) {
    const existing = candidates.find((slug) =>
      fs.existsSync(path.join(sessionRoot, slug)),
    );
    if (existing) {
      return existing;
    }
  }
  return candidates[0];
}

/**
 * Parse a session JSONL file into structured evidence.
 *
 * Malformed lines are skipped rather than aborting the read: a truncated
 * trailing line must not discard the completed work recorded before it.
 */
function readSessionEvidence(sessionFile) {
  const reviewedFiles = new Set();
  const failedFiles = new Set();
  let ended = false;
  let sessionId = '';

  let contents = '';
  try {
    contents = fs.readFileSync(sessionFile, 'utf8');
  } catch {
    return { sessionId: '', reviewedFiles: [], failedFiles: [], eventCount: 0, ended: false };
  }

  let eventCount = 0;
  for (const line of contents.split('\n')) {
    const trimmed = line.trim();
    if (trimmed.length === 0) {
      continue;
    }
    let event;
    try {
      event = JSON.parse(trimmed);
    } catch {
      continue;
    }
    eventCount += 1;
    if (!sessionId && typeof event.sessionId === 'string') {
      sessionId = event.sessionId;
    }
    if (event.type === REVIEW_ITEM_DONE) {
      const filePath = String(event.filePath || event.newPath || '').trim();
      if (filePath.length > 0) {
        reviewedFiles.add(filePath);
      }
    } else if (event.type === REVIEW_ITEM_FAILED) {
      const filePath = String(event.filePath || event.newPath || '').trim();
      if (filePath.length > 0) {
        failedFiles.add(filePath);
      }
    } else if (event.type === SESSION_END) {
      ended = true;
    }
  }

  return {
    sessionId,
    reviewedFiles: [...reviewedFiles],
    failedFiles: [...failedFiles],
    eventCount,
    ended,
  };
}

/**
 * Select the session that holds review evidence.
 *
 * A single OCR invocation writes several session files: version probes and
 * previews produce empty ones. Selection is therefore by content — the session
 * containing review events — never by counting how many files appeared, since
 * any such count breaks as soon as another invocation is added.
 *
 * When `expectedSessionId` is supplied it must match exactly; otherwise the
 * candidate with the most review events wins. Ties fail closed by returning
 * null rather than guessing.
 */
function selectReviewSession(sessionDir, expectedSessionId) {
  let entries = [];
  try {
    entries = fs.readdirSync(sessionDir).filter((name) => name.endsWith('.jsonl'));
  } catch {
    return null;
  }

  const candidates = [];
  for (const entry of entries) {
    const evidence = readSessionEvidence(path.join(sessionDir, entry));
    // A session holding only failures is still review evidence. Requiring a
    // completed file would discard it, making a run that failed every file
    // indistinguishable from one that reviewed nothing.
    if (evidence.reviewedFiles.length === 0 && evidence.failedFiles.length === 0) {
      continue;
    }
    const id = evidence.sessionId || path.basename(entry, '.jsonl');
    candidates.push({ ...evidence, sessionId: id });
  }

  if (candidates.length === 0) {
    return null;
  }

  if (expectedSessionId) {
    return candidates.find((c) => c.sessionId === expectedSessionId) || null;
  }

  // Ranked by distinct paths carrying review evidence, so a failed-only session
  // is not outranked by an emptier one. Counting the union rather than summing
  // avoids double-counting a path that somehow carries both terminal events,
  // which could otherwise manufacture a false tie.
  const weight = (c) => new Set([...c.reviewedFiles, ...c.failedFiles]).size;
  candidates.sort((a, b) => weight(b) - weight(a));
  if (candidates.length > 1 && weight(candidates[0]) === weight(candidates[1])) {
    return null;
  }
  return candidates[0];
}

module.exports = {
  sessionSlugForWorkspace,
  sessionSlugCandidatesForWorkspace,
  readSessionEvidence,
  selectReviewSession,
  REVIEW_ITEM_DONE,
  SESSION_END,
};
