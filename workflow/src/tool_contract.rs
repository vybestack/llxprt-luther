//! Typed contracts describing what an external tool actually does.
//!
//! Six of the ten failures analysed in the convergence retrospective
//! (`docs/architecture/convergence-retrospective.md`) share one shape: the
//! caller assumed a behaviour the tool does not have. The parser expected
//! `Excluded (2):` and the tool printed `Excluded from review (2):`. The
//! adapter passed `--repo` to a subcommand that ignores it. The client
//! requested `--json` and the tool printed a table.
//!
//! In every case the underlying operation succeeded and the gate reported
//! failure because it could not interpret the result. No routing policy fixes
//! that, because nothing was mis-routed.
//!
//! The distinction this module exists to make: the fields Luther already
//! carries describe **what the caller sent**. A contract describes **what the
//! callee honours**.
//!
//! Contracts are checked against captured output from the real binary, pinned
//! to the exact version that produced it, and digested so a hand-edited
//! capture is detectable. A fixture written from the same assumption as the
//! parser agrees with the parser and disagrees only with reality.

use std::collections::BTreeMap;

pub mod ocr;

/// What a recorded behaviour is evidenced by.
///
/// A behaviour with no capture is an assertion, not an observation. Requiring
/// evidence is what stops a contract from becoming another place to write
/// guesses down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    /// File under `tests/fixtures/tool-contracts/<tool>/`.
    pub file: String,
    /// SHA-256 of the captured bytes at capture time.
    ///
    /// The digest is what makes "captured from the real binary" checkable
    /// rather than merely claimed: editing the file to agree with a wrong
    /// parser changes the digest.
    pub sha256: String,
}

/// How a tool subcommand decides which repository's state to read.
///
/// Recorded because three campaign failures came from guessing wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateKey {
    /// The logical working directory as inherited from the environment,
    /// **without** symlink resolution.
    ///
    /// Distinct from [`Self::CanonicalWorkingDirectory`] because the two
    /// disagree under a symlinked path, and that disagreement is a real
    /// campaign failure: a caller that canonicalises before handing a path to
    /// a tool which does not will look in a directory the tool never uses.
    LogicalWorkingDirectory,
    /// A path argument, made absolute against the working directory and
    /// lexically cleaned, but **not** symlink-resolved.
    ///
    /// The absence of symlink resolution is the point: it is the same
    /// logical-not-canonical rule as [`Self::LogicalWorkingDirectory`], so a
    /// caller that canonicalises before passing a path looks somewhere the
    /// tool does not.
    PathArgumentAbsoluteCleaned {
        /// The flag supplying it.
        flag: &'static str,
    },
}

/// What a flag actually does, as opposed to what its name suggests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlagBehaviour {
    /// The tool changes its behaviour in response to this flag.
    Honoured,
    /// The tool accepts the flag, exits zero, and ignores it. This is the
    /// dangerous case: the caller sees success and wrong output.
    AcceptedAndIgnored {
        /// What the caller can rely on instead. Must be non-empty.
        use_instead: String,
    },
    /// The tool rejects the flag.
    Rejected,
}

/// Where the authoritative result lives when stdout is not it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultSource {
    /// Stdout carries the result in the documented format.
    Stdout,
    /// Stdout is human-oriented; the durable artifact is authoritative.
    DurableArtifact {
        /// How to locate it, for a human re-verifying this contract.
        description: String,
    },
}

/// A field a consumer depends on being present in captured output.
///
/// Recorded so that renaming it in a capture is caught. Asserting only that
/// the capture parses leaves the fields the durable-artifact story depends on
/// entirely unconstrained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredField {
    pub name: String,
    /// Why a consumer needs it, so a future maintainer can judge a rename.
    pub used_for: String,
}

/// One subcommand's observed behaviour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubcommandContract {
    pub subcommand: String,
    pub state_key: StateKey,
    pub result_source: ResultSource,
    /// Flags this caller relies on, and what each actually does.
    ///
    /// A map rather than a list so a duplicated flag cannot silently shadow.
    pub flags: BTreeMap<String, FlagBehaviour>,
    /// Fields consumers read out of this subcommand's output.
    pub required_fields: Vec<RequiredField>,
    /// Real captured output evidencing the behaviour above.
    pub captures: Vec<Capture>,
}

impl SubcommandContract {
    /// Behaviour of `flag`, if this contract records it.
    #[must_use]
    pub fn flag(&self, flag: &str) -> Option<&FlagBehaviour> {
        self.flags.get(flag)
    }

