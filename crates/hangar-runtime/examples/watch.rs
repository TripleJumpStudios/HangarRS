//! Report — and optionally stop — a running app, by executable name.
//!
//! ```text
//! cargo run -p hangar-runtime --example watch "Project Airports.exe"
//! cargo run -p hangar-runtime --example watch "Project Airports.exe" --kill
//! ```
//!
//! Without `--kill` it only reads `/proc`.

use hangar_runtime::{kill_app, proc};

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let Some(needle) = args.next() else {
        eprintln!("usage: watch <executable name> [--kill]");
        std::process::exit(2);
    };
    let should_kill = args.any(|a| a == "--kill");

    let found = proc::find_app(&needle);
    println!("\n\x1b[1m{needle}\x1b[0m");

    if !found.is_running() {
        println!("  not running");
        if !found.helpers.is_empty() {
            println!(
                "  ({} helper process(es) with no main process — unusual)",
                found.helpers.len()
            );
        }
        return;
    }

    let main = found.main.as_ref().unwrap();
    println!("  running as pid {}", main.pid);
    println!("  {} process(es) in total", found.count());
    // `comm` and `exe` are shown to make the point, not to match on.
    if let Ok(comm) = proc::read_comm(main.pid) {
        println!("  comm:    {comm}   <- not the app's name");
    }
    if let Ok(exe) = std::fs::read_link(format!("/proc/{}/exe", main.pid)) {
        println!(
            "  exe:     {}   <- the same for every Wine process",
            exe.display()
        );
    }
    println!("  cmdline: {}", truncate(&main.cmdline, 100));
    if let Ok(ppid) = proc::read_ppid(main.pid) {
        let parent = proc::read_comm(ppid).unwrap_or_default();
        println!("  parent:  {ppid} ({parent})");
    }
    for helper in &found.helpers {
        println!(
            "    helper {} — {}",
            helper.pid,
            truncate(&helper.cmdline, 76)
        );
    }

    if !should_kill {
        println!("\n  read-only. pass --kill to stop it.");
        return;
    }

    println!("\n  stopping…");
    let outcome = kill_app(&needle).await;
    println!("  {}", outcome.summary());
    let after = proc::find_app(&needle);
    println!(
        "  now: {} process(es){}",
        after.count(),
        if after.is_running() { "" } else { " — gone" }
    );
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    s.chars().take(max - 1).collect::<String>() + "…"
}
