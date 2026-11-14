//! Reading button presses and axis movement straight from a device.
//!
//! `/dev/input/eventN` delivers fixed-size `input_event` structs. Nothing here
//! interprets them as a *control* — that is
//! `hangar_bindings::joystick::Control`'s job, because what MSFS calls a button
//! is a matter of what MSFS wrote in a file, not of what the kernel reports.
//!
//! # Non-blocking on purpose
//!
//! A GUI capture reads whatever has arrived since the last frame and returns.
//! The alternative — a thread blocked in `read` — has to be woken to be
//! cancelled, and a joystick that is perfectly still sends nothing to wake it
//! with. So the descriptor is opened `O_NONBLOCK` and polled.
//!
//! # Permissions
//!
//! Reading these needs membership of the `input` group, or the udev ACL a
//! desktop session grants for the local seat. Both are normal; neither is
//! guaranteed. [`Reader::open`] fails with the real `io::Error` rather than an
//! empty stream, so a permission problem says so instead of looking like a
//! device that is not sending anything.

use std::io::{self, Read};
use std::path::Path;

/// `EV_KEY` — a button went down or up.
pub const EV_KEY: u16 = 0x01;
/// `EV_ABS` — an absolute axis moved.
pub const EV_ABS: u16 = 0x03;

/// `struct input_event` on 64-bit Linux: two 8-byte time fields, then
/// `type`, `code`, `value`.
const EVENT_SIZE: usize = 24;

/// How many events to take from the queue in one poll.
///
/// A stick pushed to a corner produces a steady stream, and a frame that
/// drains an unbounded queue is a frame that does not end.
const MAX_PER_POLL: usize = 256;

/// One event, with the parts that matter kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    pub kind: u16,
    pub code: u16,
    pub value: i32,
}

impl Event {
    /// A button going **down**. Releases are ignored by capture: the control
    /// someone means is the one they pressed, and a release arrives after they
    /// have already seen the result.
    pub fn is_button_press(&self) -> bool {
        self.kind == EV_KEY && self.value != 0
    }

    pub fn is_axis(&self) -> bool {
        self.kind == EV_ABS
    }
}

/// An open device, read without blocking.
#[derive(Debug)]
pub struct Reader {
    file: std::fs::File,
    path: std::path::PathBuf,
}

impl Reader {
    /// Open `/dev/input/eventN` for reading, non-blocking.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        use nix::fcntl::{open, OFlag};
        use nix::sys::stat::Mode;

        let path = path.as_ref();
        // `nix` hands back an `OwnedFd`, so the descriptor has a single owner
        // from here on and no `unsafe` is needed to take it.
        let fd = open(path, OFlag::O_RDONLY | OFlag::O_NONBLOCK, Mode::empty())
            .map_err(|e| io::Error::from_raw_os_error(e as i32))?;
        let file = std::fs::File::from(fd);

        Ok(Reader {
            file,
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Everything queued since the last call. Empty when nothing happened.
    ///
    /// A device unplugged mid-capture reports an error from `read`; that is
    /// returned rather than silently becoming "no events", because the two mean
    /// very different things to someone waiting for a button press.
    pub fn poll(&mut self) -> io::Result<Vec<Event>> {
        let mut events = Vec::new();
        let mut buffer = [0u8; EVENT_SIZE * 32];

        while events.len() < MAX_PER_POLL {
            match self.file.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    for chunk in buffer[..read].chunks_exact(EVENT_SIZE) {
                        events.push(parse_event(chunk));
                    }
                    // A short read means the queue is drained.
                    if read < buffer.len() {
                        break;
                    }
                }
                // Nothing waiting, which is the normal case.
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(events)
    }
}

/// Decode one `input_event`. The two time fields are skipped.
fn parse_event(bytes: &[u8]) -> Event {
    debug_assert_eq!(bytes.len(), EVENT_SIZE);
    Event {
        kind: u16::from_ne_bytes([bytes[16], bytes[17]]),
        code: u16::from_ne_bytes([bytes[18], bytes[19]]),
        value: i32::from_ne_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(kind: u16, code: u16, value: i32) -> [u8; EVENT_SIZE] {
        let mut bytes = [0u8; EVENT_SIZE];
        // The time fields are left zero; nothing here reads them.
        bytes[16..18].copy_from_slice(&kind.to_ne_bytes());
        bytes[18..20].copy_from_slice(&code.to_ne_bytes());
        bytes[20..24].copy_from_slice(&value.to_ne_bytes());
        bytes
    }

    #[test]
    fn an_event_decodes_at_the_right_offsets() {
        // BTN_TRIGGER down.
        let event = parse_event(&encode(EV_KEY, 0x120, 1));
        assert_eq!(event.kind, EV_KEY);
        assert_eq!(event.code, 0x120);
        assert_eq!(event.value, 1);
        assert!(event.is_button_press());
    }

    #[test]
    fn a_release_is_not_a_press() {
        let release = parse_event(&encode(EV_KEY, 0x120, 0));
        assert!(!release.is_button_press());
        assert!(!release.is_axis());
    }

    #[test]
    fn a_negative_axis_value_survives_the_decode() {
        // Axes are signed, and a stick pulled back reads negative.
        let event = parse_event(&encode(EV_ABS, 1, -32768));
        assert!(event.is_axis());
        assert_eq!(event.value, -32768);
        assert!(!event.is_button_press());
    }

    #[test]
    fn the_struct_is_the_size_the_kernel_writes() {
        // 64-bit Linux: two 8-byte time fields, u16, u16, i32.
        assert_eq!(EVENT_SIZE, 16 + 2 + 2 + 4);
    }

    #[test]
    fn opening_something_that_is_not_a_device_says_why() {
        let err = Reader::open("/dev/input/event-does-not-exist").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
