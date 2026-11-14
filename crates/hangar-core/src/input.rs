//! What is actually plugged in, from `/proc/bus/input/devices`.
//!
//! A plain text file the kernel maintains, so this needs no library, no udev
//! connection and no permissions beyond reading `/proc`.
//!
//! # Why this is worth having
//!
//! MSFS records a device by name and by `ProductID`, and **only one of those
//! is trustworthy**. The kernel reports the Gladiator as
//!
//! ```text
//! "VKB-Sim (C) Alex Oz 2023  VKBsim Gladiator EVO R FSM.GA "
//! ```
//!
//! and MSFS stored the same stick as
//!
//! ```text
//! "Sim (C) Alex Oz 2023 VKBsim Gladiator EVO R FSM.GA"
//! ```
//!
//! — `VKB-` dropped from the front, the double space collapsed, the trailing
//! space gone. Matching on the name gets this wrong. `ProductID` is the USB
//! product id in decimal and matches exactly: `0x0220` = 544 for the Gladiator,
//! `0x012e` = 302 for the S-TECS, `0x7012` = 28690 for the VelocityOne pedals.
//!
//! So: **correlate on the USB id, never on the name.**

use std::io;

/// One device the kernel is reporting.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InputDevice {
    /// `N: Name="…"`, exactly as the kernel has it, trailing spaces included.
    pub name: String,
    pub bus: u16,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
    /// `P: Phys=` — the physical port. Stable across replug of the same socket.
    pub phys: String,
    /// `U: Uniq=` — a serial number when the device offers one. Usually empty.
    pub uniq: Option<String>,
    /// `H: Handlers=` — `event6`, `js0`, `mouse0`, `kbd`, …
    pub handlers: Vec<String>,
    /// `B: ABS=` — absolute axes. A stick or pedal set has them; a mouse does
    /// not report them this way.
    pub abs: Option<String>,
    /// `B: KEY=` — every key or button the device can report.
    pub key: Option<String>,
}

impl InputDevice {
    /// `231d:0220`, the way `lsusb` writes it.
    pub fn usb_id(&self) -> String {
        format!("{:04x}:{:04x}", self.vendor, self.product)
    }

    /// Whether the kernel gave this device a joystick handler.
    ///
    /// The `jsN` handler is the signal, not the name: "Rudder" and "Throttle"
    /// are not joysticks by name and both are by behaviour.
    pub fn is_joystick(&self) -> bool {
        self.handlers.iter().any(|h| h.starts_with("js"))
    }

    /// The `eventN` handler, which is what `evdev` opens.
    pub fn event_handler(&self) -> Option<&str> {
        self.handlers
            .iter()
            .find(|h| h.starts_with("event"))
            .map(String::as_str)
    }

    /// Every button code this device can report, ascending.
    ///
    /// **Ascending code order is the button order.** That is how `joydev`
    /// numbers buttons, so a device's *n*-th entry here is the one MSFS calls
    /// `Joystick Button n+1`.
    pub fn buttons(&self) -> Vec<u16> {
        self.key.as_deref().map(parse_bitmap).unwrap_or_default()
    }

    /// Every absolute axis code this device reports, ascending.
    ///
    /// Includes hats — `ABS_HAT0X` is 16 and `ABS_HAT0Y` is 17 — which are axes
    /// to the kernel and buttons to most people. Callers that only want the
    /// stick axes should filter to codes below 16.
    pub fn axes(&self) -> Vec<u16> {
        self.abs.as_deref().map(parse_bitmap).unwrap_or_default()
    }

    /// The name with the kernel's stray whitespace tidied, for display only.
    ///
    /// Never use this to match anything — see the module note.
    pub fn tidy_name(&self) -> String {
        self.name.split_whitespace().collect::<Vec<_>>().join(" ")
    }
}

/// Read and parse `/proc/bus/input/devices`.
pub fn connected() -> io::Result<Vec<InputDevice>> {
    Ok(parse(&std::fs::read_to_string("/proc/bus/input/devices")?))
}

/// Just the ones with a joystick handler.
pub fn controllers() -> io::Result<Vec<InputDevice>> {
    Ok(connected()?
        .into_iter()
        .filter(|d| d.is_joystick())
        .collect())
}

