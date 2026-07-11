// Single source of truth for OCR secret redaction.
//
// This module is loaded via require() from every actions/github-script step
// that emits OCR diagnostics (the sticky-summary posting step, the artifact
// redaction step, and the infrastructure-failure notification job) so the
// redaction patterns are defined exactly once. Previously each step inlined its
// own copy of redactSecretDiagnostics, which meant any new secret format had to
// be added in three places and risked drifting out of sync.
//
// Fork-safety: this file is only ever loaded from the trusted base-branch
// checkout (pull_request_target runs the base workflow, and every job that
// requires it checks out the base SHA — never PR-supplied code). It performs
// pure string transformation with no I/O or process execution.

'use strict';

const REDACTION = '[REDACTED]';

function escapeRegExp(value) {
  return String(value).replace(/[\\^$.*+?()[\]{}|]/g, '\\$&');
}

// Redact known-exact secret values (endpoint/token supplied by the caller) plus
// a set of structural patterns that catch common credential shapes even when
// the exact value is unknown or only partially present in diagnostics.
function redactSecretDiagnostics(value, exactSecrets = []) {
  let sanitized = String(value ?? '');
  const secrets = (Array.isArray(exactSecrets) ? exactSecrets : [exactSecrets])
    .filter((secret) => typeof secret === 'string' && secret.length > 0);
  for (const secret of secrets) {
    try {
      sanitized = sanitized.replace(new RegExp(escapeRegExp(secret), 'g'), REDACTION);
    } catch (_) {
      sanitized = sanitized.split(secret).join(REDACTION);
    }
  }
  return sanitized
    .replace(/\b(Authorization\s*:\s*(?:(?:Bearer|Basic|token|ApiKey)\s+)?)([^\s,;]+)/gi, '$1[REDACTED]')
    .replace(/\b(x-api-key\s*:\s*)([^\s,;]+)/gi, '$1[REDACTED]')
    .replace(/\b(api[_-]?key\s*[=:]\s*)([^\s,;&]+)/gi, '$1[REDACTED]')
    .replace(/([?&](?:key|api[_-]?key|token)=)([^\s,;&]+)/gi, '$1[REDACTED]')
    .replace(/\b(access[_-]?token\s*[=:]\s*)([^\s,;&]+)/gi, '$1[REDACTED]')
    .replace(/\b(refresh[_-]?token\s*[=:]\s*)([^\s,;&]+)/gi, '$1[REDACTED]')
    .replace(/\b(id[_-]?token\s*[=:]\s*)([^\s,;&]+)/gi, '$1[REDACTED]')
    .replace(/\b(token\s*[=:]\s*)([A-Za-z0-9_./+=:@-]{16,})/gi, '$1[REDACTED]')
    .replace(/\b(secret\s*[=:]\s*)([A-Za-z0-9_./+=:@-]{16,})/gi, '$1[REDACTED]');
}

module.exports = { REDACTION, escapeRegExp, redactSecretDiagnostics };
