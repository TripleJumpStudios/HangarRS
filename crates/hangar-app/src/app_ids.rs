//! Stable app identities for Flight's saved choices.
//!
//! Version 1 of `preflight.toml` keyed choices by display name, so renaming an
//! app in `config.toml` detached them. Each configured app now has a persistent
//! ID, separate from its name and executable; the file is translated on load
//! and save, and the rest of the Flight code works with names in memory.
//!
//! # Where the IDs live
//!
//! In `preflight.toml`, in an `[apps.<id>]` table recording the name and
//! executable each ID was last seen with; **not** in `config.toml`, which is
//! `deny_unknown_fields` (1.0.x would refuse an `id` field). Nothing is written
//! on load: every key is reported, and the file changes only when a person
//! changes a Flight choice.
//!
//! # How an app is recognised
//!
//! An ID is the app's when its record's executable *and* name both match.
//! One that matches only on the executable is the same app **renamed**; one
//! that matches only on the name is the same app **moved**. When the
//! executable points at one record and the name at another, or two records
//! fit, the app is **ambiguous**: it gets a fresh ID and none of the stored
//! choices, and the report says it needs review. A choice is never applied to
//! an app on a guess.
//!
//! An app added since the file was saved has no record. It gets an ID, its
//! computed default placement, and a place at the end of the order.

use crate::flight::Placement;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// The current file version. Files without one are version 1: name keys.
pub const VERSION: u32 = 2;

/// `preflight.toml` as it is on disk, either version.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stored {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub order: Vec<String>,
    #[serde(default)]
    pub placement: BTreeMap<String, Placement>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub apps: BTreeMap<String, Record>,
}

/// What an ID was last seen as.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exe: Option<PathBuf>,
}

/// A configured app, as `config.toml` has it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppRef {
    pub name: String,
    pub exe: PathBuf,
}

impl From<&hangar_core::config::AppEntry> for AppRef {
    fn from(entry: &hangar_core::config::AppEntry) -> Self {
        Self {
            name: entry.name.clone(),
            exe: entry.exe.clone(),
        }
    }
}

/// The identity half of the loaded settings, kept for the next save.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ids {
    /// Display name to ID, for the apps configured now.
    by_name: BTreeMap<String, String>,
    /// Every record, including ones no configured app matches any more.
    registry: BTreeMap<String, Record>,
    /// Stored choices no current app took: written back untouched, so an app
    /// that is briefly missing from the config gets them back.
    orphan_placement: BTreeMap<String, Placement>,
    orphan_order: Vec<String>,
}

impl Ids {
    pub fn id_of(&self, name: &str) -> Option<&str> {
        self.by_name.get(name).map(String::as_str)
    }
}

/// What the loaded settings are, in memory and name-keyed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Loaded {
    pub placement: BTreeMap<String, Placement>,
    pub order: Vec<String>,
    pub ids: Ids,
    /// One line per app and per stored key.
    pub report: Vec<String>,
}

/// `Little Navmap` → `little-navmap`.
fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "app".into()
    } else {
        out
    }
}

fn mint(name: &str, taken: &BTreeSet<String>) -> String {
    let base = slug(name);
    if !taken.contains(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|candidate| !taken.contains(candidate))
        .expect("an unused suffix exists")
}

enum Match {
    Same(String),
    Renamed { id: String, was: String },
    Moved(String),
    New,
    Ambiguous,
}

fn recognise(app: &AppRef, registry: &BTreeMap<String, Record>) -> Match {
    let by_exe: Vec<&String> = registry
        .iter()
        .filter(|(_, r)| r.exe.as_ref() == Some(&app.exe))
        .map(|(id, _)| id)
        .collect();
    let by_name: Vec<&String> = registry
        .iter()
        .filter(|(_, r)| r.name == app.name)
        .map(|(id, _)| id)
        .collect();
    match (by_exe.as_slice(), by_name.as_slice()) {
        ([e], [n]) if e == n => Match::Same((*e).clone()),
        ([e], []) => Match::Renamed {
            id: (*e).clone(),
            was: registry[*e].name.clone(),
        },
        ([], [n]) => Match::Moved((*n).clone()),
        ([], []) => Match::New,
        _ => Match::Ambiguous,
    }
}

