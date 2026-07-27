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
// Rule-derived exclusion deliberately does NOT reimplement OCR's matcher.
//
// Reproducing another tool's glob semantics is a losing game: any divergence
// where this matcher is broader than OCR's silently drops a file from review.
// Instead only one exact, unambiguous shape is recognised --
//
//     **/*.<extension>
//
// -- which covers every exclusion that needs to reach the gate (OCR emits no
// preview when it selects nothing, which happens precisely for whole-file-type
// exclusions like documentation). Anything else is refused and therefore
// excludes nothing, so an unsupported pattern can only make the gate stricter.
//
// Matching is case-insensitive on the extension because OCR lowercases both
// patterns and paths; a case-sensitive test here would leave `README.MD`
// selected and unreviewable.
const EXTENSION_RULE = /^\*\*\/\*(\.[A-Za-z0-9_-]+)$/;

/**
 * Extensions from patterns of the exact form `**\/*.ext`.
 *
 * Every other pattern -- braces, commas, character classes, escapes, embedded
 * `**`, directory anchors -- is ignored rather than approximated.
 */
function supportedExtensions(globs) {
  const extensions = [];
  if (!Array.isArray(globs)) {
    return extensions;
  }
  for (const glob of globs) {
    const match = EXTENSION_RULE.exec(String(glob ?? ''));
    if (match) {
      extensions.push(match[1].toLowerCase());
    }
  }
  return extensions;
}

/**
 * Paths whose extension is excluded by a supported rule.
 *
 * The path is compared exactly as Git reported it. Trailing whitespace is
 * significant in a filename, so `evil.rs.md ` does not end with `.md` and stays
 * selected -- trimming here would exclude a file OCR never excluded.
 */
function matchGlobs(paths, globs) {
  const extensions = supportedExtensions(globs);
  if (extensions.length === 0) {
    return [];
  }
  return paths.filter((file) => {
    const lower = file.toLowerCase();
    return extensions.some((extension) => {
      if (!lower.endsWith(extension)) {
        return false;
      }
      // Must be a real extension, not a whole filename: `.md` alone is a
      // dotfile, not a markdown document.
      const base = file.slice(file.lastIndexOf('/') + 1);
      return base.length > extension.length;
    });
  });
}

function evaluateGate(params) {
  const options = params || {};
  // Every path is normalized the same way before comparison. Comparing a
  // trimmed set against untrimmed inputs would silently misclassify a file as
  // unreviewed, or fail to match a declared exclusion.
  // Changed paths are NOT trimmed. Whitespace is significant in a filename, so
  // normalizing here would let `evil.rs.md ` match a markdown exclusion that
  // OCR never applied, dropping a source file from review. Only empty entries
  // are discarded, and duplicates collapsed.
  const changedFiles = [
    ...new Set(
      (Array.isArray(options.changedFiles) ? options.changedFiles : [])
        .map((entry) => String(entry ?? ''))
        .filter((entry) => entry.length > 0),
    ),
  ];
  const preview = parsePreview(options.previewText || '');
  const previewExcludedPaths = new Set(normalizePaths(preview.excludedPaths));
  const excludedPaths = new Set(previewExcludedPaths);

  // Exclusions are also derived directly from the configured rules, not only
  // from the preview. OCR emits no preview when it selects nothing, so on a
  // documentation-only range the preview is empty and the configured
  // exclusions would otherwise never reach the gate, leaving prose files
  // permanently unresolvable.
  //
  // These globs come from the workflow definition, which pull_request_target
  // loads from the base branch, so a pull request cannot widen its own
  // exclusions.
  // Rule-derived exclusions match the path exactly as Git reported it, with no
  // trimming. These are inferred rather than declared by OCR, so a path must
  // not be able to acquire an exclusion it does not literally have.
  const ruleExcluded = matchGlobs(changedFiles, options.excludeGlobs);
  const ruleExcludedSet = new Set(ruleExcluded);

  // Preview exclusions are compared after trimming, because OCR named those
  // paths explicitly and the preview is rendered text whose surrounding
  // whitespace is a formatting artifact rather than part of the name.
  const isExcluded = (file) =>
    ruleExcludedSet.has(file) || excludedPaths.has(file) || excludedPaths.has(file.trim());

  // Selected = changed minus declared exclusions. Falling back to the changed
  // set (rather than the preview's reviewed list) keeps the gate honest when
  // the preview is missing or unparseable: unproven files stay unresolved.
  const selectedFiles = changedFiles.filter((file) => !isExcluded(file));

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
  // Resolution accepts the trimmed form: review evidence names a file that was
  // actually examined, so matching it more loosely cannot cause a file to
  // escape review -- unlike exclusion, where looseness is a bypass.
  const unreviewed = selectedFiles.filter(
    (file) => !resolvedSet.has(file) && !resolvedSet.has(file.trim()),
  );

  return {
    completeness,
    passed: completeness === STATUS.COMPLETE || completeness === STATUS.SKIPPED,
    selected: selectedFiles,
    // Every exclusion that affected the decision, so the reported count cannot
    // disagree with the verdict. Rule-derived entries carry their own reason
    // rather than borrowing the preview's.
    excluded: [
      ...preview.excluded,
      ...ruleExcluded
        .filter((file) => !previewExcludedPaths.has(file))
        .map((file) => ({ path: file, reason: 'excluded_by_configured_rule' })),
    ],
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
