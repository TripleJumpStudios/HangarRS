//! Build a preflight from the real setup and walk it through.
//!
//! ```bash
//! cargo run -p hangar-runtime --example preflight        # dry run, launches nothing
//! cargo run -p hangar-runtime --example preflight run    # actually start everything
//! ```
//!
//! **Dry run is the default.** It evaluates the real conditions — is the sim
//! process there, is SimConnect answering — and prints what it would do at each
//! step without starting anything. That is enough to see the ordering work
//! before trusting it with a flight.
//!
//! `run` presses the buttons: it opens `steam://rungameid/2537590`, the same URL
//! a person clicks, and then adopts or launches each companion app as its
//! condition comes true. It never applies a package profile — that is the one
//! step that leaves state behind, and it belongs behind the UI's own gate.

use hangar_catalog::ItemKind;
use hangar_runtime::preflight::{Action, Condition, Next, Preflight, Run, State, Step};
use hangar_runtime::{sim, LaunchRequest, RunMode};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

fn main() {
    let dry = std::env::args().nth(1).as_deref() != Some("run");

    let steam = match hangar_steam::SteamEnv::discover() {
        Ok(env) => env,
        Err(e) => {
            eprintln!("no Steam installation found: {e}");
            std::process::exit(2);
        }
    };
    let msfs = match hangar_msfs::MsfsEnv::discover(&steam) {
        Ok(env) => env,
        Err(e) => {
            eprintln!("MSFS was not found: {e}");
            std::process::exit(2);
        }
    };

    // The previous application's companion prefix, where existing companion
    // apps are installed.
    let proton = match hangar_runtime::ProtonEnv::for_msfs(&msfs, &steam)
        .and_then(|env| hangar_runtime::env::legacy_companion_dir().map(|dir| env.companion(&dir)))
    {
        Some(env) => env,
        None => {
            eprintln!("no Proton environment could be built for MSFS");
            std::process::exit(2);
        }
    };

    // The companion apps, from the catalogue.
    let catalog = hangar_catalog::scan(&msfs, &[]);
    let apps: Vec<_> = catalog
        .items
        .iter()
        .filter(|i| {
            matches!(
                i.kind,
                ItemKind::CompanionApp | ItemKind::SimSessionApp | ItemKind::NativeTool
            )
        })
        .cloned()
        .collect();

    let mut requests: BTreeMap<String, LaunchRequest> = BTreeMap::new();
    let mut preflight = Preflight::new("Before a flight").with(Step::now(Action::LaunchSim {
        app_id: sim::MSFS_2024_APP_ID.to_string(),
    }));

    for item in &apps {
        // Everything here talks to the sim, so everything waits for SimConnect.
        // Optional: one companion app failing is worth reporting, not worth
        // abandoning the flight for.
        preflight = preflight.with(
            Step::after(
                Action::LaunchApp {
                    label: item.title.clone(),
                },
                Condition::SimConnectReady,
            )
            .optional(),
        );
        requests.insert(
            item.title.clone(),
            LaunchRequest {
                label: item.title.clone(),
                exe: item.path.clone(),
                args: item.args.clone(),
                mode: match item.kind {
                    ItemKind::SimSessionApp => RunMode::SimSession,
                    ItemKind::NativeTool => RunMode::Native,
                    _ => RunMode::Prefix,
                },
            },
        );
    }

    println!("{}  \u{2014}  {} steps", preflight.name, preflight.len());
    for step in &preflight.steps {
        println!(
            "  {:<34}  waits for {}{}",
            step.action.describe(),
            step.wait_for.as_str(),
            if step.optional { "  (optional)" } else { "" }
        );
    }
    println!();

    // The SimConnect client is async and needs a reactor in scope before it is
    // spawned, so the runtime is built once here and both paths run inside it.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let _guard = runtime.enter();

    // The link reconnects on its own; giving it a moment means a dry run
    // against an already-running sim reports readiness rather than a race.
    let conditions = Conditions::new();
    std::thread::sleep(Duration::from_millis(800));

    if dry {
        dry_run(preflight, &conditions);
    } else {
        live_run(
            preflight,
            &requests,
            &conditions,
            &proton,
            hangar_runtime::ProtonEnv::for_msfs(&msfs, &steam).as_ref(),
            &runtime,
        );
    }
}

/// Walk the sequence against the real conditions, doing nothing.
fn dry_run(preflight: Preflight, conditions: &Conditions) {
    println!("Dry run \u{2014} conditions are read for real, nothing is started.\n");
    let mut run = Run::start(preflight);
    let now = Instant::now();

    loop {
        match run.tick(now, |c| conditions.met(c)) {
            Next::Do { step } => {
                let action = run.preflight().steps[step].action.clone();
                println!("  would now: {}", action.describe());
                run.acted("dry run");
            }
            Next::Waiting {
                step, condition, ..
            } => {
                println!(
                    "  would wait at step {} for {} \u{2014} not true right now",
                    step + 1,
                    condition.as_str()
                );
                break;
            }
            Next::Finished => break,
        }
    }

    println!("\n{}", run.summary());
    if !sim::is_running() {
        println!("The sim is not running, which is why the wait is where it is.");
    }
}

