//! The open-code-review contract, captured from the installed binary.
//!
//! Every entry here was verified by running the tool, not by reading its
//! documentation or its issue history. Where the campaign record and the
//! installed binary disagreed, the binary won.
//!
//! To re-capture after a version change, from the repository root:
//!
//! ```text
//! ocr version                > tests/fixtures/tool-contracts/ocr/version.txt
//! ocr session list --json    > tests/fixtures/tool-contracts/ocr/session-list--json.stdout
//! ocr session show ID --json > tests/fixtures/tool-contracts/ocr/session-show--json.stdout
//! (cd /tmp && ocr session list --json) \
//!                            > tests/fixtures/tool-contracts/ocr/session-list--json--foreign-cwd.stdout
//! ```
//!
//! The final capture is deliberately taken from outside the repository: it is
//! the control that demonstrates the lookup keys off the working directory
//! rather than off any argument.

use super::{FlagBehaviour, ResultSource, StateKey, SubcommandContract, ToolContract};

/// Version this contract was captured from and verified against.
pub const PINNED_VERSION: &str = "v1.7.16";

/// The contract for the pinned version.
#[must_use]
pub fn contract() -> ToolContract {
    ToolContract {
        tool: "open-code-review".to_string(),
        version: PINNED_VERSION.to_string(),
        capture_provenance: "captured from the installed binary at /opt/homebrew/bin/ocr by \
                             running each command and recording stdout verbatim; the \
                             foreign-cwd capture was taken from /tmp to isolate the lookup key"
            .to_string(),
        subcommands: vec![
            SubcommandContract {
                subcommand: "session list".to_string(),
                state_key: StateKey::ProcessWorkingDirectory,
                result_source: ResultSource::Stdout,
                flags: vec![("--json".to_string(), FlagBehaviour::Honoured)],
                captured_output: Some("session-list--json.stdout".to_string()),
            },
            SubcommandContract {
                subcommand: "session show".to_string(),
                state_key: StateKey::ProcessWorkingDirectory,
                result_source: ResultSource::DurableArtifact {
                    description: "the session jsonl named by session list's file_path field; \
                                  its session_end record carries files_reviewed as an explicit \
                                  array"
                        .to_string(),
                },
                // Accepted, exits zero, prints a human table anyway. Verified
                // against the installed binary, where a JSON parse of the
                // captured output fails at the first byte.
                flags: vec![(
                    "--json".to_string(),
                    FlagBehaviour::AcceptedAndIgnored {
                        use_instead: "the session jsonl identified by session list --json"
                            .to_string(),
                    },
                )],
                captured_output: Some("session-show--json.stdout".to_string()),
            },
        ],
    }
}
