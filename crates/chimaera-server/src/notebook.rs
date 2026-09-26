//! Jupyter notebook previews (`fs/notebook`): cells paged, outputs capped.
//!
//! The notebook is parsed on the blocking pool behind fs.rs's shared
//! filesystem limiter, and it streams: serde's visitor API walks the file so
//! only the requested page's cells are ever materialized — the rest of the
//! document (other cells, widget state in `metadata`) is skipped without
//! allocating. Memory stays bounded by the caps below, never by the file.

use std::collections::BTreeMap;
use std::fmt;
use std::io::BufReader;
use std::path::PathBuf;

use anyhow::Context;
use axum::extract::Query;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};

use crate::fs::blocking_response;

/// Largest `.ipynb` source `fs/notebook` parses. The walk is streaming, but
/// serde_json buffers one string at a time, so a single embedded payload can
/// cost up to this much transiently.
const MAX_NOTEBOOK_BYTES: u64 = 64 * 1024 * 1024;
/// Most cells one `fs/notebook` page returns.
const MAX_NOTEBOOK_CELLS: usize = 100;
/// Cells per page when the client does not say.
const DEFAULT_NOTEBOOK_CELLS: usize = 40;
/// Largest rich output payload (a base64 image, an HTML table) passed
/// through; a bigger one becomes a placeholder carrying its size.
const MAX_OUTPUT_PAYLOAD: usize = 8 * 1024 * 1024;
/// Largest text output (stream, traceback, text/plain…) passed through; the
/// rest is cut and the output marked truncated.
const MAX_OUTPUT_TEXT: usize = 200 * 1024;
/// Largest cell source passed through.
const MAX_CELL_SOURCE: usize = 1024 * 1024;
/// A page stops adding cells past this many payload bytes (it always carries
/// at least one), so a notebook of large figures pages by size, not count.
/// The response is built and serialized at once, so a page costs a few times
/// this in transient memory.
const MAX_NOTEBOOK_PAGE_BYTES: usize = 8 * 1024 * 1024;
/// One notebook parse at a time: each can transiently hold a page (8 MB)
/// plus one embedded string (up to the source cap), and the shared limiter
/// alone would admit eight at once. A parse is sub-second, so the queue is
/// short.
static NOTEBOOK_WORK: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

/// `~`-expanded, canonical, and a regular file (fs.rs keeps its own copy of
/// this resolution private).
fn canonical_file(raw: &str) -> anyhow::Result<PathBuf> {
    let expanded = match raw.strip_prefix("~/") {
        Some(rest) => home()?.join(rest),
        None if raw == "~" => home()?,
        None => PathBuf::from(raw),
    };
    let path = std::fs::canonicalize(&expanded).with_context(|| expanded.display().to_string())?;
    if !path.is_file() {
        anyhow::bail!("{} is not a file", path.display());
    }
    Ok(path)
}

fn home() -> anyhow::Result<PathBuf> {
    match std::env::var_os("HOME") {
        Some(home) if !home.is_empty() => Ok(PathBuf::from(home)),
        _ => anyhow::bail!("HOME is not set"),
    }
}

/// A JSON object from owned values. `json!` serializes each value by
/// reference — a deep copy of every multi-megabyte payload string — so the
/// page is assembled by moving instead.
fn object<const N: usize>(fields: [(&str, Value); N]) -> Value {
    Value::Object(
        fields
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
    )
}

#[derive(Deserialize)]
pub(crate) struct NotebookQuery {
    path: String,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    limit: Option<usize>,
}

/// GET /api/v1/fs/notebook?path=&offset=0&limit=40 — one page of a Jupyter
/// (nbformat 4) notebook: `{cells, offset, total, language, nbformat}`.
/// Each cell is `{index, cell_type, source, execution_count?, outputs?,
/// attachments?, truncated?}`; outputs are normalized (multi-line strings
/// joined) and capped — an oversize payload is listed under `omitted` with
/// its byte size instead. A page may hold fewer than `limit` cells when its
/// payload budget fills; the next page starts at `offset + cells.len()`.
pub(crate) async fn notebook(Query(query): Query<NotebookQuery>) -> Response {
    let limit = query
        .limit
        .unwrap_or(DEFAULT_NOTEBOOK_CELLS)
        .clamp(1, MAX_NOTEBOOK_CELLS);
    let _permit = NOTEBOOK_WORK
        .acquire()
        .await
        .expect("notebook semaphore is never closed");
    blocking_response(move || {
        read_notebook(&query.path, query.offset, limit, MAX_NOTEBOOK_PAGE_BYTES)
            .map(|body| Json(body).into_response())
    })
    .await
}

