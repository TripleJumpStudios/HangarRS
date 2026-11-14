//! Just enough PE to answer "what is this `.exe`?" without running it.
//!
//! Three facts are wanted from a Windows binary before anything spawns it:
//! whether it is 32- or 64-bit, what its section table is called, and what its
//! version resource says about who made it. All three are readable from the
//! headers, and none of them needs Wine.
//!
//! # Why by hand
//!
//! The formats here are small, fixed and old — a DOS stub, a COFF header, a
//! section table, and a resource tree with three levels. Reading them directly
//! costs less than a dependency. The tree walk takes a resource type rather
//! than hunting for version strings, so the same walk can reach other
//! resources such as `RT_GROUP_ICON`.
//!
//! # Only the parts that are needed are read
//!
//! An installer can be two gigabytes. The headers are in the first few
//! kilobytes and the version resource is inside `.rsrc`, so this seeks to the
//! ranges it wants rather than reading the file in. Even `.rsrc` is not read
//! whole: the resource tree at its front is walked from a [`TREE_WINDOW`], and
//! then the one leaf that was asked for is read.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::InstallError;

/// How much of `.rsrc` is read to walk the resource tree.
///
/// The tree is at the front of the section and is small — three levels of
/// eight-byte entries. What follows it is the resource data, and that can be
/// most of the file: iTunes' installer carries its MSIs as resources, so its
/// `.rsrc` is 190 MB of a 191 MB binary.
pub const TREE_WINDOW: u64 = 8 * 1024 * 1024;

/// The largest single resource this will read. A version block is a couple of
/// kilobytes; the cap is for a corrupt length, not for size.
pub const MAX_RESOURCE: u64 = 16 * 1024 * 1024;

/// Resource type 16 — `RT_VERSION`, the `VS_VERSIONINFO` block.
pub const RT_VERSION: u32 = 16;

/// Resource type 14 — `RT_GROUP_ICON`, an executable's icon group. Not read by
/// the pipeline; available to other callers of [`Pe::resource`].
pub const RT_GROUP_ICON: u32 = 14;

/// 32-bit or 64-bit, from the optional header's magic.
///
/// Worth knowing before an install rather than after: InstallShield's engine
/// is 32-bit and spawns 64-bit helpers, which is how a `.NET` x64 app ends up
/// inheriting `ProgramFiles=C:\Program Files (x86)` and looking for its
/// runtime in a directory that does not exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bitness {
    Pe32,
    Pe32Plus,
}

impl Bitness {
    pub fn label(self) -> &'static str {
        match self {
            Bitness::Pe32 => "PE32 (32-bit)",
            Bitness::Pe32Plus => "PE32+ (64-bit)",
        }
    }
}

/// One entry in the section table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// The eight-byte name, NUL-stripped. May be empty — packers write
    /// unnamed sections, and that absence is itself a signal.
    pub name: String,
    pub virtual_address: u32,
    pub virtual_size: u32,
    pub raw_size: u32,
    pub raw_offset: u32,
}

impl Section {
    /// Does this section contain the given RVA?
    fn contains(&self, rva: u32) -> bool {
        let span = self.virtual_size.max(self.raw_size).max(1);
        rva >= self.virtual_address && rva - self.virtual_address < span
    }
}

/// The header facts, plus whatever the version resource carried.
#[derive(Debug, Clone)]
pub struct Pe {
    pub bitness: Bitness,
    pub machine: u16,
    pub subsystem: u16,
    /// The COFF timestamp. Reproducible builds write a hash here rather than a
    /// time, so it is reported and never interpreted.
    pub timestamp: u32,
    pub sections: Vec<Section>,
    pub version: VersionInfo,
}

