//! Replace a few entries of a zip package (an Office deck, workbook or
//! document) while every other entry stays byte-identical: the backend of
//! `POST /fs/package`.
//!
//! An edit to a 100 MB deck touches one or two small XML parts, and a deck
//! on a remote host must not travel whole for that. So the archive is never
//! re-encoded: untouched local records are copied verbatim in offset order,
//! each replaced one becomes a fresh deflated record, and the central
//! directory is rewritten with patched offsets. Memory stays bounded by the
//! central directory ([`MAX_CENTRAL_DIR_BYTES`]) plus the replacements; the
//! archive itself streams.
//!
//! Office files are untrusted input, and an archive this code cannot copy
//! exactly is refused rather than guessed at ([`SpliceError::Refused`]):
//! zip64, multi-disk, encrypted entries, duplicate names, local records that
//! overlap or leave the file, and a central directory that does not end where
//! the end record says. v1 only replaces existing entries.

use std::collections::HashMap;
use std::fmt;
use std::io::{self, Read, Seek, SeekFrom, Write};

/// Most entries one request may replace.
pub(crate) const MAX_REPLACED_ENTRIES: usize = 64;
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
    let mut seen = std::collections::HashSet::new();
    for r in replacements {
        check_name(&r.name)?;
        if !seen.insert(r.name.as_str()) {
            return refuse(format!("entry {:?} is listed twice", r.name));
        }
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
    let mut names = std::collections::HashSet::with_capacity(usize::from(total));
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

/// Pairs each replacement with the index of the entry it replaces.
fn match_replacements<'a>(
    archive: &Archive,
    replacements: &'a [Replacement],
) -> Result<HashMap<usize, &'a [u8]>, SpliceError> {
    check_request(replacements)?;
    let index: HashMap<&[u8], usize> = archive
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| (e.name(), i))
        .collect();
    let mut out = HashMap::with_capacity(replacements.len());
    for r in replacements {
        let Some(&i) = index.get(r.name.as_bytes()) else {
            return refuse(format!("the archive has no entry {:?}", r.name));
        };
        out.insert(i, r.data.as_slice());
    }
    Ok(out)
}

/// A deflated replacement, ready to be written.
struct Fresh {
    crc: u32,
    compressed: Vec<u8>,
    size: u32,
}

fn deflate(data: &[u8]) -> Result<Fresh, SpliceError> {
    let size = u32::try_from(data.len())
        .ok()
        .filter(|&s| s != 0xFFFF_FFFF)
        .ok_or_else(|| SpliceError::Refused("a replacement is too large".into()))?;
    let mut crc = flate2::Crc::new();
    crc.update(data);
    let mut encoder = flate2::write::DeflateEncoder::new(
        Vec::with_capacity(data.len() / 2),
        flate2::Compression::default(),
    );
    encoder.write_all(data)?;
    Ok(Fresh {
        crc: crc.sum(),
        compressed: encoder.finish()?,
        size,
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

/// Write `archive` (read from `r`) to `w` with `replacements` swapped in.
///
/// Before the central directory everything is copied byte for byte in
/// offset order — a prefix before the first record, and each untouched
/// record together with whatever follows it up to the next one — except a
/// replaced record, which becomes a fresh local header (no data descriptor,
/// deflate, the original flags without bit 3, time, date and name, no extra)
/// plus its deflated data. Each central record is copied with its local
/// offset patched (a replaced one also gets its version, flags, method, CRC
/// and sizes), and the end record with the directory's new size and offset.
pub(crate) fn splice<R: Read + Seek, W: Write>(
    r: &mut R,
    archive: &Archive,
    replacements: &[Replacement],
    w: W,
) -> Result<(), SpliceError> {
    let replaced = match_replacements(archive, replacements)?;
    let mut fresh: HashMap<usize, Fresh> = HashMap::with_capacity(replaced.len());
    for (&i, data) in &replaced {
        fresh.insert(i, deflate(data)?);
    }

    let mut out = Tally { inner: w, at: 0 };
    let first = archive
        .by_offset
        .first()
        .map_or(archive.cd_offset, |&i| archive.entries[i].local_offset);
    out.copy(r, 0, first)?;

    let mut offsets = vec![0u32; archive.entries.len()];
    for (pos, &i) in archive.by_offset.iter().enumerate() {
        let entry = &archive.entries[i];
        offsets[i] = out.offset32()?;
        if let Some(new) = fresh.get(&i) {
            let central = &entry.central;
            let name = entry.name();
            let mut header = [0u8; LOCAL_LEN];
            put_u32(&mut header, 0, LOCAL_SIG);
            put_u16(&mut header, 4, 20);
            put_u16(&mut header, 6, u16_at(central, 8) & !FLAG_DESCRIPTOR);
            put_u16(&mut header, 8, 8);
            put_u16(&mut header, 10, u16_at(central, 12));
            put_u16(&mut header, 12, u16_at(central, 14));
            put_u32(&mut header, 14, new.crc);
            put_u32(&mut header, 18, compressed_size(new)?);
            put_u32(&mut header, 22, new.size);
            put_u16(&mut header, 26, u16_at(central, 28));
            put_u16(&mut header, 28, 0);
            out.put(&header)?;
            out.put(name)?;
            out.put(&new.compressed)?;
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

    let cd_start = out.offset32()?;
    for (i, entry) in archive.entries.iter().enumerate() {
        let mut record = entry.central.clone();
        put_u32(&mut record, 42, offsets[i]);
        if let Some(new) = fresh.get(&i) {
            put_u16(&mut record, 6, 20);
            let flags = u16_at(&record, 8) & !FLAG_DESCRIPTOR;
            put_u16(&mut record, 8, flags);
            put_u16(&mut record, 10, 8);
            put_u32(&mut record, 16, new.crc);
            put_u32(&mut record, 20, compressed_size(new)?);
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
    let cd_size = u32::try_from(out.at - u64::from(cd_start))
        .map_err(|_| SpliceError::Refused("the edited archive would need zip64".into()))?;
    let mut eocd = archive.eocd.clone();
    put_u32(&mut eocd, 12, cd_size);
    put_u32(&mut eocd, 16, cd_start);
    out.put(&eocd)?;
    out.inner.flush()?;
    Ok(())
}

fn compressed_size(fresh: &Fresh) -> Result<u32, SpliceError> {
    u32::try_from(fresh.compressed.len())
        .ok()
        .filter(|&s| s != 0xFFFF_FFFF)
        .ok_or_else(|| SpliceError::Refused("a replacement is too large".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    use zip::write::SimpleFileOptions;
    use zip::CompressionMethod;

    const SLIDE: &[u8] = b"<p:sld><p:cSld><p:spTree/></p:cSld></p:sld>";

    fn options(method: CompressionMethod) -> SimpleFileOptions {
        SimpleFileOptions::default().compression_method(method)
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
}
