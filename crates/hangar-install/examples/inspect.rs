//! Stage 1 against a real file, read-only.
//!
//! ```bash
//! cargo run -p hangar-install --example inspect -- ~/Downloads/setup.exe
//! ```
//!
//! Reports what built the installer and what said so. Nothing is run, nothing
//! is unpacked, and no prefix is touched — this is the check that is safe to
//! point at a file you have not decided to install yet.

use std::path::PathBuf;

use hangar_install::vendor::Table;
use hangar_install::{inspect, Engine, Inspection};

fn main() {
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if args.is_empty() {
        eprintln!("usage: inspect <file.exe> [more files…]");
        eprintln!();
        eprintln!("Reads headers, version resources and the files beside it.");
        eprintln!("Runs nothing.");
        std::process::exit(2);
    }

    let table = Table::load();
    for warning in &table.warnings {
        eprintln!("vendor table: {warning}");
    }

    for path in &args {
        println!("{}", "─".repeat(72));
        match inspect(path) {
            Ok(report) => {
                print_report(&report);
                print_vendor(&table, &report);
            }
            Err(e) => println!("{}\n  {e}", path.display()),
        }
    }
}

fn print_report(report: &Inspection) {
    println!("{}", report.path.display());
    println!("  {:.1} MB", report.size as f64 / 1_048_576.0);

    let engine = report.engine.label();
    if report.looks_like_installer() {
        let hand_off = if report.engine.self_extracting() {
            "  (self-extracting: the stub exits while its engine carries on)"
        } else {
            ""
        };
        println!("  engine       {engine}{hand_off}");
    } else {
        println!("  engine       {engine} — nothing identifies this as an installer");
    }

    println!(
        "  identity     {}  [{}]",
        report.identity.describe(),
        report.identity.source.label()
    );

    if let Some(pe) = &report.pe {
        println!("  format       {}", pe.bitness.label());
        println!("  sections     {}", pe.section_names().join(" "));
        if !pe.version.is_empty() {
            for key in [
                "CompanyName",
                "ProductName",
                "ProductVersion",
                "FileVersion",
                "FileDescription",
                "OriginalFilename",
            ] {
                if let Some(value) = pe.version.get(key) {
                    println!("    {key:<17} {}", elide(value, 60));
                }
            }
        }
    }

    if let Some(packed) = &report.packed {
        println!("  packed       {packed} — its strings are not readable until it runs");
    }

    for e in &report.evidence {
        println!("  evidence     {}  [{}]", e.fact, e.source);
    }

    let siblings = &report.siblings;
    if !siblings.cabs.is_empty() {
        let bytes: u64 = siblings
            .cabs
            .iter()
            .filter_map(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .sum();
        println!(
            "  payload      {} cab{} beside it, {:.1} GB",
            siblings.cabs.len(),
            if siblings.cabs.len() == 1 { "" } else { "s" },
            bytes as f64 / 1_073_741_824.0
        );
    }
    if let Some(ini) = &siblings.setup_ini {
        if let Some(script) = &ini.script_driven {
            println!("  setup.ini    ScriptDriven={script}");
        }
    }

    if report.engine == Engine::Unknown && report.packed.is_none() {
        println!("  → treat as an ordinary application, not an installer");
    }
}

/// Stage 2: what the table says about running this family.
fn print_vendor(table: &Table, report: &Inspection) {
    let Some(found) = table.lookup(report) else {
        println!("  vendor       no profile — stage 3 would have to ask");
        return;
    };
    let profile = found.profile;
    let generic = if found.is_generic() {
        " (family default)"
    } else {
        ""
    };
    println!("  vendor       {}{generic}", profile.name);
    println!("    matched on       {}", found.why.join(", "));
    println!(
        "    run              proton {} in the {} prefix, cwd {}",
        profile.verb.as_str(),
        profile.prefix.label(),
        profile.cwd.label()
    );
    println!("    expect           {}", profile.produces.label());
    if !profile.runtimes.is_empty() {
        println!("    needs            {}", profile.runtimes.join(", "));
    }
    for (key, value) in &profile.env {
        println!("    env              {key}={value}");
    }
    if !profile.bad_console.is_empty() {
        println!(
            "    failure strings  {}",
            profile
                .bad_console
                .iter()
                .map(|s| format!("{s:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if let Some(notes) = &profile.notes {
        for line in wrap(notes.trim(), 68) {
            println!("    │ {line}");
        }
    }
}

/// Wrap prose to a width a terminal can read. A note is the one field here
/// that is a paragraph rather than a value.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for paragraph in text.split("\n\n") {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
                out.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        if !line.is_empty() {
            out.push(line);
        }
        out.push(String::new());
    }
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out
}

fn elide(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let head: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{head}…")
}