fn read_notebook(
    raw: &str,
    offset: usize,
    limit: usize,
    page_budget: usize,
) -> anyhow::Result<Value> {
    let path = canonical_file(raw)?;
    let file = std::fs::File::open(&path)
        .with_context(|| format!("{}: failed to open", path.display()))?;
    let size = file
        .metadata()
        .with_context(|| format!("{}: failed to stat", path.display()))?
        .len();
    if size > MAX_NOTEBOOK_BYTES {
        anyhow::bail!(
            "notebook is {} MB — over the {} MB preview cap",
            size / (1024 * 1024),
            MAX_NOTEBOOK_BYTES / (1024 * 1024)
        );
    }
    let mut page = Page {
        start: offset,
        end: offset.saturating_add(limit),
        budget: page_budget,
        used: 0,
        cells: Vec::new(),
        total: 0,
        full: false,
    };
    let mut de = serde_json::Deserializer::from_reader(BufReader::with_capacity(64 * 1024, file));
    let meta = RootSeed { page: &mut page }
        .deserialize(&mut de)
        .with_context(|| format!("{}: not a readable notebook", path.display()))?;
    de.end()
        .with_context(|| format!("{}: trailing data after the notebook", path.display()))?;
    if meta.worksheets {
        anyhow::bail!("nbformat 3 notebooks are not supported — upgrade it with `jupyter nbconvert --to notebook`");
    }
    Ok(object([
        ("cells", Value::Array(page.cells)),
        ("offset", json!(offset)),
        ("total", json!(page.total)),
        ("language", meta.language.map_or(Value::Null, Value::String)),
        ("nbformat", json!(meta.nbformat)),
    ]))
}

/// The page being collected while the cell array streams past.
struct Page {
    start: usize,
    end: usize,
    budget: usize,
    used: usize,
    cells: Vec<Value>,
    total: usize,
    /// The payload budget filled: later in-range cells are skipped.
    full: bool,
}

#[derive(Default)]
struct RootMeta {
    language: Option<String>,
    nbformat: Option<u64>,
    worksheets: bool,
}

struct RootSeed<'a> {
    page: &'a mut Page,
}

impl<'de> DeserializeSeed<'de> for RootSeed<'_> {
    type Value = RootMeta;

    fn deserialize<D: Deserializer<'de>>(self, de: D) -> Result<RootMeta, D::Error> {
        de.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for RootSeed<'_> {
    type Value = RootMeta;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a notebook object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<RootMeta, A::Error> {
        let mut meta = RootMeta::default();
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "cells" => map.next_value_seed(CellsSeed {
                    page: &mut *self.page,
                })?,
                "metadata" => {
                    let m: NotebookMetadata = map.next_value()?;
                    meta.language = m
                        .kernelspec
                        .and_then(|k| k.language)
                        .or_else(|| m.language_info.and_then(|l| l.name));
                }
                "nbformat" => meta.nbformat = map.next_value::<Option<u64>>()?,
                "worksheets" => {
                    meta.worksheets = true;
                    map.next_value::<IgnoredAny>()?;
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(meta)
    }
}

/// Only the two language hints; everything else in `metadata` (widget state
/// can be megabytes) is skipped unallocated.
#[derive(Deserialize)]
struct NotebookMetadata {
    #[serde(default)]
    kernelspec: Option<KernelSpec>,
    #[serde(default)]
    language_info: Option<LanguageInfo>,
}

#[derive(Deserialize)]
struct KernelSpec {
    #[serde(default)]
    language: Option<String>,
}

#[derive(Deserialize)]
struct LanguageInfo {
    #[serde(default)]
    name: Option<String>,
}

struct CellsSeed<'a> {
    page: &'a mut Page,
}

