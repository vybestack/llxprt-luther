'use strict';

const fs = require('fs');
const os = require('os');
const path = require('path');

const { STATUS, resolveCompleteness, computeCoverage } = require('./ocr-completeness');
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

/**
 * Normalize path inputs so every comparison uses the same representation.
 */
function normalizePaths(values) {
  return (Array.isArray(values) ? values : [])
    .map((value) => String(value ?? '').trim())
    .filter((value) => value.length > 0);
}

/**
 * Read an optional artifact.
 *
 * A missing artifact is an expected condition. Any other failure is not, and
 * is surfaced so a permission or IO problem is debuggable rather than looking
 * like an absent file.
 */
function readFileSafe(filePath) {
  try {
    return fs.readFileSync(filePath, 'utf8');
  } catch (error) {
    if (error && error.code !== 'ENOENT') {
      console.warn(`ocr-gate: could not read ${filePath}: ${error.message}`);
    }
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

/**
 * Read one string field from a JSON artifact, or '' when it is absent,
 * unparseable, or not a string.
 */
function readJsonField(filePath, fieldName) {
  const raw = readFileSafe(filePath).trim();
  if (raw.length === 0) {
    return '';
  }
  try {
    const parsed = JSON.parse(raw);
    return typeof parsed[fieldName] === 'string' ? parsed[fieldName] : '';
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
  // Every path is normalized the same way before comparison. Comparing a
  // trimmed set against untrimmed inputs would silently misclassify a file as
  // unreviewed, or fail to match a declared exclusion.
  const changedFiles = normalizePaths(options.changedFiles);
  const preview = parsePreview(options.previewText || '');
  const excludedPaths = new Set(normalizePaths(preview.excludedPaths));

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

  const reviewedSet = new Set(normalizePaths(options.reviewedFiles));
  const unreviewed = selectedFiles.filter((file) => !reviewedSet.has(file));

  return {
    completeness,
    passed: completeness === STATUS.COMPLETE || completeness === STATUS.SKIPPED,
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
  // sessionSlugForWorkspace reports an unresolvable workspace as '' and logs
  // the cause, so no try/catch is needed here.
  const slug = sessionSlugForWorkspace(workspace);
  const sessionDir = slug ? path.join(sessionRoot, slug) : '';

  const expectedSessionId = readJsonField(resultPath, 'session_id');
  const session = sessionDir ? selectReviewSession(sessionDir, expectedSessionId) : null;

  return evaluateGate({
    skipped: options.skipped === true,
    ocrExitCode:
      options.ocrExitCode !== undefined
        ? options.ocrExitCode
        : readExitCode(path.join(artifactDir, 'ocr-exit-code.txt')),
    ocrStatus:
      options.ocrStatus !== undefined ? options.ocrStatus : readJsonField(resultPath, 'status'),
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
