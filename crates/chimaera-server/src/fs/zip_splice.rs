//! Edit a zip package (an Office deck, workbook or document) while every
//! untouched entry stays byte-identical: the backend of `POST /fs/package`
//! (which only replaces existing entries, [`splice`]) and of package edits
//! that also add or remove parts ([`splice_edit`]; a new slide brings its
//! XML, relationships and media).
//!
//! An edit to a 100 MB deck touches one or two small XML parts, and a deck
//! on a remote host must not travel whole for that. So the archive is never
//! re-encoded: untouched local records are copied verbatim in offset order,
//! a removed one is skipped, each replaced one becomes a fresh deflated
//! record, added ones follow the last record, and the central directory is
//! rewritten with patched offsets. Memory stays bounded by the central
//! directory ([`MAX_CENTRAL_DIR_BYTES`]) plus the in-memory entry data; the
//! archive itself and a streamed entry ([`EntryData::Stream`]) stream.
//!
//! Office files are untrusted input, and an archive this code cannot copy
//! exactly is refused rather than guessed at ([`SpliceError::Refused`]):
//! zip64, multi-disk, encrypted entries, duplicate names, local records that
//! overlap or leave the file, and a central directory that does not end where
//! the end record says. So is an edit it cannot apply as asked: a replaced or
//! removed name the archive lacks, an added one it already has, a name listed
//! twice, or removing `[Content_Types].xml`.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::{self, Read, Seek, SeekFrom, Write};

// The largest package `fs/package` will edit: an edit that would grow a
// package past it is refused, or the result could not be edited again.
use super::MAX_PACKAGE_BYTES;

/// Most entries one request may replace.
pub(crate) const MAX_REPLACED_ENTRIES: usize = 64;
/// Most entries one [`Edit`] may name across replace, add and remove:
/// adding or removing a slide names a handful of parts each.
pub(crate) const MAX_EDITED_ENTRIES: usize = 256;
/// Largest uncompressed entry a streamed source may hold: the package cap,
/// since a bigger entry could not fit anyway.
pub(crate) const MAX_STREAMED_ENTRY_BYTES: u64 = MAX_PACKAGE_BYTES;
/// Most in-memory ([`EntryData::Bytes`]) data one edit may carry: it and its
/// deflated copies are all held at once.
pub(crate) const MAX_EDIT_BYTES: usize = 32 * 1024 * 1024;
/// Largest central directory read into memory. A real deck's is tens of KB;
/// this only bounds a hostile one.
const MAX_CENTRAL_DIR_BYTES: u64 = 16 * 1024 * 1024;

const LOCAL_SIG: u32 = 0x0403_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const EOCD_SIG: u32 = 0x0605_4b50;
const ZIP64_LOCATOR_SIG: u32 = 0x0706_4b50;
const DESCRIPTOR_SIG: u32 = 0x0807_4b50;
const LOCAL_LEN: usize = 30;
const CENTRAL_LEN: usize = 46;
const EOCD_LEN: usize = 22;
/// The end record sits within its own length plus the largest comment.
const EOCD_SEARCH: u64 = EOCD_LEN as u64 + 0xFFFF;
const ZIP64_EXTRA_ID: u16 = 0x0001;

/// Flag bits: encrypted, data descriptor follows, strong encryption,
/// encrypted central directory.
const FLAG_ENCRYPTED: u16 = 1 << 0;
const FLAG_DESCRIPTOR: u16 = 1 << 3;
const FLAG_STRONG: u16 = 1 << 6;
const FLAG_CD_ENCRYPTED: u16 = 1 << 13;
/// Version 2.0: deflate. "Made by" for an added entry says MS-DOS (high
/// byte 0), so its zero external attributes mean a plain file.
const VERSION_DEFLATE: u16 = 20;
const METHOD_DEFLATE: u16 = 8;
/// 1980-01-01 00:00, the earliest DOS (date, time): an added entry's stamp
/// in an archive with no entry to take one from.
const DOS_EPOCH: (u16, u16) = (0x21, 0);
/// The one part every Office package needs: removing it is refused.
const CONTENT_TYPES: &str = "[Content_Types].xml";

#[derive(Debug)]
pub(crate) enum SpliceError {
    /// The archive or request is outside what a byte-exact splice supports;
    /// the message is shown to the user as is.
    Refused(String),
    Io(io::Error),
}

impl fmt::Display for SpliceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(why) => f.write_str(why),
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for SpliceError {}

impl From<io::Error> for SpliceError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

fn refuse<T>(why: impl Into<String>) -> Result<T, SpliceError> {
    Err(SpliceError::Refused(why.into()))
}

/// New contents for one existing entry.
pub(crate) struct Replacement {
    pub(crate) name: String,
    pub(crate) data: Vec<u8>,
}

/// Anything an entry can be streamed from.
pub(crate) trait ReadSeek: Read + Seek {}
impl<T: Read + Seek + ?Sized> ReadSeek for T {}

/// Where a replaced or added entry's contents come from.
pub(crate) enum EntryData<'a> {
    Bytes(&'a [u8]),
    /// From the source's current position to its end, ≤
    /// [`MAX_STREAMED_ENTRY_BYTES`]: a part too large to hold (a slide's
    /// video). It is read twice, see [`measure`].
    // Built by the plugin package host function; until then only by tests.
    #[cfg_attr(not(test), allow(dead_code))]
    Stream(&'a mut dyn ReadSeek),
}

/// One entry an [`Edit`] writes.
pub(crate) struct EntryEdit<'a> {
    pub(crate) name: &'a str,
    pub(crate) data: EntryData<'a>,
}

/// What [`splice_edit`] changes: existing entries rewritten, new entries
/// appended (in this order), existing entries dropped. A name appears once.
#[derive(Default)]
pub(crate) struct Edit<'a> {
    pub(crate) replace: Vec<EntryEdit<'a>>,
    pub(crate) add: Vec<EntryEdit<'a>>,
    pub(crate) remove: Vec<&'a str>,
}

/// The ceilings one edit is held to ([`LIMITS`]); tests shrink them.
#[derive(Clone, Copy)]
struct Limits {
    /// The whole output, projected before a byte is written.
    output: u64,
    /// All [`EntryData::Bytes`] together.
    bytes: usize,
    /// One [`EntryData::Stream`], uncompressed.
    stream: u64,
}

const LIMITS: Limits = Limits {
    output: MAX_PACKAGE_BYTES,
    bytes: MAX_EDIT_BYTES,
    stream: MAX_STREAMED_ENTRY_BYTES,
};

/// One entry as the central directory describes it.
struct Entry {
    /// The whole central record (fixed part + name + extra + comment).
    central: Vec<u8>,
    local_offset: u64,
}

impl Entry {
    fn name(&self) -> &[u8] {
        &self.central[CENTRAL_LEN..CENTRAL_LEN + usize::from(u16_at(&self.central, 28))]
    }
}

/// A parsed, validated archive: enough to splice it.
pub(crate) struct Archive {
    /// Central-directory order.
    entries: Vec<Entry>,
    /// Indexes into `entries`, by local-header offset.
    by_offset: Vec<usize>,
    cd_offset: u64,
    /// The end record plus its comment, as on disk.
    eocd: Vec<u8>,
}

