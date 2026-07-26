'use strict';

const fs = require('fs');
const os = require('os');
const path = require('path');

const { resolveCompleteness, computeCoverage } = require('./ocr-completeness');
const { parsePreview } = require('./ocr-preview');
const { selectReviewSession, sessionSlugForWorkspace } = require('./ocr-session-evidence');

// Reviewed-range gate.
//
// Combines the three deterministic inputs into a single verdict:
//   - the changed-file set for the reviewed range
//   - the preview listing, which declares which files were selected and which
//     were excluded (with a reason)
//   - durable session evidence, which records the files actually reviewed
//
// Files the tool explicitly declined to review are resolved by their declared
// exclusion, not treated as missing coverage. Everything else must be proven
// reviewed, or the verdict degrades from 'complete'.

function readFileSafe(filePath) {
  try {
    return fs.readFileSync(filePath, 'utf8');
  } catch {
    return '';
  }
}

function readExitCode(filePath) {
  const raw = readFileSafe(filePath).trim();
  if (!/^-?\d+$/.test(raw)) {
    return null;
  }
  return Number.parseInt(raw, 10);
}

function readStatus(resultPath) {
  const raw = readFileSafe(resultPath).trim();
  if (raw.length === 0) {
    return '';
  }
  try {
    const parsed = JSON.parse(raw);
    return typeof parsed.status === 'string' ? parsed.status : '';
  } catch {
    return '';
  }
}

function readSessionId(resultPath) {
  const raw = readFileSafe(resultPath).trim();
  if (raw.length === 0) {
    return '';
  }
  try {
    const parsed = JSON.parse(raw);
    return typeof parsed.session_id === 'string' ? parsed.session_id : '';
  } catch {
    return '';
  }
}

/**
 * Build a reviewed-range verdict.
 *
 * `changedFiles` is authoritative for what the range contains. The preview
 * declares selection and exclusions; session evidence proves what was
 * reviewed. Exclusions only excuse files the preview actually declared.
 */
function evaluateGate(params) {
  const options = params || {};
  const changedFiles = Array.isArray(options.changedFiles) ? options.changedFiles : [];
  const preview = parsePreview(options.previewText || '');
  const excludedPaths = new Set(preview.excludedPaths);

  // Selected = changed minus declared exclusions. Falling back to the changed
  // set (rather than the preview's reviewed list) keeps the gate honest when
  // the preview is missing or unparseable: unproven files stay unresolved.
  const selectedFiles = changedFiles.filter((file) => !excludedPaths.has(file));

  const completeness = resolveCompleteness({
    skipped: options.skipped === true,
    ocrExitCode: options.ocrExitCode,
    ocrStatus: options.ocrStatus,
    selectedFiles,
    completedFiles: options.reviewedFiles,
    failedFiles: options.failedFiles,
    reusedFiles: options.reusedFiles,
    waivedFiles: options.waivedFiles,
  });

  const reviewedSet = new Set(
    (Array.isArray(options.reviewedFiles) ? options.reviewedFiles : []).map((file) =>
      String(file || '').trim(),
    ),
  );
  const unreviewed = selectedFiles.filter((file) => !reviewedSet.has(file));

  return {
    completeness,
    passed: completeness === 'complete' || completeness === 'skipped',
    selected: selectedFiles,
    excluded: preview.excluded,
    unreviewed,
    coverage: computeCoverage({
      selected: selectedFiles.length,
      completed: selectedFiles.length - unreviewed.length,
    }),
  };
}

/**
 * Gather gate inputs from a workspace and produce a verdict.
 */
function evaluateWorkspace(params) {
  const options = params || {};
  const workspace = options.workspace || process.cwd();
  const artifactDir = options.artifactDir || workspace;
  const resultPath = options.resultPath || path.join(artifactDir, 'ocr-result.json');

  const sessionRoot =
    options.sessionRoot || path.join(os.homedir(), '.opencodereview', 'sessions');
  let sessionDir = '';
  try {
    sessionDir = path.join(sessionRoot, sessionSlugForWorkspace(workspace));
  } catch {
    sessionDir = '';
  }

  const expectedSessionId = readSessionId(resultPath);
  const session = sessionDir ? selectReviewSession(sessionDir, expectedSessionId) : null;

  return evaluateGate({
    skipped: options.skipped === true,
    ocrExitCode:
      options.ocrExitCode !== undefined
        ? options.ocrExitCode
        : readExitCode(path.join(artifactDir, 'ocr-exit-code.txt')),
    ocrStatus: options.ocrStatus !== undefined ? options.ocrStatus : readStatus(resultPath),
    changedFiles: options.changedFiles,
    previewText:
      options.previewText !== undefined
        ? options.previewText
        : readFileSafe(path.join(artifactDir, 'ocr-preview.txt')),
    reviewedFiles: session ? session.reviewedFiles : [],
    failedFiles: options.failedFiles,
    reusedFiles: options.reusedFiles,
    waivedFiles: options.waivedFiles,
  });
}

module.exports = {
  evaluateGate,
  evaluateWorkspace,
};
