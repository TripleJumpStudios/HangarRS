//! Translating a key press into the `<KEY>` elements MSFS writes for a keyboard.
//!
//! The keyboard counterpart of [`crate::joystick`], and held to the same rule:
//! **the label/code pairs are observed, never guessed.** A keyboard binding
//! that looks right in the file and carries the wrong code does nothing in the
//! sim, and nobody can tell from the controls menu.
//!
//! # What the real profiles show
//!
//! Two keyboard profiles (`GUID="{0}"`, 2026-11-02) carry 88 distinct
//! label/code pairs, every one listed in [`OBSERVED`]. Every code is the
//! Windows virtual-key code for the key its label names (`BackSpace` 8,
//! `Return` 13, `a` 65, `F1` 112, `-` 189), and the labels are X11 keysym
//! names (`Prior` for Page Up, `KP_Add`), which is what Wine's keyboard driver
//! hands the sim. That pattern is **noted, not used**: nothing here derives a
//! code from it. A key absent from the table is refused, not computed.
//!
//! Left and right modifiers carry distinct codes and the same label. The
//! action names settle which is which — `KEY_DEBUG_LSHIFT` is 160,
//! `KEY_DEBUG_RSHIFT` 161, `KEY_DEBUG_LCTRL` 162, `KEY_DEBUG_RCTRL` 163,
//! `KEY_DEBUG_LALT` 164, `KEY_DEBUG_RALT` 165.
//!
//! # Combinations
//!
//! A combination is written modifiers first, then the key: all 334 observed
//! chords end in a non-modifier. Three rules follow from what is *not* there:
//!
//! - **No chord uses a right-hand modifier.** Every one of the 334 uses 160,
//!   162 or 164. Pressing the right Shift with a key is refused, not quietly
//!   written as the left one.
//! - **The modifier order is taken per combination, from the files.**
//!   `Control + Shift` (six), `Control + Shift + Alt` (two) and `Alt + Shift`
//!   (two) are each seen in one order only. They do not agree on a single
//!   global order — Alt comes after Shift in one and before it in the other —
//!   so a combination never seen (`Control + Alt`) is refused rather than
//!   given an invented one. See [`CHORD_ORDERS`].
//! - **A modifier on its own is a binding** (27 observed, sides included), but
//!   two modifiers with no key is not one any profile has.

use crate::model::{Binding, Key};

/// Every keyboard label/code pair in the real profiles, by code.
///
/// The ground truth this module is checked against. `examples/keys` re-reads
/// the real profiles and fails if a pair there is missing from this list or
/// disagrees with it.
///
/// The numpad digits really are written with an **empty** label —
/// `<KEY Information="">96</KEY>` — and `KEY_DEBUG_NUMPAD_0` is bound to 96,
/// which is what identifies them.
pub const OBSERVED: &[(u32, &str)] = &[
    (8, "BackSpace"),
    (9, "Tab"),
    (13, "Return"),
    (19, "Pause"),
    (27, "Escape"),
    (32, "space"),
    (33, "Prior"),
    (34, "Next"),
    (35, "End"),
    (36, "Home"),
    (37, "Left"),
    (38, "Up"),
    (39, "Right"),
    (40, "Down"),
    (46, "Delete"),
    (48, "0"),
    (49, "1"),
    (50, "2"),
    (51, "3"),
    (52, "4"),
    (53, "5"),
    (54, "6"),
    (55, "7"),
    (56, "8"),
    (57, "9"),
    (65, "a"),
    (66, "b"),
    (67, "c"),
    (68, "d"),
    (69, "e"),
    (70, "f"),
    (71, "g"),
    (72, "h"),
    (73, "i"),
    (74, "j"),
    (75, "k"),
    (76, "l"),
    (77, "m"),
    (78, "n"),
    (79, "o"),
    (80, "p"),
    (81, "q"),
    (82, "r"),
    (83, "s"),
    (84, "t"),
    (85, "u"),
    (86, "v"),
    (87, "w"),
    (88, "x"),
    (89, "y"),
    (90, "z"),
    (96, ""),
    (97, ""),
    (98, ""),
    (99, ""),
    (100, ""),
    (101, ""),
    (102, ""),
    (103, ""),
    (104, ""),
    (105, ""),
    (107, "KP_Add"),
    (109, "KP_Subtract"),
    (110, "KP_Delete"),
    (112, "F1"),
    (113, "F2"),
    (114, "F3"),
    (115, "F4"),
    (116, "F5"),
    (117, "F6"),
    (118, "F7"),
    (119, "F8"),
    (120, "F9"),
    (121, "F10"),
    (122, "F11"),
    (160, "Shift"),
    (161, "Shift"),
    (162, "Control"),
    (163, "Control"),
    (164, "Alt"),
    (165, "Alt"),
    (187, "="),
    (188, ","),
    (189, "-"),
    (219, "["),
    (220, "\\"),
    (221, "]"),
    (222, "'"),
];

