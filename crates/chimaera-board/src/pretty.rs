//! Byte-stable JSON formatting.
//!
//! A board must read like prose under `git diff` and a semantically identical
//! save must be byte-identical, so the exact bytes are part of the format.
//! `serde_json::to_string_pretty` gets two things wrong for this: it explodes
//! every array onto one element per line (turning `"at": [80, 130]` into four
//! lines and a 40-object page into a wall), and it writes an integral `f64` as
//! `960.0`, so a hand-written `960` churns on first save.
//!
//! Reformatting the compact output — rather than implementing
//! `serde_json::ser::Formatter` — is deliberate. The inline-vs-expanded
//! decision needs to know whether an array holds only scalars, which is
//! lookahead the `Formatter` trait cannot express: its callbacks are told a
//! value *began*, never what is coming. Enabling serde_json's `preserve_order`
//! instead would leak an insertion-ordered `Map` into every other crate in the
//! workspace through feature unification, which is a wire-visible change to
//! the daemon made as a side effect of a formatting choice.
//!
//! Four rules, applied recursively:
//!
//! - An array whose elements are all scalars stays on one line —
//!   `"at": [80, 130]`, `"spines": ["left", "bottom"]`.
//! - Any other array gets **one element per line, always** — never inlined,
//!   however small, because a page's `objects` order is z-order and a chart's
//!   `values` are rows: element-per-line is what makes a reorder or an edit
//!   read as a line move under `git diff`.
//! - A nested object small enough to read at a glance (compact form within
//!   [`BUDGET`] bytes) stays on one line — `"canvas": { "preset": "talk-16x9",
//!   "size": [960, 540] }`; anything larger goes one property per line.
//! - The root object always expands: a board file has its multi-line spine
//!   even when it is nearly empty.

const INDENT: &str = "  ";

/// The inline ceiling for a nested object, in bytes of its compact rendering.
/// Part of the format: changing it rewrites every board on next save.
const BUDGET: usize = 100;

/// Reformat compact JSON into Board's canonical layout.
///
/// The input must be well-formed compact JSON as produced by
/// `serde_json::to_string`; this is a formatter, not a validator.
pub fn pretty(compact: &str) -> String {
    let b = compact.as_bytes();
    let mut out = String::with_capacity(compact.len() * 2);
    let mut i = 0;
    write_value(b, &mut i, 0, &mut out);
    out.push('\n');
    out
}

/// Advance past whitespace. serde_json's compact output has none, but tests
/// and hand-fed input can, and skipping is cheaper than trusting.
fn skip_ws(b: &[u8], i: &mut usize) {
    while *i < b.len() && (b[*i] as char).is_ascii_whitespace() {
        *i += 1;
    }
}

/// The end index (exclusive) of the value starting at `i`, without emitting.
fn scan_value(b: &[u8], i: usize) -> usize {
    let mut i = i;
    skip_ws(b, &mut i);
    match b.get(i) {
        Some(b'"') => scan_string(b, i),
        Some(b'{') | Some(b'[') => {
            let open = b[i];
            let close = if open == b'{' { b'}' } else { b']' };
            let mut depth = 0usize;
            while i < b.len() {
                match b[i] {
                    b'"' => {
                        i = scan_string(b, i);
                        continue;
                    }
                    c if c == open => depth += 1,
                    c if c == close => {
                        depth -= 1;
                        i += 1;
                        if depth == 0 {
                            return i;
                        }
                        continue;
                    }
                    _ => {}
                }
                i += 1;
            }
            i
        }
        // A number or a bare literal runs until a structural delimiter.
        _ => {
            while i < b.len() && !matches!(b[i], b',' | b'}' | b']') {
                i += 1;
            }
            i
        }
    }
}

/// The end index (exclusive) of the string starting at `i`, honouring escapes
/// so a `}` or `"` inside a string never ends a scan early.
fn scan_string(b: &[u8], i: usize) -> usize {
    debug_assert_eq!(b[i], b'"');
    let mut i = i + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return i + 1,
            _ => i += 1,
        }
    }
    i
}

/// The immediate children of the container at `i`, as (start, end) spans.
fn children(b: &[u8], i: usize) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut j = i + 1; // past the opening brace/bracket
    let is_object = b[i] == b'{';
    loop {
        skip_ws(b, &mut j);
        if j >= b.len() || matches!(b[j], b'}' | b']') {
            return spans;
        }
        if is_object {
            // Skip the key and its colon; the value is what we classify.
            j = scan_string(b, j);
            skip_ws(b, &mut j);
            j += 1; // ':'
            skip_ws(b, &mut j);
        }
        let end = scan_value(b, j);
        spans.push((j, end));
        j = end;
        skip_ws(b, &mut j);
        if j < b.len() && b[j] == b',' {
            j += 1;
        }
    }
}

