//! Drop-install's acceptance check: the whole stage-verify-swap on a scratch
//! tree.
//!
//! `cargo run -p hangar-install --example drop_scratch`
//!
//! The same shape as `switch_scratch` next door, and for the same reason. The
//! live Community folder is **opened read-only**: the scratch tree is built
//! from the real package *names*, `manifest.json` files and `ContentInfo`
//! trees, and no payload is copied — so this runs in milliseconds while still
//! exercising the real names and the real identity signals.
//!
//! What it drives, end to end:
//!
//! 1. an update of a package already there, proposed and placed;
//! 2. the replace path — the old copy renamed into `.hangar-replaced/<stamp>/`;
//! 3. the undo, which is one rename;
//! 4. a new package, which replaces nothing;
//! 5. the overlap detector against the real library's declarations.
//!
//! Everything it writes lives under one temporary directory, which it removes
//! on the way out.

use std::path::{Path, PathBuf};

use hangar_install::drop::{self, Approval, Reading, Verdict};
use hangar_msfs::MsfsEnv;
use hangar_steam::SteamEnv;

fn main() {
    let steam = SteamEnv::discover().expect("Steam");
    let env = MsfsEnv::discover(&steam).expect("MSFS");

    let scratch = std::env::temp_dir().join(format!("hangar-drop-scratch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let community = scratch.join("Packages/Community2024");
    std::fs::create_dir_all(&community).expect("scratch");

    // Names, manifests and ContentInfo only. Nothing else is read.
    let live = env
        .communities
        .iter()
        .map(|c| c.path.clone())
        .max_by_key(|p| drop::installed_in(p).len())
        .unwrap_or_else(|| env.community.clone());

    let mut copied = Vec::new();
    for entry in std::fs::read_dir(&live).expect("read live").flatten() {
        let from = entry.path();
        if !from.join("manifest.json").is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let to = community.join(&name);
        skeleton(&from, &to);
        copied.push(name);
    }
    copied.sort();

    println!("{}", "─".repeat(72));
    println!("scratch copy of {}", live.display());
    println!(
        "  {} packages, manifests and ContentInfo only",
        copied.len()
    );
    println!("  at {}", community.display());
    println!();

    // ── the overlap detector, against the real declarations ──────────────────
    let installed = drop::installed_in(&community);
    let found = drop::overlaps(&installed);
    println!("{}", "─".repeat(72));
    println!("packages declaring the same content as each other");
    if found.is_empty() {
        println!("  none");
    }
    for overlap in &found {
        println!(
            "  {}  ←  {}",
            overlap.shared.join(", "),
            overlap.packages.join(" + ")
        );
    }
    println!();

    // ── an update of something real ──────────────────────────────────────────
    let Some(target) = installed.first().cloned() else {
        eprintln!("the scratch library is empty, so there is nothing to update");
        let _ = std::fs::remove_dir_all(&scratch);
        return;
    };

    let drop_dir = scratch.join("download").join(&target.name);
    skeleton(&community.join(&target.name), &drop_dir);
    bump_version(&drop_dir, "99.0.0");
    // A file the installed copy does not have, so the swap is visible.
    write(
        &drop_dir.join("scenery/new-in-99.bgl"),
        b"only in the new version",
    );
    relayout(&drop_dir);

    let stamp = hangar_core::time::Utc::now().stamp();
    let approval = Approval::given("drop_scratch, against a scratch tree");

    println!("{}", "─".repeat(72));
    println!("an update of {}", target.name);
    let package = one(&drop_dir, &community);
    let verdict = drop::propose(&package, &installed);
    println!("  verdict   {}", verdict.label());
    match &verdict {
        Verdict::Update { confidence, .. } => println!("  match     {}", confidence.label()),
        other => println!("  match     unexpected: {other:?}"),
    }

    let placed =
        drop::place(&package, &community, &stamp, &approval, verdict.replaces()).expect("placed");
    println!("  placed    {}", placed.verification.summary());
    let moved = placed
        .replaced
        .first()
        .cloned()
        .expect("something was replaced")
        .moved_to;
    println!("  replaced  → {}", moved.display());
    println!(
        "  the new file is there: {}",
        community
            .join(&target.name)
            .join("scenery/new-in-99.bgl")
            .is_file()
    );
    println!(
        "  the old copy is intact: {}",
        moved.join("manifest.json").is_file()
    );
    println!();

    // ── the undo, which is one rename ────────────────────────────────────────
    println!("{}", "─".repeat(72));
    println!("the undo");
    let back = community.join(&target.name);
    std::fs::remove_dir_all(&back).expect("clear");
    std::fs::rename(&moved, &back).expect("undo");
    let restored = drop::installed_in(&community)
        .into_iter()
        .find(|i| i.name == target.name)
        .expect("restored");
    println!(
        "  one rename put {} back at v{}",
        restored.name, restored.version
    );
    println!(
        "  the new file is gone: {}",
        !back.join("scenery/new-in-99.bgl").is_file()
    );
    println!();

    // ── something genuinely new ──────────────────────────────────────────────
    println!("{}", "─".repeat(72));
    println!("something new");
    let new_dir = scratch.join("download/sample-airport-test");
    write(
        &new_dir.join("manifest.json"),
        br#"{"title":"Test Field","creator":"nobody","content_type":"SCENERY",
             "package_version":"1.0.0","minimum_game_version":"1.39.12","dependencies":[]}"#,
    );
    write(&new_dir.join("scenery/test.bgl"), b"a test field");
    relayout(&new_dir);

    let package = one(&new_dir, &community);
    let verdict = drop::propose(&package, &drop::installed_in(&community));
    println!("  verdict   {}", verdict.label());
    let placed =
        drop::place(&package, &community, &stamp, &approval, verdict.replaces()).expect("placed");
    println!("  placed    {}", placed.verification.summary());
    println!(
        "  replaced  {}",
        if placed.replaced.is_empty() {
            "nothing"
        } else {
            "something"
        }
    );
    println!();

    let _ = std::fs::remove_dir_all(&scratch);
    println!("scratch removed. The live library was never written to.");
}

/// Read one package out of a directory.
fn one(dir: &Path, community: &Path) -> drop::Found {
    match drop::read(dir, &drop::staging_root(community)).expect("read") {
        Reading::Packages(mut found) => found.remove(0),
        other => panic!("expected a package, got {other:?}"),
    }
}

/// Copy a package's *identity* — manifest and ContentInfo — and nothing else.
fn skeleton(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("mkdir");
    let _ = std::fs::copy(from.join("manifest.json"), to.join("manifest.json"));
    if let Ok(entries) = std::fs::read_dir(from.join("ContentInfo")) {
        for entry in entries.flatten() {
            let history = entry.path().join("ContentHistory.json");
            if history.is_file() {
                let into = to.join("ContentInfo").join(entry.file_name());
                std::fs::create_dir_all(&into).expect("mkdir");
                let _ = std::fs::copy(&history, into.join("ContentHistory.json"));
            }
        }
    }
    relayout(to);
}

/// Write a `layout.json` describing what is actually in the directory, so the
/// staged copy verifies against itself the way a real download does.
fn relayout(dir: &Path) {
    let layout = dir.join("layout.json");
    let _ = std::fs::remove_file(&layout);
    let mut content = Vec::new();
    walk(dir, dir, &mut content);
    content.sort();
    let entries: Vec<String> = content
        .into_iter()
        .filter(|(path, _)| path != "manifest.json")
        .map(|(path, size)| format!(r#"{{"path":"{path}","size":{size},"date":0}}"#))
        .collect();
    write(
        &layout,
        format!(r#"{{"content":[{}]}}"#, entries.join(",")).as_bytes(),
    );
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, u64)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, out);
        } else if let Ok(meta) = entry.metadata() {
            if let Ok(rel) = path.strip_prefix(root) {
                out.push((rel.to_string_lossy().replace('\\', "/"), meta.len()));
            }
        }
    }
}

fn bump_version(dir: &Path, to: &str) {
    let path = dir.join("manifest.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    // Crude on purpose: this is a fixture, and a real JSON rewrite here would
    // reorder keys the manifest reader has opinions about.
    let bumped = match (text.find("\"package_version\""), text.len()) {
        (Some(start), _) => {
            let rest = &text[start..];
            match (rest.find(':'), rest.find(',').or_else(|| rest.find('}'))) {
                (Some(colon), Some(end)) if colon < end => {
                    format!(
                        "{}\"package_version\": \"{to}\"{}",
                        &text[..start],
                        &rest[end..]
                    )
                }
                _ => text.clone(),
            }
        }
        _ => text.clone(),
    };
    write(&path, bumped.as_bytes());
}

fn write(path: &PathBuf, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(path, bytes).expect("write");
}
