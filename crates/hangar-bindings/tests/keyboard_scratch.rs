//! Keyboard capture to a written file, end to end, on a scratch copy.
//!
//! Presses go into `KeyCapture`, the binding it proposes goes into a synthetic
//! keyboard profile, and the profile goes through `save::save` — the same
//! snapshot-first gate every real write uses. Everything happens inside a
//! temporary directory this test makes and removes; the path carries no
//! `userdata` component, so the Steam-closed gate correctly does not apply.

use hangar_bindings::keyboard::{Input, KeyCapture, Modifier, Side};
use hangar_bindings::save::{self, Approval};
use hangar_bindings::{parse, roundtrip_ok, to_xml, Slot};

/// A keyboard profile in the real files' shape: `GUID="{0}"`, no `<Axes>`, one
/// bound action and one unbound. No personal ids.
const KEYBOARD: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
    "<Version Num=\"-1\"/>\n",
    "<FriendlyName PlatformAvailability=\"15\" Locked=\"false\">Scratch keyboard</FriendlyName>\n",
    "<Device DeviceName=\"Keyboard\" GUID=\"{0}\" ProductID=\"1\" CompositeID=\"0\" HWVer=\"1.0.0.0\">\n",
    "\t<Context ContextName=\"PLANE\">\n",
    "\t\t<Action ActionName=\"KEY_PARKING_BRAKES\" ValueEvent=\"0.000000\" Delay=\"0.000000\" Flag=\"2\">\n",
    "\t\t\t<Primary>\n",
    "\t\t\t\t<KEY Information=\"Control\">162</KEY>\n",
    "\t\t\t\t<KEY Information=\"space\">32</KEY>\n",
    "\t\t\t</Primary>\n",
    "\t\t</Action>\n",
    "\t\t<Action ActionName=\"KEY_TOGGLE_JETWAY\" ValueEvent=\"0.000000\" Delay=\"0.000000\" Flag=\"2\"/>\n",
    "\t</Context>\n",
    "</Device>\n",
    "\n",
);

/// What the file must say once Control + Shift + j is bound to the jetway.
const EXPECTED_ACTION: &str = concat!(
    "\t\t<Action ActionName=\"KEY_TOGGLE_JETWAY\" ValueEvent=\"0.000000\" Delay=\"0.000000\" Flag=\"2\">\n",
    "\t\t\t<Primary>\n",
    "\t\t\t\t<KEY Information=\"Control\">162</KEY>\n",
    "\t\t\t\t<KEY Information=\"Shift\">160</KEY>\n",
    "\t\t\t\t<KEY Information=\"j\">74</KEY>\n",
    "\t\t\t</Primary>\n",
    "\t\t</Action>\n",
);

#[test]
fn a_captured_key_combination_is_written_through_the_snapshot_first_gate() {
    let root = std::env::temp_dir().join(format!("hangar-keyboard-scratch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let remote = root.join("remote");
    let store = root.join("snapshots");
    std::fs::create_dir_all(&remote).unwrap();
    let path = remote.join("inputprofile_0000000001");
    std::fs::write(&path, KEYBOARD).unwrap();
    roundtrip_ok(&path).expect("the fixture round-trips before anything is written");
    assert_eq!(
        save::Destination::of(&path),
        save::Destination::Elsewhere,
        "a scratch path must not look like Steam userdata"
    );

    // Pressed Shift, then Control, then j: written in the observed order.
    let mut capture = KeyCapture::new();
    assert!(capture
        .press(Input::Modifier(Modifier::Shift, Side::Left))
        .is_none());
    assert!(capture
        .press(Input::Modifier(Modifier::Control, Side::Left))
        .is_none());
    let caught = capture
        .press(Input::Key(74))
        .expect("a key with modifiers held is a chord")
        .expect("Control + Shift is an observed combination");
    assert_eq!(
        caught.describe(),
        "Control + Shift + j (codes 162 + 160 + 74)"
    );

    let mut profile = parse(KEYBOARD).unwrap();
    let action = profile.device.contexts[0]
        .action_mut("KEY_TOGGLE_JETWAY")
        .unwrap();
    assert_eq!(
        action.bind(Slot::Primary, caught.binding.clone()),
        Slot::Primary
    );

    let saved = save::save(
        &profile,
        &path,
        &store,
        &Approval::given(format!(
            "test: bind PLANE/KEY_TOGGLE_JETWAY to {}",
            caught.describe()
        )),
    )
    .expect("no I/O failure")
    .expect("the gate lets a scratch write through");

    // The file is exactly the original with the one action filled in.
    let written = std::fs::read_to_string(&path).unwrap();
    let unbound = "\t\t<Action ActionName=\"KEY_TOGGLE_JETWAY\" ValueEvent=\"0.000000\" Delay=\"0.000000\" Flag=\"2\"/>\n";
    assert_eq!(written, KEYBOARD.replace(unbound, EXPECTED_ACTION));
    assert_eq!(written, to_xml(&profile));
    roundtrip_ok(&path).expect("the written file round-trips");

    // The snapshot was taken first and holds the original bytes.
    let before = std::fs::read_to_string(saved.snapshot_dir.join("inputprofile_0000000001"))
        .expect("the snapshot holds the profile");
    assert_eq!(before, KEYBOARD);

    std::fs::remove_dir_all(&root).unwrap();
}