/// Translate a stored file for the apps configured now. Reads nothing, writes
/// nothing.
pub fn load(stored: Stored, apps: &[AppRef]) -> Loaded {
    let mut report = Vec::new();
    let mut registry = stored.apps.clone();
    let mut taken: BTreeSet<String> = registry.keys().cloned().collect();
    taken.extend(stored.placement.keys().cloned());
    taken.extend(stored.order.iter().cloned());

    // First pass: who is who. An ID claimed by two apps is nobody's.
    let mut matches: Vec<(usize, Match)> = apps
        .iter()
        .enumerate()
        .map(|(i, app)| (i, recognise(app, &stored.apps)))
        .collect();
    let mut claims: BTreeMap<String, usize> = BTreeMap::new();
    for (_, m) in &matches {
        if let Match::Same(id) | Match::Renamed { id, .. } | Match::Moved(id) = m {
            *claims.entry(id.clone()).or_default() += 1;
        }
    }
    for (_, m) in &mut matches {
        if let Match::Same(id) | Match::Renamed { id, .. } | Match::Moved(id) = m {
            if claims[id.as_str()] > 1 {
                *m = Match::Ambiguous;
            }
        }
    }

    let mut by_name = BTreeMap::new();
    // Stored key → the name it now applies to.
    let mut keys: BTreeMap<String, String> = BTreeMap::new();
    let mut held_back = BTreeSet::new();
    for (i, m) in matches {
        let app = &apps[i];
        let (id, line) = match m {
            Match::Same(id) => {
                let line = format!("{:?}: id {id}", app.name);
                (id, line)
            }
            Match::Renamed { id, was } => {
                let line = format!(
                    "{:?}: id {id}, renamed from {was:?}; choices kept",
                    app.name
                );
                (id, line)
            }
            Match::Moved(id) => {
                let line = format!(
                    "{:?}: id {id}, executable moved to {}; choices kept",
                    app.name,
                    app.exe.display()
                );
                (id, line)
            }
            Match::New => {
                let id = mint(&app.name, &taken);
                taken.insert(id.clone());
                let line = if stored.apps.is_empty() {
                    format!("{:?}: id {id} assigned", app.name)
                } else {
                    format!("{:?}: id {id} assigned, new since the last save", app.name)
                };
                (id, line)
            }
            Match::Ambiguous => {
                let id = mint(&app.name, &taken);
                taken.insert(id.clone());
                held_back.insert(app.name.clone());
                let line = format!(
                    "{:?}: id {id} assigned; matches more than one saved app, so no saved choice is applied (needs review)",
                    app.name
                );
                (id, line)
            }
        };
        report.push(line);
        if !held_back.contains(&app.name) {
            if stored.apps.contains_key(&id) {
                keys.insert(id.clone(), app.name.clone());
            }
            // A name key — a version 1 file, or a choice made before the app
            // had an ID — applies too, unless an ID key says otherwise.
            keys.entry(app.name.clone())
                .or_insert_with(|| app.name.clone());
        }
        registry.insert(
            id.clone(),
            Record {
                name: app.name.clone(),
                exe: Some(app.exe.clone()),
            },
        );
        by_name.insert(app.name.clone(), id);
    }

    // Per app: the stored values that name it, ID keys first.
    let mut placement = BTreeMap::new();
    let mut orphan_placement = BTreeMap::new();
    let mut offered: BTreeMap<&String, Vec<(&String, Placement)>> = BTreeMap::new();
    for (key, value) in &stored.placement {
        match keys.get(key) {
            Some(name) => offered.entry(name).or_default().push((key, *value)),
            None => {
                orphan_placement.insert(key.clone(), *value);
                report.push(format!(
                    "placement {key:?}: no configured app; kept in the file"
                ));
            }
        }
    }
    for (name, mut values) in offered {
        values.sort_by_key(|(key, _)| *key == name);
        for (i, (key, value)) in values.into_iter().enumerate() {
            if i > 0 {
                report.push(format!(
                    "placement {key:?}: superseded by {name:?}'s id key; dropped"
                ));
                continue;
            }
            placement.insert(name.clone(), value);
            report.push(if key == name {
                format!("placement {key:?}: {value:?}, name key; saved under its id next time")
            } else {
                format!("placement {key:?}: {value:?}, applies to {name:?}")
            });
        }
    }

    let mut order = Vec::new();
    let mut orphan_order = Vec::new();
    for key in &stored.order {
        match keys.get(key) {
            Some(name) if order.contains(name) => {
                report.push(format!("order {key:?}: {name:?} already placed; dropped"));
            }
            Some(name) => {
                order.push(name.clone());
                report.push(format!("order {key:?}: {name:?}"));
            }
            None => {
                orphan_order.push(key.clone());
                report.push(format!(
                    "order {key:?}: no configured app; kept in the file"
                ));
            }
        }
    }

    Loaded {
        placement,
        order,
        ids: Ids {
            by_name,
            registry,
            orphan_placement,
            orphan_order,
        },
        report,
    }
}

