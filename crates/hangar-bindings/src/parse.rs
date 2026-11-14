//! Reading an input profile.
//!
//! Two things about these files break the obvious approaches:
//!
//! 1. **There is no root element.** `<Version>`, `<FriendlyName>` and
//!    `<Device>` sit side by side at the top level, so the document is a
//!    fragment. `serde`-shaped deserialisers reject it outright.
//! 2. **A self-closing `<Action/>` means unbound**, and carries exactly the
//!    same attributes as a bound one. The distinction is the element form, not
//!    a value, so it has to survive parsing.
//!
//! Unknown attributes are a hard error rather than a shrug. This crate writes
//! back to the only copy of someone's bindings, and an attribute we silently
//! dropped is a setting silently lost.

use crate::model::*;
use crate::{BindingsError, Result};
use quick_xml::events::attributes::Attributes;
use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};
use std::path::{Path, PathBuf};

/// Attribute-value normalisation follows XML 1.0 rules. Every profile declares
/// `version="1.0"`, and nothing in them relies on 1.1's extra line endings.
const XML_VERSION: XmlVersion = XmlVersion::Explicit1_0;

/// Read and parse a profile from disk.
pub fn read(path: impl AsRef<Path>) -> Result<Profile> {
    let path = path.as_ref();
    let bytes = std::fs::read(path).map_err(|source| BindingsError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let text = String::from_utf8(bytes).map_err(|_| {
        BindingsError::malformed(
            path,
            "not valid UTF-8; every observed profile is UTF-8 and refusing is \
             safer than transcoding a file we have to write back byte-identical",
        )
    })?;
    parse(&text).map_err(|e| e.at(path))
}

/// Parse a profile from text already in memory.
pub fn parse(text: &str) -> Result<Profile> {
    let mut reader = Reader::from_str(text);
    reader.config_mut().check_end_names = true;

    let mut xml_version = String::new();
    let mut encoding = String::new();
    let mut version_num = None;
    let mut friendly_name = None;
    let mut device = None;

    loop {
        match reader.read_event().map_err(xml_err)? {
            Event::Decl(d) => {
                xml_version = d.version().map_err(xml_err)?.into_owned();
                encoding = match d.encoding() {
                    Some(e) => e.map_err(xml_err)?.into_owned(),
                    None => "UTF-8".to_string(),
                };
            }
            Event::Empty(e) | Event::Start(e) if e.name().as_ref() == "Version" => {
                let mut num = None;
                for_each_attr(e.attributes(), |key, value| {
                    match key {
                        "Num" => num = Some(value),
                        other => return Err(unknown_attr("Version", other)),
                    }
                    Ok(())
                })?;
                version_num = Some(require(num, "Version", "Num")?);
            }
            Event::Start(e) if e.name().as_ref() == "FriendlyName" => {
                let mut platform_availability = None;
                let mut locked = None;
                for_each_attr(e.attributes(), |key, value| {
                    match key {
                        "PlatformAvailability" => platform_availability = Some(value),
                        "Locked" => locked = Some(value),
                        other => return Err(unknown_attr("FriendlyName", other)),
                    }
                    Ok(())
                })?;
                let name = read_text(&mut reader, "FriendlyName")?;
                friendly_name = Some(FriendlyName {
                    platform_availability: require(
                        platform_availability,
                        "FriendlyName",
                        "PlatformAvailability",
                    )?,
                    locked: require(locked, "FriendlyName", "Locked")?,
                    name,
                });
            }
            Event::Start(e) if e.name().as_ref() == "Device" => {
                device = Some(parse_device(&mut reader, e.attributes())?);
            }
            Event::Eof => break,
            Event::Text(t) => reject_stray_text(t.xml10_content().as_ref())?,
            Event::Comment(_) | Event::DocType(_) | Event::PI(_) => {}
            Event::End(_) => {}
            other => return Err(unexpected(&other, "at the top level")),
        }
    }

    Ok(Profile {
        xml_version,
        encoding,
        version_num: require(version_num, "profile", "<Version Num>")?,
        friendly_name: friendly_name
            .ok_or_else(|| BindingsError::parse("no <FriendlyName> element"))?,
        device: device.ok_or_else(|| BindingsError::parse("no <Device> element"))?,
    })
}

fn parse_device(reader: &mut Reader<&[u8]>, attrs: Attributes) -> Result<Device> {
    let (mut name, mut guid, mut product_id, mut composite_id, mut hw_ver) =
        (None, None, None, None, None);
    for_each_attr(attrs, |key, value| {
        match key {
            "DeviceName" => name = Some(value),
            "GUID" => guid = Some(value),
            "ProductID" => product_id = Some(value),
            "CompositeID" => composite_id = Some(value),
            "HWVer" => hw_ver = Some(value),
            other => return Err(unknown_attr("Device", other)),
        }
        Ok(())
    })?;

    let mut aircraft_info = None;
    let mut axes = None;
    let mut contexts = Vec::new();

    loop {
        match reader.read_event().map_err(xml_err)? {
            Event::Empty(e) | Event::Start(e) if e.name().as_ref() == "AircraftInfo" => {
                let mut category_name = None;
                for_each_attr(e.attributes(), |key, value| {
                    match key {
                        "CategoryName" => category_name = Some(value),
                        other => return Err(unknown_attr("AircraftInfo", other)),
                    }
                    Ok(())
                })?;
                aircraft_info = Some(AircraftInfo {
                    category_name: require(category_name, "AircraftInfo", "CategoryName")?,
                });
            }
            Event::Start(e) if e.name().as_ref() == "Axes" => axes = Some(parse_axes(reader)?),
            Event::Empty(e) if e.name().as_ref() == "Axes" => axes = Some(Vec::new()),
            Event::Start(e) if e.name().as_ref() == "Context" => {
                contexts.push(parse_context(reader, e.attributes())?);
            }
            Event::End(e) if e.name().as_ref() == "Device" => break,
            Event::Text(t) => reject_stray_text(t.xml10_content().as_ref())?,
            Event::Comment(_) | Event::PI(_) => {}
            Event::Eof => return Err(BindingsError::parse("file ends inside <Device>")),
            other => return Err(unexpected(&other, "inside <Device>")),
        }
    }

    Ok(Device {
        name: require(name, "Device", "DeviceName")?,
        guid: require(guid, "Device", "GUID")?,
        product_id: require(product_id, "Device", "ProductID")?,
        composite_id: require(composite_id, "Device", "CompositeID")?,
        hw_ver: require(hw_ver, "Device", "HWVer")?,
        aircraft_info,
        axes,
        contexts,
    })
}

fn parse_axes(reader: &mut Reader<&[u8]>) -> Result<Vec<Axis>> {
    let mut axes = Vec::new();
    loop {
        match reader.read_event().map_err(xml_err)? {
            Event::Empty(e) | Event::Start(e) if e.name().as_ref() == "Axis" => {
                axes.push(parse_axis(e.attributes())?);
            }
            Event::End(e) if e.name().as_ref() == "Axes" => break,
            Event::End(e) if e.name().as_ref() == "Axis" => {}
            Event::Text(t) => reject_stray_text(t.xml10_content().as_ref())?,
            Event::Comment(_) | Event::PI(_) => {}
            Event::Eof => return Err(BindingsError::parse("file ends inside <Axes>")),
            other => return Err(unexpected(&other, "inside <Axes>")),
        }
    }
    Ok(axes)
}

fn parse_axis(attrs: Attributes) -> Result<Axis> {
    let (
        mut name,
        mut sensitivity,
        mut sensitivity_minus,
        mut neutral,
        mut dead_zone,
        mut out_dead_zone,
        mut response_rate,
    ) = (None, None, None, None, None, None, None);
    for_each_attr(attrs, |key, value| {
        match key {
            "AxisName" => name = Some(value),
            // Asobo's typo. Preserved deliberately; see model::Axis.
            "AxisSensitivy" => sensitivity = Some(value),
            "AxisSensitivyMinus" => sensitivity_minus = Some(value),
            "AxisNeutral" => neutral = Some(value),
            "AxisDeadZone" => dead_zone = Some(value),
            "AxisOutDeadZone" => out_dead_zone = Some(value),
            "AxisResponseRate" => response_rate = Some(value),
            other => return Err(unknown_attr("Axis", other)),
        }
        Ok(())
    })?;
    Ok(Axis {
        name: require(name, "Axis", "AxisName")?,
        sensitivity: require(sensitivity, "Axis", "AxisSensitivy")?,
        sensitivity_minus: require(sensitivity_minus, "Axis", "AxisSensitivyMinus")?,
        neutral: require(neutral, "Axis", "AxisNeutral")?,
        dead_zone: require(dead_zone, "Axis", "AxisDeadZone")?,
        out_dead_zone: require(out_dead_zone, "Axis", "AxisOutDeadZone")?,
        response_rate: require(response_rate, "Axis", "AxisResponseRate")?,
    })
}

fn parse_context(reader: &mut Reader<&[u8]>, attrs: Attributes) -> Result<Context> {
    let mut name = None;
    for_each_attr(attrs, |key, value| {
        match key {
            "ContextName" => name = Some(value),
            other => return Err(unknown_attr("Context", other)),
        }
        Ok(())
    })?;

    let mut actions = Vec::new();
    loop {
        match reader.read_event().map_err(xml_err)? {
            // The whole unbound/bound distinction: `Empty` is `<Action …/>`.
            Event::Empty(e) if e.name().as_ref() == "Action" => {
                actions.push(parse_action_attrs(e.attributes())?);
            }
            Event::Start(e) if e.name().as_ref() == "Action" => {
                let mut action = parse_action_attrs(e.attributes())?;
                parse_action_body(reader, &mut action)?;
                actions.push(action);
            }
            Event::End(e) if e.name().as_ref() == "Context" => break,
            Event::Text(t) => reject_stray_text(t.xml10_content().as_ref())?,
            Event::Comment(_) | Event::PI(_) => {}
            Event::Eof => return Err(BindingsError::parse("file ends inside <Context>")),
            other => return Err(unexpected(&other, "inside <Context>")),
        }
    }

    Ok(Context {
        name: require(name, "Context", "ContextName")?,
        actions,
    })
}

fn parse_action_attrs(attrs: Attributes) -> Result<Action> {
    let (mut name, mut value_event, mut delay, mut flag) = (None, None, None, None);
    for_each_attr(attrs, |key, value| {
        match key {
            "ActionName" => name = Some(value),
            "ValueEvent" => value_event = Some(value),
            "Delay" => delay = Some(value),
            "Flag" => flag = Some(value),
            other => return Err(unknown_attr("Action", other)),
        }
        Ok(())
    })?;
    Ok(Action {
        name: require(name, "Action", "ActionName")?,
        value_event: require(value_event, "Action", "ValueEvent")?,
        delay: require(delay, "Action", "Delay")?,
        flag: require(flag, "Action", "Flag")?,
        primary: None,
        secondary: None,
    })
}

fn parse_action_body(reader: &mut Reader<&[u8]>, action: &mut Action) -> Result<()> {
    loop {
        match reader.read_event().map_err(xml_err)? {
            Event::Start(e) if e.name().as_ref() == "Primary" => {
                action.primary = Some(parse_binding(reader, "Primary")?);
            }
            Event::Start(e) if e.name().as_ref() == "Secondary" => {
                action.secondary = Some(parse_binding(reader, "Secondary")?);
            }
            Event::Empty(e) if e.name().as_ref() == "Primary" => {
                action.primary = Some(Binding { keys: Vec::new() });
            }
            Event::Empty(e) if e.name().as_ref() == "Secondary" => {
                action.secondary = Some(Binding { keys: Vec::new() });
            }
            Event::End(e) if e.name().as_ref() == "Action" => break,
            Event::Text(t) => reject_stray_text(t.xml10_content().as_ref())?,
            Event::Comment(_) | Event::PI(_) => {}
            Event::Eof => {
                return Err(BindingsError::parse(format!(
                    "file ends inside <Action ActionName=\"{}\">",
                    action.name
                )))
            }
            other => {
                return Err(unexpected(
                    &other,
                    &format!("inside <Action ActionName=\"{}\">", action.name),
                ))
            }
        }
    }
    Ok(())
}

fn parse_binding(reader: &mut Reader<&[u8]>, slot: &'static str) -> Result<Binding> {
    let mut keys = Vec::new();
    loop {
        match reader.read_event().map_err(xml_err)? {
            Event::Start(e) if e.name().as_ref() == "KEY" => {
                let mut information = None;
                for_each_attr(e.attributes(), |key, value| {
                    match key {
                        "Information" => information = Some(value),
                        other => return Err(unknown_attr("KEY", other)),
                    }
                    Ok(())
                })?;
                let code = read_text(reader, "KEY")?;
                keys.push(Key {
                    information: require(information, "KEY", "Information")?,
                    code,
                });
            }
            Event::End(e) if e.name().as_ref() == slot => break,
            Event::Text(t) => reject_stray_text(t.xml10_content().as_ref())?,
            Event::Comment(_) | Event::PI(_) => {}
            Event::Eof => return Err(BindingsError::parse(format!("file ends inside <{slot}>"))),
            other => return Err(unexpected(&other, &format!("inside <{slot}>"))),
        }
    }
    Ok(Binding { keys })
}

/// Read the text content of an element and consume its end tag.
///
/// quick-xml 0.42 reports entity references as their own `GeneralRef` events
/// rather than folding them into the text, so `&amp;` in a profile name
/// arrives here as three events. No real profile contains one, but a person
/// naming a profile "Yoke & Pedals" would produce one, so it is resolved
/// rather than rejected.
fn read_text(reader: &mut Reader<&[u8]>, element: &str) -> Result<String> {
    let mut text = String::new();
    loop {
        match reader.read_event().map_err(xml_err)? {
            Event::Text(t) => text.push_str(t.xml10_content().as_ref()),
            Event::CData(c) => text.push_str(c.xml10_content().as_ref()),
            Event::GeneralRef(r) => {
                let name = r.into_inner();
                let entity = format!("&{name};");
                let resolved = quick_xml::escape::unescape(&entity).map_err(|e| {
                    BindingsError::parse(format!(
                        "<{element}> contains an entity this parser cannot resolve, \
                         {entity}: {e}"
                    ))
                })?;
                text.push_str(resolved.as_ref());
            }
            Event::End(_) => break,
            Event::Comment(_) | Event::PI(_) => {}
            Event::Eof => {
                return Err(BindingsError::parse(format!(
                    "file ends inside <{element}>"
                )))
            }
            other => return Err(unexpected(&other, &format!("inside <{element}>"))),
        }
    }
    Ok(text)
}

/// Text between elements is indentation and nothing else. Anything with
/// non-whitespace in it is a shape this parser does not model, and quietly
/// dropping it would lose it on write.
fn reject_stray_text(text: &str) -> Result<()> {
    if text.trim().is_empty() {
        Ok(())
    } else {
        Err(BindingsError::parse(format!(
            "unexpected text between elements: {:?}",
            text.trim()
        )))
    }
}

fn for_each_attr(
    attrs: Attributes,
    mut take: impl FnMut(&str, String) -> Result<()>,
) -> Result<()> {
    for attr in attrs {
        let attr = attr.map_err(|e| BindingsError::parse(format!("bad attribute: {e}")))?;
        let key = attr.key.as_ref().to_string();
        let value = attr
            .normalized_value(XML_VERSION)
            .map_err(|e| BindingsError::parse(format!("bad value for {key}: {e}")))?
            .into_owned();
        take(&key, value)?;
    }
    Ok(())
}

fn require(value: Option<String>, element: &str, attr: &str) -> Result<String> {
    value.ok_or_else(|| BindingsError::parse(format!("<{element}> has no {attr}")))
}

fn unknown_attr(element: &str, attr: &str) -> BindingsError {
    BindingsError::parse(format!(
        "<{element}> carries an attribute this parser does not know: {attr}. \
         Refusing rather than dropping it, because the file gets written back."
    ))
}

fn unexpected(event: &Event, where_: &str) -> BindingsError {
    BindingsError::parse(format!("unexpected {} {where_}", describe(event)))
}

fn xml_err(e: impl std::fmt::Display) -> BindingsError {
    BindingsError::parse(format!("XML error: {e}"))
}

fn describe(event: &Event) -> String {
    match event {
        Event::Start(e) => format!("<{}>", e.name().as_ref()),
        Event::End(e) => format!("</{}>", e.name().as_ref()),
        Event::Empty(e) => format!("<{}/>", e.name().as_ref()),
        Event::Text(_) => "text".to_string(),
        Event::CData(_) => "CDATA".to_string(),
        Event::Decl(_) => "XML declaration".to_string(),
        Event::PI(_) => "processing instruction".to_string(),
        Event::DocType(_) => "doctype".to_string(),
        Event::Comment(_) => "comment".to_string(),
        Event::GeneralRef(_) => "entity reference".to_string(),
        Event::Eof => "end of file".to_string(),
    }
}

impl BindingsError {
    /// Attach the file a parse error came from, once it is known.
    fn at(self, path: &Path) -> Self {
        match self {
            BindingsError::Parse { reason } => BindingsError::Malformed {
                path: PathBuf::from(path),
                reason,
            },
            other => other,
        }
    }
}
