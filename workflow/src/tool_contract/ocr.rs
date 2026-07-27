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
//! ocr session list --json --limit 1                > $D/session-list--json--limit-1.stdout
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

/// Builds the flag map, rejecting a duplicate rather than discarding it.
///
/// `SubcommandContract::flags` is a map so a duplicated flag cannot silently
/// shadow, but a plain `collect` would drop the earlier entry and honour the
/// later one without complaint - defeating the guarantee at the one site meant
/// to enforce it. A duplicate here means two different behaviours were recorded
/// for one flag, and there is no safe way to choose between them.
fn flags(entries: &[(&str, FlagBehaviour)]) -> BTreeMap<String, FlagBehaviour> {
    let mut map = BTreeMap::new();
    for (name, behaviour) in entries {
        assert!(
            map.insert((*name).to_string(), behaviour.clone()).is_none(),
            "{name} is recorded twice; a flag has one observed behaviour, so remove the \
             duplicate rather than letting the later entry win silently"
        );
    }
    map
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
        subcommands: vec![session_list(), session_show(), review()],
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
            // Honoured, and evidenced: --limit 1 returns one entry where the
            // same command without it returns three. Previously recorded as
            // ignored, which was an assertion with no capture behind it - the
            // failure this module exists to prevent. The hazard is the default
            // of 20 truncating silently, which is a caller concern rather than
            // an unhonoured flag, and is recorded on the subcommand's
            // truncation note instead.
            ("--limit", FlagBehaviour::Honoured),
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
        // duration_ns is deliberately absent from the required set: the tool
        // omits it for a session that did not finish, and stamps end_time
        // with the zero time rather than a real one. A caller that reads it
        // unconditionally gets a decode error on exactly the sessions worth
        // investigating. The capture holds one such session so this stays
        // evidenced rather than remembered.
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
            // The discriminating capture for --limit: one entry where the
            // unrestricted capture above holds three.
            capture(
                "session-list--json--limit-1.stdout",
                "d895311d94ee221633b56982081abb4ae3d9866e51037797755c7239a968e675",
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

/// Behaviour of `review`, the subcommand that writes the session store.
///
/// Recorded because the two read subcommands were modelled while the writer -
/// the one that decides where the store goes - was not, so the contract
/// described how to read a store without recording what created it.
fn review() -> SubcommandContract {
    SubcommandContract {
        subcommand: "review".to_string(),
        // Keys on the Git root, measured rather than assumed: invoked from
        // /tmp/gr179/sub/deep the store appeared under the slug for
        // /tmp/gr179. Control: the same binary outside a repository created
        // no store and did not walk up past it.
        //
        // This is the disagreement behind issue 248. The readers above key on
        // the working directory, so a review run from a subdirectory writes
        // where a read from that same subdirectory does not look.
        state_key: StateKey::GitRoot,
        result_source: ResultSource::DurableArtifact {
            description: "the session jsonl written under the Git root's store slug; \
                          stdout carries progress, not the reviewable result"
                .to_string(),
        },
        flags: flags(&[
            // Honoured: lists which files would be reviewed and exits without
            // contacting a model, which is what makes it usable for capturing
            // behaviour without spending a review.
            ("--preview", FlagBehaviour::Honoured),
            ("--from", FlagBehaviour::Honoured),
            ("--to", FlagBehaviour::Honoured),
        ]),
        // The preview's section header. Issue 174 was a parser expecting
        // "Excluded (2):" against a tool printing "Excluded from review (2):",
        // so the exact wording is the thing worth constraining, and the digest
        // above makes it uneditable without detection.
        required_fields: vec![RequiredField {
            name: "Excluded from review".to_string(),
            used_for: "the preview's exclusion header; issue 174 was a parser expecting \
                       \"Excluded (2):\" against a tool printing \"Excluded from review (2):\", \
                       so a parser must be checked against this exact wording"
                .to_string(),
        }],
        captures: vec![Capture {
            file: "review--preview-from-subdirectory.stdout".to_string(),
            sha256: "0b7a7d3c1e86d73e976fc27c4963cf19e6321336c68f53c81e33eb2f2bed2216".to_string(),
        }],
    }
}
