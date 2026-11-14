//! The library: item taxonomy, scanner, symlink engine and profiles.
//!
//! Each kind has its own lifecycle rather than everything being an "App". See
//! [`item::ItemKind`] — an installer is deliberately not a library item.
//!
//! # What writes
//!
//! Scanning writes nothing. Profile switching moves packages, and only ever in
//! a Community folder the user has approved.

pub mod apps;
pub mod item;
pub mod legacy;
pub mod profile;
pub mod scanner;
pub mod section;
pub mod snapshot;
pub mod switcher;

pub use item::{Item, ItemKind, ItemState};
pub use profile::{Profile, ProfileStore};
pub use scanner::{scan, Problem, ScanResult};
pub use section::Section;
pub use snapshot::CatalogSnapshot;
pub use switcher::{apply_diff, plan_diff, SwitchReport, Switcher};