impl<'de> DeserializeSeed<'de> for CellsSeed<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, de: D) -> Result<(), D::Error> {
        de.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for CellsSeed<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an array of cells")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let page = self.page;
        loop {
            let index = page.total;
            let wanted = index >= page.start && index < page.end && !page.full;
            if wanted {
                let Some(cell) = seq.next_element_seed(CellSeed { index })? else {
                    break;
                };
                page.used = page.used.saturating_add(cell.bytes);
                page.cells.push(cell.json);
                if page.used >= page.budget {
                    page.full = true;
                }
            } else if seq.next_element::<IgnoredAny>()?.is_none() {
                break;
            }
            page.total += 1;
        }
        Ok(())
    }
}

/// One cell, already normalized to its wire shape, plus its payload bytes.
struct Cell {
    json: Value,
    bytes: usize,
}

struct CellSeed {
    index: usize,
}

impl<'de> DeserializeSeed<'de> for CellSeed {
    type Value = Cell;

    fn deserialize<D: Deserializer<'de>>(self, de: D) -> Result<Cell, D::Error> {
        de.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for CellSeed {
    type Value = Cell;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a notebook cell")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Cell, A::Error> {
        let mut cell_type = String::from("raw");
        let mut source = Capped::default();
        let mut execution_count: Option<i64> = None;
        let mut outputs: Vec<(Value, usize)> = Vec::new();
        let mut attachments = serde_json::Map::new();
        let mut bytes = 0usize;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "cell_type" => cell_type = map.next_value()?,
                "source" => {
                    source = map.next_value_seed(TextSeed {
                        cap: MAX_CELL_SOURCE,
                    })?
                }
                "execution_count" => execution_count = map.next_value::<Option<i64>>()?,
                "outputs" => outputs = map.next_value_seed(OutputsSeed)?,
                "attachments" => {
                    let bundles: BTreeMap<String, MimeBundle> = map.next_value()?;
                    for (name, bundle) in bundles {
                        let (data, omitted, size) = bundle.into_parts();
                        bytes = bytes.saturating_add(size);
                        attachments.insert(name, object([("data", data), ("omitted", omitted)]));
                    }
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        bytes = bytes.saturating_add(source.text.len());
        let is_code = cell_type == "code";
        let mut json = object([
            ("index", json!(self.index)),
            ("cell_type", Value::String(cell_type)),
            ("source", Value::String(source.text)),
        ]);
        let obj = json.as_object_mut().expect("built as an object");
        if source.truncated {
            obj.insert("truncated".into(), Value::Bool(true));
        }
        if is_code {
            obj.insert("execution_count".into(), json!(execution_count));
            let mut list = Vec::with_capacity(outputs.len());
            for (output, size) in outputs {
                bytes = bytes.saturating_add(size);
                list.push(output);
            }
            obj.insert("outputs".into(), Value::Array(list));
        }
        if !attachments.is_empty() {
            obj.insert("attachments".into(), Value::Object(attachments));
        }
        Ok(Cell { json, bytes })
    }
}

/// A multi-line notebook string (a string, or an array of strings to join)
/// cut at `cap` bytes; `total` is the uncut length.
#[derive(Default)]
struct Capped {
    text: String,
    total: usize,
    truncated: bool,
}

impl Capped {
    fn push(&mut self, piece: &str, cap: usize) {
        self.total = self.total.saturating_add(piece.len());
        if self.truncated {
            return;
        }
        let room = cap.saturating_sub(self.text.len());
        if piece.len() <= room {
            self.text.push_str(piece);
        } else {
            let mut cut = room;
            while !piece.is_char_boundary(cut) {
                cut -= 1;
            }
            self.text.push_str(&piece[..cut]);
            self.truncated = true;
        }
    }
}

struct TextSeed {
    cap: usize,
}

impl<'de> DeserializeSeed<'de> for TextSeed {
    type Value = Capped;

    fn deserialize<D: Deserializer<'de>>(self, de: D) -> Result<Capped, D::Error> {
        de.deserialize_any(TextVisitor {
            cap: self.cap,
            acc: Capped::default(),
        })
    }
}

struct TextVisitor {
    cap: usize,
    acc: Capped,
}

