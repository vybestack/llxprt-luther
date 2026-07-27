'use strict';

const fs = require('fs');
const os = require('os');
const path = require('path');

const {
  STATUS,
  collectValidWaivers,
  resolveCompleteness,
  computeCoverage,
  normalizePaths,
} = require('./ocr-completeness');
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
/**
 * Convert one exclusion glob to a RegExp.
 *
 * Supports the subset the OCR rules actually use: `**` across separators, `*`
 * within a segment, `?`, and `{a,b}` alternation. Every other character is
 * escaped, so a malformed pattern cannot become a catch-all that silently
 * excludes source files.
 */
function globToRegExp(glob) {
  let out = '';
  for (let i = 0; i < glob.length; i += 1) {
    const ch = glob[i];
    if (ch === '*') {
      if (glob[i + 1] === '*') {
        // `**/` also matches zero directories, so `**/*.md` matches `a.md`.
        if (glob[i + 2] === '/') {
          out += '(?:.*/)?';
          i += 2;
        } else {
          out += '.*';
          i += 1;
        }
      } else {
        out += '[^/]*';
      }
    } else if (ch === '?') {
      out += '[^/]';
    } else if (ch === '{') {
      out += '(?:';
    } else if (ch === '}') {
      out += ')';
    } else if (ch === ',') {
      out += '|';
    } else {
      out += ch.replace(/[.+^${}()|[\]\\]/g, '\\$&');
    }
  }
  return new RegExp(`^${out}$`);
}

/**
 * Paths matching any configured exclusion glob.
 *
 * A non-array or empty pattern list excludes nothing, which keeps unproven
 * files selected rather than silently resolving them.
 */
function matchGlobs(paths, globs) {
  if (!Array.isArray(globs) || globs.length === 0) {
    return [];
  }
  const patterns = [];
  for (const glob of globs) {
    const text = String(glob || '').trim();
    if (text.length === 0) {
      continue;
    }
    try {
      patterns.push(globToRegExp(text));
    } catch {
      // An uncompilable pattern must not exclude anything.
      continue;
    }
  }
  return paths.filter((file) => patterns.some((pattern) => pattern.test(file)));
}

function evaluateGate(params) {
  const options = params || {};
  // Every path is normalized the same way before comparison. Comparing a
  // trimmed set against untrimmed inputs would silently misclassify a file as
  // unreviewed, or fail to match a declared exclusion.
  const changedFiles = normalizePaths(options.changedFiles);
  const preview = parsePreview(options.previewText || '');
  const excludedPaths = new Set(normalizePaths(preview.excludedPaths));

  // Exclusions are also derived directly from the configured rules, not only
  // from the preview. OCR emits no preview when it selects nothing, so on a
  // documentation-only range the preview is empty and the configured
  // exclusions would otherwise never reach the gate, leaving prose files
  // permanently unresolvable.
  //
  // These globs come from the workflow definition, which pull_request_target
  // loads from the base branch, so a pull request cannot widen its own
  // exclusions.
  const ruleExcluded = matchGlobs(changedFiles, options.excludeGlobs);
  for (const file of ruleExcluded) {
    excludedPaths.add(file);
  }

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
  // The report must use the same notion of "resolved" as the verdict.
  // resolveCompleteness counts reused and validly waived files as resolved, so
  // omitting them here would report a file as unreviewed while the gate passes.
  //
  // Waivers are deliberately filtered through the same validator rather than
  // trusted as given: a waiver only counts when it carries a reason and names a
  // file that actually failed. Accepting the raw list would let an arbitrary
  // path silently excuse an unreviewed file.
  const resolvedSet = new Set([
    ...reviewedSet,
    ...normalizePaths(options.reusedFiles),
    ...collectValidWaivers(options.waivedFiles, normalizePaths(options.failedFiles)),
  ]);
  const unreviewed = selectedFiles.filter((file) => !resolvedSet.has(file));

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
    excludeGlobs: options.excludeGlobs,
    failedFiles: session ? session.failedFiles : options.failedFiles,
    reusedFiles: options.reusedFiles,
    waivedFiles: options.waivedFiles,
  });
}

module.exports = {
  evaluateGate,
  evaluateWorkspace,
};
