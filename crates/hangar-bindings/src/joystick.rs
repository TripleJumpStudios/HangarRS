//! Translating between a Linux joystick and the codes MSFS writes.
//!
//! To rebind something from inside HangarRS, a button press has to become the
//! pair MSFS stores: a label like `Joystick Button 8` and a numeric code like
//! `7`. Getting that wrong writes a binding that looks right in the file, looks
//! right in the sim's controls menu, and does nothing.
//!
//! # None of this is guessed
//!
//! The mapping is derived from bindings that already exist. Across a set of
//! real profiles there are 46 label/code pairs, and they are completely
//! regular:
//!
//! | Observed | Rule |
//! |---|---|
//! | `Joystick Button 1` → 0 … `Joystick Button 75` → 74, 41 samples | `code = number - 1` |
//! | `Joystick R-Axis X/Y/Z` → 770, 786, 802 | `0x0302 + axis * 0x10` |
//! | `Joystick L-Axis X/Y/Z` → 1026, 1042, 1058 | `0x0402 + axis * 0x10` |
//!
//! Which bank is which is confirmed by two devices independently. A profile
//! lists its axes as `X Y Z rX rY rZ SliderX SliderY`, and:
//!
//! - the S-TECS binds throttles and mixture to `L-Axis X/Y/Z` and camera pan to
//!   `R-Axis X/Y`;
//! - the VelocityOne pedals have only `X Y Z` and bind both brakes and the
//!   rudder to `L-Axis X/Y/Z`.
//!
//! So **L is the `X/Y/Z` triplet and R is `rX/rY/rZ`** — not left and right
//! hand, and not the order they are numbered in.
//!
//! # What is still a hypothesis
//!
//! That the profile's axis list lines up with evdev's `ABS_*` order. It fits
//! everything observed — the S-TECS reports exactly eight `ABS` axes and lists
//! exactly eight — but it cannot be *proved* from a file. So capture never
//! writes what it inferred: it shows the control it thinks you moved and waits
//! for you to agree. See [`Control::from_abs`].

use crate::model::Key;

/// Which triplet of axes a control belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bank {
    /// The `X`, `Y`, `Z` triplet — the first three axes a device reports.
    L,
    /// The `rX`, `rY`, `rZ` triplet.
    R,
}

impl Bank {
    /// The code of this bank's `X` axis.
    fn base(self) -> u32 {
        match self {
            Bank::R => 0x0302,
            Bank::L => 0x0402,
        }
    }

    fn letter(self) -> char {
        match self {
            Bank::L => 'L',
            Bank::R => 'R',
        }
    }
}

/// `X`, `Y` or `Z` within a bank.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Axis {
    X,
    Y,
    Z,
}

impl Axis {
    /// Steps of `0x10` from the bank's base.
    fn offset(self) -> u32 {
        match self {
            Axis::X => 0,
            Axis::Y => 1,
            Axis::Z => 2,
        }
    }

    fn from_offset(offset: u32) -> Option<Self> {
        match offset {
            0 => Some(Axis::X),
            1 => Some(Axis::Y),
            2 => Some(Axis::Z),
            _ => None,
        }
    }

    fn letter(self) -> char {
        match self {
            Axis::X => 'X',
            Axis::Y => 'Y',
            Axis::Z => 'Z',
        }
    }
}

/// One control on a stick, as MSFS names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Control {
    /// **One-based**, the way the label reads. `Button(8)` writes code `7`.
    Button(u32),
    Axis {
        bank: Bank,
        axis: Axis,
    },
}

impl Control {
    /// The number MSFS stores as the element's text.
    pub fn code(self) -> u32 {
        match self {
            Control::Button(n) => n.saturating_sub(1),
            Control::Axis { bank, axis } => bank.base() + axis.offset() * 0x10,
        }
    }

    /// The label MSFS stores in `Information`.
    ///
    /// Axis labels carry a **trailing space** in every real profile —
    /// `"Joystick L-Axis X "`. It is preserved here because this string is
    /// written into a file that has to round-trip byte-identical.
    pub fn label(self) -> String {
        match self {
            Control::Button(n) => format!("Joystick Button {n}"),
            Control::Axis { bank, axis } => {
                format!("Joystick {}-Axis {} ", bank.letter(), axis.letter())
            }
        }
    }