/// Actually do it.
fn live_run(
    preflight: Preflight,
    requests: &BTreeMap<String, LaunchRequest>,
    conditions: &Conditions,
    proton: &hangar_runtime::ProtonEnv,
    sim_proton: Option<&hangar_runtime::ProtonEnv>,
    runtime: &tokio::runtime::Runtime,
) {
    let sessions = hangar_runtime::sim_session::Runtime::live();
    let mut run = Run::start(preflight);
    let mut last_note = String::new();

    loop {
        let next = run.tick(Instant::now(), |c| conditions.met(c));
        match next {
            Next::Do { step } => {
                let action = run.preflight().steps[step].action.clone();
                println!("\n{}", action.describe());
                match &action {
                    Action::LaunchSim { app_id } => match sim::launch(app_id) {
                        Ok(handoff) => {
                            run.acted(format!("opened {} via {}", handoff.url, handoff.opener))
                        }
                        Err(e) => {
                            run.failed(e.to_string());
                        }
                    },
                    Action::LaunchApp { label } => match requests.get(label) {
                        Some(request) => {
                            let env = proton.clone();
                            let launched = if request.mode == RunMode::SimSession {
                                match sim_proton {
                                    Some(env) => {
                                        runtime.block_on(sessions.launch(env, request, None))
                                    }
                                    None => hangar_runtime::Launched::SpawnError(
                                        "No simulator environment.".into(),
                                    ),
                                }
                            } else {
                                runtime.block_on(hangar_runtime::launch(&env, request))
                            };
                            match launched {
                                hangar_runtime::Launched::Started(app) => {
                                    run.acted(format!("started, pid {}", app.pid))
                                }
                                hangar_runtime::Launched::Adopted(app) => {
                                    run.acted(format!("already running, adopted pid {}", app.pid))
                                }
                                other => {
                                    run.failed(format!("{other:?}"));
                                }
                            }
                        }
                        None => {
                            run.failed("no launch request for that app");
                        }
                    },
                    Action::ApplyProfile { .. } => {
                        run.failed("profile steps are not run from this example");
                    }
                }
                report_last(&run);
            }
            Next::Waiting {
                step,
                condition,
                waited,
                timeout,
            } => {
                let note = format!(
                    "waiting at step {} for {} ({}s of {}s)",
                    step + 1,
                    condition.as_str(),
                    waited.as_secs(),
                    timeout.as_secs()
                );
                if note != last_note {
                    println!("  {note}");
                    last_note = note;
                }
                std::thread::sleep(Duration::from_secs(2));
            }
            Next::Finished => break,
        }
    }

    println!("\n{}", run.summary());
}

fn report_last(run: &Run) {
    if let Some(state) = run.states().iter().rev().find(|s| s.is_finished()) {
        match state {
            State::Done(what) => println!("  {what}"),
            State::Failed(why) => println!("  failed: {why}"),
            _ => {}
        }
    }
}

/// Evaluate a condition against the real machine.
///
/// `SimStarted` is a `/proc` scan. Readiness is a **real SimConnect
/// handshake**, not a TCP connect: the sim accepts the socket during its
/// loading screen and never answers `MSG_OPEN`, so "the port is open" would
/// start every companion app several minutes early — the precise failure this
/// whole feature exists to avoid.
struct Conditions {
    client: simconnect_tcp::Client,
}

impl Conditions {
    fn new() -> Self {
        Conditions {
            // Reconnects on its own for as long as it lives, so there is
            // nothing to start or stop as the sim comes and goes.
            client: simconnect_tcp::Client::spawn(simconnect_tcp::Config::default(), None),
        }
    }

    fn met(&self, condition: Condition) -> bool {
        match condition {
            Condition::Nothing => true,
            Condition::SimStarted => sim::is_running(),
            Condition::SimConnectReady => self.client.is_connected(),
            // Nothing to press in a terminal run, so both overridable gates are
            // treated as satisfied. The Preflight tab is where they are answered
            // properly, with the sync's own progress beside them.
            Condition::Manual | Condition::AppsReady => true,
            // Connected is not the same as receiving: the link can be up while
            // the aircraft is still loading. `latest_fresh` is `None` until an
            // update has actually arrived recently.
            Condition::TelemetryFlowing => self.client.latest_fresh().is_some(),
        }
    }
}
