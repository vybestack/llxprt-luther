//! Incremental stdout outcome-marker scanning independent of bounded rendering.
//!
//! Diagnostic stream captures truncate large outputs to a bounded head/tail
//! window, which can omit the middle >64 KiB where an outcome marker may land.
//! This scanner processes every complete stdout line as bytes arrive, carrying
//! partial lines across chunk boundaries, so detection never depends on the
//! bounded render. Marker precedence follows `outcome_on_stdout` iteration
//! order: the first pattern (in insertion order) whose marker matches any seen
//! line wins, matching the original whole-buffer scan semantics.

use std::sync::{Arc, Mutex};

use crate::engine::transition::StepOutcome;

use super::parse_outcome_name;

/// Shared, thread-safe scanner handle for concurrent feeding and querying.
pub(super) type SharedScanner = Arc<Mutex<Option<OutcomeScanner>>>;

/// Incremental outcome-marker scanner for stdout.
pub(super) struct OutcomeScanner {
    /// `(marker, outcome)` pairs in precedence (insertion) order.
    patterns: Vec<(String, StepOutcome)>,
    /// Parallel flags: `seen[index]` is true once `patterns[index].0` matched.
    seen: Vec<bool>,
    /// Trailing bytes not yet terminated by a newline.
    partial: String,
}

impl OutcomeScanner {
    /// Build a scanner from `outcome_on_stdout` parameters. Returns `None`
    /// when no markers are configured (detection stays disabled).
    pub(super) fn from_params(params: &serde_json::Value) -> Option<Self> {
        let pattern_map = params.get("outcome_on_stdout")?.as_object()?;
        let patterns = pattern_map
            .iter()
            .filter_map(|(pattern, outcome_value)| {
                outcome_value
                    .as_str()
                    .map(|name| (pattern.clone(), parse_outcome_name(name)))
            })
            .collect::<Vec<_>>();
        if patterns.is_empty() {
            return None;
        }
        let seen = vec![false; patterns.len()];
        Some(Self {
            patterns,
            seen,
            partial: String::new(),
        })
    }

    /// Feed a chunk of stdout bytes, scanning every newly completed line.
    pub(super) fn append(&mut self, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes);
        self.partial.push_str(&text);
        let mut start = 0;
        let mut matched_indices = Vec::new();
        while let Some(offset) = self.partial[start..].find('\n') {
            let line_end = start + offset;
            let line = self.partial[start..line_end].to_string();
            if let Some(index) = self.match_index(&line) {
                matched_indices.push(index);
            }
            start = line_end + 1;
        }
        if start > 0 {
            self.partial = self.partial[start..].to_string();
        }
        for index in matched_indices {
            self.seen[index] = true;
        }
    }

    /// Flush the trailing partial line (output without a terminating newline).
    pub(super) fn finish(&mut self) {
        if !self.partial.is_empty() {
            let line = std::mem::take(&mut self.partial);
            if let Some(index) = self.match_index(&line) {
                self.seen[index] = true;
            }
        }
    }

    /// Return the precedence index of the first pattern matching `line`, if any.
    fn match_index(&self, line: &str) -> Option<usize> {
        let trimmed = line.trim();
        self.patterns
            .iter()
            .position(|(marker, _)| trimmed == marker)
    }

    /// Resolve the detected outcome using pattern-precedence order: the first
    /// configured marker that matched any seen line wins.
    pub(super) fn detected(&self) -> Option<StepOutcome> {
        self.seen
            .iter()
            .position(|&matched| matched)
            .map(|index| self.patterns[index].1)
    }
}

/// Query the shared scanner for a detected outcome without consuming it.
pub(super) fn detected(shared: &SharedScanner) -> Option<StepOutcome> {
    shared
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().and_then(OutcomeScanner::detected))
}

/// Finish the shared scanner (flush trailing partial line) and return the
/// final detected outcome. The scanner is consumed and cleared.
pub(super) fn finish(shared: &SharedScanner) -> Option<StepOutcome> {
    shared.lock().ok().and_then(|mut guard| {
        guard.take().map(|mut scanner| {
            scanner.finish();
            scanner.detected()
        })?
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scanner(markers: &[(&str, &str)]) -> Option<OutcomeScanner> {
        let mut map = serde_json::Map::new();
        for (pattern, outcome) in markers {
            map.insert(
                (*pattern).to_string(),
                serde_json::Value::String((*outcome).to_string()),
            );
        }
        OutcomeScanner::from_params(&serde_json::json!({ "outcome_on_stdout": map }))
    }

    #[test]
    fn detects_marker_in_omitted_middle_beyond_capture_limit() {
        let mut scanner = scanner(&[("READY", "retryable")]).unwrap();
        // Simulate >64 KiB of filler before the marker so it would land in the
        // omitted middle of a bounded render.
        scanner.append(&vec![b'x'; 70_000]);
        scanner.append(b"\nREADY\n");
        scanner.append(&vec![b'y'; 70_000]);
        scanner.finish();
        assert_eq!(scanner.detected(), Some(StepOutcome::Retryable));
    }

    #[test]
    fn detects_marker_split_across_chunks() {
        let mut scanner = scanner(&[("READY", "retryable")]).unwrap();
        scanner.append(b"prelude\nREA");
        scanner.append(b"DY\n");
        scanner.finish();
        assert_eq!(scanner.detected(), Some(StepOutcome::Retryable));
    }

    #[test]
    fn preserves_pattern_precedence_over_line_order() {
        let mut scanner = scanner(&[("READY", "retryable"), ("DONE", "success")]).unwrap();
        // The serde_json map iteration order is authoritative, matching the
        // original whole-buffer scanner. DONE sorts before READY here.
        scanner.append(b"READY\nDONE\n");
        scanner.finish();
        assert_eq!(scanner.detected(), Some(StepOutcome::Success));
    }

    #[test]
    fn trailing_line_without_newline_is_flushed() {
        let mut scanner = scanner(&[("DONE", "success")]).unwrap();
        scanner.append(b"working\nDONE");
        scanner.finish();
        assert_eq!(scanner.detected(), Some(StepOutcome::Success));
    }

    #[test]
    fn unmatched_output_yields_no_detection() {
        let mut scanner = scanner(&[("READY", "retryable")]).unwrap();
        scanner.append(b"working\nstill working\n");
        scanner.finish();
        assert_eq!(scanner.detected(), None);
    }
}
