//! Typed contracts describing what an external tool actually does.
//!
//! Six of the ten failures analysed in the convergence retrospective
//! (`docs/architecture/convergence-retrospective.md`) share one shape: the
//! caller assumed a behaviour the tool does not have. The parser expected
//! `Excluded (2):` and the tool printed `Excluded from review (2):`. The
//! adapter passed `--repo` and the tool ignored it. The client requested
//! `--json` and the tool printed a table.
//!
//! In every case the underlying operation succeeded and the gate reported
//! failure because it could not interpret the result. No routing policy fixes
//! that, because nothing was mis-routed.
//!
//! The distinction this module exists to make: the fields Luther already
//! carries describe **what the caller sent**. A contract describes **what the
//! callee honours**.
//!
//! Contracts are checked against captured output from the real binary, and
//! pinned to the version that produced it, because a fixture written from the
//! same assumption as the parser agrees with the parser and disagrees only
//! with reality.

use serde::{Deserialize, Serialize};

pub mod ocr;

/// How a tool subcommand decides which repository's state to read.
///
/// Recorded because two campaign failures came from guessing wrong: one used
/// the passed path where the tool canonicalises, and one used the build
/// workspace root where the tool uses the Git root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StateKey {
    /// Derived from the process working directory. A path flag, if accepted,
    /// does not affect the lookup.
    ProcessWorkingDirectory,
    /// Derived from a path argument, canonicalised before use.
    CanonicalPathArgument,
    /// Derived from the enclosing Git repository root, which is not
    /// necessarily the build workspace root.
    GitRepositoryRoot,
}

/// What a flag actually does, as opposed to what its name suggests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FlagBehaviour {
    /// The tool changes its behaviour in response to this flag.
    Honoured,
    /// The tool accepts the flag, exits zero, and ignores it. This is the
    /// dangerous case: the caller sees success and wrong output.
    AcceptedAndIgnored {
        /// What the caller can rely on instead.
        use_instead: String,
    },
    /// The tool rejects the flag.
    Rejected,
}

/// Where the authoritative result lives when stdout is not it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResultSource {
    /// Stdout carries the result in the documented format.
    Stdout,
    /// Stdout is human-oriented; the durable artifact is authoritative.
    DurableArtifact {
        /// How to locate it, for a human re-verifying this contract.
        description: String,
    },
}

/// One subcommand's observed behaviour.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubcommandContract {
    pub subcommand: String,
    pub state_key: StateKey,
    pub result_source: ResultSource,
    /// Flags this caller relies on, and what each actually does.
    pub flags: Vec<(String, FlagBehaviour)>,
    /// File under `tests/fixtures/tool-contracts/` holding real captured
    /// output, so a parser is validated against reality rather than a guess.
    pub captured_output: Option<String>,
}

impl SubcommandContract {
    /// Behaviour of `flag`, if this contract records it.
    #[must_use]
    pub fn flag(&self, flag: &str) -> Option<&FlagBehaviour> {
        self.flags
            .iter()
            .find(|(name, _)| name == flag)
            .map(|(_, behaviour)| behaviour)
    }

    /// Rejects a caller that depends on a flag the tool ignores.
    ///
    /// Fails at validation with a diagnostic naming the alternative, rather
    /// than at parse time with a decode error pointing at the wrong layer.
    /// Issue #183 records exactly that misdirection: a decode failure at byte
    /// one sent the investigation hunting through path derivation and session
    /// ids, when the requested format was simply never produced.
    pub fn require_honoured(&self, flag: &str) -> Result<(), ContractViolation> {
        match self.flag(flag) {
            Some(FlagBehaviour::Honoured) => Ok(()),
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

/// A tool's contract, pinned to the version it was captured from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolContract {
    pub tool: String,
    /// Exact version string this contract was verified against.
    pub version: String,
    /// How and when the captures were taken, so a reader can repeat them.
    pub capture_provenance: String,
    pub subcommands: Vec<SubcommandContract>,
}

impl ToolContract {
    #[must_use]
    pub fn subcommand(&self, name: &str) -> Option<&SubcommandContract> {
        self.subcommands
            .iter()
            .find(|contract| contract.subcommand == name)
    }

    /// Fails closed when the installed tool is not the pinned version.
    ///
    /// A contract is a claim about one build. Proceeding against a different
    /// version silently reinstates the assumption the contract exists to
    /// remove, so the mismatch is an error and names both versions.
    pub fn verify_version(&self, observed: &str) -> Result<(), ContractViolation> {
        if observed.contains(&self.version) {
            return Ok(());
        }
        Err(ContractViolation::VersionMismatch {
            tool: self.tool.clone(),
            pinned: self.version.clone(),
            observed: observed.trim().to_string(),
        })
    }
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
                "{subcommand} accepts {flag} and ignores it; use {use_instead} instead"
            ),
            Self::FlagRejected { subcommand, flag } => {
                write!(f, "{subcommand} rejects {flag}")
            }
            Self::FlagNotRecorded { subcommand, flag } => write!(
                f,
                "{subcommand} has no recorded behaviour for {flag}; capture it against the real \
                 tool before depending on it"
            ),
        }
    }
}

impl std::error::Error for ContractViolation {}

#[cfg(test)]
#[path = "tool_contract_tests.rs"]
mod tool_contract_tests;
