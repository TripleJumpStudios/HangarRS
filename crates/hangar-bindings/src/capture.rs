//! Watching the real sticks so a control can be bound by pressing it.
//!
//! Opens every connected controller, polls them without blocking, and turns
//! the first thing that moves into the [`Control`] MSFS would name.
//!
//! # It proposes; it never decides
//!
//! One step in the chain is inference rather than fact. Label-to-code is
//! derived from bindings that already exist and is certain. Which *physical*
//! control an evdev code refers to is not: it rests on the profile's axis list
//! lining up with evdev's `ABS_*` order, which fits everything observed and
//! cannot be proved from a file.
//!
//! So a capture produces a [`Captured`] saying what it read, what it would
//! write, and how sure it is — and something else decides. A wrong guess here
//! writes a binding that looks correct everywhere and does nothing in the sim,
//! which is the worst kind of wrong.
//!
//! # Axes need a shove, not a nudge
//!
//! A stick at rest still emits: springs settle, potentiometers drift, and a
//! pedal set reports its own weight. Capturing the first `EV_ABS` that arrives
//! binds whichever axis was noisiest. So an axis has to move by a real fraction
//! of its travel before it counts, and buttons — which are unambiguous — are
//! always preferred over an axis seen in the same poll.

use crate::joystick::Control;
use hangar_core::evdev::{self, Reader};
use hangar_core::input::InputDevice;

/// How far an axis must move from where capture started before it counts.
///
/// Axis ranges vary by device, so this is a fraction of the travel seen so far
/// rather than a raw count. A quarter is far more than drift and far less than
/// a deliberate sweep.
const AXIS_TRAVEL: f32 = 0.25;

/// How confident the translation is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// A button. The number follows the device's own capability order, which is
    /// what `joydev` uses, and the label-to-code rule is derived from 41 real
    /// samples.
    Button,
    /// An axis mapped to one of the six MSFS names, resting on the axis-order
    /// assumption.
    InferredAxis,
}

impl Confidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Confidence::Button => "button",
            Confidence::InferredAxis => "axis (inferred)",
        }
    }

    /// Whether the person should be asked to confirm before this is written.
    ///
    /// Both are, in practice. The distinction is what the prompt says.
    pub fn needs_confirmation(self) -> bool {
        true
    }
}

/// One control, caught in the act.
#[derive(Debug, Clone)]
pub struct Captured {
    /// The device it came from.
    pub device: InputDevice,
    /// What MSFS would call it.
    pub control: Control,
    pub confidence: Confidence,
    /// The raw event, so a report can say what was actually read.
    pub raw_kind: u16,
    pub raw_code: u16,
}

impl Captured {
    /// `Joystick Button 8 (code 7)` — what would be written.
    pub fn describe(&self) -> String {
        format!(
            "{} (code {})",
            self.control.label().trim(),
            self.control.code()
        )
    }
}

/// Something a device did that could not be translated.
#[derive(Debug, Clone)]
pub struct Unmapped {
    pub device_name: String,
    pub raw_kind: u16,
    pub raw_code: u16,
}

impl Unmapped {
    /// Why nothing was proposed, in a sentence someone can act on.
    pub fn reason(&self) -> String {
        if self.raw_kind == evdev::EV_ABS {
            format!(
                "that axis (ABS {}) has no MSFS name we can derive \u{2014} no existing binding \
                 uses one, so writing a code for it would be a guess",
                self.raw_code
            )
        } else {
            format!(
                "that control (type {}, code {}) is not one this device advertised",
                self.raw_kind, self.raw_code
            )
        }
    }
}

/// A capture in progress: every connected controller, open and polled.
pub struct Capture {
    devices: Vec<Watched>,
    /// The last thing that could not be translated, for reporting.
    pub last_unmapped: Option<Unmapped>,
}

struct Watched {
    device: InputDevice,
    reader: Reader,
    /// This device's button codes in ascending order — the index into it is
    /// the button number MSFS uses, less one.
    buttons: Vec<u16>,
    /// Where each axis sat when capture began, and the widest excursion seen,
    /// so movement can be judged as a fraction of travel.
    axis_rest: std::collections::BTreeMap<u16, AxisRest>,
}

#[derive(Debug, Clone, Copy)]
struct AxisRest {
    first: i32,
    min: i32,
    max: i32,
}

impl Capture {
    /// Open every connected controller.
    ///
    /// Devices that cannot be opened are skipped and named in the returned
    /// list of problems — usually a permissions issue, which is worth saying
    /// out loud rather than presenting as a stick that never responds.
    pub fn start(devices: &[InputDevice]) -> (Self, Vec<(String, String)>) {
        let mut watched = Vec::new();
        let mut problems = Vec::new();

        for device in devices {
            let Some(handler) = device.event_handler() else {
                continue;
            };
            let path = format!("/dev/input/{handler}");
            match Reader::open(&path) {
                Ok(reader) => watched.push(Watched {
                    buttons: device.buttons(),
                    device: device.clone(),
                    reader,
                    axis_rest: Default::default(),
                }),
                Err(e) => problems.push((device.tidy_name(), format!("{path}: {e}"))),
            }
        }

        (
            Capture {
                devices: watched,
                last_unmapped: None,
            },
            problems,
        )
    }