impl<'de> Visitor<'de> for TextVisitor {
    type Value = Capped;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a string or an array of strings")
    }

    fn visit_str<E: de::Error>(mut self, v: &str) -> Result<Capped, E> {
        self.acc.push(v, self.cap);
        Ok(self.acc)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Capped, E> {
        Ok(self.acc)
    }

    fn visit_seq<A: SeqAccess<'de>>(mut self, mut seq: A) -> Result<Capped, A::Error> {
        while seq
            .next_element_seed(Piece {
                acc: &mut self.acc,
                cap: self.cap,
            })?
            .is_some()
        {}
        Ok(self.acc)
    }

    // Anything else (a number where a line was expected) is skipped rather
    // than failing the whole notebook.
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Capped, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(self.acc)
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Capped, E> {
        Ok(self.acc)
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Capped, E> {
        Ok(self.acc)
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Capped, E> {
        Ok(self.acc)
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Capped, E> {
        Ok(self.acc)
    }
}

/// One element of a multi-line string array, appended in place.
struct Piece<'a> {
    acc: &'a mut Capped,
    cap: usize,
}

impl<'de> DeserializeSeed<'de> for Piece<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, de: D) -> Result<(), D::Error> {
        de.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Piece<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a line of text")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<(), E> {
        self.acc.push(v, self.cap);
        Ok(())
    }

    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(())
    }
}

/// Mime types a notebook output may carry that the viewer draws, richest
/// first. Only the richest present one is kept (plus `text/plain` as the
/// fallback); the rest never cross the tunnel.
const RICH_MIMES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/svg+xml",
    "text/html",
    "text/markdown",
    "text/latex",
];

fn mime_cap(mime: &str) -> usize {
    if mime.starts_with("image/") || mime == "text/html" {
        MAX_OUTPUT_PAYLOAD
    } else {
        MAX_OUTPUT_TEXT
    }
}

/// A display bundle (`data` of an output, or one attachment): the drawable
/// string-valued mimes, capped. Non-string values (widget state, vendor JSON)
/// are skipped unallocated.
#[derive(Default)]
struct MimeBundle {
    data: BTreeMap<String, Capped>,
    /// Mime → size of a payload over its cap.
    omitted: BTreeMap<String, usize>,
}

impl MimeBundle {
    /// The wire `data` / `omitted` maps (richest drawable mime + text/plain)
    /// and the payload bytes kept.
    fn into_parts(mut self) -> (Value, Value, usize) {
        let rich = RICH_MIMES
            .iter()
            .find(|m| self.data.contains_key(**m) || self.omitted.contains_key(**m))
            .copied();
        let mut data = serde_json::Map::new();
        let mut omitted = serde_json::Map::new();
        let mut bytes = 0usize;
        for mime in rich.into_iter().chain(std::iter::once("text/plain")) {
            if let Some(size) = self.omitted.remove(mime) {
                omitted.insert(mime.to_string(), json!(size));
            } else if let Some(text) = self.data.remove(mime) {
                bytes = bytes.saturating_add(text.text.len());
                if text.truncated {
                    // Only text mimes truncate (payloads are omitted whole).
                    data.insert(
                        mime.to_string(),
                        Value::String(format!("{}\n… [truncated]", text.text)),
                    );
                } else {
                    data.insert(mime.to_string(), Value::String(text.text));
                }
            }
        }
        (Value::Object(data), Value::Object(omitted), bytes)
    }
}

impl<'de> Deserialize<'de> for MimeBundle {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        de.deserialize_map(BundleVisitor)
    }
}

struct BundleVisitor;

impl<'de> Visitor<'de> for BundleVisitor {
    type Value = MimeBundle;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a mime bundle")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<MimeBundle, A::Error> {
        let mut bundle = MimeBundle::default();
        while let Some(mime) = map.next_key::<String>()? {
            if mime != "text/plain" && !RICH_MIMES.contains(&mime.as_str()) {
                map.next_value::<IgnoredAny>()?;
                continue;
            }
            let cap = mime_cap(&mime);
            let text = map.next_value_seed(TextSeed { cap })?;
            if text.truncated && cap == MAX_OUTPUT_PAYLOAD {
                // A cut image or document is useless: say how big it was.
                bundle.omitted.insert(mime, text.total);
            } else {
                bundle.data.insert(mime, text);
            }
        }
        Ok(bundle)
    }
}

struct OutputsSeed;

impl<'de> DeserializeSeed<'de> for OutputsSeed {
    type Value = Vec<(Value, usize)>;

    fn deserialize<D: Deserializer<'de>>(self, de: D) -> Result<Self::Value, D::Error> {
        de.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for OutputsSeed {
    type Value = Vec<(Value, usize)>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an array of outputs")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut out = Vec::new();
        while let Some(output) = seq.next_element_seed(OutputSeed)? {
            out.push(output);
        }
        Ok(out)
    }
}

