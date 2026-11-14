//! The installer pipeline — seven stages, from a dropped `.exe` to a filed
//! library item.
//!
//! 1. **Static inspection**: PE header, version resources, engine signature.
//! 2. Vendor profile lookup, keyed on `(engine, company, product-regex)`.
//! 3. Pre-flight gate: typed errors, never a vendor dialog.
//! 4. Run under observation, with CWD as an explicit parameter.
//! 5. Classify the outcome from three before/after diffs.
//! 6. Verify against `layout.json`, which is ground truth.
//! 7. File it, with an `InstallRecord` in history.
//!
//! Two hard rules: **never auto-answer Abort** (it
//! rolls back the whole install; Ignore is nearly always right), and **exit
//! code is never success** — four of the five known failure modes return 0.
//!
//! # Stages 1–3: before anything runs
//!
//! [`inspect`] reads a file and says what built it, who made it and what it
//! carries beside it, with the evidence for each answer. Everything it reports
//! is observed: an engine nothing pointed at is [`Engine::Unknown`] rather
//! than a guess, because the vendor table keys off this and a wrong key runs
//! an installer under another vendor's rules.
//!
//! ```bash
//! cargo run -p hangar-install --example inspect -- <file.exe>
//! cargo run -p hangar-install --example vendors
//! cargo run -p hangar-install --example gate -- <file.exe>
//! ```
//!
//! [`vendor::Table`] is stage 2: a shipped TOML table of what each vendor's
//! installers need — prefix, Proton verb, working directory, environment,
//! required runtimes, what a success should leave behind, and the console
//! strings that mean failure whatever the exit code says. It is a table rather
//! than a set of rules because the requirements contradict each other: Just
//! Flight's installer works with the working directory it inherits, and the
//! Garmin launcher requires its own.
//!
//! [`gate`] is stage 3, and it runs nothing: the prefix proven by `UserCfg.opt`,
//! runtime requirements checked against the prefix's text registry and
//! filesystem, room for the payload, and no sim running when the target is
//! Community. Each check answers Pass with evidence, Refuse with a typed
//! error, or Unchecked with why it could not judge — and Unchecked is never
//! silently a pass.
//!
//! # And a sibling pipeline, for what does not arrive as an `.exe`
//!
//! [`drop`] takes a folder or a `.zip` of scenery straight into Community. It is deliberately not an eighth stage. The seven exist because
//! a Windows `.exe` will not say what it is about to do; a scenery download
//! hides nothing, so what carries across is everything *after* the running —
//! gate, place, verify, file — with stages 1, 2 and 4 replaced by "read the
//! drop" and "swap it in".
//!
//! ```bash
//! cargo run -p hangar-install --example drop                 # overlaps, no drop needed
//! cargo run -p hangar-install --example drop -- <path>       # read and plan
//! cargo run -p hangar-install --example drop -- <path> --write
//! ```
//!
//! # Why not just run it and see
//!
//! Because four of the five known failure modes exit 0. An installer that ran
//! in the wrong prefix, or was torn down mid-unpack by `proton run`, or hit a
//! purchase check that could not spawn its helper, all look from outside
//! exactly like one that worked. Knowing what the thing *is* before it runs is
//! what makes the difference detectable afterwards.

pub mod classify;
pub mod drop;
pub mod engine;
pub mod gate;
pub mod inspect;
pub mod pe;
pub mod record;
pub mod registry;
pub mod run;
pub mod scan;
pub mod shim;
pub mod siblings;
pub mod snapshot;
pub mod vendor;
pub mod verify;

pub use classify::{classify, Concern, Landed};
pub use drop::{
    gate as drop_gate, installed_in, overlaps, place, propose, read as read_drop, Approval,
    Confidence, DropRefusal, Found, Installed, Overlap, PlaceError, Placed, Reading, Verification,
    VersionChange,
};
// `drop::Verdict` is deliberately not re-exported: `run::Verdict` is what a
// process did, and these two are never the same question.
pub use engine::{detect, Detection, Engine, Evidence, Facts};
pub use gate::{Checked, Outcome, Refusal, Report, Request};
pub use inspect::{inspect, Identity, Inspection, Source};
pub use pe::{Bitness, Pe, Section, VersionInfo};
pub use record::{InstallRecord, RecordedItem, Replaced, ORIGIN_DROP, ORIGIN_INSTALLER};
pub use run::{judge, ConsoleHit, Invocation, PlanError, Ran, Verdict};
pub use scan::{Encoding, Hit};
pub use shim::{park, restore_stray, Parked, Survey};
pub use siblings::{CabVersion, SetupIni, Siblings};
pub use snapshot::{Addon, Change, Diff, Package, Places, Seen, Snapshot};
pub use vendor::{Cwd, Match, Origin, PowerShell, Prefix, Produces, Profile, Table, Verb};
pub use verify::{applied, verify_package, Applied, Layout, PackageReport};

use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum InstallError {
    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path} is not a Windows executable: {why}")]
    NotPe { path: PathBuf, why: &'static str },
}

pub type Result<T> = std::result::Result<T, InstallError>;

pub mod remove;
pub mod replacements;