/// The label MSFS writes for an observed code, or `None` when no real profile
/// has that code.
pub fn label(code: u32) -> Option<&'static str> {
    OBSERVED
        .binary_search_by_key(&code, |(c, _)| *c)
        .ok()
        .map(|i| OBSERVED[i].1)
}

/// The `<KEY>` element for an observed code.
pub fn key(code: u32) -> Option<Key> {
    label(code).map(|label| Key::new(label, code.to_string()))
}

/// Control, Shift or Alt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Modifier {
    Control,
    Shift,
    Alt,
}

/// Which of a modifier's two keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    pub fn as_str(self) -> &'static str {
        match self {
            Side::Left => "left",
            Side::Right => "right",
        }
    }
}

impl Modifier {
    /// The code for this modifier on this side. Both sides are observed, and
    /// the `KEY_DEBUG_L*`/`R*` actions say which is which.
    pub fn code(self, side: Side) -> u32 {
        match (self, side) {
            (Modifier::Shift, Side::Left) => 160,
            (Modifier::Shift, Side::Right) => 161,
            (Modifier::Control, Side::Left) => 162,
            (Modifier::Control, Side::Right) => 163,
            (Modifier::Alt, Side::Left) => 164,
            (Modifier::Alt, Side::Right) => 165,
        }
    }

    /// The label, which is the same for both sides.
    pub fn label(self) -> &'static str {
        match self {
            Modifier::Control => "Control",
            Modifier::Shift => "Shift",
            Modifier::Alt => "Alt",
        }
    }
}

/// Every modifier set observed in front of a key, in the order it was written.
///
/// Counts are slots across both keyboard profiles. A set not listed here has
/// no example, so its order would be a guess and the combination is refused.
pub const CHORD_ORDERS: &[&[Modifier]] = &[
    // 603 single keys.
    &[],
    // 53, 222 and 49.
    &[Modifier::Control],
    &[Modifier::Shift],
    &[Modifier::Alt],
    // 6: KEY_DEVMODE_SAVE_AS_DOCUMENT and three more DEVMODE actions.
    &[Modifier::Control, Modifier::Shift],
    // 2: KEY_DEVMODE_HIDE_ALL_EDITION.
    &[Modifier::Alt, Modifier::Shift],
    // 2: KEY_DEBUG_TELEMETRY.
    &[Modifier::Control, Modifier::Shift, Modifier::Alt],
];

/// One key, as capture reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Modifier(Modifier, Side),
    /// Any other key, by the code MSFS writes for it.
    Key(u32),
}

/// What was pressed, before it is checked against the evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stroke {
    /// A modifier pressed and released with nothing else.
    Lone(Modifier, Side),
    /// The modifiers held, in the order they went down, then a key.
    Chord {
        held: Vec<(Modifier, Side)>,
        key: u32,
    },
}

/// Why a press cannot become a binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unbindable {
    /// No real profile has a code for this key.
    NoEvidence { name: String },
    /// A right-hand modifier held with a key.
    RightModifierInChord(Modifier),
    /// Modifiers held with a key in a set no profile has.
    UnobservedCombination(Vec<Modifier>),
    /// Several modifiers released with no key.
    ModifiersOnly,
}

