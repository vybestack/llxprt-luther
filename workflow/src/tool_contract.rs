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
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
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

/// Round constants: the first 32 bits of the fractional parts of the cube
/// roots of the first 64 primes.
const K: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

/// SHA-256 of `bytes`, lowercase hex.
///
/// Implemented here rather than pulled in as a dependency because the crate
/// has no hashing dependency and this is the only use.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut state: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];

    let mut message = bytes.to_vec();
    let bit_len = (bytes.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in message.chunks_exact(64) {
        compress_block(&mut state, chunk, &K);
    }

    state
        .iter()
        .map(|word| format!("{word:08x}"))
        .collect::<String>()
}

fn compress_block(state: &mut [u32; 8], chunk: &[u8], k: &[u32; 64]) {
    let mut w = [0u32; 64];
    for (index, word) in w.iter_mut().take(16).enumerate() {
        let start = index * 4;
        *word = u32::from_be_bytes([
            chunk[start],
            chunk[start + 1],
            chunk[start + 2],
            chunk[start + 3],
        ]);
    }
    for index in 16..64 {
        let s0 =
            w[index - 15].rotate_right(7) ^ w[index - 15].rotate_right(18) ^ (w[index - 15] >> 3);
        let s1 =
            w[index - 2].rotate_right(17) ^ w[index - 2].rotate_right(19) ^ (w[index - 2] >> 10);
        w[index] = w[index - 16]
            .wrapping_add(s0)
            .wrapping_add(w[index - 7])
            .wrapping_add(s1);
    }

    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for index in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ ((!e) & g);
        let temp1 = h
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(k[index])
            .wrapping_add(w[index]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let temp2 = s0.wrapping_add(maj);

        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(temp1);
        d = c;
        c = b;
        b = a;
        a = temp1.wrapping_add(temp2);
    }

    for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *slot = slot.wrapping_add(value);
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
        }
    }
}

impl std::error::Error for ContractViolation {}

#[cfg(test)]
#[path = "tool_contract_tests.rs"]
mod tool_contract_tests;