impl Pe {
    /// Read the headers, then the version resource if there is one.
    pub fn read(path: &Path) -> Result<Pe, InstallError> {
        let mut file = File::open(path).map_err(|source| InstallError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let mut headers = Headers::read(&mut file, path)?;
        let version = match headers.resource(&mut file, path, RT_VERSION)? {
            Some(bytes) => version_info(&bytes),
            None => VersionInfo::default(),
        };
        Ok(Pe {
            bitness: headers.bitness,
            machine: headers.machine,
            subsystem: headers.subsystem,
            timestamp: headers.timestamp,
            sections: std::mem::take(&mut headers.sections),
            version,
        })
    }

    /// Section names, in table order.
    pub fn section_names(&self) -> Vec<&str> {
        self.sections.iter().map(|s| s.name.as_str()).collect()
    }

    /// Is there a section with this name?
    pub fn has_section(&self, name: &str) -> bool {
        self.sections.iter().any(|s| s.name == name)
    }

    /// Read one resource of the given type, if present.
    ///
    /// Reopens the file, because the common case wants only the version block
    /// and holding a handle open for the life of a [`Pe`] buys nothing.
    pub fn resource(&self, path: &Path, kind: u32) -> Result<Option<Vec<u8>>, InstallError> {
        let mut file = File::open(path).map_err(|source| InstallError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let mut headers = Headers::read(&mut file, path)?;
        headers.resource(&mut file, path, kind)
    }
}

/// Everything read from the fixed-position headers.
struct Headers {
    bitness: Bitness,
    machine: u16,
    subsystem: u16,
    timestamp: u32,
    sections: Vec<Section>,
    rsrc_rva: u32,
    rsrc_size: u32,
}

impl Headers {
    fn read(file: &mut File, path: &Path) -> Result<Headers, InstallError> {
        let read_at = |file: &mut File, at: u64, len: usize| -> Result<Vec<u8>, InstallError> {
            let mut buf = vec![0u8; len];
            file.seek(SeekFrom::Start(at))
                .and_then(|_| file.read_exact(&mut buf))
                .map_err(|source| InstallError::Read {
                    path: path.to_path_buf(),
                    source,
                })?;
            Ok(buf)
        };

        let dos = read_at(file, 0, 0x40)?;
        if dos[0..2] != *b"MZ" {
            return Err(InstallError::NotPe {
                path: path.to_path_buf(),
                why: "no MZ signature",
            });
        }
        let pe_at = u32::from_le_bytes([dos[0x3c], dos[0x3d], dos[0x3e], dos[0x3f]]) as u64;

        let coff = read_at(file, pe_at, 24)?;
        if coff[0..4] != *b"PE\0\0" {
            return Err(InstallError::NotPe {
                path: path.to_path_buf(),
                why: "no PE signature at e_lfanew",
            });
        }
        let machine = u16::from_le_bytes([coff[4], coff[5]]);
        let section_count = u16::from_le_bytes([coff[6], coff[7]]) as usize;
        let timestamp = u32::from_le_bytes([coff[8], coff[9], coff[10], coff[11]]);
        let optional_size = u16::from_le_bytes([coff[20], coff[21]]) as usize;

        if optional_size < 96 {
            return Err(InstallError::NotPe {
                path: path.to_path_buf(),
                why: "optional header too short to hold a data directory",
            });
        }
        let optional = read_at(file, pe_at + 24, optional_size)?;
        let magic = u16::from_le_bytes([optional[0], optional[1]]);
        let bitness = match magic {
            0x10b => Bitness::Pe32,
            0x20b => Bitness::Pe32Plus,
            _ => {
                return Err(InstallError::NotPe {
                    path: path.to_path_buf(),
                    why: "unknown optional header magic",
                })
            }
        };
        let subsystem = u16::from_le_bytes([optional[68], optional[69]]);
        // The data directory sits after the windows-specific fields, whose
        // size is the whole difference between PE32 and PE32+.
        let dir_start = match bitness {
            Bitness::Pe32 => 96,
            Bitness::Pe32Plus => 112,
        };
        let (rsrc_rva, rsrc_size) = data_directory(&optional, dir_start, 2);

        let mut sections = Vec::with_capacity(section_count);
        let table = read_at(file, pe_at + 24 + optional_size as u64, section_count * 40)?;
        for chunk in table.chunks_exact(40) {
            let name = String::from_utf8_lossy(&chunk[0..8])
                .trim_end_matches('\0')
                .trim_end()
                .to_string();
            sections.push(Section {
                name,
                virtual_size: u32::from_le_bytes([chunk[8], chunk[9], chunk[10], chunk[11]]),
                virtual_address: u32::from_le_bytes([chunk[12], chunk[13], chunk[14], chunk[15]]),
                raw_size: u32::from_le_bytes([chunk[16], chunk[17], chunk[18], chunk[19]]),
                raw_offset: u32::from_le_bytes([chunk[20], chunk[21], chunk[22], chunk[23]]),
            });
        }

        Ok(Headers {
            bitness,
            machine,
            subsystem,
            timestamp,
            sections,
            rsrc_rva,
            rsrc_size,
        })
    }

    /// The first resource of `kind`, walking type → name → language.
    fn resource(
        &mut self,
        file: &mut File,
        path: &Path,
        kind: u32,
    ) -> Result<Option<Vec<u8>>, InstallError> {
        if self.rsrc_rva == 0 || self.rsrc_size == 0 {
            return Ok(None);
        }
        let Some(section) = self
            .sections
            .iter()
            .find(|s| s.contains(self.rsrc_rva))
            .cloned()
        else {
            return Ok(None);
        };
        if section.raw_size == 0 {
            return Ok(None);
        }
        let read_at = |file: &mut File, at: u64, len: usize| -> Result<Vec<u8>, InstallError> {
            let mut buf = vec![0u8; len];
            file.seek(SeekFrom::Start(at))
                .and_then(|_| file.read_exact(&mut buf))
                .map_err(|source| InstallError::Read {
                    path: path.to_path_buf(),
                    source,
                })?;
            Ok(buf)
        };

        let tree_len = (section.raw_size as u64).min(TREE_WINDOW) as usize;
        let tree = read_at(file, section.raw_offset as u64, tree_len)?;

        // Offsets inside the tree are relative to the start of the resource
        // section; the leaf's own pointer is an RVA, which is not the same
        // thing and is the mistake that yields plausible-looking rubbish.
        let Some(leaf) = find_resource(&tree, kind) else {
            return Ok(None);
        };
        let Some(within) = leaf.rva.checked_sub(section.virtual_address) else {
            return Ok(None);
        };
        if leaf.size as u64 > MAX_RESOURCE || within + leaf.size > section.raw_size {
            return Ok(None);
        }
        Ok(Some(read_at(
            file,
            section.raw_offset as u64 + within as u64,
            leaf.size as usize,
        )?))
    }
}

/// `(rva, size)` of one data directory entry, or zeroes if the header is too
/// short to hold it.
fn data_directory(optional: &[u8], start: usize, index: usize) -> (u32, u32) {
    let at = start + index * 8;
    if optional.len() < at + 8 {
        return (0, 0);
    }
    let rva = u32::from_le_bytes([
        optional[at],
        optional[at + 1],
        optional[at + 2],
        optional[at + 3],
    ]);
    let size = u32::from_le_bytes([
        optional[at + 4],
        optional[at + 5],
        optional[at + 6],
        optional[at + 7],
    ]);
    (rva, size)
}

/// Where a resource's bytes live: an RVA and a length, from its leaf entry.
struct Leaf {
    rva: u32,
    size: u32,
}

/// Walk the three-level resource tree — type, then name, then language — and
/// return the first leaf under `kind`.
fn find_resource(rsrc: &[u8], kind: u32) -> Option<Leaf> {
    let type_entry = directory_entries(rsrc, 0)?
        .into_iter()
        .find(|e| !e.is_named && e.id == kind && e.is_directory)?;
    let name_entry = directory_entries(rsrc, type_entry.offset)?
        .into_iter()
        .find(|e| e.is_directory)?;
    let lang_entry = directory_entries(rsrc, name_entry.offset)?
        .into_iter()
        .find(|e| !e.is_directory)?;

    let at = lang_entry.offset as usize;
    let leaf = rsrc.get(at..at + 16)?;
    Some(Leaf {
        rva: u32::from_le_bytes([leaf[0], leaf[1], leaf[2], leaf[3]]),
        size: u32::from_le_bytes([leaf[4], leaf[5], leaf[6], leaf[7]]),
    })
}

struct DirEntry {
    id: u32,
    is_named: bool,
    is_directory: bool,
    /// Offset from the start of the resource section.
    offset: u32,
}

fn directory_entries(rsrc: &[u8], at: u32) -> Option<Vec<DirEntry>> {
    let at = at as usize;
    let header = rsrc.get(at..at + 16)?;
    let named = u16::from_le_bytes([header[12], header[13]]) as usize;
    let ids = u16::from_le_bytes([header[14], header[15]]) as usize;
    let mut out = Vec::with_capacity(named + ids);
    for i in 0..named + ids {
        let e = at + 16 + i * 8;
        let entry = rsrc.get(e..e + 8)?;
        let name = u32::from_le_bytes([entry[0], entry[1], entry[2], entry[3]]);
        let offset = u32::from_le_bytes([entry[4], entry[5], entry[6], entry[7]]);
        out.push(DirEntry {
            id: name & 0x7fff_ffff,
            is_named: name & 0x8000_0000 != 0,
            is_directory: offset & 0x8000_0000 != 0,
            offset: offset & 0x7fff_ffff,
        });
    }
    Some(out)
}

/// The `StringFileInfo` pairs from a `VS_VERSIONINFO` block.
///
/// Stored as read, with only trailing whitespace trimmed — InstallShield pads
/// every value out to a fixed width with spaces, and `"InstallShield        "`
/// is nobody's product name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VersionInfo {
    pub strings: BTreeMap<String, String>,
}

impl VersionInfo {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.strings
            .get(key)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    pub fn company_name(&self) -> Option<&str> {
        self.get("CompanyName")
    }