impl Archive {
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

fn u16_at(buf: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([buf[at], buf[at + 1]])
}

fn u32_at(buf: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
}

fn put_u16(buf: &mut [u8], at: usize, value: u16) {
    buf[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(buf: &mut [u8], at: usize, value: u32) {
    buf[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn read_at<R: Read + Seek>(r: &mut R, at: u64, buf: &mut [u8]) -> Result<(), SpliceError> {
    r.seek(SeekFrom::Start(at))?;
    match r.read_exact(buf) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => {
            refuse("the archive is truncated")
        }
        Err(err) => Err(err.into()),
    }
}

/// Whether `name` is a plain relative entry name a request may target.
pub(crate) fn check_name(name: &str) -> Result<(), SpliceError> {
    if name.is_empty()
        || name.starts_with('/')
        || name.contains('\\')
        || name.contains('\0')
        || name.split('/').any(|seg| seg == "..")
    {
        return refuse(format!("invalid entry name {name:?}"));
    }
    Ok(())
}

/// Request-level checks that need no archive: count, names, duplicates.
pub(crate) fn check_request(replacements: &[Replacement]) -> Result<(), SpliceError> {
    if replacements.is_empty() {
        return refuse("no entries to replace");
    }
    if replacements.len() > MAX_REPLACED_ENTRIES {
        return refuse(format!(
            "too many entries in one save ({}, limit {MAX_REPLACED_ENTRIES})",
            replacements.len()
        ));
    }
    let mut seen = HashSet::new();
    for r in replacements {
        check_name(&r.name)?;
        if !seen.insert(r.name.as_str()) {
            return refuse(format!("entry {:?} is listed twice", r.name));
        }
    }
    Ok(())
}

/// Whether an added `name` is a valid OPC part name, beyond [`check_name`]:
/// PowerPoint meets a package that breaks these with a Repair prompt.
/// Replaced and removed names are not held to them: they must name what is
/// there, byte for byte.
fn check_part_name(name: &str) -> Result<(), SpliceError> {
    let why = if !name.is_ascii() {
        // OPC maps part names to ASCII URIs: the caller percent-encodes.
        "it must be ASCII (percent-encode other characters)"
    } else if name.ends_with('/') {
        "it ends with /"
    } else if name.split('/').any(str::is_empty) {
        "it has an empty segment"
    } else if name.split('/').any(|seg| seg.ends_with('.')) {
        "a segment is . or .., or ends with ."
    } else {
        return Ok(());
    };
    refuse(format!("{name:?} is not a valid part name: {why}"))
}

/// Edit-level checks that need no archive: count, in-memory data, names
/// (added ones as part names), a name used twice (within one list or across
/// them), and the part a package cannot lose.
fn check_edit(edit: &Edit<'_>, limits: Limits) -> Result<(), SpliceError> {
    let total = edit.replace.len() + edit.add.len() + edit.remove.len();
    if total == 0 {
        return refuse("no entries to edit");
    }
    if total > MAX_EDITED_ENTRIES {
        return refuse(format!(
            "too many entries in one edit ({total}, limit {MAX_EDITED_ENTRIES})"
        ));
    }
    let in_memory: usize = (edit.replace.iter().chain(&edit.add))
        .map(|e| match e.data {
            EntryData::Bytes(bytes) => bytes.len(),
            EntryData::Stream(_) => 0,
        })
        .sum();
    if in_memory > limits.bytes {
        return refuse(format!(
            "the edit's data is too large ({in_memory} bytes, limit {})",
            limits.bytes
        ));
    }
    for entry in &edit.add {
        check_name(entry.name)?;
        check_part_name(entry.name)?;
    }
    let names = edit.replace.iter().chain(&edit.add).map(|e| e.name);
    let mut seen = HashSet::with_capacity(total);
    for name in names.chain(edit.remove.iter().copied()) {
        check_name(name)?;
        if name.len() > usize::from(u16::MAX) {
            return refuse(format!("entry name too long ({} bytes)", name.len()));
        }
        if !seen.insert(name) {
            return refuse(format!("entry {name:?} is listed twice"));
        }
    }
    if edit.remove.contains(&CONTENT_TYPES) {
        return refuse(format!("{CONTENT_TYPES} cannot be removed"));
    }
    Ok(())
}

/// Find the end record: the last 64 KiB + 22 bytes, scanned backwards for a
/// signature whose comment ends exactly at the end of the file.
fn find_eocd<R: Read + Seek>(r: &mut R, len: u64) -> Result<(u64, Vec<u8>), SpliceError> {
    if len < EOCD_LEN as u64 {
        return refuse("not a zip archive (too short)");
    }
    let tail_len = len.min(EOCD_SEARCH);
    let start = len - tail_len;
    // ≤ 64 KiB + 22 bytes.
    let mut tail = vec![0u8; tail_len as usize];
    read_at(r, start, &mut tail)?;
    let mut pos = tail.len() - EOCD_LEN;
    loop {
        if u32_at(&tail, pos) == EOCD_SIG
            && usize::from(u16_at(&tail, pos + 20)) == tail.len() - pos - EOCD_LEN
        {
            return Ok((start + pos as u64, tail[pos..].to_vec()));
        }
        if pos == 0 {
            return refuse("not a zip archive (no end-of-central-directory record)");
        }
        pos -= 1;
    }
}

/// Parse and validate the archive in `r` (whose length is `len`).
pub(crate) fn read_archive<R: Read + Seek>(r: &mut R, len: u64) -> Result<Archive, SpliceError> {
    let (eocd_offset, eocd) = find_eocd(r, len)?;
    if eocd_offset >= 20 {
        let mut locator = [0u8; 4];
        read_at(r, eocd_offset - 20, &mut locator)?;
        if u32::from_le_bytes(locator) == ZIP64_LOCATOR_SIG {
            return refuse("zip64 archives are not supported for editing");
        }
    }
    let disk = u16_at(&eocd, 4);
    let cd_disk = u16_at(&eocd, 6);
    let on_disk = u16_at(&eocd, 8);
    let total = u16_at(&eocd, 10);
    let cd_size = u32_at(&eocd, 12);
    let cd_offset = u32_at(&eocd, 16);
    if [disk, cd_disk, on_disk, total].contains(&0xFFFF)
        || cd_size == 0xFFFF_FFFF
        || cd_offset == 0xFFFF_FFFF
    {
        return refuse("zip64 archives are not supported for editing");
    }
    if disk != 0 || cd_disk != 0 || on_disk != total {
        return refuse("multi-disk archives are not supported");
    }
    let (cd_offset, cd_size) = (u64::from(cd_offset), u64::from(cd_size));
    if cd_offset + cd_size != eocd_offset {
        return refuse("the central directory does not end where the end record says");
    }
    if cd_size > MAX_CENTRAL_DIR_BYTES {
        return refuse("the archive's central directory is too large");
    }
    let mut cd = vec![0u8; cd_size as usize];
    read_at(r, cd_offset, &mut cd)?;

    let mut entries = Vec::with_capacity(usize::from(total));
    let mut names = HashSet::with_capacity(usize::from(total));
    let mut at = 0usize;
    while at < cd.len() {
        if cd.len() - at < CENTRAL_LEN || u32_at(&cd, at) != CENTRAL_SIG {
            return refuse("malformed central directory");
        }
        let flags = u16_at(&cd, at + 8);
        let csize = u32_at(&cd, at + 20);
        let usize_ = u32_at(&cd, at + 24);
        let name_len = usize::from(u16_at(&cd, at + 28));
        let extra_len = usize::from(u16_at(&cd, at + 30));
        let comment_len = usize::from(u16_at(&cd, at + 32));
        let disk_start = u16_at(&cd, at + 34);
        let local_offset = u32_at(&cd, at + 42);
        let record_len = CENTRAL_LEN + name_len + extra_len + comment_len;
        if cd.len() - at < record_len {
            return refuse("malformed central directory");
        }
        if csize == 0xFFFF_FFFF
            || usize_ == 0xFFFF_FFFF
            || local_offset == 0xFFFF_FFFF
            || disk_start == 0xFFFF
        {
            return refuse("zip64 archives are not supported for editing");
        }
        if disk_start != 0 {
            return refuse("multi-disk archives are not supported");
        }
        if flags & (FLAG_ENCRYPTED | FLAG_STRONG | FLAG_CD_ENCRYPTED) != 0 {
            return refuse("encrypted archives are not supported for editing");
        }
        let entry = Entry {
            central: cd[at..at + record_len].to_vec(),
            local_offset: u64::from(local_offset),
        };
        if !names.insert(entry.name().to_vec()) {
            return refuse(format!(
                "the archive lists {:?} twice",
                String::from_utf8_lossy(entry.name())
            ));
        }
        entries.push(entry);
        at += record_len;
    }
    if entries.len() != usize::from(total) {
        return refuse("the central directory does not match the end record's entry count");
    }
    drop(names);

    let mut by_offset: Vec<usize> = (0..entries.len()).collect();
    by_offset.sort_by_key(|&i| entries[i].local_offset);
    let mut prev_end = 0u64;
    for &i in &by_offset {
        let end = record_end(r, &entries[i], cd_offset)?;
        if entries[i].local_offset < prev_end {
            return refuse("the archive's entries overlap");
        }
        prev_end = end;
    }
    Ok(Archive {
        entries,
        by_offset,
        cd_offset,
        eocd,
    })
}

/// Where `entry`'s local record (header, data, descriptor) ends, after
/// checking it lies wholly before the central directory and agrees with its
/// central record.
fn record_end<R: Read + Seek>(
    r: &mut R,
    entry: &Entry,
    cd_offset: u64,
) -> Result<u64, SpliceError> {
    let start = entry.local_offset;
    if start + LOCAL_LEN as u64 > cd_offset {
        return refuse("an entry's local header lies outside the archive");
    }
    let mut header = [0u8; LOCAL_LEN];
    read_at(r, start, &mut header)?;
    if u32_at(&header, 0) != LOCAL_SIG {
        return refuse("an entry's local header is missing");
    }
    let local_flags = u16_at(&header, 6);
    if local_flags & (FLAG_ENCRYPTED | FLAG_STRONG) != 0 {
        return refuse("encrypted archives are not supported for editing");
    }
    let name_len = u64::from(u16_at(&header, 26));
    let extra_len = u64::from(u16_at(&header, 28));
    let data_start = start + LOCAL_LEN as u64 + name_len + extra_len;
    let central_flags = u16_at(&entry.central, 8);
    let csize = u64::from(u32_at(&entry.central, 20));
    let data_end = data_start + csize;
    if data_end > cd_offset {
        return refuse("an entry's data lies outside the archive");
    }
    let mut name = vec![0u8; name_len as usize];
    read_at(r, start + LOCAL_LEN as u64, &mut name)?;
    if name != entry.name() {
        return refuse("an entry's local header does not match the central directory");
    }
    if central_flags & FLAG_DESCRIPTOR == 0 {
        return Ok(data_end);
    }
    // The descriptor is copied with whatever follows it up to the next
    // record, so its length only matters for the overlap check: an
    // unrecognized one counts as empty.
    let crc = u32_at(&entry.central, 16);
    let room = (cd_offset - data_end).min(16) as usize;
    let mut tail = [0u8; 16];
    read_at(r, data_end, &mut tail[..room])?;
    let len = if room >= 16 && u32_at(&tail, 0) == DESCRIPTOR_SIG && u32_at(&tail, 4) == crc {
        16
    } else if room >= 12 && u32_at(&tail, 0) == crc {
        12
    } else {
        0
    };
    Ok(data_end + len)
}

/// A replaced or added entry, deflated or measured, ready to be written.
struct Fresh<'a> {
    crc: u32,
    /// Uncompressed size.
    size: u32,
    csize: u32,
    body: Body<'a>,
}

enum Body<'a> {
    Deflated(Vec<u8>),
    /// Deflated again as it is written; `start` is where [`measure`] began.
    Stream {
        src: &'a mut dyn ReadSeek,
        start: u64,
        cap: u64,
    },
}

/// A 32-bit zip size field (zip64 is never written).
fn size32(len: u64) -> Result<u32, SpliceError> {
    u32::try_from(len)
        .ok()
        .filter(|&s| s != 0xFFFF_FFFF)
        .ok_or_else(|| SpliceError::Refused("a replacement is too large".into()))
}

fn deflate<'a>(data: &[u8]) -> Result<Fresh<'a>, SpliceError> {
    let size = size32(data.len() as u64)?;
    let mut crc = flate2::Crc::new();
    crc.update(data);
    let mut encoder = flate2::write::DeflateEncoder::new(
        Vec::with_capacity(data.len() / 2),
        flate2::Compression::default(),
    );
    encoder.write_all(data)?;
    let compressed = encoder.finish()?;
    Ok(Fresh {
        crc: crc.sum(),
        size,
        csize: size32(compressed.len() as u64)?,
        body: Body::Deflated(compressed),
    })
}

/// Deflate `src` (≤ `cap` bytes from its position) into `w`: its CRC, its
/// size and the compressed size. Memory stays at `io::copy`'s buffer plus
/// the encoder's window.
fn deflate_stream<W: Write>(
    src: &mut dyn ReadSeek,
    cap: u64,
    w: W,
) -> Result<(u32, u64, u64), SpliceError> {
    let mut reader = flate2::CrcReader::new(src.take(cap.saturating_add(1)));
    let mut encoder = flate2::write::DeflateEncoder::new(
        Tally { inner: w, at: 0 },
        flate2::Compression::default(),
    );
    let size = io::copy(&mut reader, &mut encoder)?;
    if size > cap {
        return refuse(format!("a streamed entry is too large (limit {cap} bytes)"));
    }
    let crc = reader.crc().sum();
    let written = encoder.finish()?;
    Ok((crc, size, written.at))
}

/// First pass over a streamed entry. A local header carries the CRC and
/// sizes before the data, and the output cannot be patched afterwards: it
/// is a stream, hashed as it is written. A temp spool would put up to
/// [`MAX_STREAMED_ENTRY_BYTES`] of compressed data on a disk the daemon
/// does not own (a cluster's shared /tmp). So the source is deflated once
/// into a counter for the header, and again into the output
/// ([`write_body`]), which refuses if the second pass's CRC or sizes differ.
fn measure(src: &mut dyn ReadSeek, cap: u64) -> Result<Fresh<'_>, SpliceError> {
    let start = src.stream_position()?;
    let (crc, size, csize) = deflate_stream(&mut *src, cap, io::sink())?;
    Ok(Fresh {
        crc,
        size: size32(size)?,
        csize: size32(csize)?,
        body: Body::Stream { src, start, cap },
    })
}

fn prepare(data: EntryData<'_>, limits: Limits) -> Result<Fresh<'_>, SpliceError> {
    match data {
        EntryData::Bytes(bytes) => deflate(bytes),
        EntryData::Stream(src) => measure(src, limits.stream),
    }
}

/// An edit matched against the archive, its entries ready to write.
struct Plan<'a> {
    /// By central-directory index.
    replaced: HashMap<usize, Fresh<'a>>,
    removed: HashSet<usize>,
    added: Vec<(&'a str, Fresh<'a>)>,
}

/// Each entry's local record span, by central-directory index: from its
/// header to the next record, or to the central directory for the last.
fn record_spans(archive: &Archive) -> Vec<u64> {
    let mut spans = vec![0u64; archive.entries.len()];
    for (pos, &i) in archive.by_offset.iter().enumerate() {
        let end = archive
            .by_offset
            .get(pos + 1)
            .map_or(archive.cd_offset, |&next| {
                archive.entries[next].local_offset
            });
        spans[i] = end - archive.entries[i].local_offset;
    }
    spans
}

/// Match `edit` to `archive`'s entries, then deflate or measure what it
/// writes and project the output's size. Every name is matched first, so a
/// refusal on a name reads no data; nothing is written before this returns.
fn plan<'a>(archive: &Archive, edit: Edit<'a>, limits: Limits) -> Result<Plan<'a>, SpliceError> {
    check_edit(&edit, limits)?;
    let index: HashMap<&[u8], usize> = archive
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| (e.name(), i))
        .collect();
    let find = |name: &str| match index.get(name.as_bytes()) {
        Some(&i) => Ok(i),
        None => refuse(format!("the archive has no entry {name:?}")),
    };
    let mut replaced = Vec::with_capacity(edit.replace.len());
    for entry in edit.replace {
        replaced.push((find(entry.name)?, entry.data));
    }
    let mut removed = HashSet::with_capacity(edit.remove.len());
    for name in &edit.remove {
        removed.insert(find(name)?);
    }
    // OPC part names compare ignoring ASCII case. An entry being removed
    // still counts: a re-add under another case is a rename, not this edit.
    let mut folded: HashSet<Vec<u8>> = archive
        .entries
        .iter()
        .map(|e| e.name().to_ascii_lowercase())
        .collect();
    for entry in &edit.add {
        if index.contains_key(entry.name.as_bytes()) {
            return refuse(format!("the archive already has an entry {:?}", entry.name));
        }
        if !folded.insert(entry.name.to_ascii_lowercase().into_bytes()) {
            return refuse(format!(
                "{:?} differs from another part name only in case",
                entry.name
            ));
        }
    }
    drop(folded);
    let count = archive.entries.len() - removed.len() + edit.add.len();
    if count >= 0xFFFF {
        return refuse("the edited archive would need zip64");
    }

    let mut fresh = HashMap::with_capacity(replaced.len());
    for (i, data) in replaced {
        fresh.insert(i, prepare(data, limits)?);
    }
    let mut added = Vec::with_capacity(edit.add.len());
    for entry in edit.add {
        added.push((entry.name, prepare(entry.data, limits)?));
    }

    // What `splice_edit` will write: everything up to the central directory
    // and the end record, less removed records, with replaced records at
    // their fresh size, plus the added records. A replaced central record
    // can only shrink (a zip64 extra dropped), so this is an upper bound.
    let spans = record_spans(archive);
    let mut projected = archive.cd_offset + archive.eocd.len() as u64;
    for (i, entry) in archive.entries.iter().enumerate() {
        if removed.contains(&i) {
            projected -= spans[i];
            continue;
        }
        projected += entry.central.len() as u64;
        if let Some(new) = fresh.get(&i) {
            projected -= spans[i];
            projected += (LOCAL_LEN + entry.name().len()) as u64 + u64::from(new.csize);
        }
    }
    for (name, new) in &added {
        projected += (LOCAL_LEN + CENTRAL_LEN + 2 * name.len()) as u64 + u64::from(new.csize);
    }
    if projected > limits.output {
        return refuse(format!(
            "the edited file would be too large to edit afterwards ({projected} bytes, limit {})",
            limits.output
        ));
    }
    Ok(Plan {
        replaced: fresh,
        removed,
        added,
    })
}