/// Parse the file's contents. Blocks are separated by a blank line.
pub fn parse(text: &str) -> Vec<InputDevice> {
    let mut devices = Vec::new();
    let mut current = InputDevice::default();
    let mut started = false;

    for line in text.lines() {
        if line.trim().is_empty() {
            if started {
                devices.push(std::mem::take(&mut current));
                started = false;
            }
            continue;
        }
        let Some((tag, rest)) = line.split_once(": ") else {
            continue;
        };
        started = true;
        match tag {
            "I" => {
                for field in rest.split_whitespace() {
                    let Some((key, value)) = field.split_once('=') else {
                        continue;
                    };
                    // Every one of these is hex, without an 0x prefix.
                    let parsed = u16::from_str_radix(value, 16).unwrap_or(0);
                    match key {
                        "Bus" => current.bus = parsed,
                        "Vendor" => current.vendor = parsed,
                        "Product" => current.product = parsed,
                        "Version" => current.version = parsed,
                        _ => {}
                    }
                }
            }
            "N" => {
                current.name = rest
                    .trim_start_matches("Name=")
                    .trim_matches('"')
                    .to_string();
            }
            "P" => current.phys = rest.trim_start_matches("Phys=").to_string(),
            "U" => {
                let uniq = rest.trim_start_matches("Uniq=").trim();
                current.uniq = (!uniq.is_empty()).then(|| uniq.to_string());
            }
            "H" => {
                current.handlers = rest
                    .trim_start_matches("Handlers=")
                    .split_whitespace()
                    .map(str::to_string)
                    .collect();
            }
            "B" => {
                if let Some(abs) = rest.strip_prefix("ABS=") {
                    current.abs = Some(abs.trim().to_string());
                } else if let Some(key) = rest.strip_prefix("KEY=") {
                    current.key = Some(key.trim().to_string());
                }
            }
            _ => {}
        }
    }

    // A file that does not end with a blank line still has a last device.
    if started {
        devices.push(current);
    }
    devices
}

