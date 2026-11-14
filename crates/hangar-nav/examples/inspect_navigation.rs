//! Read a selected database without changing it; never run by tests or renders.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("usage: inspect_navigation <LittleNavMap.sqlite>")?;
    let snapshot = hangar_nav::load(std::path::Path::new(&path))?;
    println!(
        "{} airports, {} navaids; AIRAC {}",
        snapshot.airports,
        snapshot.navaids,
        snapshot.cycle.as_deref().unwrap_or("not supplied")
    );
    Ok(())
}