    /// The `<KEY>` element this control becomes.
    pub fn to_key(self) -> Key {
        Key::new(self.label(), self.code().to_string())
    }

    /// Recover a control from a label MSFS wrote.
    pub fn from_label(label: &str) -> Option<Self> {
        let label = label.trim();
        if let Some(rest) = label.strip_prefix("Joystick Button ") {
            return rest.trim().parse().ok().map(Control::Button);
        }
        let rest = label.strip_prefix("Joystick ")?;
        let (bank, rest) = match rest.strip_prefix("L-Axis ") {
            Some(rest) => (Bank::L, rest),
            None => (Bank::R, rest.strip_prefix("R-Axis ")?),
        };
        let axis = match rest.trim() {
            "X" => Axis::X,
            "Y" => Axis::Y,
            "Z" => Axis::Z,
            _ => return None,
        };
        Some(Control::Axis { bank, axis })
    }

    /// Recover a control from a code.
    ///
    /// Ambiguous by nature at the low end — code `7` is `Button 8`, and there
    /// is no code that is only an axis below `0x0302` — so buttons win below
    /// the first axis bank.
    pub fn from_code(code: u32) -> Option<Self> {
        for bank in [Bank::R, Bank::L] {
            if code >= bank.base() {
                let delta = code - bank.base();
                if delta % 0x10 == 0 {
                    if let Some(axis) = Axis::from_offset(delta / 0x10) {
                        return Some(Control::Axis { bank, axis });
                    }
                }
            }
        }
        (code < Bank::R.base()).then(|| Control::Button(code + 1))
    }

    /// The control for a device's *n*-th button, counted from zero in
    /// ascending evdev code order.
    ///
    /// That order is what `joydev` uses to number buttons, and what a device's
    /// capability bitmap gives directly — see
    /// `hangar_core::input::InputDevice::buttons`.
    pub fn from_button_index(index: usize) -> Self {
        Control::Button(index as u32 + 1)
    }