/// Counts what passes through, so the splice knows each record's new offset.
struct Tally<W> {
    inner: W,
    at: u64,
}

impl<W: Write> Tally<W> {
    fn put(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.inner.write_all(bytes)?;
        self.at += bytes.len() as u64;
        Ok(())
    }

    fn copy<R: Read + Seek>(&mut self, r: &mut R, from: u64, len: u64) -> Result<(), SpliceError> {
        r.seek(SeekFrom::Start(from))?;
        let copied = io::copy(&mut r.take(len), &mut self.inner)?;
        self.at += copied;
        if copied != len {
            // The file shrank under us; the precondition re-check would
            // catch it, but a short copy must never pass as a good one.
            return refuse("the archive is truncated");
        }
        Ok(())
    }

    /// The current offset as a 32-bit zip field (zip64 is never written).
    fn offset32(&self) -> Result<u32, SpliceError> {
        u32::try_from(self.at)
            .ok()
            .filter(|&o| o != 0xFFFF_FFFF)
            .ok_or_else(|| SpliceError::Refused("the edited archive would need zip64".into()))
    }
}

impl<W: Write> Write for Tally<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.at += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Write a fresh entry's data. A streamed one is deflated a second time, and
/// the header already written carries the first pass's CRC and sizes: a
/// mismatch in any of them is refused. With all three equal, either pass's
/// bytes make a valid record for that header, whether or not the deflated
/// bytes themselves are identical.
fn write_body<W: Write>(out: &mut Tally<W>, fresh: &mut Fresh<'_>) -> Result<(), SpliceError> {
    match &mut fresh.body {
        Body::Deflated(bytes) => out.put(bytes)?,
        Body::Stream { src, start, cap } => {
            src.seek(SeekFrom::Start(*start))?;
            let (crc, size, csize) = deflate_stream(&mut **src, *cap, &mut *out)?;
            if crc != fresh.crc || size != u64::from(fresh.size) || csize != u64::from(fresh.csize)
            {
                return refuse("a streamed entry changed while it was written");
            }
        }
    }
    Ok(())
}