    pub fn product_name(&self) -> Option<&str> {
        self.get("ProductName")
    }

    pub fn product_version(&self) -> Option<&str> {
        self.get("ProductVersion")
    }

    pub fn file_version(&self) -> Option<&str> {
        self.get("FileVersion")
    }

    pub fn file_description(&self) -> Option<&str> {
        self.get("FileDescription")
    }

    pub fn original_filename(&self) -> Option<&str> {
        self.get("OriginalFilename")
    }

    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }
}

/// Parse a `VS_VERSIONINFO` block into its string pairs.
///
/// # The length trap
///
/// A `String` entry's `wValueLength` is documented as a count of **words**,
/// and producers disagree: Garmin's and Microsoft's bootstrappers write a byte
/// count instead, so doubling it runs the value off the end of its entry and
/// into the next key. The entry's own `wLength` is the authority here — the
/// value is whatever lies between the padded key and the end of the entry,
/// cut at the first NUL.
pub fn version_info(buf: &[u8]) -> VersionInfo {
    let mut strings = BTreeMap::new();
    // VS_VERSIONINFO -> StringFileInfo -> <lang> -> String
    if let Some(root) = node(buf, 0) {
        let mut at = root.children;
        while at < root.end.min(buf.len()) {
            let Some(child) = node(buf, at) else { break };
            if child.key == "StringFileInfo" {
                collect_tables(buf, &child, &mut strings);
            }
            let next = align4(child.end);
            if next <= at {
                break;
            }
            at = next;
        }
    }
    VersionInfo { strings }
}