struct OutputSeed;

impl<'de> DeserializeSeed<'de> for OutputSeed {
    type Value = (Value, usize);

    fn deserialize<D: Deserializer<'de>>(self, de: D) -> Result<Self::Value, D::Error> {
        de.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for OutputSeed {
    type Value = (Value, usize);

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a notebook output")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut output_type = String::new();
        let mut name: Option<String> = None;
        let mut text = Capped::default();
        let mut ename: Option<String> = None;
        let mut evalue = Capped::default();
        let mut traceback = Capped::default();
        let mut execution_count: Option<i64> = None;
        let mut bundle: Option<MimeBundle> = None;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "output_type" => output_type = map.next_value()?,
                "name" => name = map.next_value()?,
                "text" => {
                    text = map.next_value_seed(TextSeed {
                        cap: MAX_OUTPUT_TEXT,
                    })?
                }
                "ename" => ename = map.next_value()?,
                "evalue" => {
                    evalue = map.next_value_seed(TextSeed {
                        cap: MAX_OUTPUT_TEXT,
                    })?
                }
                "traceback" => traceback = map.next_value_seed(TracebackSeed)?,
                "execution_count" => execution_count = map.next_value::<Option<i64>>()?,
                "data" => bundle = Some(map.next_value()?),
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        let kind = output_type.clone();
        let (json, bytes) = match kind.as_str() {
            "stream" => {
                let bytes = text.text.len();
                (
                    object([
                        ("output_type", Value::String(output_type)),
                        (
                            "name",
                            Value::String(name.unwrap_or_else(|| "stdout".into())),
                        ),
                        ("text", Value::String(text.text)),
                        ("truncated", Value::Bool(text.truncated)),
                    ]),
                    bytes,
                )
            }
            "error" => {
                let bytes = traceback.text.len() + evalue.text.len();
                (
                    object([
                        ("output_type", Value::String(output_type)),
                        ("ename", Value::String(ename.unwrap_or_default())),
                        ("evalue", Value::String(evalue.text)),
                        ("traceback", Value::String(traceback.text)),
                        (
                            "truncated",
                            Value::Bool(traceback.truncated || evalue.truncated),
                        ),
                    ]),
                    bytes,
                )
            }
            "display_data" | "execute_result" | "update_display_data" => {
                let (data, omitted, bytes) = bundle.unwrap_or_default().into_parts();
                let result = output_type == "execute_result";
                let mut out = object([
                    ("output_type", Value::String(output_type)),
                    ("data", data),
                    ("omitted", omitted),
                ]);
                if result {
                    out["execution_count"] = json!(execution_count);
                }
                (out, bytes)
            }
            _ => (object([("output_type", Value::String(output_type))]), 0),
        };
        Ok((json, bytes))
    }
}

/// A traceback: an array of lines joined with newlines, capped.
struct TracebackSeed;

impl<'de> DeserializeSeed<'de> for TracebackSeed {
    type Value = Capped;

    fn deserialize<D: Deserializer<'de>>(self, de: D) -> Result<Capped, D::Error> {
        de.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for TracebackSeed {
    type Value = Capped;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a traceback")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Capped, E> {
        let mut acc = Capped::default();
        acc.push(v, MAX_OUTPUT_TEXT);
        Ok(acc)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Capped, E> {
        Ok(Capped::default())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Capped, A::Error> {
        let mut acc = Capped::default();
        let mut first = true;
        loop {
            if !first {
                // The separator is pushed before we know a line follows;
                // trimmed below when the array ends.
                acc.push("\n", MAX_OUTPUT_TEXT);
            }
            let seed = Piece {
                acc: &mut acc,
                cap: MAX_OUTPUT_TEXT,
            };
            if seq.next_element_seed(seed)?.is_none() {
                if !first && acc.text.ends_with('\n') {
                    acc.text.pop();
                    acc.total = acc.total.saturating_sub(1);
                }
                break;
            }
            first = false;
        }
        Ok(acc)
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn capped_text_cuts_on_a_char_boundary_and_counts_the_rest() {
        let mut acc = Capped::default();
        acc.push("héllo", 2);
        acc.push(" world", 2);
        assert_eq!(acc.text, "h");
        assert!(acc.truncated);
        assert_eq!(acc.total, "héllo world".len());
    }
}