/// A fresh local record: deflate, no data descriptor, no extra field.
fn put_local<W: Write>(
    out: &mut Tally<W>,
    flags: u16,
    (date, time): (u16, u16),
    name: &[u8],
    fresh: &mut Fresh<'_>,
) -> Result<(), SpliceError> {
    let mut header = [0u8; LOCAL_LEN];
    put_u32(&mut header, 0, LOCAL_SIG);
    put_u16(&mut header, 4, VERSION_DEFLATE);
    put_u16(&mut header, 6, flags);
    put_u16(&mut header, 8, METHOD_DEFLATE);
    put_u16(&mut header, 10, time);
    put_u16(&mut header, 12, date);
    put_u32(&mut header, 14, fresh.crc);
    put_u32(&mut header, 18, fresh.csize);
    put_u32(&mut header, 22, fresh.size);
    // ≤ u16::MAX: an archive name came from a u16 field, an added one passed
    // `check_edit`.
    put_u16(&mut header, 26, name.len() as u16);
    put_u16(&mut header, 28, 0);
    out.put(&header)?;
    out.put(name)?;
    write_body(out, fresh)
}

/// An added entry's central record: made by MS-DOS 2.0, no extra, no
/// comment, zero attributes.
fn new_central(
    name: &str,
    flags: u16,
    (date, time): (u16, u16),
    fresh: &Fresh<'_>,
    offset: u32,
) -> Vec<u8> {
    let mut record = vec![0u8; CENTRAL_LEN];
    put_u32(&mut record, 0, CENTRAL_SIG);
    put_u16(&mut record, 4, VERSION_DEFLATE);
    put_u16(&mut record, 6, VERSION_DEFLATE);
    put_u16(&mut record, 8, flags);
    put_u16(&mut record, 10, METHOD_DEFLATE);
    put_u16(&mut record, 12, time);
    put_u16(&mut record, 14, date);
    put_u32(&mut record, 16, fresh.crc);
    put_u32(&mut record, 20, fresh.csize);
    put_u32(&mut record, 24, fresh.size);
    put_u16(&mut record, 28, name.len() as u16);
    put_u32(&mut record, 42, offset);
    record.extend_from_slice(name.as_bytes());
    record
}

/// Drop zip64 extra blocks from a replaced entry's central extra field: no
/// marker points at them, but a reader that honours them anyway would see
/// the old sizes. A malformed extra is kept as it was.
fn without_zip64(extra: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(extra.len());
    let mut at = 0;
    while at < extra.len() {
        if extra.len() - at < 4 {
            return extra.to_vec();
        }
        let id = u16_at(extra, at);
        let len = 4 + usize::from(u16_at(extra, at + 2));
        if extra.len() - at < len {
            return extra.to_vec();
        }
        if id != ZIP64_EXTRA_ID {
            out.extend_from_slice(&extra[at..at + len]);
        }
        at += len;
    }
    out
}

/// Write `archive` (read from `r`) to `w` with `replacements` swapped in:
/// the `POST /fs/package` edit, which only replaces existing entries (see
/// [`splice_edit`]).
pub(crate) fn splice<R: Read + Seek, W: Write>(
    r: &mut R,
    archive: &Archive,
    replacements: &[Replacement],
    w: W,
) -> Result<(), SpliceError> {
    check_request(replacements)?;
    let edit = Edit {
        replace: replacements
            .iter()
            .map(|entry| EntryEdit {
                name: &entry.name,
                data: EntryData::Bytes(&entry.data),
            })
            .collect(),
        ..Edit::default()
    };
    splice_edit(r, archive, edit, w)
}

/// Write `archive` (read from `r`) to `w` with `edit` applied.
///
/// Before the central directory everything is copied byte for byte in
/// offset order — a prefix before the first record, and each untouched
/// record together with whatever follows it up to the next one — except a
/// removed record (and what follows it), which is skipped, and a replaced
/// record, which becomes a fresh local header (no data descriptor, deflate,
/// the original flags without bit 3, time, date and name, no extra) plus its
/// deflated data. Added records follow the last one, in the edit's order,
/// as fresh local headers too: no flags (their names are ASCII part names),
/// and the archive's newest time and date, so the output stays a
/// deterministic function of the input and the edit. Each kept central
/// record is copied with its local offset patched (a replaced one also gets
/// its version, flags, method, CRC and sizes), the added ones are appended,
/// and the end record gets the new entry count and the directory's size and
/// offset. An output that would outgrow [`MAX_PACKAGE_BYTES`] is refused
/// before a byte is written.
pub(crate) fn splice_edit<R: Read + Seek, W: Write>(
    r: &mut R,
    archive: &Archive,
    edit: Edit<'_>,
    w: W,
) -> Result<(), SpliceError> {
    splice_within(r, archive, edit, w, LIMITS)
}