fn collect_tables(buf: &[u8], parent: &Node, out: &mut BTreeMap<String, String>) {
    let mut at = parent.children;
    while at < parent.end.min(buf.len()) {
        let Some(table) = node(buf, at) else { break };
        let mut inner = table.children;
        while inner < table.end.min(buf.len()) {
            let Some(entry) = node(buf, inner) else { break };
            let value = utf16_at(buf, entry.value, entry.end);
            if !entry.key.is_empty() && !value.is_empty() {
                out.insert(entry.key.clone(), value);
            }
            let next = align4(entry.end);
            if next <= inner {
                break;
            }
            inner = next;
        }
        let next = align4(table.end);
        if next <= at {
            break;
        }
        at = next;
    }
}

struct Node {
    key: String,
    /// Offset of this node's value, past the key and its padding.
    value: usize,
    /// Offset of the first child, past the value and its padding.
    ///
    /// The value has to be stepped over even when nothing reads it: the root
    /// `VS_VERSIONINFO` node carries a 52-byte `VS_FIXEDFILEINFO` before its
    /// children, and a walk that skips only the key lands in the middle of it
    /// and finds no `StringFileInfo` at all.
    children: usize,
    /// Offset one past the last byte of this node.
    end: usize,
}

fn node(buf: &[u8], at: usize) -> Option<Node> {
    let head = buf.get(at..at + 6)?;
    let length = u16::from_le_bytes([head[0], head[1]]) as usize;
    let value_length = u16::from_le_bytes([head[2], head[3]]) as usize;
    let is_text = u16::from_le_bytes([head[4], head[5]]) == 1;
    if length < 6 {
        return None;
    }
    let mut p = at + 6;
    let mut key = String::new();
    loop {
        let pair = buf.get(p..p + 2)?;
        p += 2;
        let unit = u16::from_le_bytes([pair[0], pair[1]]);
        if unit == 0 {
            break;
        }
        key.push(char::from_u32(unit as u32).unwrap_or('\u{fffd}'));
    }
    let value = align4(p);
    let value_bytes = if is_text {
        value_length * 2
    } else {
        value_length
    };
    let end = at + length;
    Some(Node {
        key,
        value,
        children: align4(value + value_bytes).min(end),
        end,
    })
}