impl std::fmt::Display for Unbindable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unbindable::NoEvidence { name } => write!(
                f,
                "{name} has no example in any keyboard profile, so its code would be a guess."
            ),
            Unbindable::RightModifierInChord(m) => write!(
                f,
                "Use the left {} in a combination; no profile pairs the right one with a key.",
                m.label()
            ),
            Unbindable::UnobservedCombination(mods) => write!(
                f,
                "{} with a key has no example in any profile, so its order would be a guess.",
                mods.iter()
                    .map(|m| m.label())
                    .collect::<Vec<_>>()
                    .join(" + ")
            ),
            Unbindable::ModifiersOnly => {
                write!(f, "Hold modifiers with a key, or press one on its own.")
            }
        }
    }
}

/// A key or combination, checked and ready to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caught {
    pub stroke: Stroke,
    pub binding: Binding,
}

impl Caught {
    /// `Control + Shift + t (codes 162 + 160 + 84)` — what would be written.
    ///
    /// A lone modifier names its side, because the label does not.
    pub fn describe(&self) -> String {
        let codes: Vec<&str> = self.binding.keys.iter().map(|k| k.code.as_str()).collect();
        let label = match &self.stroke {
            Stroke::Lone(m, side) => format!("{} {}", side.as_str(), m.label()),
            Stroke::Chord { .. } => self.binding.label(),
        };
        if codes.len() == 1 {
            format!("{label} (code {})", codes[0])
        } else {
            format!("{label} (codes {})", codes.join(" + "))
        }
    }

    /// The non-modifier key's code, if there is one.
    pub fn key_code(&self) -> Option<u32> {
        match &self.stroke {
            Stroke::Chord { key, .. } => Some(*key),
            Stroke::Lone(..) => None,
        }
    }
}

/// Turn a stroke into the binding MSFS would have written, or say why not.
pub fn binding(stroke: &Stroke) -> Result<Caught, Unbindable> {
    let keys = match stroke {
        Stroke::Lone(m, side) => vec![modifier_key(*m, *side)],
        Stroke::Chord { held, key: code } => {
            let key = key(*code).ok_or_else(|| Unbindable::NoEvidence {
                name: format!("code {code}"),
            })?;
            if (160..=165).contains(code) {
                // A modifier is never the last key of an observed chord.
                return Err(Unbindable::ModifiersOnly);
            }
            if let Some((m, _)) = held.iter().find(|(_, side)| *side == Side::Right) {
                return Err(Unbindable::RightModifierInChord(*m));
            }
            let mut set: Vec<Modifier> = held.iter().map(|(m, _)| *m).collect();
            set.sort();
            set.dedup();
            let order = CHORD_ORDERS
                .iter()
                .find(|order| {
                    let mut sorted = order.to_vec();
                    sorted.sort();
                    sorted == set
                })
                .ok_or_else(|| Unbindable::UnobservedCombination(set.clone()))?;
            let mut keys: Vec<Key> = order.iter().map(|m| modifier_key(*m, Side::Left)).collect();
            keys.push(key);
            keys
        }
    };
    Ok(Caught {
        stroke: stroke.clone(),
        binding: Binding { keys },
    })
}

fn modifier_key(m: Modifier, side: Side) -> Key {
    Key::new(m.label(), m.code(side).to_string())
}

/// Key presses in, one binding out.
///
/// Fed the presses and releases a window saw, in order. A key pressed while
/// modifiers are held is a chord; a modifier pressed and released alone is a
/// lone binding. A release whose press was never seen — a key already down
/// when capture began — is ignored.
#[derive(Debug, Default)]
pub struct KeyCapture {
    held: Vec<(Modifier, Side)>,
    /// More than one modifier has been down at once since all were last up.
    combined: bool,
    /// A key went down while modifiers were held, so letting them go is not
    /// a lone press — even when that chord was refused.
    spent: bool,
}

impl KeyCapture {
    pub fn new() -> Self {
        Self::default()
    }

