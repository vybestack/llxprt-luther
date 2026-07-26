'use strict';

// OCR preview listing parser.
//
// The preview groups rows under section headers and uses a DIFFERENT row shape
// per section:
//
//   Will review (28):
//     [A]  path/to/file.ts        +220  -0
//   Excluded from review (2):
//     [M]  path/to/file.md        (unsupported_ext)
//
// Rows are therefore parsed according to their section, and the trailing
// churn/reason field is stripped structurally rather than by whitespace column
// position: paths may contain spaces, so splitting on whitespace and taking a
// fixed field is unsound.
//
// All output is ANSI-colored, so escape sequences are removed first.

// eslint-disable-next-line no-control-regex
const ANSI_PATTERN = /\u001b\[[0-9;]*m/g;

const SECTION_REVIEWED = 'reviewed';
const SECTION_EXCLUDED = 'excluded';

function stripAnsi(text) {
  return String(text ?? '').replace(ANSI_PATTERN, '');
}

/**
 * Identify a section header, returning the section it opens or null.
 */
function detectSection(line) {
  const normalized = stripAnsi(line).trim();
  if (/^will review\s*\(\d+\)\s*:$/i.test(normalized)) {
    return SECTION_REVIEWED;
  }
  if (/^excluded from review\s*\(\d+\)\s*:$/i.test(normalized)) {
    return SECTION_EXCLUDED;
  }
  // Any other "Header (n):" line ends the current section rather than
  // silently absorbing unrelated rows.
  if (/^[a-z][a-z ]*\(\d+\)\s*:$/i.test(normalized)) {
    return null;
  }
  return undefined;
}

/**
 * Remove the trailing churn columns ("+12  -3") from a reviewed row.
 */
function stripChurn(text) {
  return text.replace(/\s*[+-]\d+\s*(?:[+-]\d+\s*)*$/, '').trim();
}

/**
 * Remove the trailing parenthesised exclusion reason from an excluded row.
 */
function stripExclusionReason(text) {
  const match = text.match(/^(.*?)\s*\(([^()]*)\)\s*$/);
  if (!match) {
    return { path: text.trim(), reason: '' };
  }
  return { path: match[1].trim(), reason: match[2].trim() };
}

/**
 * Parse one row into its path (and reason, for excluded rows).
 *
 * Returns null when the line is not a file row.
 */
function parseRow(line, section) {
  const normalized = stripAnsi(line);
  const match = normalized.match(/^\s+\[([A-Z])\]\s+(.*)$/);
  if (!match) {
    return null;
  }
  const status = match[1];
  const remainder = match[2];
  if (section === SECTION_EXCLUDED) {
    const { path, reason } = stripExclusionReason(remainder);
    return path.length > 0 ? { status, path, reason } : null;
  }
  const path = stripChurn(remainder);
  return path.length > 0 ? { status, path, reason: '' } : null;
}

/**
 * Parse a preview listing into reviewed and excluded path sets.
 *
 * Unknown sections are ignored rather than being merged into either set.
 */
function parsePreview(text) {
  const reviewed = [];
  const excluded = [];
  let section = null;

  for (const line of String(text ?? '').split('\n')) {
    const detected = detectSection(line);
    if (detected !== undefined) {
      section = detected;
      continue;
    }
    if (section === null) {
      continue;
    }
    const row = parseRow(line, section);
    if (!row) {
      continue;
    }
    if (section === SECTION_EXCLUDED) {
      excluded.push({ path: row.path, reason: row.reason });
    } else {
      reviewed.push(row.path);
    }
  }

  return {
    reviewed: [...new Set(reviewed)],
    excluded,
    excludedPaths: [...new Set(excluded.map((entry) => entry.path))],
  };
}

// parsePreview is the module's public API. The strip* helpers are exported
// only because they carry parsing rules worth unit-testing directly; parseRow
// and the section constants are internal and are not exported.
module.exports = {
  parsePreview,
  stripAnsi,
  stripChurn,
  stripExclusionReason,
};