fn align4(at: usize) -> usize {
    (at + 3) & !3
}

/// UTF-16LE from `start` to at most `end`, cut at the first NUL.
fn utf16_at(buf: &[u8], start: usize, end: usize) -> String {
    let end = end.min(buf.len());
    if start >= end {
        return String::new();
    }
    let units: Vec<u16> = buf[start..end]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units).trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build one `VS_VERSIONINFO` node the way Windows writes them.
    fn build(key: &str, value: &[u8], is_text: bool, children: &[Vec<u8>]) -> Vec<u8> {
        let mut out = vec![0u8; 6];
        for unit in key.encode_utf16().chain(std::iter::once(0)) {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        while out.len() % 4 != 0 {
            out.push(0);
        }
        out.extend_from_slice(value);
        for child in children {
            while out.len() % 4 != 0 {
                out.push(0);
            }
            out.extend_from_slice(child);
        }
        let length = out.len() as u16;
        out[0..2].copy_from_slice(&length.to_le_bytes());
        // Text values are counted in UTF-16 code units, binary ones in bytes.
        let value_length = if is_text {
            (value.len() / 2) as u16
        } else {
            value.len() as u16
        };
        out[2..4].copy_from_slice(&value_length.to_le_bytes());
        out[4..6].copy_from_slice(&(is_text as u16).to_le_bytes());
        out
    }

    fn wide(text: &str) -> Vec<u8> {
        text.encode_utf16()
            .chain(std::iter::once(0))
            .flat_map(|u| u.to_le_bytes())
            .collect()
    }

    fn version_block(pairs: &[(&str, &str)]) -> Vec<u8> {
        let strings: Vec<Vec<u8>> = pairs
            .iter()
            .map(|(k, v)| build(k, &wide(v), true, &[]))
            .collect();
        let table = build("040904b0", &[], false, &strings);
        let sfi = build("StringFileInfo", &[], false, &[table]);
        // VS_FIXEDFILEINFO is 52 bytes and nothing here reads it.
        build("VS_VERSION_INFO", &[0u8; 52], false, &[sfi])
    }

    #[test]
    fn reads_string_pairs() {
        let block = version_block(&[
            ("CompanyName", "Just Flight (MSFS 2024)"),
            ("ProductName", "InstallShield"),
        ]);
        let info = version_info(&block);
        assert_eq!(info.company_name(), Some("Just Flight (MSFS 2024)"));
        assert_eq!(info.product_name(), Some("InstallShield"));
    }

    /// The root node carries a 52-byte `VS_FIXEDFILEINFO` before its children, and
    /// a walk that steps over only the key lands inside it.
    #[test]
    fn steps_over_the_fixed_info_block() {
        let block = version_block(&[("ProductName", "Garmin Aviation Trainers")]);
        assert_eq!(
            version_info(&block).product_name(),
            Some("Garmin Aviation Trainers")
        );
    }

    /// Garmin's and Microsoft's bootstrappers write `wValueLength` in bytes
    /// where the documentation says words. Doubling it walks off the end of
    /// the entry and into the next key, so the entry's own length is what
    /// bounds the value.
    #[test]
    fn survives_a_value_length_written_in_bytes() {
        let mut block = version_block(&[
            ("CompanyName", "Garmin"),
            ("ProductName", "Garmin Aviation Trainers"),
        ]);
        // Find the CompanyName entry and double its declared value length.
        let needle = wide("CompanyName");
        let at = block
            .windows(needle.len())
            .position(|w| w == needle)
            .expect("key present")
            - 6;
        let declared = u16::from_le_bytes([block[at + 2], block[at + 3]]);
        block[at + 2..at + 4].copy_from_slice(&(declared * 2).to_le_bytes());

        let info = version_info(&block);
        assert_eq!(info.company_name(), Some("Garmin"));
        assert_eq!(
            info.product_name(),
            Some("Garmin Aviation Trainers"),
            "the next entry must still be found"
        );
    }

    /// InstallShield pads every value out to a fixed width with spaces.
    /// `"InstallShield        "` is nobody's product name.
    #[test]
    fn trims_the_padding_installshield_writes() {
        let block = version_block(&[("ProductName", "InstallShield                    ")]);
        assert_eq!(version_info(&block).product_name(), Some("InstallShield"));
    }

    #[test]
    fn an_empty_value_is_absent_rather_than_blank() {
        let block = version_block(&[("ProductName", "")]);
        assert_eq!(version_info(&block).product_name(), None);
    }

    #[test]
    fn garbage_does_not_loop_or_panic() {
        assert!(version_info(&[]).is_empty());
        assert!(version_info(&[0u8; 64]).is_empty());
        assert!(version_info(&[0xff; 64]).is_empty());
    }

    #[test]
    fn a_short_optional_header_has_no_data_directory() {
        assert_eq!(data_directory(&[0u8; 100], 96, 2), (0, 0));
    }

    #[test]
    fn a_section_holds_the_rvas_inside_it() {
        let section = Section {
            name: ".rsrc".into(),
            virtual_address: 0x5000,
            virtual_size: 0x1000,
            raw_size: 0x1000,
            raw_offset: 0x4000,
        };
        assert!(section.contains(0x5000));
        assert!(section.contains(0x5fff));
        assert!(!section.contains(0x4fff));
        assert!(!section.contains(0x6000));
    }

    // ── Whole files: headers, section table and the `.rsrc` walk ─────────────

    /// A real file under the system temp directory, removed on drop.
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn file(tag: &str, bytes: &[u8]) -> Self {
            let path = std::env::temp_dir().join(format!(
                "hangar-pe-{tag}-{}-{:?}.exe",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::write(&path, bytes).expect("scratch");
            Scratch(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    const RSRC_RVA: u32 = 0x2000;
    const RSRC_AT: usize = 0x400;

    /// A `.rsrc` section holding one `RT_VERSION` resource: type → name →
    /// language directories, one data entry, then the block itself.
    fn rsrc_section(block: &[u8]) -> Vec<u8> {
        fn directory(out: &mut Vec<u8>, id: u32, offset: u32) {
            out.extend_from_slice(&[0u8; 12]);
            out.extend_from_slice(&0u16.to_le_bytes()); // named entries
            out.extend_from_slice(&1u16.to_le_bytes()); // id entries
            out.extend_from_slice(&id.to_le_bytes());
            out.extend_from_slice(&offset.to_le_bytes());
        }
        let mut out = Vec::new();
        directory(&mut out, RT_VERSION, 0x8000_0000 | 0x18);
        directory(&mut out, 1, 0x8000_0000 | 0x30);
        directory(&mut out, 0x409, 0x48);
        // The data entry's pointer is an RVA, not a section offset.
        out.extend_from_slice(&(RSRC_RVA + 0x58).to_le_bytes());
        out.extend_from_slice(&(block.len() as u32).to_le_bytes());
        out.extend_from_slice(&[0u8; 8]); // code page, reserved
        assert_eq!(out.len(), 0x58);
        out.extend_from_slice(block);
        out
    }

    /// A minimal PE: DOS header, COFF header, optional header with a data
    /// directory, `.text` and `.rsrc` section entries, and the `.rsrc` bytes.
    fn pe_file(magic: u16, rsrc: Option<&[u8]>) -> Vec<u8> {
        let mut out = vec![0u8; 0x40];
        out[0..2].copy_from_slice(b"MZ");
        out[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());

        let dir_start = if magic == 0x20b { 112 } else { 96 };
        let optional_size = dir_start + 16 * 8;
        out.extend_from_slice(b"PE\0\0");
        out.extend_from_slice(&0x14cu16.to_le_bytes()); // machine
        out.extend_from_slice(&2u16.to_le_bytes()); // sections
        out.extend_from_slice(&0x5f5e_1000u32.to_le_bytes()); // timestamp
        out.extend_from_slice(&[0u8; 8]);
        out.extend_from_slice(&(optional_size as u16).to_le_bytes());
        out.extend_from_slice(&0x0102u16.to_le_bytes());

        let mut optional = vec![0u8; optional_size];
        optional[0..2].copy_from_slice(&magic.to_le_bytes());
        optional[68..70].copy_from_slice(&2u16.to_le_bytes()); // GUI subsystem
        let rsrc_len = rsrc.map_or(0, <[u8]>::len) as u32;
        if rsrc.is_some() {
            let at = dir_start + 2 * 8;
            optional[at..at + 4].copy_from_slice(&RSRC_RVA.to_le_bytes());
            optional[at + 4..at + 8].copy_from_slice(&rsrc_len.to_le_bytes());
        }
        out.extend_from_slice(&optional);

        let section = |out: &mut Vec<u8>, name: &[u8], rva: u32, size: u32, at: u32| {
            let mut entry = [0u8; 40];
            entry[..name.len()].copy_from_slice(name);
            entry[8..12].copy_from_slice(&size.to_le_bytes());
            entry[12..16].copy_from_slice(&rva.to_le_bytes());
            entry[16..20].copy_from_slice(&size.to_le_bytes());
            entry[20..24].copy_from_slice(&at.to_le_bytes());
            out.extend_from_slice(&entry);
        };
        section(&mut out, b".text", 0x1000, 0, 0);
        section(&mut out, b".rsrc", RSRC_RVA, rsrc_len, RSRC_AT as u32);

        if let Some(rsrc) = rsrc {
            assert!(out.len() <= RSRC_AT);
            out.resize(RSRC_AT, 0);
            out.extend_from_slice(rsrc);
        }
        out
    }

    #[test]
    fn a_pe32_file_reads_its_headers_sections_and_version() {
        let block = version_block(&[("CompanyName", "Orbx"), ("ProductName", "Orbx Central")]);
        let file = Scratch::file("pe32", &pe_file(0x10b, Some(&rsrc_section(&block))));

        let pe = Pe::read(&file.0).expect("a valid PE");
        assert_eq!(pe.bitness, Bitness::Pe32);
        assert_eq!(pe.machine, 0x14c);
        assert_eq!(pe.subsystem, 2);
        assert_eq!(pe.timestamp, 0x5f5e_1000);
        assert_eq!(pe.section_names(), vec![".text", ".rsrc"]);
        assert!(pe.has_section(".rsrc"));
        assert!(!pe.has_section(".data"));
        assert_eq!(pe.version.company_name(), Some("Orbx"));
        assert_eq!(pe.version.product_name(), Some("Orbx Central"));

        assert_eq!(
            pe.resource(&file.0, RT_VERSION).expect("read"),
            Some(block),
            "the leaf's RVA is mapped through the section, not used as an offset"
        );
        assert_eq!(pe.resource(&file.0, RT_GROUP_ICON).expect("read"), None);
    }

    /// PE32+ moves the data directory 16 bytes further on; reading it at the
    /// PE32 offset would find no `.rsrc` and report no version.
    #[test]
    fn a_pe32_plus_file_finds_its_data_directory_further_on() {
        let block = version_block(&[("ProductName", "Little Navmap")]);
        let file = Scratch::file("pe32plus", &pe_file(0x20b, Some(&rsrc_section(&block))));

        let pe = Pe::read(&file.0).expect("a valid PE");
        assert_eq!(pe.bitness, Bitness::Pe32Plus);
        assert_eq!(pe.version.product_name(), Some("Little Navmap"));
    }

    #[test]
    fn a_file_with_no_resources_has_no_version_rather_than_an_error() {
        let file = Scratch::file("norsrc", &pe_file(0x10b, None));
        let pe = Pe::read(&file.0).expect("a valid PE");
        assert!(pe.version.is_empty());
        assert_eq!(pe.resource(&file.0, RT_VERSION).expect("read"), None);
    }

    /// A version leaf claiming more bytes than the section holds is ignored,
    /// not read past the section's end.
    #[test]
    fn a_leaf_running_past_its_section_is_ignored() {
        let block = version_block(&[("ProductName", "Truncated")]);
        let mut rsrc = rsrc_section(&block);
        rsrc[0x4c..0x50].copy_from_slice(&0x10_0000u32.to_le_bytes());
        let file = Scratch::file("overrun", &pe_file(0x10b, Some(&rsrc)));
        assert!(Pe::read(&file.0).expect("a valid PE").version.is_empty());
    }

    #[test]
    fn what_is_not_a_pe_says_which_check_refused_it() {
        let why = |tag: &str, bytes: Vec<u8>| match Pe::read(&Scratch::file(tag, &bytes).0) {
            Err(InstallError::NotPe { why, .. }) => why,
            other => panic!("{tag}: expected NotPe, got {other:?}"),
        };

        let mut not_mz = pe_file(0x10b, None);
        not_mz[0..2].copy_from_slice(b"%P");
        assert_eq!(why("notmz", not_mz), "no MZ signature");

        let mut not_pe = pe_file(0x10b, None);
        not_pe[0x40..0x44].copy_from_slice(b"NE\0\0");
        assert_eq!(why("notpe", not_pe), "no PE signature at e_lfanew");

        assert_eq!(
            why("magic", pe_file(0x107, None)),
            "unknown optional header magic"
        );

        let mut short = pe_file(0x10b, None);
        short[0x54..0x56].copy_from_slice(&64u16.to_le_bytes());
        assert_eq!(
            why("short", short),
            "optional header too short to hold a data directory"
        );
    }

    /// Shorter than a DOS header is a read error, not a panic on a slice.
    #[test]
    fn a_truncated_file_is_a_read_error() {
        let file = Scratch::file("tiny", b"MZ");
        assert!(matches!(Pe::read(&file.0), Err(InstallError::Read { .. })));
    }
}