    /// Rejects a caller that depends on a flag the tool does not honour.
    ///
    /// Fails at validation with a diagnostic naming the alternative, rather
    /// than at parse time with a decode error pointing at the wrong layer.
    /// Issue #183 records exactly that misdirection: a decode failure at byte
    /// one sent the investigation hunting through path derivation and session
    /// ids, when the requested format was simply never produced.
    pub fn require_honoured(&self, flag: &str) -> Result<(), ContractViolation> {
        match self.flag(flag) {
            Some(FlagBehaviour::Honoured) => Ok(()),
            // An empty remediation would render as "rely on  instead",
            // which sends the caller looking for a missing word rather than
            // at the flag. Treated as an unrecorded behaviour, because a
            // remedy that names nothing records nothing.
            Some(FlagBehaviour::AcceptedAndIgnored { use_instead })
                if use_instead.trim().is_empty() =>
            {
                Err(ContractViolation::MalformedRemediation {
                    subcommand: self.subcommand.clone(),
                    flag: flag.to_string(),
                })
            }
            Some(FlagBehaviour::AcceptedAndIgnored { use_instead }) => {
                Err(ContractViolation::FlagIgnored {
                    subcommand: self.subcommand.clone(),
                    flag: flag.to_string(),
                    use_instead: use_instead.clone(),
                })
            }
            Some(FlagBehaviour::Rejected) => Err(ContractViolation::FlagRejected {
                subcommand: self.subcommand.clone(),
                flag: flag.to_string(),
            }),
            None => Err(ContractViolation::FlagNotRecorded {
                subcommand: self.subcommand.clone(),
                flag: flag.to_string(),
            }),
        }
    }
}

/// A tool's contract, pinned to the exact version it was captured from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolContract {
    pub tool: String,
    /// Exact version token this contract was verified against.
    pub version: String,
    /// How and when the captures were taken, so a reader can repeat them.
    pub capture_provenance: String,
    /// The captured `version` output the pin was read from, digested like any
    /// other evidence so it cannot be quietly replaced with a bare token.
    pub version_capture: Capture,
    pub subcommands: Vec<SubcommandContract>,
}

impl ToolContract {
    #[must_use]
    pub fn subcommand(&self, name: &str) -> Option<&SubcommandContract> {
        self.subcommands
            .iter()
            .find(|contract| contract.subcommand == name)
    }

    /// Fails closed unless the observed version token matches exactly.
    ///
    /// Matching is on whitespace-delimited tokens rather than substrings: a
    /// substring check accepts `v1.7.1` against `v1.7.16`, and a pin that
    /// accepts a version it was never verified against is not a pin.
    pub fn verify_version(&self, observed: &str) -> Result<(), ContractViolation> {
        if observed
            .split_whitespace()
            .any(|token| token == self.version)
        {
            return Ok(());
        }
        Err(ContractViolation::VersionMismatch {
            tool: self.tool.clone(),
            pinned: self.version.clone(),
            observed: observed.trim().to_string(),
        })
    }
}

/// Hex-encoded SHA-256 of `bytes`.
///
/// Uses the same audited implementation the persistence layer already relies
/// on, rather than a local one: the digest is what makes a capture evidence,
/// so a subtle padding or endianness bug here would silently accept a
/// fabricated fixture.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        // Hex-encoded by indexing rather than by formatting: a discarded
        // formatting Result could yield a short digest that then compares
        // unequal for a reason having nothing to do with the capture, and
        // this digest is what makes a capture evidence. Indexing cannot fail.
        .fold(String::with_capacity(64), |mut hex, byte| {
            const DIGITS: &[u8; 16] = b"0123456789abcdef";
            hex.push(DIGITS[usize::from(byte >> 4)] as char);
            hex.push(DIGITS[usize::from(byte & 0x0f)] as char);
            hex
        })
}

/// A caller assumption the contract contradicts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractViolation {
    VersionMismatch {
        tool: String,
        pinned: String,
        observed: String,
    },
    FlagIgnored {
        subcommand: String,
        flag: String,
        use_instead: String,
    },
    FlagRejected {
        subcommand: String,
        flag: String,
    },
    FlagNotRecorded {
        subcommand: String,
        flag: String,
    },
    /// The flag's behaviour is recorded but its remediation is empty.
    ///
    /// Distinct from `FlagNotRecorded` because the fix is different: the
    /// contract needs editing, not re-capturing. Conflating the two sends a
    /// maintainer to re-run the tool when the tool was never the problem.
    MalformedRemediation {
        subcommand: String,
        flag: String,
    },
}

impl std::fmt::Display for ContractViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::VersionMismatch {
                tool,
                pinned,
                observed,
            } => write!(
                f,
                "{tool} contract is pinned to {pinned} but the installed tool reports {observed}; \
                 re-capture the contract fixtures against the installed version before relying on them"
            ),
            Self::FlagIgnored {
                subcommand,
                flag,
                use_instead,
            } => write!(
                f,
                "{subcommand} accepts {flag} and ignores it; rely on {use_instead} instead"
            ),
            Self::FlagRejected { subcommand, flag } => {
                write!(f, "{subcommand} rejects {flag}")
            }
            Self::FlagNotRecorded { subcommand, flag } => write!(
                f,
                "{subcommand} has no recorded behaviour for {flag}; capture it against the real \
                 tool before depending on it"
            ),
            Self::MalformedRemediation { subcommand, flag } => write!(
                f,
                "{subcommand} records {flag} as ignored but names no alternative; the contract \
                 entry is incomplete, so fix the recorded remediation rather than re-capturing \
                 the tool"
            ),
        }
    }
}

impl std::error::Error for ContractViolation {}

#[cfg(test)]
#[path = "tool_contract_tests.rs"]
mod tool_contract_tests;
