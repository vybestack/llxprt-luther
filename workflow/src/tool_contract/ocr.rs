//! The open-code-review contract, captured from the installed binary.
//!
//! Every entry here was verified by running the tool, not by reading its
//! documentation or its issue history. Where the campaign record and the
//! installed binary disagreed, the binary won.
//!
//! To re-capture after a version change, from the `workflow/` directory, with
//! `REPO` set to the repository root:
//!
//! ```text
//! D=tests/fixtures/tool-contracts/ocr
//! ocr version                                      > $D/version.txt
//! ocr session list --json                          > $D/session-list--json.stdout
//! ocr session show ID --json                       > $D/session-show--json.stdout
//! (cd /tmp && ocr session list --json)             > $D/session-list--json--foreign-cwd.stdout
//! (cd /tmp && ocr session list --json --repo $REPO) > $D/session-list--json--foreign-cwd-with-repo.stdout
//! (cd /tmp && ocr session show ID --repo $REPO)    > $D/session-show--foreign-cwd-with-repo.stderr 2>&1
//! ```
//!
//! The last three form the discriminating experiment. Varying only the working
//! directory cannot distinguish "keys on the working directory" from "defaults
//! to the working directory when no path argument is given" — both predict the
//! same result. Adding `--repo` separates them, and the answer differs by
//! subcommand: `session list` honours it, `session show` does not.
//!
//! Then refresh the digests:
//!
//! ```text
//! (cd tests/fixtures/tool-contracts/ocr && shasum -a 256 *)
//! ```

use super::{
    Capture, FlagBehaviour, RequiredField, ResultSource, StateKey, SubcommandContract, ToolContract,
};
use std::collections::BTreeMap;

/// Exact version token this contract was captured from and verified against.
///
/// Kept in step with the `OCR_VERSION` the review workflow installs; a test
/// fails if they diverge, so upgrading the tool forces re-verification here.
pub const PINNED_VERSION: &str = "v1.7.16";

fn capture(file: &str, sha256: &str) -> Capture {
    Capture {
        file: file.to_string(),
        sha256: sha256.to_string(),
    }
}

fn flags(entries: &[(&str, FlagBehaviour)]) -> BTreeMap<String, FlagBehaviour> {
    entries
        .iter()
        .map(|(name, behaviour)| ((*name).to_string(), behaviour.clone()))
        .collect()
}

fn field(name: &str, used_for: &str) -> RequiredField {
    RequiredField {
        name: name.to_string(),
        used_for: used_for.to_string(),
    }
}

/// The contract for the pinned version.
#[must_use]
pub fn contract() -> ToolContract {
    ToolContract {
        tool: "open-code-review".to_string(),
        version: PINNED_VERSION.to_string(),
        capture_provenance: "captured by running the ocr binary resolved through PATH and \
                             recording its output verbatim; the foreign-cwd captures were taken \
                             from /tmp, with and without --repo, to separate the working \
                             directory from the path argument. Captures embed absolute paths \
                             from the capturing machine, so re-capturing produces different \
                             bytes and the recorded digests must be refreshed with them."
            .to_string(),
        version_capture: capture(
            "version.txt",
            "40fad1ebc6e301cf7569b6d8540ce17184fa8f97744d95462dff51a41d03b8bc",
        ),
        subcommands: vec![session_list(), session_show()],
    }
}

/// Behaviour of `session list`, evidenced by the captures it names.
fn session_list() -> SubcommandContract {
    SubcommandContract {
        subcommand: "session list".to_string(),
        // Honours --repo, so the working directory is the default
        // rather than the key. Established by the with/without --repo
        // pair, not by varying the directory alone. The argument is
        // made absolute against the working directory and cleaned,
        // but not symlink-resolved: a relative path and a trailing
        // "/." both resolve, while a symlink to the same directory
        // does not.
        state_key: StateKey::PathArgumentAbsoluteCleaned { flag: "--repo" },
        result_source: ResultSource::Stdout,
        flags: flags(&[
            ("--json", FlagBehaviour::Honoured),
            ("--repo", FlagBehaviour::Honoured),
            // Defaults to 20 and truncates silently. A caller scanning
            // for a session in a store with more than twenty entries
            // gets exit zero and an incomplete answer, which is the
            // shape this contract exists to name.
            (
                "--limit",
                FlagBehaviour::AcceptedAndIgnored {
                    use_instead: "an explicit limit of 0, which the tool documents as \
                                      unlimited; the default of 20 truncates silently"
                        .to_string(),
                },
            ),
        ]),
        required_fields: vec![
            field(
                "session_id",
                "identifies the session a caller then reads evidence from",
            ),
            field(
                "file_path",
                "locates the durable session jsonl, which is the authoritative \
                     source for session show",
            ),
            field(
                "repo_dir",
                "confirms the listed session belongs to the intended repository",
            ),
        ],
        captures: vec![
            capture(
                "session-list--help.txt",
                "50b75d768412f9e17f35e0483a5220b7e74a4adf6016d439e69e552a4af23684",
            ),
            capture(
                "session-list--json.stdout",
                "30425b13ead2873a956c0cc8f340f0fbfe3b912630d246a3752a304e870d391e",
            ),
            capture(
                "session-list--json--foreign-cwd.stdout",
                "38e0b9de817f645c4bec37c0d4a3e58baecccb040f5718dc069a72c7385a0bed",
            ),
            capture(
                "session-list--json--foreign-cwd-with-repo.stdout",
                "30425b13ead2873a956c0cc8f340f0fbfe3b912630d246a3752a304e870d391e",
            ),
        ],
    }
}

/// Behaviour of `session show`, evidenced by the captures it names.
fn session_show() -> SubcommandContract {
    SubcommandContract {
        subcommand: "session show".to_string(),
        // Ignores --repo and derives the session-store slug from the
        // logical working directory. Symlink-sensitive: from a
        // symlinked path the lookup fails, and unsetting PWD so the
        // process reports the resolved path makes it succeed. A caller
        // that canonicalises before invoking is therefore looking
        // somewhere the tool would not have looked.
        state_key: StateKey::LogicalWorkingDirectory,
        result_source: ResultSource::DurableArtifact {
            description: "the session jsonl named by session list's file_path field; \
                              its session_end record carries files_reviewed as an explicit \
                              array"
                .to_string(),
        },
        flags: flags(&[
            // Accepted, exits zero, prints a human table anyway.
            (
                "--json",
                FlagBehaviour::AcceptedAndIgnored {
                    use_instead: "the session jsonl named by session list --json".to_string(),
                },
            ),
            (
                "--repo",
                FlagBehaviour::AcceptedAndIgnored {
                    use_instead: "the working directory of the invoking process".to_string(),
                },
            ),
        ]),
        required_fields: vec![field(
            "Session:",
            "the human table header, which is what this subcommand emits even when \
                 --json is requested",
        )],
        captures: vec![
            capture(
                "session-show--help.txt",
                "285ed5248aaaa92ef472118d5902e27eb823d643799269857d5a68d4bf0dda92",
            ),
            capture(
                "session-show--json.stdout",
                "144e2c69cfe6cb6afb96e81c0b172a3aa0f29bdbdf2084e2c6692d1770ae28fc",
            ),
            capture(
                "session-show--foreign-cwd-with-repo.stderr",
                "c3e5b0709108fb4e56e41e06bb3458013555f807762b7e9ca31ce3c29bcbd205",
            ),
        ],
    }
}