/// The file to write for these choices. Names with an ID are written as the
/// ID; any other name (an app not in the config) as itself.
pub fn store(placement: &BTreeMap<String, Placement>, order: &[String], ids: &Ids) -> Stored {
    let key = |name: &String| {
        ids.by_name
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.clone())
    };
    let mut out_placement: BTreeMap<String, Placement> = ids.orphan_placement.clone();
    for (name, value) in placement {
        out_placement.insert(key(name), *value);
    }
    let mut out_order: Vec<String> = order.iter().map(key).collect();
    for orphan in &ids.orphan_order {
        if !out_order.contains(orphan) {
            out_order.push(orphan.clone());
        }
    }
    Stored {
        version: VERSION,
        order: out_order,
        placement: out_placement,
        apps: ids.registry.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str, exe: &str) -> AppRef {
        AppRef {
            name: name.into(),
            exe: exe.into(),
        }
    }

    /// A real version 1 `preflight.toml`: keyed by names.
    const SAVED_V1: &str = r#"
order = ["Project Airports", "Little Navmap", "BeyondATC"]

[placement]
BeyondATC = "after"
"Little Navmap" = "after"
"Orbx Central" = "off"
"Pilot Client" = "off"
"Project Airports" = "before"
"#;

    /// The five apps that file was saved with, home directory replaced.
    fn saved_apps() -> Vec<AppRef> {
        let sim = "/mnt/steam/SteamLibrary/steamapps/compatdata/2537590/pfx/drive_c";
        let companion = "/home/user/.local/share/msfs-manager/companion-prefix/pfx/drive_c/users/steamuser/AppData/Local";
        vec![
            app("BeyondATC", &format!("{sim}/BeyondATC/BeyondATC.exe")),
            app(
                "Pilot Client",
                &format!("{companion}/PilotEdge/Pilot Client/PilotEdge.PilotClient.exe"),
            ),
            app(
                "Project Airports",
                &format!("{companion}/Programs/@project-airportsdesktop/Project Airports.exe"),
            ),
            app(
                "Orbx Central",
                &format!("{companion}/Programs/orbx-central/Orbx Central.exe"),
            ),
            app(
                "Little Navmap",
                "/home/user/.local/share/hangar-rs/littlenavmap-msfs/littlenavmap.exe",
            ),
        ]
    }

    fn parse(text: &str) -> Stored {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn slugs_are_plain_and_unique() {
        assert_eq!(slug("Little Navmap"), "little-navmap");
        assert_eq!(slug("@Project  Airports!"), "project-airports");
        assert_eq!(slug("…"), "app");
        let taken: BTreeSet<String> = ["beyondatc".to_string()].into();
        assert_eq!(mint("BeyondATC", &taken), "beyondatc-2");
    }

    #[test]
    fn the_saved_v1_file_loads_to_the_same_choices_and_reports_every_key() {
        let stored = parse(SAVED_V1);
        let loaded = load(stored.clone(), &saved_apps());
        assert_eq!(
            loaded.placement, stored.placement,
            "same names, same values"
        );
        assert_eq!(loaded.order, stored.order);
        // One line per app, per placement key and per order key.
        assert_eq!(loaded.report.len(), 5 + 5 + 3, "{:#?}", loaded.report);
        for key in stored.placement.keys().chain(&stored.order) {
            assert!(
                loaded
                    .report
                    .iter()
                    .any(|l| l.contains(&format!("{key:?}"))),
                "{key} unreported: {:#?}",
                loaded.report
            );
        }
        assert_eq!(loaded.ids.id_of("BeyondATC"), Some("beyondatc"));
        assert_eq!(loaded.ids.id_of("Little Navmap"), Some("little-navmap"));
    }

    #[test]
    fn a_saved_v1_file_round_trips_through_ids() {
        let loaded = load(parse(SAVED_V1), &saved_apps());
        let written = store(&loaded.placement, &loaded.order, &loaded.ids);
        assert_eq!(written.version, VERSION);
        assert_eq!(
            written.order,
            ["project-airports", "little-navmap", "beyondatc"]
        );
        assert_eq!(written.placement["pilot-client"], Placement::Off);
        assert_eq!(written.apps.len(), 5);
        let text = toml::to_string_pretty(&written).unwrap();
        let again = load(parse(&text), &saved_apps());
        assert_eq!(again.placement, loaded.placement);
        assert_eq!(again.order, loaded.order);
        assert_eq!(again.ids, loaded.ids);
    }

    fn saved_v2() -> String {
        let loaded = load(parse(SAVED_V1), &saved_apps());
        toml::to_string_pretty(&store(&loaded.placement, &loaded.order, &loaded.ids)).unwrap()
    }

    #[test]
    fn a_renamed_app_keeps_its_choices_and_its_place() {
        let mut apps = saved_apps();
        apps[4].name = "LNM".into();
        let loaded = load(parse(&saved_v2()), &apps);
        assert_eq!(loaded.placement.get("LNM"), Some(&Placement::After));
        assert!(!loaded.placement.contains_key("Little Navmap"));
        assert_eq!(loaded.order, ["Project Airports", "LNM", "BeyondATC"]);
        assert!(loaded
            .report
            .iter()
            .any(|l| l.contains("renamed from \"Little Navmap\"")));
        assert_eq!(loaded.ids.id_of("LNM"), Some("little-navmap"));
    }

    #[test]
    fn a_moved_executable_keeps_its_choices() {
        let mut apps = saved_apps();
        apps[0].exe = "/elsewhere/BeyondATC.exe".into();
        let loaded = load(parse(&saved_v2()), &apps);
        assert_eq!(loaded.placement.get("BeyondATC"), Some(&Placement::After));
        assert!(loaded.report.iter().any(|l| l.contains("executable moved")));
    }

    #[test]
    fn an_added_app_gets_an_id_and_its_default_and_nothing_else_moves() {
        let mut apps = saved_apps();
        apps.push(app("Volanta", "/home/user/volanta/Volanta.exe"));
        let before = load(parse(&saved_v2()), &saved_apps());
        let after = load(parse(&saved_v2()), &apps);
        assert_eq!(after.placement, before.placement, "no stored choice for it");
        assert_eq!(after.order, before.order);
        assert_eq!(after.ids.id_of("Volanta"), Some("volanta"));
        assert!(after
            .report
            .iter()
            .any(|l| l.contains("new since the last save")));
    }

    #[test]
    fn an_app_that_fits_two_records_gets_no_saved_choice() {
        let mut apps = saved_apps();
        // Little Navmap's executable under BeyondATC's name.
        apps[4].name = "BeyondATC".into();
        apps.remove(0);
        let loaded = load(parse(&saved_v2()), &apps);
        assert!(!loaded.placement.contains_key("BeyondATC"), "{loaded:#?}");
        assert!(loaded.report.iter().any(|l| l.contains("needs review")));
        // Its stored choices are not lost.
        let written = store(&loaded.placement, &loaded.order, &loaded.ids);
        assert_eq!(written.placement.get("beyondatc"), Some(&Placement::After));
    }

    #[test]
    fn choices_for_an_app_no_longer_configured_are_kept() {
        let mut apps = saved_apps();
        apps.retain(|a| a.name != "Pilot Client");
        let loaded = load(parse(&saved_v2()), &apps);
        assert!(!loaded.placement.contains_key("Pilot Client"));
        let written = store(&loaded.placement, &loaded.order, &loaded.ids);
        assert_eq!(written.placement.get("pilot-client"), Some(&Placement::Off));
        // And it gets them back when it returns.
        let text = toml::to_string_pretty(&written).unwrap();
        let back = load(parse(&text), &saved_apps());
        assert_eq!(back.placement.get("Pilot Client"), Some(&Placement::Off));
    }

    #[test]
    fn a_name_saved_before_its_app_had_an_id_still_applies() {
        // A v2 file plus a name key written by a choice made after an app was
        // added to the config at runtime.
        let mut text = saved_v2();
        text = text.replace("[placement]\n", "[placement]\nVolanta = \"before\"\n");
        let mut apps = saved_apps();
        apps.push(app("Volanta", "/home/user/volanta/Volanta.exe"));
        let loaded = load(parse(&text), &apps);
        assert_eq!(loaded.placement.get("Volanta"), Some(&Placement::Before));
    }

    /// Against a read-only copy of a real `config.toml` and `preflight.toml`:
    /// every route and the plan resolve unchanged across the version 2 load.
    ///
    /// `HANGAR_RO_CONFIG_DIR=/tmp/copy cargo test -p hangar-app -- --ignored
    /// saved_config`. Reads the copy only; never the live files.
    #[test]
    #[ignore = "needs HANGAR_RO_CONFIG_DIR pointing at a copy of the real config"]
    fn the_saved_config_resolves_unchanged() {
        use crate::flight::{PlanEntry, Settings};
        use hangar_runtime::RunMode;
        let dir = PathBuf::from(std::env::var("HANGAR_RO_CONFIG_DIR").expect("set it"));
        let config = hangar_core::config::Config::read(&dir.join("config.toml"))
            .ok()
            .expect("config parses");
        let text = std::fs::read_to_string(dir.join("preflight.toml")).unwrap();
        let v1: Settings = toml::from_str(&text).unwrap();
        let refs: Vec<AppRef> = config.apps.iter().map(Into::into).collect();
        let loaded = Settings::from_stored(parse(&text), &refs);
        for line in &loaded.report {
            println!("{line}");
        }

        // Routes: the same executor for every app.
        for entry in &config.apps {
            let item = hangar_catalog::apps::item(entry);
            let mode = crate::apps::request_for(&item).mode;
            let want = match entry.name.as_str() {
                "BeyondATC" => RunMode::SimSession,
                _ => RunMode::Prefix,
            };
            assert_eq!(mode, want, "{}", entry.name);
        }

        // The plan: the same order and placements, by name.
        let placements = |s: &Settings| -> BTreeMap<String, Placement> {
            config
                .apps
                .iter()
                .map(|a| {
                    let p = s.placement.get(&a.name).copied().unwrap_or(
                        if hangar_runtime::readiness::probe_for(&a.name).is_some() {
                            Placement::Before
                        } else {
                            Placement::After
                        },
                    );
                    (a.name.clone(), p)
                })
                .collect()
        };
        let before = v1.plan(&placements(&v1));
        let after = loaded.plan(&placements(&loaded));
        assert_eq!(before, after);
        let names: Vec<_> = after
            .iter()
            .map(|e| match e {
                PlanEntry::App(n) => n.as_str(),
                PlanEntry::Simulator => "MSFS",
            })
            .collect();
        println!("plan: {names:?}");
        assert_eq!(
            names.first(),
            Some(&"Project Airports"),
            "PA stays before the sim"
        );
    }

    #[test]
    fn an_empty_file_with_no_apps_is_empty() {
        let loaded = load(Stored::default(), &[]);
        assert_eq!(loaded, Loaded::default());
    }
}