fn splice_within<R: Read + Seek, W: Write>(
    r: &mut R,
    archive: &Archive,
    edit: Edit<'_>,
    w: W,
    limits: Limits,
) -> Result<(), SpliceError> {
    let mut plan = plan(archive, edit, limits)?;

    let mut out = Tally { inner: w, at: 0 };
    let first = archive
        .by_offset
        .first()
        .map_or(archive.cd_offset, |&i| archive.entries[i].local_offset);
    out.copy(r, 0, first)?;

    let mut offsets = vec![0u32; archive.entries.len()];
    for (pos, &i) in archive.by_offset.iter().enumerate() {
        if plan.removed.contains(&i) {
            continue;
        }
        let entry = &archive.entries[i];
        offsets[i] = out.offset32()?;
        if let Some(new) = plan.replaced.get_mut(&i) {
            let central = &entry.central;
            let flags = u16_at(central, 8) & !FLAG_DESCRIPTOR;
            let stamp = (u16_at(central, 14), u16_at(central, 12));
            put_local(&mut out, flags, stamp, entry.name(), new)?;
        } else {
            let end = archive
                .by_offset
                .get(pos + 1)
                .map_or(archive.cd_offset, |&next| {
                    archive.entries[next].local_offset
                });
            out.copy(r, entry.local_offset, end - entry.local_offset)?;
        }
    }
    let stamp = archive
        .entries
        .iter()
        .map(|e| (u16_at(&e.central, 14), u16_at(&e.central, 12)))
        .max()
        .unwrap_or(DOS_EPOCH);
    let mut added_offsets = Vec::with_capacity(plan.added.len());
    for (name, new) in &mut plan.added {
        added_offsets.push(out.offset32()?);
        put_local(&mut out, 0, stamp, name.as_bytes(), new)?;
    }

    let cd_start = out.offset32()?;
    for (i, entry) in archive.entries.iter().enumerate() {
        if plan.removed.contains(&i) {
            continue;
        }
        let mut record = entry.central.clone();
        put_u32(&mut record, 42, offsets[i]);
        if let Some(new) = plan.replaced.get(&i) {
            put_u16(&mut record, 6, VERSION_DEFLATE);
            let flags = u16_at(&record, 8) & !FLAG_DESCRIPTOR;
            put_u16(&mut record, 8, flags);
            put_u16(&mut record, 10, METHOD_DEFLATE);
            put_u32(&mut record, 16, new.crc);
            put_u32(&mut record, 20, new.csize);
            put_u32(&mut record, 24, new.size);
            let name_len = usize::from(u16_at(&record, 28));
            let extra_len = usize::from(u16_at(&record, 30));
            let extra_at = CENTRAL_LEN + name_len;
            let extra = without_zip64(&record[extra_at..extra_at + extra_len]);
            if extra.len() != extra_len {
                let tail = record.split_off(extra_at + extra_len);
                record.truncate(extra_at);
                record.extend_from_slice(&extra);
                record.extend_from_slice(&tail);
                put_u16(&mut record, 30, extra.len() as u16);
            }
        }
        out.put(&record)?;
    }
    for ((name, new), offset) in plan.added.iter().zip(added_offsets) {
        out.put(&new_central(name, 0, stamp, new, offset))?;
    }
    let cd_size = u32::try_from(out.at - u64::from(cd_start))
        .map_err(|_| SpliceError::Refused("the edited archive would need zip64".into()))?;
    // < 0xFFFF, checked by `plan`.
    let count = (archive.entries.len() - plan.removed.len() + plan.added.len()) as u16;
    let mut eocd = archive.eocd.clone();
    put_u16(&mut eocd, 8, count);
    put_u16(&mut eocd, 10, count);
    put_u32(&mut eocd, 12, cd_size);
    put_u32(&mut eocd, 16, cd_start);
    out.put(&eocd)?;
    out.inner.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    use zip::write::SimpleFileOptions;
    use zip::CompressionMethod;

    const SLIDE: &[u8] = b"<p:sld><p:cSld><p:spTree/></p:cSld></p:sld>";

    // The zip crate writes the host it runs on into each header (DOS on
    // Windows, with DOS attributes) unless told one, so a fixture built on
    // Windows would hash differently.
    fn options(method: CompressionMethod) -> SimpleFileOptions {
        SimpleFileOptions::default()
            .compression_method(method)
            .system(zip::System::Unix)
    }

    /// A deck-shaped archive: stored and deflated entries, an archive
    /// comment, and an optional prefix before the first record.
    fn deck(prefix: &[u8]) -> Vec<u8> {
        let mut cursor = Cursor::new(prefix.to_vec());
        cursor.set_position(prefix.len() as u64);
        let mut zip = zip::ZipWriter::new(cursor);
        zip.set_comment("deck comment").unwrap();
        zip.start_file("[Content_Types].xml", options(CompressionMethod::Stored))
            .unwrap();
        zip.write_all(b"<Types/>").unwrap();
        zip.start_file(
            "ppt/slides/slide1.xml",
            options(CompressionMethod::Deflated),
        )
        .unwrap();
        zip.write_all(SLIDE).unwrap();
        zip.start_file(
            "ppt/slides/slide2.xml",
            options(CompressionMethod::Deflated),
        )
        .unwrap();
        zip.write_all(&SLIDE.repeat(50)).unwrap();
        zip.start_file("ppt/media/image1.png", options(CompressionMethod::Stored))
            .unwrap();
        zip.write_all(&[0x89, b'P', b'N', b'G', 1, 2, 3, 4])
            .unwrap();
        zip.finish().unwrap().into_inner()
    }

    /// Entries written to a non-seekable stream carry data descriptors.
    fn streamed_deck() -> Vec<u8> {
        let mut zip = zip::ZipWriter::new_stream(Vec::new());
        zip.start_file("a.xml", options(CompressionMethod::Deflated))
            .unwrap();
        zip.write_all(b"<a>first</a>").unwrap();
        zip.start_file("b.xml", options(CompressionMethod::Deflated))
            .unwrap();
        zip.write_all(b"<b>second</b>").unwrap();
        zip.start_file("c.bin", options(CompressionMethod::Stored))
            .unwrap();
        zip.write_all(b"raw bytes").unwrap();
        zip.finish().unwrap().into_inner()
    }

    fn run(input: &[u8], replacements: Vec<Replacement>) -> Result<Vec<u8>, SpliceError> {
        let mut r = Cursor::new(input);
        let archive = read_archive(&mut r, input.len() as u64)?;
        let mut out = Vec::new();
        splice(&mut r, &archive, &replacements, &mut out)?;
        Ok(out)
    }

    fn one(name: &str, data: &[u8]) -> Vec<Replacement> {
        vec![Replacement {
            name: name.into(),
            data: data.to_vec(),
        }]
    }

    fn refusal(result: Result<Vec<u8>, SpliceError>) -> String {
        match result {
            Err(SpliceError::Refused(why)) => why,
            Err(SpliceError::Io(err)) => panic!("io error, not a refusal: {err}"),
            Ok(_) => panic!("spliced, not refused"),
        }
    }

    /// Each entry's raw local record — header to the next record (or the
    /// central directory) — as an independent reader finds it.
    fn raw_records(bytes: &[u8]) -> HashMap<String, Vec<u8>> {
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let cd = zip.central_directory_start();
        let mut starts = Vec::new();
        for i in 0..zip.len() {
            let file = zip.by_index_raw(i).unwrap();
            starts.push((file.header_start(), file.name().unwrap().into_owned()));
        }
        starts.sort();
        let mut out = HashMap::new();
        for (k, (start, name)) in starts.iter().enumerate() {
            let end = starts.get(k + 1).map_or(cd, |next| next.0);
            out.insert(name.clone(), bytes[*start as usize..end as usize].to_vec());
        }
        out
    }

    fn contents(bytes: &[u8]) -> HashMap<String, Vec<u8>> {
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut out = HashMap::new();
        for i in 0..zip.len() {
            let mut file = zip.by_index(i).unwrap();
            let mut data = Vec::new();
            file.read_to_end(&mut data).unwrap();
            out.insert(file.name().unwrap().into_owned(), data);
        }
        out
    }

    /// THE requirement: replaced entries read back as the new bytes, every
    /// other entry's raw record is byte-identical, and the comment survives.
    fn assert_spliced(input: &[u8], output: &[u8], replaced: &[(&str, &[u8])]) {
        let before = contents(input);
        let after = contents(output);
        assert_eq!(before.len(), after.len());
        let raw_before = raw_records(input);
        let raw_after = raw_records(output);
        for (name, old) in &before {
            match replaced.iter().find(|(n, _)| n == name) {
                Some((_, new)) => assert_eq!(&after[name], new, "{name}"),
                None => {
                    assert_eq!(&after[name], old, "{name}");
                    assert_eq!(raw_after[name], raw_before[name], "raw record of {name}");
                }
            }
        }
        let a = zip::ZipArchive::new(Cursor::new(input)).unwrap();
        let b = zip::ZipArchive::new(Cursor::new(output)).unwrap();
        assert_eq!(a.comment(), b.comment());
        // Central-directory order is kept.
        let names = |z: &zip::ZipArchive<Cursor<&[u8]>>| {
            z.file_names()
                .map(|n| n.unwrap().into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&a), names(&b));
    }

    #[test]
    fn replaces_one_entry_and_keeps_the_rest_byte_identical() {
        let input = deck(b"");
        let new = b"<p:sld>edited</p:sld>";
        let output = run(&input, one("ppt/slides/slide1.xml", new)).unwrap();
        assert_spliced(&input, &output, &[("ppt/slides/slide1.xml", new)]);
        assert_fresh_records(&output, &["ppt/slides/slide1.xml"]);
    }

    #[test]
    fn replaces_a_stored_entry_and_several_at_once() {
        let input = deck(b"");
        let types = b"<Types><Default Extension=\"xml\"/></Types>".to_vec();
        let slide2 = SLIDE.repeat(3);
        let output = run(
            &input,
            vec![
                Replacement {
                    name: "[Content_Types].xml".into(),
                    data: types.clone(),
                },
                Replacement {
                    name: "ppt/slides/slide2.xml".into(),
                    data: slide2.clone(),
                },
            ],
        )
        .unwrap();
        assert_spliced(
            &input,
            &output,
            &[
                ("[Content_Types].xml", &types),
                ("ppt/slides/slide2.xml", &slide2),
            ],
        );
        assert_fresh_records(&output, &["[Content_Types].xml", "ppt/slides/slide2.xml"]);
        // The replaced records are deflated now.
        let mut zip = zip::ZipArchive::new(Cursor::new(&output[..])).unwrap();
        let method = zip.by_name("[Content_Types].xml").unwrap().compression();
        assert_eq!(method, CompressionMethod::Deflated);
    }

    #[test]
    fn keeps_a_prefix_before_the_first_record() {
        let input = deck(b"#!prefix bytes\n");
        assert!(input.starts_with(b"#!prefix"));
        let new = b"<p:sld/>";
        let output = run(&input, one("ppt/slides/slide2.xml", new)).unwrap();
        assert!(output.starts_with(b"#!prefix bytes\n"));
        assert_spliced(&input, &output, &[("ppt/slides/slide2.xml", new)]);
    }

    #[test]
    fn splices_entries_written_with_data_descriptors() {
        let input = streamed_deck();
        let mut zip = zip::ZipArchive::new(Cursor::new(&input[..])).unwrap();
        let flags = u16_at(
            &input,
            zip.by_index_raw(zip.index_for_name("a.xml").unwrap())
                .unwrap()
                .header_start() as usize
                + 6,
        );
        assert_ne!(
            flags & FLAG_DESCRIPTOR,
            0,
            "fixture lacks a data descriptor"
        );

        // The middle one: its neighbours keep their descriptors verbatim.
        let new = b"<b>changed, and longer than before</b>";
        let output = run(&input, one("b.xml", new)).unwrap();
        assert_spliced(&input, &output, &[("b.xml", new)]);
        let mut zip = zip::ZipArchive::new(Cursor::new(&output[..])).unwrap();
        let start = zip
            .by_index_raw(zip.index_for_name("b.xml").unwrap())
            .unwrap()
            .header_start() as usize;
        assert_eq!(u16_at(&output, start + 6) & FLAG_DESCRIPTOR, 0);
    }

    /// Splicing the output again with the same entries changes nothing, so
    /// the bytes are a deterministic function of the input and the request.
    #[test]
    fn splicing_is_deterministic() {
        let input = streamed_deck();
        let first = run(&input, one("a.xml", b"<a/>")).unwrap();
        let second = run(&first, one("a.xml", b"<a/>")).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn refuses_bad_request_names() {
        let input = deck(b"");
        for name in [
            "",
            "/abs.xml",
            "ppt/../x.xml",
            "..",
            "ppt\\slide.xml",
            "a\0b",
        ] {
            let why = refusal(run(&input, one(name, b"x")));
            assert!(why.contains("invalid entry name"), "{name:?}: {why}");
        }
        let why = refusal(run(&input, one("ppt/slides/slide9.xml", b"x")));
        assert!(why.contains("no entry"), "{why}");
        let twice = vec![
            Replacement {
                name: "ppt/slides/slide1.xml".into(),
                data: b"a".to_vec(),
            },
            Replacement {
                name: "ppt/slides/slide1.xml".into(),
                data: b"b".to_vec(),
            },
        ];
        assert!(refusal(run(&input, twice)).contains("twice"));
        let many = (0..=MAX_REPLACED_ENTRIES)
            .map(|i| Replacement {
                name: format!("e{i}.xml"),
                data: Vec::new(),
            })
            .collect();
        assert!(refusal(run(&input, many)).contains("too many"));
        assert!(refusal(run(&input, Vec::new())).contains("no entries"));
    }

    /// Offsets of every central record in `bytes`.
    fn central_records(bytes: &[u8]) -> Vec<usize> {
        let comment = zip::ZipArchive::new(Cursor::new(bytes))
            .unwrap()
            .comment()
            .len();
        let eocd = bytes.len() - EOCD_LEN - comment;
        let mut at = u32_at(bytes, eocd + 16) as usize;
        let mut out = Vec::new();
        while u32_at(bytes, at) == CENTRAL_SIG {
            out.push(at);
            at += CENTRAL_LEN
                + usize::from(u16_at(bytes, at + 28))
                + usize::from(u16_at(bytes, at + 30))
                + usize::from(u16_at(bytes, at + 32));
        }
        out
    }

    #[test]
    fn refuses_zip64_markers() {
        let input = deck(b"");
        let records = central_records(&input);
        for field in [20, 24, 42] {
            let mut bad = input.clone();
            put_u32(&mut bad, records[1] + field, 0xFFFF_FFFF);
            assert!(refusal(run(&bad, one("ppt/slides/slide1.xml", b"x"))).contains("zip64"));
        }
        let mut bad = input.clone();
        let eocd = bad.len() - EOCD_LEN - "deck comment".len();
        put_u16(&mut bad, eocd + 10, 0xFFFF);
        assert!(refusal(run(&bad, one("ppt/slides/slide1.xml", b"x"))).contains("zip64"));
    }

    #[test]
    fn refuses_a_zip64_locator() {
        let input = deck(b"");
        let eocd = input.len() - EOCD_LEN - "deck comment".len();
        let mut bad = input.clone();
        put_u32(&mut bad, eocd - 20, ZIP64_LOCATOR_SIG);
        assert!(refusal(run(&bad, one("ppt/slides/slide1.xml", b"x"))).contains("zip64"));
    }

    #[test]
    fn refuses_encrypted_entries_and_multi_disk_archives() {
        let input = deck(b"");
        let records = central_records(&input);
        let mut bad = input.clone();
        let flags = u16_at(&bad, records[2] + 8);
        put_u16(&mut bad, records[2] + 8, flags | FLAG_ENCRYPTED);
        assert!(refusal(run(&bad, one("ppt/slides/slide1.xml", b"x"))).contains("encrypted"));

        let mut bad = input.clone();
        let eocd = bad.len() - EOCD_LEN - "deck comment".len();
        put_u16(&mut bad, eocd + 4, 1);
        assert!(refusal(run(&bad, one("ppt/slides/slide1.xml", b"x"))).contains("multi-disk"));
    }

    #[test]
    fn refuses_duplicate_names() {
        // slide1.xml and slide2.xml have names of equal length: rename the
        // second, in its local header and its central record, to the first.
        let input = deck(b"");
        let mut bad = input.clone();
        let records = central_records(&input);
        let central = records[2];
        assert_eq!(
            &bad[central + CENTRAL_LEN..central + CENTRAL_LEN + 21],
            b"ppt/slides/slide2.xml"
        );
        bad[central + CENTRAL_LEN + 16] = b'1';
        let local = u32_at(&bad, central + 42) as usize;
        bad[local + LOCAL_LEN + 16] = b'1';
        assert!(refusal(run(&bad, one("ppt/slides/slide1.xml", b"x"))).contains("twice"));
    }

    #[test]
    fn refuses_overlapping_or_escaping_records() {
        let input = deck(b"");
        let records = central_records(&input);
        // Two central records pointing at one local record.
        let mut bad = input.clone();
        let first = u32_at(&bad, records[1] + 42);
        put_u32(&mut bad, records[2] + 42, first);
        let why = refusal(run(&bad, one("ppt/slides/slide1.xml", b"x")));
        assert!(
            why.contains("overlap") || why.contains("does not match"),
            "{why}"
        );

        // A compressed size that runs into the next record.
        let mut bad = input.clone();
        let csize = u32_at(&bad, records[1] + 20);
        put_u32(&mut bad, records[1] + 20, csize + 40);
        assert!(refusal(run(&bad, one("ppt/slides/slide1.xml", b"x"))).contains("overlap"));

        // A local offset past the central directory.
        let mut bad = input.clone();
        put_u32(&mut bad, records[3] + 42, (input.len() - 10) as u32);
        assert!(refusal(run(&bad, one("ppt/slides/slide1.xml", b"x"))).contains("outside"));
    }

    #[test]
    fn refuses_a_central_directory_that_ends_elsewhere() {
        let input = deck(b"");
        let mut bad = input.clone();
        let eocd = bad.len() - EOCD_LEN - "deck comment".len();
        let size = u32_at(&bad, eocd + 12);
        put_u32(&mut bad, eocd + 12, size - 1);
        let why = refusal(run(&bad, one("ppt/slides/slide1.xml", b"x")));
        assert!(why.contains("central directory does not end"), "{why}");
    }

    #[test]
    fn refuses_truncated_and_garbage_files() {
        let input = deck(b"");
        for cut in [10, input.len() / 2, input.len() - 5] {
            let why = refusal(run(&input[..cut], one("ppt/slides/slide1.xml", b"x")));
            assert!(!why.is_empty());
        }
        // Lose the head: the end record survives, its records do not.
        let tail = &input[20..];
        let why = refusal(run(tail, one("ppt/slides/slide1.xml", b"x")));
        assert!(!why.is_empty());
        let garbage: Vec<u8> = (0..70_000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
            .collect();
        assert!(refusal(run(&garbage, one("x", b"x"))).contains("not a zip"));
        assert!(refusal(run(b"", one("x", b"x"))).contains("not a zip"));
    }

    #[test]
    fn parses_an_empty_archive() {
        let empty = zip::ZipWriter::new(Cursor::new(Vec::new()))
            .finish()
            .unwrap()
            .into_inner();
        let mut r = Cursor::new(&empty[..]);
        let archive = read_archive(&mut r, empty.len() as u64).unwrap();
        assert_eq!(archive.len(), 0);
        assert!(refusal(run(&empty, one("a.xml", b"x"))).contains("no entry"));
    }

    #[test]
    fn strips_only_zip64_extra_blocks() {
        let extra = [
            &[0x01, 0x00, 0x04, 0x00, 1, 2, 3, 4][..],
            &[0x55, 0x54, 0x01, 0x00, 7][..],
        ]
        .concat();
        assert_eq!(without_zip64(&extra), vec![0x55, 0x54, 0x01, 0x00, 7]);
        let malformed = [0x01, 0x00, 0x09, 0x00, 1];
        assert_eq!(without_zip64(&malformed), malformed.to_vec());
    }

    fn run_edit(input: &[u8], edit: Edit<'_>) -> Result<Vec<u8>, SpliceError> {
        let mut r = Cursor::new(input);
        let archive = read_archive(&mut r, input.len() as u64)?;
        let mut out = Vec::new();
        splice_edit(&mut r, &archive, edit, &mut out)?;
        Ok(out)
    }

    fn bytes<'a>(name: &'a str, data: &'a [u8]) -> EntryEdit<'a> {
        EntryEdit {
            name,
            data: EntryData::Bytes(data),
        }
    }

    fn names(bytes: &[u8]) -> Vec<String> {
        zip::ZipArchive::new(Cursor::new(bytes))
            .unwrap()
            .file_names()
            .map(|n| n.unwrap().into_owned())
            .collect()
    }

    /// Every entry of `input` not in `touched` reads back the same from
    /// `output`, and its raw local record is byte-identical.
    fn assert_untouched(input: &[u8], output: &[u8], touched: &[&str]) {
        let before = contents(input);
        let after = contents(output);
        let raw_before = raw_records(input);
        let raw_after = raw_records(output);
        for (name, old) in &before {
            if touched.contains(&name.as_str()) {
                continue;
            }
            assert_eq!(&after[name], old, "{name}");
            assert_eq!(raw_after[name], raw_before[name], "raw record of {name}");
        }
        let a = zip::ZipArchive::new(Cursor::new(input)).unwrap();
        let b = zip::ZipArchive::new(Cursor::new(output)).unwrap();
        assert_eq!(a.comment(), b.comment());
    }

    fn central_record_of<'a>(bytes: &'a [u8], name: &str) -> &'a [u8] {
        let at = central_records(bytes)
            .into_iter()
            .find(|&at| {
                let len = usize::from(u16_at(bytes, at + 28));
                &bytes[at + CENTRAL_LEN..at + CENTRAL_LEN + len] == name.as_bytes()
            })
            .unwrap();
        &bytes[at..at + CENTRAL_LEN]
    }

    #[test]
    fn adds_an_entry_after_the_last_record() {
        let input = deck(b"");
        let new = SLIDE.repeat(2);
        let edit = Edit {
            add: vec![bytes("ppt/slides/slide3.xml", &new)],
            ..Edit::default()
        };
        let output = run_edit(&input, edit).unwrap();
        assert_untouched(&input, &output, &[]);
        assert_fresh_records(&output, &["ppt/slides/slide3.xml"]);
        assert_eq!(contents(&output)["ppt/slides/slide3.xml"], new);
        let mut expected = names(&input);
        expected.push("ppt/slides/slide3.xml".into());
        assert_eq!(names(&output), expected);

        let mut zip = zip::ZipArchive::new(Cursor::new(&output[..])).unwrap();
        let file = zip.by_name("ppt/slides/slide3.xml").unwrap();
        assert_eq!(file.compression(), CompressionMethod::Deflated);
        let start = file.header_start() as usize;
        drop(file);
        // After every original record, and with no data descriptor.
        assert_eq!(
            start,
            zip::ZipArchive::new(Cursor::new(&input[..]))
                .unwrap()
                .central_directory_start() as usize
        );
        assert_eq!(u16_at(&output, start + 6), 0);
        // Stamped like the archive's newest entry.
        let newest = central_records(&input)
            .into_iter()
            .map(|at| (u16_at(&input, at + 14), u16_at(&input, at + 12)))
            .max()
            .unwrap();
        let added = central_record_of(&output, "ppt/slides/slide3.xml");
        assert_eq!((u16_at(added, 14), u16_at(added, 12)), newest);
    }

    /// Added names are OPC part names. A non-ASCII one is refused rather
    /// than written with the UTF-8 flag: OPC maps part names to ASCII URIs,
    /// so the caller percent-encodes, and PowerPoint asks to repair a
    /// package that breaks the rules.
    #[test]
    fn refuses_added_names_that_are_not_part_names() {
        let input = deck(b"");
        for (name, why) in [
            ("ppt/media/bild-ä.png", "ASCII"),
            ("ppt/media/", "ends with /"),
            ("ppt//slide.xml", "empty segment"),
            ("ppt/./slide.xml", "ends with ."),
            ("./slide.xml", "ends with ."),
            ("ppt/slides./slide.xml", "ends with ."),
            ("ppt/slide.", "ends with ."),
            ("PPT/SLIDES/SLIDE1.XML", "only in case"),
            ("[content_types].xml", "only in case"),
        ] {
            let edit = Edit {
                add: vec![bytes(name, b"x")],
                ..Edit::default()
            };
            let refused = refusal(run_edit(&input, edit));
            assert!(refused.contains(why), "{name:?}: {refused}");
        }
        // Two added names that differ only in case.
        let edit = Edit {
            add: vec![bytes("ppt/a.xml", b"x"), bytes("ppt/A.xml", b"y")],
            ..Edit::default()
        };
        assert!(refusal(run_edit(&input, edit)).contains("only in case"));
        // Replaced and removed names match bytes as they are.
        let edit = Edit {
            replace: vec![bytes("PPT/SLIDES/SLIDE1.XML", b"x")],
            ..Edit::default()
        };
        assert!(refusal(run_edit(&input, edit)).contains("no entry"));
    }

    /// Every freshly written record's local header agrees with its central
    /// record. The zip reader the tests use trusts the central directory
    /// alone, so a wrong local header would pass every other check.
    fn assert_fresh_records(output: &[u8], fresh: &[&str]) {
        for name in fresh {
            let central = central_record_of(output, name);
            let local = u32_at(central, 42) as usize;
            assert_eq!(u32_at(output, local), LOCAL_SIG, "{name}");
            for (l, c) in [(4, 6), (6, 8), (8, 10), (10, 12), (12, 14), (26, 28)] {
                assert_eq!(
                    u16_at(output, local + l),
                    u16_at(central, c),
                    "{name}: local field {l}"
                );
            }
            for (l, c) in [(14, 16), (18, 20), (22, 24)] {
                assert_eq!(
                    u32_at(output, local + l),
                    u32_at(central, c),
                    "{name}: local field {l}"
                );
            }
            assert_eq!(u16_at(output, local + 6) & FLAG_DESCRIPTOR, 0, "{name}");
            assert_eq!(u16_at(output, local + 28), 0, "{name}: extra");
            let name_at = local + LOCAL_LEN;
            assert_eq!(&output[name_at..name_at + name.len()], name.as_bytes());
        }
    }

    #[test]
    fn removes_an_entry_and_its_local_record() {
        let input = deck(b"");
        let gone = raw_records(&input)["ppt/slides/slide1.xml"].clone();
        let edit = Edit {
            remove: vec!["ppt/slides/slide1.xml"],
            ..Edit::default()
        };
        let output = run_edit(&input, edit).unwrap();
        assert_untouched(&input, &output, &["ppt/slides/slide1.xml"]);
        let expected: Vec<String> = names(&input)
            .into_iter()
            .filter(|n| n != "ppt/slides/slide1.xml")
            .collect();
        assert_eq!(names(&output), expected);
        assert!(!output.windows(gone.len()).any(|w| w == gone.as_slice()));
        let kept = central_records(&output);
        assert_eq!(kept.len(), central_records(&input).len() - 1);
    }

    #[test]
    fn adds_removes_and_replaces_in_one_edit() {
        let input = streamed_deck();
        let new_a = b"<a>rewritten</a>".to_vec();
        let new_d = b"<d>new part</d>".repeat(40);
        let edit = Edit {
            replace: vec![bytes("a.xml", &new_a)],
            add: vec![bytes("d.xml", &new_d), bytes("media/e.bin", b"\x00\x01")],
            remove: vec!["b.xml"],
        };
        let output = run_edit(&input, edit).unwrap();
        assert_untouched(&input, &output, &["a.xml", "b.xml"]);
        assert_fresh_records(&output, &["a.xml", "d.xml", "media/e.bin"]);
        let after = contents(&output);
        assert_eq!(after["a.xml"], new_a);
        assert_eq!(after["d.xml"], new_d);
        assert_eq!(after["media/e.bin"], b"\x00\x01");
        assert!(!after.contains_key("b.xml"));
        assert_eq!(names(&output), ["a.xml", "c.bin", "d.xml", "media/e.bin"]);
        // The same edit again is the same bytes.
        let again = Edit {
            replace: vec![bytes("a.xml", &new_a)],
            add: vec![bytes("d.xml", &new_d), bytes("media/e.bin", b"\x00\x01")],
            remove: vec!["b.xml"],
        };
        assert_eq!(run_edit(&input, again).unwrap(), output);
    }

    #[test]
    fn refuses_edits_it_cannot_apply() {
        let input = deck(b"");
        let slide1 = "ppt/slides/slide1.xml";
        let cases: Vec<(Edit<'_>, &str)> = vec![
            (Edit::default(), "no entries"),
            (
                Edit {
                    add: vec![bytes(slide1, b"x")],
                    ..Edit::default()
                },
                "already has",
            ),
            (
                Edit {
                    remove: vec!["ppt/slides/slide9.xml"],
                    ..Edit::default()
                },
                "no entry",
            ),
            (
                Edit {
                    replace: vec![bytes("ppt/slides/slide9.xml", b"x")],
                    ..Edit::default()
                },
                "no entry",
            ),
            (
                Edit {
                    remove: vec!["[Content_Types].xml"],
                    ..Edit::default()
                },
                "cannot be removed",
            ),
            (
                Edit {
                    replace: vec![bytes(slide1, b"x")],
                    remove: vec![slide1],
                    ..Edit::default()
                },
                "twice",
            ),
            (
                Edit {
                    add: vec![bytes("new.xml", b"x"), bytes("new.xml", b"y")],
                    ..Edit::default()
                },
                "twice",
            ),
            (
                Edit {
                    remove: vec![slide1, slide1],
                    ..Edit::default()
                },
                "twice",
            ),
            (
                Edit {
                    add: vec![bytes("ppt/../evil.xml", b"x")],
                    ..Edit::default()
                },
                "invalid entry name",
            ),
            (
                Edit {
                    remove: vec!["/abs.xml"],
                    ..Edit::default()
                },
                "invalid entry name",
            ),
            (
                Edit {
                    remove: vec!["x"; MAX_EDITED_ENTRIES + 1],
                    ..Edit::default()
                },
                "too many",
            ),
        ];
        for (edit, expect) in cases {
            let why = refusal(run_edit(&input, edit));
            assert!(why.contains(expect), "{expect}: {why}");
        }
        let long = "x".repeat(usize::from(u16::MAX) + 1);
        let edit = Edit {
            add: vec![bytes(&long, b"x")],
            ..Edit::default()
        };
        assert!(refusal(run_edit(&input, edit)).contains("too long"));
    }

    /// An edit to an archive this code cannot copy is refused the same way
    /// a replacement is.
    #[test]
    fn refuses_adds_to_unsupported_archives() {
        let input = deck(b"");
        let mut bad = input.clone();
        let eocd = bad.len() - EOCD_LEN - "deck comment".len();
        put_u16(&mut bad, eocd + 4, 1);
        let edit = Edit {
            add: vec![bytes("new.xml", b"x")],
            ..Edit::default()
        };
        assert!(refusal(run_edit(&bad, edit)).contains("multi-disk"));
    }

    fn noise(len: usize) -> Vec<u8> {
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 32) as u8
            })
            .collect()
    }

    #[test]
    fn streams_an_entry_from_a_reader() {
        let input = deck(b"");
        // A few MiB, half incompressible, read from past a prefix.
        let mut data = noise(3 << 20);
        data.extend(SLIDE.repeat(20_000));
        let mut source = Cursor::new([b"skip me".as_slice(), &data].concat());
        source.set_position(7);
        let mut replacement = Cursor::new(SLIDE.repeat(3));
        let edit = Edit {
            replace: vec![EntryEdit {
                name: "ppt/slides/slide2.xml",
                data: EntryData::Stream(&mut replacement),
            }],
            add: vec![EntryEdit {
                name: "ppt/media/media1.mp4",
                data: EntryData::Stream(&mut source),
            }],
            ..Edit::default()
        };
        let output = run_edit(&input, edit).unwrap();
        assert_untouched(&input, &output, &["ppt/slides/slide2.xml"]);
        assert_fresh_records(&output, &["ppt/slides/slide2.xml", "ppt/media/media1.mp4"]);
        // The reader checks each entry's CRC as it reads to the end.
        let after = contents(&output);
        assert_eq!(after["ppt/slides/slide2.xml"], SLIDE.repeat(3));
        assert!(after["ppt/media/media1.mp4"] == data);
        let mut crc = flate2::Crc::new();
        crc.update(&data);
        let mut zip = zip::ZipArchive::new(Cursor::new(&output[..])).unwrap();
        let file = zip.by_name("ppt/media/media1.mp4").unwrap();
        assert_eq!(file.crc32(), crc.sum());
        assert_eq!(file.size(), data.len() as u64);
    }

    #[test]
    fn refuses_a_stream_over_its_cap() {
        let mut source = Cursor::new(vec![0u8; 1025]);
        match measure(&mut source, 1024) {
            Err(SpliceError::Refused(why)) => assert!(why.contains("too large"), "{why}"),
            _ => panic!("measured past the cap"),
        }
        let mut source = Cursor::new(vec![0u8; 1024]);
        assert_eq!(measure(&mut source, 1024).ok().map(|f| f.size), Some(1024));
    }

    /// A source whose bytes change after the first pass.
    struct Shifty {
        inner: Cursor<Vec<u8>>,
        seeks: u32,
    }

    impl Read for Shifty {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.inner.read(buf)
        }
    }

    impl Seek for Shifty {
        fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
            self.seeks += 1;
            if self.seeks == 2 {
                self.inner.get_mut()[0] ^= 0xFF;
            }
            self.inner.seek(to)
        }
    }

    #[test]
    fn refuses_a_stream_that_changed_between_passes() {
        let input = deck(b"");
        let mut source = Shifty {
            inner: Cursor::new(noise(4096)),
            seeks: 0,
        };
        let edit = Edit {
            add: vec![EntryEdit {
                name: "ppt/media/image2.png",
                data: EntryData::Stream(&mut source),
            }],
            ..Edit::default()
        };
        assert!(refusal(run_edit(&input, edit)).contains("changed"));
    }

    /// Header start of each entry in `bytes`, by name.
    fn header_starts(bytes: &[u8]) -> HashMap<String, u64> {
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        (0..zip.len())
            .map(|i| {
                let file = zip.by_index_raw(i).unwrap();
                (file.name().unwrap().into_owned(), file.header_start())
            })
            .collect()
    }

    #[test]
    fn removes_the_record_just_before_the_central_directory() {
        let input = deck(b"");
        let starts = header_starts(&input);
        let last = starts.iter().max_by_key(|(_, &at)| at).unwrap().0.clone();
        assert_eq!(last, "ppt/media/image1.png");
        let edit = Edit {
            remove: vec![last.as_str()],
            ..Edit::default()
        };
        let output = run_edit(&input, edit).unwrap();
        assert_untouched(&input, &output, &[last.as_str()]);
        assert!(!names(&output).contains(&last));
        // The central directory now starts where the removed record did.
        let cd = zip::ZipArchive::new(Cursor::new(&output[..]))
            .unwrap()
            .central_directory_start();
        assert_eq!(cd, starts[&last]);
    }

    #[test]
    fn removes_the_first_record_after_a_prefix() {
        let prefix = b"#!prefix bytes\n";
        let mut cursor = Cursor::new(prefix.to_vec());
        cursor.set_position(prefix.len() as u64);
        let mut zip = zip::ZipWriter::new(cursor);
        zip.start_file("a.xml", options(CompressionMethod::Deflated))
            .unwrap();
        zip.write_all(b"<a>first</a>").unwrap();
        zip.start_file("[Content_Types].xml", options(CompressionMethod::Stored))
            .unwrap();
        zip.write_all(b"<Types/>").unwrap();
        zip.start_file("b.xml", options(CompressionMethod::Deflated))
            .unwrap();
        zip.write_all(b"<b>second</b>").unwrap();
        let input = zip.finish().unwrap().into_inner();
        assert_eq!(header_starts(&input)["a.xml"], prefix.len() as u64);

        let edit = Edit {
            remove: vec!["a.xml"],
            ..Edit::default()
        };
        let output = run_edit(&input, edit).unwrap();
        assert!(output.starts_with(prefix));
        assert_untouched(&input, &output, &["a.xml"]);
        assert_eq!(names(&output), ["[Content_Types].xml", "b.xml"]);
        assert_eq!(
            header_starts(&output)["[Content_Types].xml"],
            prefix.len() as u64
        );
    }

    #[test]
    fn removes_everything_but_the_content_types() {
        let input = deck(b"");
        let edit = Edit {
            remove: vec![
                "ppt/slides/slide1.xml",
                "ppt/slides/slide2.xml",
                "ppt/media/image1.png",
            ],
            ..Edit::default()
        };
        let output = run_edit(&input, edit).unwrap();
        assert_eq!(names(&output), ["[Content_Types].xml"]);
        assert_untouched(
            &input,
            &output,
            &[
                "ppt/slides/slide1.xml",
                "ppt/slides/slide2.xml",
                "ppt/media/image1.png",
            ],
        );
    }

    /// Splice with `limits`: the result, and whatever was written.
    fn run_within(
        input: &[u8],
        edit: Edit<'_>,
        limits: Limits,
    ) -> (Result<(), SpliceError>, Vec<u8>) {
        let mut r = Cursor::new(input);
        let archive = read_archive(&mut r, input.len() as u64).unwrap();
        let mut out = Vec::new();
        let result = splice_within(&mut r, &archive, edit, &mut out, limits);
        (result, out)
    }

    /// The projected size is exact for these fixtures (no zip64 extra to
    /// drop), so a cap one byte under the output refuses, before writing.
    #[test]
    fn refuses_an_output_over_the_package_cap() {
        let input = streamed_deck();
        let new_d = b"<d>new part</d>".repeat(40);
        let edit = || Edit {
            replace: vec![bytes("a.xml", b"<a>rewritten</a>")],
            add: vec![bytes("d.xml", &new_d)],
            remove: vec!["b.xml"],
        };
        let expected = run_edit(&input, edit()).unwrap();
        let exact = expected.len() as u64;

        let (result, written) = run_within(
            &input,
            edit(),
            Limits {
                output: exact - 1,
                ..LIMITS
            },
        );
        match result {
            Err(SpliceError::Refused(why)) => {
                assert!(why.contains("too large to edit afterwards"), "{why}");
            }
            _ => panic!("spliced past the cap"),
        }
        assert!(written.is_empty(), "wrote before refusing");

        let (result, written) = run_within(
            &input,
            edit(),
            Limits {
                output: exact,
                ..LIMITS
            },
        );
        assert!(result.is_ok());
        assert_eq!(written, expected);

        // A removal alone is projected exactly too.
        let remove = || Edit {
            remove: vec!["c.bin"],
            ..Edit::default()
        };
        let exact = run_edit(&input, remove()).unwrap().len() as u64;
        let limits = Limits {
            output: exact - 1,
            ..LIMITS
        };
        assert!(run_within(&input, remove(), limits).0.is_err());
    }

    #[test]
    fn refuses_in_memory_data_over_its_cap() {
        let input = deck(b"");
        let limits = Limits {
            bytes: 10,
            ..LIMITS
        };
        let edit = Edit {
            replace: vec![bytes("ppt/slides/slide1.xml", b"123456")],
            add: vec![bytes("ppt/slides/slide3.xml", b"12345")],
            ..Edit::default()
        };
        let (result, written) = run_within(&input, edit, limits);
        match result {
            Err(SpliceError::Refused(why)) => assert!(why.contains("data is too large"), "{why}"),
            _ => panic!("held more than the cap"),
        }
        assert!(written.is_empty());
        let edit = Edit {
            replace: vec![bytes("ppt/slides/slide1.xml", b"123456")],
            add: vec![bytes("ppt/slides/slide3.xml", b"1234")],
            ..Edit::default()
        };
        assert!(run_within(&input, edit, limits).0.is_ok());
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::Digest as _;
        sha2::Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// The route's replace-only splice writes exactly what it wrote before
    /// the splicer learned to add and remove. The output hashes were taken
    /// by running the previous `zip_splice::splice` (origin/main 8e077606)
    /// on these same fixtures; the input hashes pin the fixtures, so a zip
    /// writer upgrade that changes them fails here, not as a false
    /// regression (recompute both from that commit's splicer then).
    #[test]
    fn replace_only_output_is_unchanged_from_before_add_and_remove() {
        let types = b"<Types><Default Extension=\"xml\"/></Types>".to_vec();
        // (label, input, replacements, input hash, output hash)
        let cases = [
            (
                "deck, slide1",
                deck(b""),
                one("ppt/slides/slide1.xml", b"<p:sld>edited</p:sld>"),
                "e29cefefc95b3231a6bd7eeb700e7536ebce1b0943ea8e8a2cd309538923ecf7",
                "1f2212ac04f5694700a5986237f1cb17fd41b09e97eff0bbb459607e15541f75",
            ),
            (
                "deck, content types + slide2",
                deck(b""),
                vec![
                    Replacement {
                        name: "[Content_Types].xml".into(),
                        data: types,
                    },
                    Replacement {
                        name: "ppt/slides/slide2.xml".into(),
                        data: SLIDE.repeat(3),
                    },
                ],
                "e29cefefc95b3231a6bd7eeb700e7536ebce1b0943ea8e8a2cd309538923ecf7",
                "a59e797e50ea7cdcf71846070d1be862569579c69d583678aacdcb02b42b075e",
            ),
            (
                "prefixed deck, slide2",
                deck(b"#!prefix bytes\n"),
                one("ppt/slides/slide2.xml", b"<p:sld/>"),
                "c8cc9c5043d361d309aa8e4396fdbf5070c136a3cf70195f580df0637961f022",
                "6138d5d573a6ba4d71944d1fdbc25f86ac826a351ca8b10996ad647e499c383c",
            ),
            (
                "data descriptors, b.xml",
                streamed_deck(),
                one("b.xml", b"<b>changed, and longer than before</b>"),
                "aad8abab129ae8ccb21cff8940350a1230b40117bea8766906849eb82a8317ec",
                "4b94f1e689cb67a095735e269f82dfd92e27228f365446ce3ce7fe36ab4c5ea7",
            ),
        ];
        for (label, input, replacements, input_hash, output_hash) in cases {
            assert_eq!(sha256_hex(&input), input_hash, "fixture moved: {label}");
            let output = run(&input, replacements).unwrap();
            assert_eq!(sha256_hex(&output), output_hash, "{label}");
        }
    }
}