/// Are all of this array's elements scalars?
fn all_scalar(b: &[u8], i: usize) -> bool {
    children(b, i).iter().all(|(start, _)| {
        let mut s = *start;
        skip_ws(b, &mut s);
        !matches!(b.get(s), Some(b'{') | Some(b'['))
    })
}

/// Emit a value compactly, on one line, with readable spacing.
fn write_compact(b: &[u8], i: &mut usize, out: &mut String) {
    skip_ws(b, i);
    match b.get(*i) {
        Some(b'{') => {
            let kids = children(b, *i);
            if kids.is_empty() {
                out.push_str("{}");
                *i = scan_value(b, *i);
                return;
            }
            out.push_str("{ ");
            let mut j = *i + 1;
            for (n, _) in kids.iter().enumerate() {
                if n > 0 {
                    out.push_str(", ");
                }
                skip_ws(b, &mut j);
                let key_end = scan_string(b, j);
                out.push_str(&compact_str(b, j, key_end));
                out.push_str(": ");
                j = key_end;
                skip_ws(b, &mut j);
                j += 1; // ':'
                write_compact(b, &mut j, out);
                skip_ws(b, &mut j);
                if j < b.len() && b[j] == b',' {
                    j += 1;
                }
            }
            out.push_str(" }");
            *i = scan_value(b, *i);
        }
        Some(b'[') => {
            let kids = children(b, *i);
            if kids.is_empty() {
                out.push_str("[]");
                *i = scan_value(b, *i);
                return;
            }
            out.push('[');
            let mut j = *i + 1;
            for n in 0..kids.len() {
                if n > 0 {
                    out.push_str(", ");
                }
                write_compact(b, &mut j, out);
                skip_ws(b, &mut j);
                if j < b.len() && b[j] == b',' {
                    j += 1;
                }
            }
            out.push(']');
            *i = scan_value(b, *i);
        }
        _ => {
            let end = scan_value(b, *i);
            out.push_str(&scalar_str(b, *i, end));
            *i = end;
        }
    }
}

fn write_value(b: &[u8], i: &mut usize, depth: usize, out: &mut String) {
    skip_ws(b, i);
    match b.get(*i) {
        Some(b'{') => {
            // A nested object small enough to read at a glance stays inline.
            // The root never does — a board file keeps its multi-line spine.
            if depth > 0 {
                let mut probe = String::new();
                let mut j = *i;
                write_compact(b, &mut j, &mut probe);
                if probe.len() <= BUDGET {
                    out.push_str(&probe);
                    *i = j;
                    return;
                }
            }
            write_object(b, i, depth, out);
        }
        Some(b'[') => {
            if all_scalar(b, *i) {
                write_compact(b, i, out);
            } else {
                write_array(b, i, depth, out);
            }
        }
        _ => {
            let end = scan_value(b, *i);
            out.push_str(&scalar_str(b, *i, end));
            *i = end;
        }
    }
}

fn write_object(b: &[u8], i: &mut usize, depth: usize, out: &mut String) {
    let kids = children(b, *i);
    if kids.is_empty() {
        out.push_str("{}");
        *i = scan_value(b, *i);
        return;
    }
    out.push_str("{\n");
    let mut j = *i + 1;
    for n in 0..kids.len() {
        for _ in 0..=depth {
            out.push_str(INDENT);
        }
        skip_ws(b, &mut j);
        let key_end = scan_string(b, j);
        out.push_str(&compact_str(b, j, key_end));
        out.push_str(": ");
        j = key_end;
        skip_ws(b, &mut j);
        j += 1; // ':'
        write_value(b, &mut j, depth + 1, out);
        skip_ws(b, &mut j);
        if j < b.len() && b[j] == b',' {
            j += 1;
        }
        if n + 1 < kids.len() {
            out.push(',');
        }
        out.push('\n');
    }
    for _ in 0..depth {
        out.push_str(INDENT);
    }
    out.push('}');
    *i = scan_value(b, *i);
}

/// A container array: one element per line, always. Each element is written
/// through [`write_value`], so a small object row (a chart's `values`) inlines
/// while a full-size object (a page's `objects`) expands — but the *elements*
/// never share a line, because their order is data (z-order, row order) and a
/// reorder must diff as a line move.
fn write_array(b: &[u8], i: &mut usize, depth: usize, out: &mut String) {
    let kids = children(b, *i);
    if kids.is_empty() {
        out.push_str("[]");
        *i = scan_value(b, *i);
        return;
    }
    out.push_str("[\n");
    let mut j = *i + 1;
    for n in 0..kids.len() {
        for _ in 0..=depth {
            out.push_str(INDENT);
        }
        write_value(b, &mut j, depth + 1, out);
        skip_ws(b, &mut j);
        if j < b.len() && b[j] == b',' {
            j += 1;
        }
        if n + 1 < kids.len() {
            out.push(',');
        }
        out.push('\n');
    }
    for _ in 0..depth {
        out.push_str(INDENT);
    }
    out.push(']');
    *i = scan_value(b, *i);
}

