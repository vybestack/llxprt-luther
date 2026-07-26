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
const SESSION_END = 'session_end';

/**
 * Convert a workspace path into the slug the OCR session store uses.
 *
 * The store keys off the resolved (symlink-free) path, so `/tmp/...` on macOS
 * becomes `/private/tmp/...`. Resolving here avoids the mismatch that occurs
 * when a caller passes the unresolved path.
 */
function sessionSlugForWorkspace(workspacePath) {
  // An unresolvable path yields '' rather than throwing, so a caller cannot be
  // crashed by an inaccessible workspace and every caller gets the same
  // predictable "no slug" result. Path separators for both conventions are
  // replaced so the slug never retains a separator.
  let resolved;
  try {
    resolved = fs.realpathSync(workspacePath);
  } catch (error) {
    console.warn(
      `ocr-session-evidence: could not resolve workspace ${workspacePath}: ${error.message}`,
    );
    return '';
  }
  return resolved.replace(/^\//, '').replace(/[/\\]/g, '-');
}

/**
 * Parse a session JSONL file into structured evidence.
 *
 * Malformed lines are skipped rather than aborting the read: a truncated
 * trailing line must not discard the completed work recorded before it.
 */
function readSessionEvidence(sessionFile) {
  const reviewedFiles = new Set();
  let ended = false;
  let sessionId = '';

  let contents = '';
  try {
    contents = fs.readFileSync(sessionFile, 'utf8');
  } catch {
    return { sessionId: '', reviewedFiles: [], eventCount: 0, ended: false };
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
    } else if (event.type === SESSION_END) {
      ended = true;
    }
  }

  return {
    sessionId,
    reviewedFiles: [...reviewedFiles],
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
    if (evidence.reviewedFiles.length === 0) {
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

  candidates.sort((a, b) => b.reviewedFiles.length - a.reviewedFiles.length);
  if (
    candidates.length > 1 &&
    candidates[0].reviewedFiles.length === candidates[1].reviewedFiles.length
  ) {
    return null;
  }
  return candidates[0];
}

module.exports = {
  sessionSlugForWorkspace,
  readSessionEvidence,
  selectReviewSession,
  REVIEW_ITEM_DONE,
  SESSION_END,
};