    /// The control for an evdev `ABS_*` code, if one is known.
    ///
    /// `None` for anything past the six mapped axes — hats, sliders and the
    /// rest. **`None` means "do not write this"**, not "pick something": no
    /// real binding uses a slider, so there is no evidence for what MSFS would
    /// call it, and inventing a code here writes a binding that silently does
    /// nothing.
    pub fn from_abs(abs_code: u16) -> Option<Self> {
        // ABS_X=0, ABS_Y=1, ABS_Z=2, ABS_RX=3, ABS_RY=4, ABS_RZ=5.
        let (bank, axis) = match abs_code {
            0 => (Bank::L, Axis::X),
            1 => (Bank::L, Axis::Y),
            2 => (Bank::L, Axis::Z),
            3 => (Bank::R, Axis::X),
            4 => (Bank::R, Axis::Y),
            5 => (Bank::R, Axis::Z),
            _ => return None,
        };
        Some(Control::Axis { bank, axis })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every distinct label/code pair in the real profiles the mapping was
    /// derived from.
    ///
    /// This is the ground truth the whole module is derived from, so it is
    /// checked rather than described.
    const REAL: &[(&str, u32)] = &[
        ("Joystick Button 1", 0),
        ("Joystick Button 2", 1),
        ("Joystick Button 4", 3),
        ("Joystick Button 6", 5),
        ("Joystick Button 8", 7),
        ("Joystick Button 16", 15),
        ("Joystick Button 31", 30),
        ("Joystick Button 55", 54),
        ("Joystick Button 75", 74),
        ("Joystick R-Axis X ", 770),
        ("Joystick R-Axis Y ", 786),
        ("Joystick R-Axis Z ", 802),
        ("Joystick L-Axis X ", 1026),
        ("Joystick L-Axis Y ", 1042),
        ("Joystick L-Axis Z ", 1058),
    ];

    #[test]
    fn every_real_label_produces_its_real_code() {
        for (label, code) in REAL {
            let control =
                Control::from_label(label).unwrap_or_else(|| panic!("could not parse {label:?}"));
            assert_eq!(control.code(), *code, "wrong code for {label:?}");
        }
    }

    #[test]
    fn every_real_code_produces_its_real_label() {
        for (label, code) in REAL {
            let control =
                Control::from_code(*code).unwrap_or_else(|| panic!("could not read code {code}"));
            assert_eq!(&control.label(), label, "wrong label for code {code}");
        }
    }

    #[test]
    fn the_trailing_space_on_an_axis_label_is_preserved() {
        // MSFS writes `Joystick L-Axis X ` and the file has to round-trip
        // byte-identical, so this is not cosmetic.
        let control = Control::Axis {
            bank: Bank::L,
            axis: Axis::X,
        };
        assert_eq!(control.label(), "Joystick L-Axis X ");
        assert!(control.label().ends_with(' '));
        // A button label has no trailing space.
        assert_eq!(Control::Button(8).label(), "Joystick Button 8");
    }

    #[test]
    fn a_key_element_carries_both_halves() {
        let key = Control::Button(8).to_key();
        assert_eq!(key.information, "Joystick Button 8");
        assert_eq!(key.code, "7");
        assert_eq!(key.code_num(), Some(7));
    }

    #[test]
    fn buttons_are_one_based_in_the_label_and_zero_based_in_the_code() {
        // The single easiest thing to get wrong, and the whole reason this
        // module exists rather than a format! at the call site.
        assert_eq!(Control::from_button_index(0), Control::Button(1));
        assert_eq!(Control::from_button_index(0).code(), 0);
        assert_eq!(Control::from_button_index(7), Control::Button(8));
        assert_eq!(Control::from_button_index(7).code(), 7);
    }

    #[test]
    fn the_first_three_evdev_axes_are_the_l_bank() {
        // Confirmed by the pedals, which have only X/Y/Z and bind all three as
        // L-Axis.
        assert_eq!(Control::from_abs(0).unwrap().label(), "Joystick L-Axis X ");
        assert_eq!(Control::from_abs(1).unwrap().label(), "Joystick L-Axis Y ");
        assert_eq!(Control::from_abs(2).unwrap().label(), "Joystick L-Axis Z ");
        // And rX/rY/rZ are the R bank, confirmed by the Gladiator's camera pan.
        assert_eq!(Control::from_abs(3).unwrap().label(), "Joystick R-Axis X ");
        assert_eq!(Control::from_abs(4).unwrap().label(), "Joystick R-Axis Y ");
        assert_eq!(Control::from_abs(5).unwrap().label(), "Joystick R-Axis Z ");
    }

    #[test]
    fn an_axis_with_no_evidence_behind_it_refuses_rather_than_inventing_one() {
        // Sliders and hats. No real binding uses one, so there is nothing to
        // derive a code from, and a guess writes a binding that does nothing.
        for abs in [6u16, 7, 8, 16, 17, 40] {
            assert_eq!(
                Control::from_abs(abs),
                None,
                "invented a code for ABS {abs}"
            );
        }
    }

    #[test]
    fn nonsense_labels_do_not_parse() {
        for label in [
            "",
            "Joystick",
            "Joystick Button",
            "Joystick Button x",
            "Joystick M-Axis X",
            "Joystick L-Axis W",
            "Keyboard A",
        ] {
            assert_eq!(Control::from_label(label), None, "parsed {label:?}");
        }
    }

    #[test]
    fn a_label_round_trips_through_its_code_and_back() {
        for n in [1u32, 2, 8, 31, 75, 128] {
            let control = Control::Button(n);
            assert_eq!(Control::from_code(control.code()), Some(control));
        }
        for bank in [Bank::L, Bank::R] {
            for axis in [Axis::X, Axis::Y, Axis::Z] {
                let control = Control::Axis { bank, axis };
                assert_eq!(Control::from_code(control.code()), Some(control));
                assert_eq!(Control::from_label(&control.label()), Some(control));
            }
        }
    }

    #[test]
    fn a_code_between_the_banks_is_not_read_as_an_axis() {
        // 0x0302 is R-Axis X. 0x0303 is nothing, and must not round to it.
        assert_eq!(Control::from_code(0x0303), None);
        assert_eq!(Control::from_code(0x0332), None, "past Z in the R bank");
        assert_eq!(Control::from_code(0x0432), None, "past Z in the L bank");
    }
}