/// Expand one of `/proc`'s capability bitmaps into the codes it has set.
///
/// The words are hex `u64`s printed **highest first**, so the last word on the
/// line holds bits 0–63. Getting that backwards yields a plausible-looking list
/// of codes that is entirely wrong, which is why it has its own test against a
/// real device's line.
pub fn parse_bitmap(bitmap: &str) -> Vec<u16> {
    let words: Vec<u64> = bitmap
        .split_whitespace()
        .map(|w| u64::from_str_radix(w, 16).unwrap_or(0))
        .collect();

    let mut codes = Vec::new();
    for (position, word) in words.iter().enumerate() {
        if *word == 0 {
            continue;
        }
        // Leftmost word is the highest.
        let index = words.len() - 1 - position;
        for bit in 0..64u32 {
            if word & (1u64 << bit) != 0 {
                codes.push((index as u32 * 64 + bit) as u16);
            }
        }
    }
    codes.sort_unstable();
    codes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Copied from the real file on this machine, whitespace included.
    const SAMPLE: &str = concat!(
        "I: Bus=0003 Vendor=231d Product=0220 Version=0111\n",
        "N: Name=\"VKB-Sim (C) Alex Oz 2023  VKBsim Gladiator EVO R FSM.GA \"\n",
        "P: Phys=usb-0000:0f:00.3-2/input0\n",
        "S: Sysfs=/devices/pci0000:00/0000:0f:00.3/usb5/5-2/input/input7\n",
        "U: Uniq=\n",
        "H: Handlers=event6 js0 \n",
        "B: PROP=0\n",
        "B: EV=1b\n",
        "B: KEY=ffffffffffffffff 0 0 0 0 0 0 ffff00000000 0 0 0 0\n",
        "B: ABS=300ff\n",
        "\n",
        "I: Bus=0003 Vendor=10f5 Product=7012 Version=0110\n",
        "N: Name=\"Turtle Beach VelocityOne Rudder\"\n",
        "P: Phys=usb-0000:00:14.0-7/input0\n",
        "U: Uniq=00008C4D6B79AB48\n",
        "H: Handlers=event13 js2 \n",
        "B: ABS=7\n",
        "\n",
        "I: Bus=0003 Vendor=046d Product=407f Version=0111\n",
        "N: Name=\"Logitech G502\"\n",
        "P: Phys=usb-0000:00:14.0-2/input1\n",
        "U: Uniq=\n",
        "H: Handlers=sysrq kbd leds mouse0 event2 \n",
    );

    #[test]
    fn the_usb_ids_come_out_as_lsusb_writes_them() {
        let devices = parse(SAMPLE);
        assert_eq!(
            devices.len(),
            3,
            "the last block has no trailing blank line"
        );
        assert_eq!(devices[0].usb_id(), "231d:0220");
        assert_eq!(devices[1].usb_id(), "10f5:7012");
        // And in decimal, which is what MSFS records as ProductID.
        assert_eq!(devices[0].product, 544);
        assert_eq!(devices[1].product, 28690);
    }

    #[test]
    fn a_joystick_is_one_by_its_handler_not_its_name() {
        let devices = parse(SAMPLE);
        assert!(devices[0].is_joystick());
        // "Rudder" is not a joystick by name and is one by behaviour.
        assert!(devices[1].is_joystick());
        // A mouse has handlers but no js.
        assert!(!devices[2].is_joystick());
    }

    #[test]
    fn the_kernels_stray_whitespace_is_kept_and_tidied_separately() {
        let devices = parse(SAMPLE);
        // Kept verbatim, because it is what the kernel said.
        assert!(devices[0].name.ends_with("FSM.GA "));
        assert!(devices[0].name.contains("2023  VKBsim"));
        // Tidied only for display.
        assert_eq!(
            devices[0].tidy_name(),
            "VKB-Sim (C) Alex Oz 2023 VKBsim Gladiator EVO R FSM.GA"
        );
    }

    #[test]
    fn an_empty_serial_is_none_rather_than_an_empty_string() {
        let devices = parse(SAMPLE);
        assert_eq!(devices[0].uniq, None);
        assert_eq!(devices[1].uniq.as_deref(), Some("00008C4D6B79AB48"));
    }

    #[test]
    fn handlers_are_split_and_the_event_node_is_reachable() {
        let devices = parse(SAMPLE);
        assert_eq!(devices[0].handlers, vec!["event6", "js0"]);
        assert_eq!(devices[0].event_handler(), Some("event6"));
        assert_eq!(devices[2].event_handler(), Some("event2"));
    }

    #[test]
    fn a_capability_bitmap_reads_from_the_right_end() {
        // The Gladiator's real KEY line. Two runs of buttons: 0x120..0x12F
        // (BTN_TRIGGER..BTN_DEAD) and 0x2C0..0x2FF (BTN_TRIGGER_HAPPY*).
        let buttons = parse_bitmap("ffffffffffffffff 0 0 0 0 0 0 ffff00000000 0 0 0 0");
        assert_eq!(buttons.len(), 80, "16 + 64 buttons");
        assert_eq!(buttons[0], 0x120, "the first button is BTN_TRIGGER");
        assert_eq!(buttons[15], 0x12F);
        assert_eq!(buttons[16], 0x2C0, "then the happy range");
        assert_eq!(buttons[79], 0x2FF);
        // Reading the words the other way round would put the first button at
        // 0, which is a plausible-looking answer and the wrong one.
        assert!(!buttons.contains(&0));
    }

    #[test]
    fn the_axis_bitmap_matches_what_the_profile_lists() {
        // 0x300ff: eight stick axes plus a hat. The S-TECS profile lists
        // exactly eight — X Y Z rX rY rZ SliderX SliderY — and no hat.
        let axes = parse_bitmap("300ff");
        assert_eq!(
            axes,
            vec![0, 1, 2, 3, 4, 5, 6, 7, 16, 17],
            "bits 16/17 are ABS_HAT0X/Y"
        );
        let stick_axes: Vec<u16> = axes.into_iter().filter(|c| *c < 16).collect();
        assert_eq!(stick_axes.len(), 8);
    }

    #[test]
    fn a_device_exposes_its_buttons_and_axes_in_order() {
        let devices = parse(SAMPLE);
        let gladiator = &devices[0];
        assert_eq!(gladiator.buttons().len(), 80);
        assert_eq!(gladiator.buttons()[7], 0x127, "the eighth button");
        assert_eq!(gladiator.axes().first(), Some(&0));
        // The pedals block has no KEY line at all.
        assert!(devices[1].buttons().is_empty());
    }

    #[test]
    fn nonsense_does_not_panic() {
        assert!(parse("").is_empty());
        assert!(parse("\n\n\n").is_empty());
        // A truncated block still yields what it had.
        let partial = parse("I: Bus=0003 Vendor=zzzz Product=0220 Version=0111\n");
        assert_eq!(partial.len(), 1);
        assert_eq!(partial[0].vendor, 0, "an unparseable field reads as zero");
        assert_eq!(partial[0].product, 544);
    }
}
