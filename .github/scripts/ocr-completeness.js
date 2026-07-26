'use strict';

// Reviewed-range completeness classification.
//
// Ported from vybestack/llxprt-code PR #2716 (closing their issue #2575),
// which is the same problem as luther issue #138: OCR can publish useful
// findings from a partial run without proving every selected file completed,
// so parseable exit-zero output is easy to mistake for complete coverage.
//
// The classification is deliberately set-based rather than count-based: counts
// can coincidentally match while the underlying paths differ, and any count of
// incidental artifacts breaks as soon as another invocation is added.
//
// Fail-closed: only positive proof of completeness yields 'complete'. Every
// unrecognized, empty, or missing signal degrades to 'partial'.
/**
 * Verdict vocabulary. Shared so a typo in one module cannot silently break a
 * comparison in another.
 */
const STATUS = Object.freeze({
  SKIPPED: 'skipped',
  FAILED: 'failed',
  PARTIAL: 'partial',
  COMPLETE: 'complete',
});


/**
 * Normalize a path array: coerce to string, trim, drop empties, dedupe.
 * Non-array input yields an empty array rather than throwing.
 */
function normalizePaths(value) {
  if (!Array.isArray(value)) {
    return [];
  }
  return [
    ...new Set(
      value.map((entry) => String(entry || '').trim()).filter((entry) => entry.length > 0),
    ),
  ];
}

/**
 * Collect waiver paths that are actually valid.
 *
 * A waiver only counts when it carries a non-empty reason AND its path is in
 * the failed set; a waiver for a file that did not fail is irrelevant and must
 * not be allowed to resolve a selected file.
 */
function collectValidWaivers(waivedFiles, failedSet) {
  const failedLookup = new Set(failedSet);
  const validWaiverPaths = new Set();
  const entries = Array.isArray(waivedFiles) ? waivedFiles : [];
  for (const entry of entries) {
    let pathStr = '';
    let reasonStr = '';
    if (typeof entry === 'string') {
      pathStr = entry.trim();
    } else if (entry && typeof entry === 'object') {
      pathStr = String(entry.path || '').trim();
      reasonStr = String(entry.reason || '').trim();
    } else {
      pathStr = String(entry || '').trim();
    }
    if (pathStr.length > 0 && reasonStr.length > 0 && failedLookup.has(pathStr)) {
      validWaiverPaths.add(pathStr);
    }
  }
  return validWaiverPaths;
}

/**
 * Classify reviewed-range completeness as 'skipped', 'failed', 'partial', or
 * 'complete'.
 *
 * Only a recognized success status combined with full set coverage yields
 * 'complete'.
 */
function resolveCompleteness(params) {
  const options = params || {};
  if (options.skipped === true) {
    return STATUS.SKIPPED;
  }

  // A missing, non-integer, or negative exit code means the result cannot be
  // trusted; treat it as a failure rather than coercing it to zero.
  const rawExitCode = options.ocrExitCode;
  if (!Number.isInteger(rawExitCode) || rawExitCode < 0) {
    return STATUS.FAILED;
  }
  if (rawExitCode !== 0) {
    return STATUS.FAILED;
  }

  // Positive allowlist: anything other than a recognized success status lacks
  // proof of completeness.
  const status = typeof options.ocrStatus === 'string' ? options.ocrStatus : '';
  if (status !== 'success' && status !== 'completed') {
    return STATUS.PARTIAL;
  }

  const selectedSet = normalizePaths(options.selectedFiles);
  const completedSet = normalizePaths(options.completedFiles);
  const failedSet = normalizePaths(options.failedFiles);
  const reusedSet = normalizePaths(options.reusedFiles);
  const validWaiverPaths = collectValidWaivers(options.waivedFiles, failedSet);

  const resolvedSet = new Set([...completedSet, ...reusedSet, ...validWaiverPaths]);

  // Every selected file must be resolved.
  for (const selectedPath of selectedSet) {
    if (!resolvedSet.has(selectedPath)) {
      return STATUS.PARTIAL;
    }
  }

  // No unresolved failures. A file appearing in both completed and failed is
  // considered resolved, because completion wins.
  // resolvedSet already contains validWaiverPaths by construction, so
  // membership in it is the whole test.
  for (const failedPath of failedSet) {
    if (!resolvedSet.has(failedPath)) {
      return STATUS.PARTIAL;
    }
  }

  // A completed file outside the selected set indicates an inflated completed
  // set, which could otherwise mask a missing selected file.
  const selectedLookup = new Set(selectedSet);
  for (const completedPath of completedSet) {
    if (!selectedLookup.has(completedPath)) {
      return STATUS.PARTIAL;
    }
  }

  return STATUS.COMPLETE;
}

/**
 * Coverage ratio for reporting. An empty selection is fully covered.
 */
function computeCoverage(params) {
  const options = params || {};
  const selected = Number(options.selected) || 0;
  const completed = Number(options.completed) || 0;
  const ratio = selected === 0 ? 1 : completed / selected;
  return { completed, selected, ratio: String(ratio) };
}

module.exports = {
  STATUS,
  collectValidWaivers,
  resolveCompleteness,
  computeCoverage,
  normalizePaths,
};