    /// A key went down. Auto-repeats should not be passed in.
    pub fn press(&mut self, input: Input) -> Option<Result<Caught, Unbindable>> {
        match input {
            Input::Modifier(m, side) => {
                if !self.held.contains(&(m, side)) {
                    self.held.push((m, side));
                }
                if self.held.len() > 1 {
                    self.combined = true;
                }
                None
            }
            Input::Key(key) => {
                if !self.held.is_empty() {
                    self.spent = true;
                }
                Some(binding(&Stroke::Chord {
                    held: self.held.clone(),
                    key,
                }))
            }
        }
    }

    /// A key came up.
    pub fn release(&mut self, input: Input) -> Option<Result<Caught, Unbindable>> {
        let Input::Modifier(m, side) = input else {
            return None;
        };
        let was_held = self.held.len();
        self.held.retain(|h| *h != (m, side));
        if self.held.len() == was_held {
            return None;
        }
        if !self.held.is_empty() {
            return None;
        }
        let combined = std::mem::take(&mut self.combined);
        if std::mem::take(&mut self.spent) {
            return None;
        }
        Some(if combined {
            Err(Unbindable::ModifiersOnly)
        } else {
            binding(&Stroke::Lone(m, side))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_sorted_by_code_with_no_repeats() {
        // `label` binary-searches it.
        assert!(OBSERVED.windows(2).all(|w| w[0].0 < w[1].0));
        assert_eq!(OBSERVED.len(), 88);
    }

    #[test]
    fn every_observed_pair_comes_back_out() {
        for (code, label) in OBSERVED {
            assert_eq!(super::label(*code), Some(*label));
            let key = key(*code).unwrap();
            assert_eq!(key.code, code.to_string());
        }
    }

    #[test]
    fn a_code_no_profile_has_is_not_invented() {
        // Insert (45), F12 (123), `;` (186), `/` (191), `` ` `` (192): all real
        // Windows keys, none in any profile.
        for code in [45u32, 123, 186, 190, 191, 192, 0, 1000] {
            assert_eq!(label(code), None, "invented a label for {code}");
        }
    }

    #[test]
    fn the_modifier_codes_match_their_observed_entries() {
        for m in [Modifier::Control, Modifier::Shift, Modifier::Alt] {
            for side in [Side::Left, Side::Right] {
                assert_eq!(label(m.code(side)), Some(m.label()), "{m:?} {side:?}");
            }
        }
    }

    #[test]
    fn a_single_key_is_its_observed_pair() {
        let caught = binding(&Stroke::Chord {
            held: vec![],
            key: 84,
        })
        .unwrap();
        assert_eq!(caught.binding, Binding::single(Key::new("t", "84")));
        assert_eq!(caught.describe(), "t (code 84)");
    }

    #[test]
    fn a_chord_is_written_in_its_observed_order_whatever_order_it_was_pressed() {
        // Pressed Alt, Shift, Control — written Control, Shift, Alt, the way
        // KEY_DEBUG_TELEMETRY has it.
        let caught = binding(&Stroke::Chord {
            held: vec![
                (Modifier::Alt, Side::Left),
                (Modifier::Shift, Side::Left),
                (Modifier::Control, Side::Left),
            ],
            key: 84,
        })
        .unwrap();
        assert_eq!(caught.binding.label(), "Control + Shift + Alt + t");
        assert_eq!(
            caught.describe(),
            "Control + Shift + Alt + t (codes 162 + 160 + 164 + 84)"
        );

        // And Alt + Shift the way KEY_DEVMODE_HIDE_ALL_EDITION has it.
        let caught = binding(&Stroke::Chord {
            held: vec![(Modifier::Shift, Side::Left), (Modifier::Alt, Side::Left)],
            key: 72,
        })
        .unwrap();
        assert_eq!(caught.binding.label(), "Alt + Shift + h");
    }

    #[test]
    fn a_combination_no_profile_has_is_refused() {
        let err = binding(&Stroke::Chord {
            held: vec![(Modifier::Control, Side::Left), (Modifier::Alt, Side::Left)],
            key: 84,
        })
        .unwrap_err();
        assert_eq!(
            err,
            Unbindable::UnobservedCombination(vec![Modifier::Control, Modifier::Alt])
        );
        assert!(err.to_string().contains("Control + Alt"), "{err}");
    }

    #[test]
    fn a_right_hand_modifier_in_a_chord_is_refused_not_swapped() {
        let err = binding(&Stroke::Chord {
            held: vec![(Modifier::Shift, Side::Right)],
            key: 84,
        })
        .unwrap_err();
        assert_eq!(err, Unbindable::RightModifierInChord(Modifier::Shift));
    }

    #[test]
    fn a_lone_modifier_keeps_its_side() {
        let right = binding(&Stroke::Lone(Modifier::Alt, Side::Right)).unwrap();
        assert_eq!(right.binding, Binding::single(Key::new("Alt", "165")));
        assert_eq!(right.describe(), "right Alt (code 165)");
        let left = binding(&Stroke::Lone(Modifier::Control, Side::Left)).unwrap();
        assert_eq!(left.binding, Binding::single(Key::new("Control", "162")));
    }

    #[test]
    fn capture_reads_a_chord_from_presses() {
        let mut capture = KeyCapture::new();
        assert_eq!(
            capture.press(Input::Modifier(Modifier::Control, Side::Left)),
            None
        );
        assert_eq!(
            capture.press(Input::Modifier(Modifier::Shift, Side::Left)),
            None
        );
        let caught = capture.press(Input::Key(83)).unwrap().unwrap();
        assert_eq!(caught.binding.label(), "Control + Shift + s");
        assert_eq!(caught.key_code(), Some(83));
    }

    #[test]
    fn capture_reads_a_lone_modifier_on_release() {
        let mut capture = KeyCapture::new();
        capture.press(Input::Modifier(Modifier::Shift, Side::Right));
        let caught = capture
            .release(Input::Modifier(Modifier::Shift, Side::Right))
            .unwrap()
            .unwrap();
        assert_eq!(caught.binding, Binding::single(Key::new("Shift", "161")));
    }

    #[test]
    fn two_modifiers_and_no_key_is_refused_once_both_are_up() {
        let mut capture = KeyCapture::new();
        capture.press(Input::Modifier(Modifier::Shift, Side::Left));
        capture.press(Input::Modifier(Modifier::Control, Side::Left));
        assert_eq!(
            capture.release(Input::Modifier(Modifier::Control, Side::Left)),
            None
        );
        assert_eq!(
            capture.release(Input::Modifier(Modifier::Shift, Side::Left)),
            Some(Err(Unbindable::ModifiersOnly))
        );
        // And the next lone press is clean again.
        capture.press(Input::Modifier(Modifier::Alt, Side::Left));
        assert!(matches!(
            capture.release(Input::Modifier(Modifier::Alt, Side::Left)),
            Some(Ok(_))
        ));
    }

    #[test]
    fn a_modifier_used_with_a_key_is_not_a_lone_press_when_it_comes_up() {
        let mut capture = KeyCapture::new();
        capture.press(Input::Modifier(Modifier::Shift, Side::Right));
        assert_eq!(
            capture.press(Input::Key(84)),
            Some(Err(Unbindable::RightModifierInChord(Modifier::Shift)))
        );
        assert_eq!(capture.release(Input::Key(84)), None);
        assert_eq!(
            capture.release(Input::Modifier(Modifier::Shift, Side::Right)),
            None,
            "a refused chord turned into a lone right Shift"
        );
        // The next lone press still counts.
        capture.press(Input::Modifier(Modifier::Shift, Side::Left));
        assert!(matches!(
            capture.release(Input::Modifier(Modifier::Shift, Side::Left)),
            Some(Ok(_))
        ));
    }

    #[test]
    fn a_release_whose_press_was_not_seen_is_ignored() {
        // Shift already held when Bind was clicked.
        let mut capture = KeyCapture::new();
        assert_eq!(
            capture.release(Input::Modifier(Modifier::Shift, Side::Left)),
            None
        );
        assert_eq!(capture.release(Input::Key(84)), None);
    }

    #[test]
    fn an_unobserved_key_code_is_refused() {
        let mut capture = KeyCapture::new();
        assert_eq!(
            capture.press(Input::Key(45)),
            Some(Err(Unbindable::NoEvidence {
                name: "code 45".into()
            }))
        );
    }
}