    /// How many controllers are being watched.
    pub fn watching(&self) -> usize {
        self.devices.len()
    }

    /// Read what has happened since the last call.
    ///
    /// Returns the first control that can be translated. Buttons win over axes
    /// within a poll, because a button press is unambiguous and an axis may
    /// only be settling.
    pub fn poll(&mut self) -> Option<Captured> {
        let mut axis_hit: Option<Captured> = None;

        for watched in &mut self.devices {
            let Ok(events) = watched.reader.poll() else {
                continue;
            };

            for event in events {
                if event.is_button_press() {
                    match watched.buttons.iter().position(|c| *c == event.code) {
                        Some(index) => {
                            return Some(Captured {
                                device: watched.device.clone(),
                                control: Control::from_button_index(index),
                                confidence: Confidence::Button,
                                raw_kind: event.kind,
                                raw_code: event.code,
                            })
                        }
                        None => {
                            self.last_unmapped = Some(Unmapped {
                                device_name: watched.device.tidy_name(),
                                raw_kind: event.kind,
                                raw_code: event.code,
                            });
                        }
                    }
                } else if event.is_axis() && axis_hit.is_none() {
                    let rest = watched.axis_rest.entry(event.code).or_insert(AxisRest {
                        first: event.value,
                        min: event.value,
                        max: event.value,
                    });
                    rest.min = rest.min.min(event.value);
                    rest.max = rest.max.max(event.value);

                    if moved_enough(*rest, event.value) {
                        match Control::from_abs(event.code) {
                            Some(control) => {
                                axis_hit = Some(Captured {
                                    device: watched.device.clone(),
                                    control,
                                    confidence: Confidence::InferredAxis,
                                    raw_kind: event.kind,
                                    raw_code: event.code,
                                });
                            }
                            None => {
                                self.last_unmapped = Some(Unmapped {
                                    device_name: watched.device.tidy_name(),
                                    raw_kind: event.kind,
                                    raw_code: event.code,
                                });
                            }
                        }
                    }
                }
            }
        }

        axis_hit
    }
}

/// Has this axis moved a real fraction of the travel it has shown?
///
/// Judged against the widest excursion seen so far rather than an absolute
/// count, because a pedal set and a mini-stick do not share a range. Until an
/// axis has shown some travel there is nothing to be a fraction of, so it takes
/// a fixed nudge to get started.
fn moved_enough(rest: AxisRest, value: i32) -> bool {
    let travel = (rest.max - rest.min).unsigned_abs();
    let moved = (value - rest.first).unsigned_abs();
    if travel < 64 {
        return moved > 64;
    }
    moved as f32 > travel as f32 * AXIS_TRAVEL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_still_axis_does_not_count_as_a_press() {
        // A pedal set reporting its own weight, jittering by a few counts.
        let rest = AxisRest {
            first: 1000,
            min: 995,
            max: 1005,
        };
        assert!(!moved_enough(rest, 1002));
        assert!(!moved_enough(rest, 1040));
    }

    #[test]
    fn a_deliberate_sweep_counts() {
        let rest = AxisRest {
            first: 0,
            min: -32768,
            max: 32767,
        };
        assert!(moved_enough(rest, 20000));
        assert!(moved_enough(rest, -20000), "either direction");
    }

    #[test]
    fn an_axis_that_has_shown_no_travel_needs_a_real_nudge() {
        let rest = AxisRest {
            first: 0,
            min: 0,
            max: 0,
        };
        assert!(!moved_enough(rest, 32), "noise");
        assert!(moved_enough(rest, 500), "a push");
    }

    #[test]
    fn an_unmapped_axis_explains_itself_rather_than_proposing_a_code() {
        let unmapped = Unmapped {
            device_name: "S-TECS".into(),
            raw_kind: hangar_core::evdev::EV_ABS,
            raw_code: 6,
        };
        let reason = unmapped.reason();
        assert!(reason.contains("ABS 6"), "{reason}");
        assert!(reason.contains("guess"), "{reason}");
    }

    #[test]
    fn a_captured_control_says_exactly_what_would_be_written() {
        let captured = Captured {
            device: InputDevice::default(),
            control: Control::Button(8),
            confidence: Confidence::Button,
            raw_kind: hangar_core::evdev::EV_KEY,
            raw_code: 0x127,
        };
        assert_eq!(captured.describe(), "Joystick Button 8 (code 7)");
        assert!(captured.confidence.needs_confirmation());
    }

    #[test]
    fn starting_with_no_devices_watches_nothing_and_complains_about_nothing() {
        let (capture, problems) = Capture::start(&[]);
        assert_eq!(capture.watching(), 0);
        assert!(problems.is_empty());
    }

    #[test]
    fn a_device_that_cannot_be_opened_is_named_rather_than_dropped() {
        let device = InputDevice {
            name: "Ghost Stick".into(),
            handlers: vec!["event9999".into(), "js9".into()],
            ..Default::default()
        };
        let (capture, problems) = Capture::start(&[device]);
        assert_eq!(capture.watching(), 0);
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].0, "Ghost Stick");
        assert!(problems[0].1.contains("event9999"), "{:?}", problems[0]);
    }
}