fn compact_str(b: &[u8], start: usize, end: usize) -> String {
    String::from_utf8_lossy(&b[start..end]).into_owned()
}

/// Render a scalar token, trimming an integral `f64`'s `.0`.
///
/// serde_json writes the `f64` 960.0 as `960.0`, so a hand-authored `960`
/// would churn on first save. Points are whole numbers far more often than
/// not, and the trim is idempotent: reparsing `960` yields the same `f64`.
/// Guarded at 2^53 because past that an `f64` no longer round-trips through
/// its integer rendering.
fn scalar_str(b: &[u8], start: usize, end: usize) -> String {
    let raw = compact_str(b, start, end);
    let t = raw.trim();
    if let Some(stripped) = t.strip_suffix(".0") {
        if let Ok(v) = stripped.parse::<f64>() {
            if v.abs() < 9_007_199_254_740_992.0 && v.fract() == 0.0 {
                return stripped.to_string();
            }
        }
    }
    t.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_arrays_stay_inline() {
        let out = pretty(r#"{"at":[80.0,130.0],"spines":["left","bottom"]}"#);
        assert_eq!(
            out,
            "{\n  \"at\": [80, 130],\n  \"spines\": [\"left\", \"bottom\"]\n}\n"
        );
    }

    #[test]
    fn flat_object_arrays_get_one_row_per_line() {
        let out = pretty(r#"{"values":[{"a":1,"b":2},{"a":3,"b":4}]}"#);
        assert_eq!(
            out,
            "{\n  \"values\": [\n    { \"a\": 1, \"b\": 2 },\n    { \"a\": 3, \"b\": 4 }\n  ]\n}\n"
        );
    }

    #[test]
    fn small_nested_objects_inline_but_array_elements_get_their_own_line() {
        // The page object is tiny, so it inlines — but it still sits on its
        // own line inside `pages`, because array order is data.
        let out = pretty(r#"{"pages":[{"objects":[{"id":"a"}]}]}"#);
        assert_eq!(
            out,
            "{\n  \"pages\": [\n    { \"objects\": [{ \"id\": \"a\" }] }\n  ]\n}\n"
        );
    }

    #[test]
    fn large_objects_expand_one_property_per_line() {
        let long = "x".repeat(120);
        let out = pretty(&format!(r#"{{"o":{{"a":"{long}","b":1}}}}"#));
        assert_eq!(
            out,
            format!("{{\n  \"o\": {{\n    \"a\": \"{long}\",\n    \"b\": 1\n  }}\n}}\n")
        );
    }

    #[test]
    fn the_root_never_inlines() {
        assert_eq!(pretty(r#"{"a":1}"#), "{\n  \"a\": 1\n}\n");
    }

    #[test]
    fn empty_containers_stay_tight() {
        assert_eq!(
            pretty(r#"{"a":{},"b":[]}"#),
            "{\n  \"a\": {},\n  \"b\": []\n}\n"
        );
    }

    #[test]
    fn braces_inside_strings_do_not_end_a_scan() {
        let out = pretty(r#"{"t":"a } b [ c \" d","n":1}"#);
        assert_eq!(out, "{\n  \"t\": \"a } b [ c \\\" d\",\n  \"n\": 1\n}\n");
    }

    #[test]
    fn integral_floats_lose_their_tail_but_real_ones_do_not() {
        let out = pretty(r#"{"a":960.0,"b":1.5,"c":-0.0,"d":1e21}"#);
        assert_eq!(
            out,
            "{\n  \"a\": 960,\n  \"b\": 1.5,\n  \"c\": -0,\n  \"d\": 1e21\n}\n"
        );
    }

    #[test]
    fn formatting_is_idempotent() {
        // Reformatting an already-formatted board must not move a byte, or
        // `git status` lies the second time one is opened. Fed back through
        // itself rather than through `serde_json::Value`, whose `Map` is a
        // `BTreeMap` and would sort the very keys this format pins by
        // declaration order.
        let src = r#"{"pages":[{"objects":[{"id":"a","at":[80.0,130.0]}]}],"n":[1,2]}"#;
        let once = pretty(src);
        assert_eq!(pretty(&once), once);
    }
}
