//! `native_view_drive`'s pure half: the actions an agent sends a pane view
//! (parsed and bounded here, before anything touches WebKit), the US key
//! map behind `key`/`text`, the injected-input ledger that tells the agent's
//! input from the user's (take-over), and the eval wrapper that turns a
//! page's throw into a message (`callAsyncJavaScript` reports only "A
//! JavaScript exception occurred"). `mac.rs` does the native half.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use serde::Deserialize;

/// A coordinate in CSS px of the view's viewport.
const COORD_MAX: f64 = 100_000.0;
/// `text` with `per_key`: paced keys cost ~12 ms each and the daemon waits
/// 15 s for an answer, so this many fit with room to spare.
pub(super) const PER_KEY_MAX: usize = 500;
/// `text` through `insertText:` (one call).
const TEXT_MAX: usize = 100_000;
/// An eval body's source.
const BODY_MAX: usize = 1024 * 1024;
/// An eval's `args`, as JSON.
const ARGS_MAX: usize = 2 * 1024 * 1024;
/// How long an eval may run.
pub(super) const EVAL_TIMEOUT_DEFAULT_MS: u64 = 10_000;
const EVAL_TIMEOUT_MAX_MS: u64 = 60_000;
/// An eval's JSON result (the wrapper refuses a larger one in the page).
pub(super) const RESULT_MAX: usize = 2 * 1024 * 1024;
/// An error message + stack from the page.
pub(super) const ERROR_MAX: usize = 2048;
/// Full-page screenshots grow the view to at most this many CSS px.
pub(super) const FULL_PAGE_MAX: f64 = 8000.0;

/// What `native_view_drive` does to one view (`type`-tagged JSON).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum DriveAction {
    /// Run `body` as an async function body; its locals are `args`' keys
    /// (and `args` itself, unless a key is named so).
    Eval {
        body: String,
        #[serde(default)]
        args: Option<serde_json::Value>,
        #[serde(default)]
        world: World,
        #[serde(default = "eval_timeout")]
        timeout_ms: u64,
    },
    Click {
        x: f64,
        y: f64,
        #[serde(default = "one")]
        clicks: u8,
        #[serde(default)]
        modifiers: Vec<Modifier>,
    },
    Move {
        x: f64,
        y: f64,
    },
    Drag {
        from: [f64; 2],
        to: [f64; 2],
        #[serde(default = "drag_steps")]
        steps: u16,
    },
    Scroll {
        x: f64,
        y: f64,
        #[serde(default)]
        dx: f64,
        #[serde(default)]
        dy: f64,
    },
    Key {
        key: String,
        #[serde(default)]
        modifiers: Vec<Modifier>,
    },
    Text {
        text: String,
        #[serde(default)]
        per_key: bool,
    },
    Screenshot {
        #[serde(default)]
        clip: Option<Clip>,
        #[serde(default)]
        full_page: bool,
        #[serde(default = "max_width")]
        max_width: u32,
    },
    Rings {
        #[serde(default)]
        console_since: u64,
        #[serde(default)]
        network_since: u64,
    },
    Instrument,
    Cursor {
        show: bool,
        /// `#rrggbb`: the agent's hue (default: the brand violet).
        #[serde(default)]
        color: Option<String>,
        /// A short name drawn by the cursor (the agent's).
        #[serde(default)]
        label: Option<String>,
    },
}

fn eval_timeout() -> u64 {
    EVAL_TIMEOUT_DEFAULT_MS
}
fn one() -> u8 {
    1
}
fn drag_steps() -> u16 {
    12
}
fn max_width() -> u32 {
    1600
}

/// Where an eval runs: the bridge's own content world (the page can't see
/// it) or the page's world (its globals, its framework objects).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum World {
    #[default]
    Bridge,
    Page,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Modifier {
    Shift,
    Meta,
    Ctrl,
    Alt,
}

/// A screenshot's region, CSS px of the viewport.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub(super) struct Clip {
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) width: f64,
    pub(super) height: f64,
}

/// Held modifier keys.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Mods {
    pub(super) shift: bool,
    pub(super) ctrl: bool,
    pub(super) alt: bool,
    pub(super) meta: bool,
}

impl Mods {
    pub(super) fn of(list: &[Modifier]) -> Self {
        let mut m = Mods::default();
        for modifier in list {
            match modifier {
                Modifier::Shift => m.shift = true,
                Modifier::Ctrl => m.ctrl = true,
                Modifier::Alt => m.alt = true,
                Modifier::Meta => m.meta = true,
            }
        }
        m
    }
}

fn bad(what: impl std::fmt::Display) -> String {
    format!("bad drive action: {what}")
}

fn coord(v: f64, name: &str) -> Result<(), String> {
    if v.is_finite() && v.abs() < COORD_MAX {
        Ok(())
    } else {
        Err(bad(format_args!("{name} is out of range")))
    }
}

/// A JS identifier an eval's `args` key may become: ASCII, not reserved,
/// not one of the wrapper's own names.
fn js_identifier(name: &str) -> bool {
    const RESERVED: &[&str] = &[
        "arguments",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "debugger",
        "default",
        "delete",
        "do",
        "else",
        "enum",
        "eval",
        "export",
        "extends",
        "false",
        "finally",
        "for",
        "function",
        "if",
        "implements",
        "import",
        "in",
        "instanceof",
        "interface",
        "let",
        "new",
        "null",
        "package",
        "private",
        "protected",
        "public",
        "return",
        "static",
        "super",
        "switch",
        "this",
        "throw",
        "true",
        "try",
        "typeof",
        "var",
        "void",
        "while",
        "with",
        "yield",
    ];
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == b'_' || first == b'$')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$')
        && name.len() <= 64
        && !RESERVED.contains(&name)
        && !name.starts_with("__chimaera")
}

/// Parse and bound one action. Everything the native half trusts is
/// checked here.
pub(super) fn parse(value: serde_json::Value) -> Result<DriveAction, String> {
    let mut action: DriveAction = serde_json::from_value(value).map_err(bad)?;
    match &mut action {
        DriveAction::Eval {
            body,
            args,
            timeout_ms,
            ..
        } => {
            if body.len() > BODY_MAX {
                return Err(bad("the script is too long"));
            }
            match args {
                None | Some(serde_json::Value::Null) => *args = None,
                Some(serde_json::Value::Object(map)) => {
                    if let Some(key) = map.keys().find(|k| !js_identifier(k)) {
                        return Err(bad(format_args!(
                            "args key {key:?} can't be a JavaScript variable name"
                        )));
                    }
                    if serde_json::to_string(map).map_or(true, |s| s.len() > ARGS_MAX) {
                        return Err(bad("args are too large"));
                    }
                }
                Some(_) => return Err(bad("args must be an object")),
            }
            *timeout_ms = (*timeout_ms).clamp(1, EVAL_TIMEOUT_MAX_MS);
        }
        DriveAction::Click { x, y, clicks, .. } => {
            coord(*x, "x")?;
            coord(*y, "y")?;
            if !(1..=3).contains(clicks) {
                return Err(bad("clicks must be 1, 2 or 3"));
            }
        }
        DriveAction::Move { x, y } => {
            coord(*x, "x")?;
            coord(*y, "y")?;
        }
        DriveAction::Drag { from, to, steps } => {
            for v in from.iter().chain(to.iter()) {
                coord(*v, "a drag point")?;
            }
            if !(1..=200).contains(steps) {
                return Err(bad("steps must be 1–200"));
            }
        }
        DriveAction::Scroll { x, y, dx, dy } => {
            coord(*x, "x")?;
            coord(*y, "y")?;
            coord(*dx, "dx")?;
            coord(*dy, "dy")?;
        }
        DriveAction::Key { key, modifiers } => {
            key_stroke(key, Mods::of(modifiers))?;
        }
        DriveAction::Text { text, per_key } => {
            let max = if *per_key { PER_KEY_MAX } else { TEXT_MAX };
            if text.chars().count() > max {
                return Err(bad(format_args!(
                    "text is longer than {max} characters{}",
                    if *per_key {
                        " (per_key: false types it at once)"
                    } else {
                        ""
                    }
                )));
            }
        }
        DriveAction::Screenshot {
            clip, max_width, ..
        } => {
            if let Some(c) = clip {
                for (v, name) in [(c.x, "clip.x"), (c.y, "clip.y")] {
                    coord(v, name)?;
                }
                if !(c.width.is_finite() && c.height.is_finite())
                    || c.width < 1.0
                    || c.height < 1.0
                    || c.width > COORD_MAX
                    || c.height > COORD_MAX
                {
                    return Err(bad("the clip is empty or out of range"));
                }
            }
            *max_width = (*max_width).clamp(64, 4096);
        }
        DriveAction::Cursor { color, label, .. } => {
            if let Some(c) = color {
                let hex = c.strip_prefix('#').unwrap_or("");
                if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(bad("color must be #rrggbb"));
                }
            }
            if let Some(l) = label {
                *l = l.chars().filter(|c| !c.is_control()).take(40).collect();
            }
        }
        DriveAction::Rings { .. } | DriveAction::Instrument => {}
    }
    Ok(action)
}

// --- keys ---------------------------------------------------------------------------

/// One key press as AppKit would make it: the virtual key code, the
/// characters, the characters with only Shift applied, and the modifiers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct KeyStroke {
    pub(super) code: u16,
    pub(super) chars: String,
    pub(super) ignoring: String,
    pub(super) mods: Mods,
}

/// A step of typed text: a key press, or text no US key makes (inserted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Stroke {
    Key(KeyStroke),
    Insert(String),
}

/// Named keys: (DOM name, macOS virtual key code, AppKit characters).
const NAMED: &[(&str, u16, &str)] = &[
    ("Enter", 36, "\r"),
    ("Tab", 48, "\t"),
    ("Space", 49, " "),
    ("Backspace", 51, "\u{7f}"),
    ("Escape", 53, "\u{1b}"),
    ("Delete", 117, "\u{f728}"),
    ("Home", 115, "\u{f729}"),
    ("End", 119, "\u{f72b}"),
    ("PageUp", 116, "\u{f72c}"),
    ("PageDown", 121, "\u{f72d}"),
    ("ArrowLeft", 123, "\u{f702}"),
    ("ArrowRight", 124, "\u{f703}"),
    ("ArrowDown", 125, "\u{f701}"),
    ("ArrowUp", 126, "\u{f700}"),
];

/// The US ANSI layout's unshifted characters → virtual key code.
fn us_code(c: char) -> Option<u16> {
    Some(match c {
        'a' => 0,
        's' => 1,
        'd' => 2,
        'f' => 3,
        'h' => 4,
        'g' => 5,
        'z' => 6,
        'x' => 7,
        'c' => 8,
        'v' => 9,
        'b' => 11,
        'q' => 12,
        'w' => 13,
        'e' => 14,
        'r' => 15,
        'y' => 16,
        't' => 17,
        '1' => 18,
        '2' => 19,
        '3' => 20,
        '4' => 21,
        '6' => 22,
        '5' => 23,
        '=' => 24,
        '9' => 25,
        '7' => 26,
        '-' => 27,
        '8' => 28,
        '0' => 29,
        ']' => 30,
        'o' => 31,
        'u' => 32,
        '[' => 33,
        'i' => 34,
        'p' => 35,
        'l' => 37,
        'j' => 38,
        '\'' => 39,
        'k' => 40,
        ';' => 41,
        '\\' => 42,
        ',' => 43,
        '/' => 44,
        'n' => 45,
        'm' => 46,
        '.' => 47,
        ' ' => 49,
        '`' => 50,
        _ => return None,
    })
}

/// US shifted symbols: (shifted, unshifted).
const SHIFTED: &[(char, char)] = &[
    ('!', '1'),
    ('@', '2'),
    ('#', '3'),
    ('$', '4'),
    ('%', '5'),
    ('^', '6'),
    ('&', '7'),
    ('*', '8'),
    ('(', '9'),
    (')', '0'),
    ('_', '-'),
    ('+', '='),
    ('{', '['),
    ('}', ']'),
    ('|', '\\'),
    (':', ';'),
    ('"', '\''),
    ('<', ','),
    ('>', '.'),
    ('?', '/'),
    ('~', '`'),
];

/// `(unshifted char, needs shift)` for a character on the US layout.
fn us_base(c: char) -> Option<(char, bool)> {
    if c.is_ascii_uppercase() {
        return Some((c.to_ascii_lowercase(), true));
    }
    if let Some(&(_, base)) = SHIFTED.iter().find(|(s, _)| *s == c) {
        return Some((base, true));
    }
    us_code(c).map(|_| (c, false))
}

fn shifted(base: char) -> char {
    if base.is_ascii_lowercase() {
        return base.to_ascii_uppercase();
    }
    SHIFTED
        .iter()
        .find(|(_, b)| *b == base)
        .map_or(base, |(s, _)| *s)
}

/// The press for `key` (a DOM key name or one character) with `mods`.
/// A character no US key makes, with no modifier held, is inserted as text.
pub(super) fn key_stroke(key: &str, mods: Mods) -> Result<Stroke, String> {
    let named = match key {
        "\n" | "\r" => Some(NAMED[0]),
        "\t" => Some(NAMED[1]),
        " " => Some(NAMED[2]),
        _ => NAMED.iter().find(|(name, _, _)| *name == key).copied(),
    };
    if let Some((_, code, chars)) = named {
        return Ok(Stroke::Key(KeyStroke {
            code,
            chars: chars.to_string(),
            ignoring: chars.to_string(),
            mods,
        }));
    }
    let mut it = key.chars();
    let (Some(c), None) = (it.next(), it.next()) else {
        return Err(bad(format_args!(
            "unknown key {key:?} (a key name such as Enter, or one character)"
        )));
    };
    let Some((base, needs_shift)) = us_base(c) else {
        if mods == Mods::default() {
            return Ok(Stroke::Insert(c.to_string()));
        }
        return Err(bad(format_args!("no key makes {c:?} with modifiers")));
    };
    let mods = Mods {
        shift: mods.shift || needs_shift,
        ..mods
    };
    let code = us_code(base).unwrap_or(0);
    // `charactersIgnoringModifiers` keeps Shift; `characters` also takes
    // Control (a control character, as a real ⌃-letter press makes).
    let with_shift = if mods.shift { shifted(base) } else { base };
    let chars = if mods.ctrl && base.is_ascii_lowercase() {
        char::from(base as u8 - b'a' + 1)
    } else {
        with_shift
    };
    Ok(Stroke::Key(KeyStroke {
        code,
        chars: chars.to_string(),
        ignoring: with_shift.to_string(),
        mods,
    }))
}

/// `text` as paced steps: one press per US character (newline = Enter,
/// tab = Tab), runs of anything else inserted at once.
pub(super) fn strokes(text: &str) -> Vec<Stroke> {
    let mut out: Vec<Stroke> = Vec::new();
    for c in text.chars() {
        let stroke = key_stroke(&c.to_string(), Mods::default())
            .unwrap_or_else(|_| Stroke::Insert(c.to_string()));
        match (out.last_mut(), stroke) {
            (Some(Stroke::Insert(run)), Stroke::Insert(more)) => run.push_str(&more),
            (_, stroke) => out.push(stroke),
        }
    }
    out
}

// --- take-over ----------------------------------------------------------------------

/// The trusted DOM input the bridge reports, by what the shell injected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InputKind {
    /// `pointerdown` (one per mouse-down: a double click is two).
    Pointer,
    /// `keydown` (one per key press; `insertText:` makes none).
    Key,
    /// `wheel` (the bridge reports at most one per 200 ms burst).
    Wheel,
}

impl InputKind {
    pub(super) fn of_report(dom_type: &str) -> Option<Self> {
        match dom_type {
            "pointerdown" => Some(Self::Pointer),
            "keydown" => Some(Self::Key),
            "wheel" => Some(Self::Wheel),
            _ => None,
        }
    }
}

/// How long an injected event waits for its report.
const INJECTED_FRESH: Duration = Duration::from_millis(1500);
/// A view's ledger never holds more than this (a drag storm, a stuck page).
const INJECTED_MAX: usize = 256;
/// At most one `user_input` event per view this often (a user scrolling or
/// typing in a shared pane reports many).
const USER_INPUT_EVERY: Duration = Duration::from_millis(500);
/// The user's input counts (`user_input`) only on a view the agent drove
/// this recently: the window takes over only for one driven in the last
/// 15 s, and every other pane view (a report the user reads) stays quiet.
const DRIVEN_RECENTLY: Duration = Duration::from_secs(60);

/// One view's injected input, oldest first, consumed by matching reports.
#[derive(Debug, Default)]
pub(super) struct Injections {
    pending: VecDeque<(InputKind, Instant)>,
    last_user: Option<Instant>,
    /// The agent's last action on this view.
    driven: Option<Instant>,
}

impl Injections {
    fn expire(&mut self, now: Instant) {
        while self
            .pending
            .front()
            .is_some_and(|(_, at)| now.saturating_duration_since(*at) > INJECTED_FRESH)
        {
            self.pending.pop_front();
        }
    }

    /// The agent acted on the view (any input, reported or not).
    pub(super) fn drove(&mut self, now: Instant) {
        self.driven = Some(now);
    }

    pub(super) fn push(&mut self, kind: InputKind, now: Instant) {
        self.drove(now);
        self.expire(now);
        if self.pending.len() >= INJECTED_MAX {
            self.pending.pop_front();
        }
        self.pending.push_back((kind, now));
    }

    /// A report of `kind` came in at `now`: true when it is the user's (no
    /// injection of that kind is waiting) and a `user_input` event is due.
    pub(super) fn report(&mut self, kind: InputKind, now: Instant) -> bool {
        self.expire(now);
        if let Some(i) = self.pending.iter().position(|(k, _)| *k == kind) {
            self.pending.remove(i);
            return false;
        }
        if !self
            .driven
            .is_some_and(|t| now.saturating_duration_since(t) < DRIVEN_RECENTLY)
        {
            return false;
        }
        if self
            .last_user
            .is_some_and(|t| now.saturating_duration_since(t) < USER_INPUT_EVERY)
        {
            return false;
        }
        self.last_user = Some(now);
        true
    }
}

// --- eval -----------------------------------------------------------------------------

/// The parameter list for an eval: each `args` key, then `args` itself
/// unless a key already has that name.
pub(super) fn eval_params(args: Option<&serde_json::Value>) -> Vec<String> {
    let mut names: Vec<String> = args
        .and_then(|a| a.as_object())
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    if !names.iter().any(|n| n == "args") {
        names.push("args".into());
    }
    names
}

/// The source `callAsyncJavaScript` runs for an eval. `body` is the
/// caller's own code (an async function body); the arguments dictionary
/// carries `__chimaeraArgs` (the args as JSON). A throw, a rejection, an
/// unserialisable or oversized result all come back as `{"ok":false,
/// "error":…}` with the error's message and stack; only a body that doesn't
/// parse reaches WebKit's opaque error (see `syntax_check_source`).
pub(super) fn eval_source(body: &str, params: &[String]) -> String {
    let list = params.join(", ");
    let values: String = params
        .iter()
        .map(|p| {
            if p == "args" {
                ", __chimaeraA".to_string()
            } else {
                format!(", __chimaeraA[{}]", serde_json::Value::from(p.as_str()))
            }
        })
        .collect();
    format!(
        "const __chimaeraErr = (e) => {{\n\
           let s;\n\
           try {{\n\
             // Duck-typed: an error from another realm (a frame) isn't `instanceof Error`.\n\
             if (e !== null && typeof e === 'object' && typeof e.message === 'string') {{\n\
               const head = `${{e.name || 'Error'}}: ${{e.message}}`;\n\
               const stack = typeof e.stack === 'string' ? e.stack : '';\n\
               s = stack === '' ? head : stack.startsWith(head) ? stack : head + '\\n' + stack;\n\
             }} else {{\n\
               s = 'Uncaught ' + (typeof e === 'string' ? e : JSON.stringify(e));\n\
             }}\n\
           }} catch (_) {{\n\
             try {{ s = 'Uncaught ' + String(e); }} catch (_) {{ s = 'Uncaught exception'; }}\n\
           }}\n\
           if (typeof s !== 'string') s = 'Uncaught exception';\n\
           return s.length > {ERROR_MAX} ? s.slice(0, {cut}) + '…' : s;\n\
         }};\n\
         const __chimaeraA = JSON.parse(__chimaeraArgs);\n\
         let __chimaeraV;\n\
         try {{\n\
           __chimaeraV = await (async function ({list}) {{\n\
         {body}\n\
           }}).call(undefined{values});\n\
         }} catch (e) {{\n\
           return JSON.stringify({{ ok: false, error: __chimaeraErr(e) }});\n\
         }}\n\
         let __chimaeraS;\n\
         try {{\n\
           __chimaeraS = JSON.stringify(__chimaeraV === undefined ? null : __chimaeraV);\n\
         }} catch (e) {{\n\
           return JSON.stringify({{ ok: false, error: 'the result is not JSON: ' + __chimaeraErr(e) }});\n\
         }}\n\
         if (__chimaeraS === undefined) __chimaeraS = 'null';\n\
         if (__chimaeraS.length > {RESULT_MAX}) {{\n\
           return JSON.stringify({{ ok: false, error: `the result is too large (${{__chimaeraS.length}} characters; the cap is {RESULT_MAX})` }});\n\
         }}\n\
         return '{{\"ok\":true,\"value\":' + __chimaeraS + '}}';\n",
        cut = ERROR_MAX - 1,
    )
}

/// Asked only after WebKit's opaque failure: compiles `__chimaeraBody` with
/// `__chimaeraParams` to name the syntax error. A page whose CSP forbids
/// `eval` answers with that refusal instead, which is still a message.
pub(super) const SYNTAX_CHECK_SOURCE: &str = "try {\n\
       const AsyncFunction = (async function () {}).constructor;\n\
       new AsyncFunction(__chimaeraParams, __chimaeraBody);\n\
       return '';\n\
     } catch (e) {\n\
       return String(e && e.name ? e.name + ': ' + e.message : e);\n\
     }";

/// The wrapper's answer → the command's: the value, or the page's error.
pub(super) fn eval_answer(text: &str) -> Result<serde_json::Value, String> {
    let parsed: serde_json::Value =
        serde_json::from_str(text).map_err(|_| "the page answered something else".to_string())?;
    match parsed.get("ok").and_then(|v| v.as_bool()) {
        Some(true) => Ok(parsed
            .get("value")
            .cloned()
            .unwrap_or(serde_json::Value::Null)),
        Some(false) => Err(parsed
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("the script failed")
            .to_string()),
        None => Err("the page answered something else".into()),
    }
}

// --- screenshots ----------------------------------------------------------------------

/// The PNG's width in px for a region `css_width` CSS px wide: one pixel
/// per CSS px, narrower when that exceeds `max_width`.
pub(super) fn shot_width(css_width: f64, max_width: u32) -> f64 {
    css_width.round().clamp(1.0, f64::from(max_width.max(1)))
}

/// `clip` within a `width`×`height` viewport, or `None` when they don't meet.
pub(super) fn clip_within(clip: Clip, width: f64, height: f64) -> Option<Clip> {
    let x0 = clip.x.max(0.0);
    let y0 = clip.y.max(0.0);
    let x1 = (clip.x + clip.width).min(width);
    let y1 = (clip.y + clip.height).min(height);
    (x1 - x0 >= 1.0 && y1 - y0 >= 1.0).then_some(Clip {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    })
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use serde_json::json;

    use super::*;

    #[test]
    fn actions_parse_with_their_defaults() {
        assert_eq!(
            parse(json!({"type": "click", "x": 10, "y": 20.5})).unwrap(),
            DriveAction::Click {
                x: 10.0,
                y: 20.5,
                clicks: 1,
                modifiers: vec![]
            }
        );
        assert_eq!(
            parse(json!({"type": "eval", "body": "return 1"})).unwrap(),
            DriveAction::Eval {
                body: "return 1".into(),
                args: None,
                world: World::Bridge,
                timeout_ms: 10_000
            }
        );
        assert_eq!(
            parse(json!({"type": "eval", "body": "", "world": "page", "timeout_ms": 999_999}))
                .unwrap(),
            DriveAction::Eval {
                body: String::new(),
                args: None,
                world: World::Page,
                timeout_ms: 60_000
            }
        );
        assert_eq!(
            parse(json!({"type": "drag", "from": [1, 2], "to": [3, 4]})).unwrap(),
            DriveAction::Drag {
                from: [1.0, 2.0],
                to: [3.0, 4.0],
                steps: 12
            }
        );
        assert_eq!(
            parse(json!({"type": "screenshot", "clip": null, "max_width": 10})).unwrap(),
            DriveAction::Screenshot {
                clip: None,
                full_page: false,
                max_width: 64
            }
        );
        assert_eq!(
            parse(json!({"type": "rings", "console_since": 4})).unwrap(),
            DriveAction::Rings {
                console_since: 4,
                network_since: 0
            }
        );
        assert_eq!(
            parse(json!({"type": "instrument"})).unwrap(),
            DriveAction::Instrument
        );
        assert_eq!(
            parse(json!({"type": "key", "key": "Enter", "modifiers": ["shift", "meta"]})).unwrap(),
            DriveAction::Key {
                key: "Enter".into(),
                modifiers: vec![Modifier::Shift, Modifier::Meta]
            }
        );
        assert_eq!(
            parse(json!({"type": "cursor", "show": true, "label": "Claude\u{7}"})).unwrap(),
            DriveAction::Cursor {
                show: true,
                color: None,
                label: Some("Claude".into())
            }
        );
    }

    #[test]
    fn bad_actions_are_refused_before_webkit() {
        for bad in [
            json!({"type": "rightclick", "x": 1, "y": 1}),
            json!({"type": "click", "x": 1}),
            json!({"type": "click", "x": f64::MAX, "y": 1}),
            json!({"type": "click", "x": 1, "y": 1, "clicks": 4}),
            json!({"type": "click", "x": 1, "y": 1, "modifiers": ["hyper"]}),
            json!({"type": "drag", "from": [0, 0], "to": [1, 1], "steps": 0}),
            json!({"type": "key", "key": "F13"}),
            json!({"type": "key", "key": "é", "modifiers": ["meta"]}),
            json!({"type": "text", "text": "x".repeat(PER_KEY_MAX + 1), "per_key": true}),
            json!({"type": "eval", "body": "1", "args": [1]}),
            json!({"type": "eval", "body": "1", "args": {"a-b": 1}}),
            json!({"type": "eval", "body": "1", "args": {"class": 1}}),
            json!({"type": "eval", "body": "1", "args": {"__chimaeraA": 1}}),
            json!({"type": "eval", "body": "1", "world": "main"}),
            json!({"type": "screenshot", "clip": {"x": 0, "y": 0, "width": 0, "height": 5}}),
            json!({"type": "cursor", "show": true, "color": "red"}),
            json!("click"),
        ] {
            assert!(parse(bad.clone()).is_err(), "{bad}");
        }
        // A non-US character types fine on its own.
        assert!(parse(json!({"type": "key", "key": "é"})).is_ok());
    }

    #[test]
    fn keys_map_to_us_presses() {
        let key = |k: &str, m: Mods| match key_stroke(k, m).unwrap() {
            Stroke::Key(s) => s,
            Stroke::Insert(t) => panic!("inserted {t}"),
        };
        let none = Mods::default();
        let enter = key("Enter", none);
        assert_eq!((enter.code, enter.chars.as_str()), (36, "\r"));
        assert_eq!(key("ArrowUp", none).chars, "\u{f700}");
        assert_eq!(key("\n", none).code, 36);
        let a = key("a", none);
        assert_eq!((a.code, a.chars.as_str(), a.mods), (0, "a", none));
        // An uppercase letter or a shifted symbol holds Shift itself.
        let cap = key("A", none);
        assert_eq!(
            (cap.code, cap.chars.as_str(), cap.mods.shift),
            (0, "A", true)
        );
        let bang = key("!", none);
        assert_eq!(
            (bang.code, bang.chars.as_str(), bang.ignoring.as_str()),
            (18, "!", "!")
        );
        assert!(bang.mods.shift);
        // Shift held on a base key shifts its characters.
        let shift = Mods {
            shift: true,
            ..none
        };
        assert_eq!(key("2", shift).chars, "@");
        // ⌃A is a control character; ⌘A is still "a".
        let ctrl = Mods { ctrl: true, ..none };
        let ca = key("a", ctrl);
        assert_eq!((ca.chars.as_str(), ca.ignoring.as_str()), ("\u{1}", "a"));
        let meta = Mods { meta: true, ..none };
        assert_eq!(key("a", meta).chars, "a");
        assert_eq!(key_stroke("é", none).unwrap(), Stroke::Insert("é".into()));
    }

    #[test]
    fn text_paces_us_keys_and_inserts_the_rest() {
        let s = strokes("Hi é漢\n");
        assert_eq!(s.len(), 5, "{s:?}");
        assert!(matches!(&s[0], Stroke::Key(k) if k.chars == "H" && k.mods.shift));
        assert!(matches!(&s[1], Stroke::Key(k) if k.chars == "i"));
        assert!(matches!(&s[2], Stroke::Key(k) if k.code == 49));
        assert_eq!(s[3], Stroke::Insert("é漢".into()));
        assert!(matches!(&s[4], Stroke::Key(k) if k.code == 36));
    }

    #[test]
    fn injected_input_is_matched_and_the_rest_is_the_user() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut ledger = Injections::default();
        // Never driven: the user's input there is nobody's business.
        assert!(!ledger.report(InputKind::Pointer, t0));
        // A double click: two pointerdowns, both the agent's.
        ledger.push(InputKind::Pointer, t0);
        ledger.push(InputKind::Pointer, t0);
        ledger.push(InputKind::Key, ms(5));
        assert!(!ledger.report(InputKind::Pointer, ms(10)));
        assert!(
            !ledger.report(InputKind::Key, ms(11)),
            "matched out of order"
        );
        assert!(!ledger.report(InputKind::Pointer, ms(12)));
        // Nothing waiting: the user.
        assert!(ledger.report(InputKind::Wheel, ms(20)));
        // …but one event per half second.
        assert!(!ledger.report(InputKind::Pointer, ms(300)));
        assert!(ledger.report(InputKind::Pointer, ms(600)));
        // An injection whose report never came expires.
        ledger.push(InputKind::Key, ms(1000));
        assert!(ledger.report(InputKind::Key, ms(2600)));
        // Long after the agent's last action the view is quiet again.
        assert!(!ledger.report(InputKind::Key, ms(70_000)));
        ledger.drove(ms(70_000));
        assert!(ledger.report(InputKind::Key, ms(70_001)));
        // A storm stays bounded.
        for _ in 0..1000 {
            ledger.push(InputKind::Wheel, ms(3000));
        }
        assert!(ledger.pending.len() <= INJECTED_MAX);
    }

    #[test]
    fn eval_wraps_args_and_errors() {
        let args = json!({"sel": "#a", "n": 2});
        let params = eval_params(Some(&args));
        assert_eq!(params, ["n", "sel", "args"]);
        let src = eval_source("return sel + n;", &params);
        assert!(src.contains("async function (n, sel, args)"));
        assert!(
            src.contains(".call(undefined, __chimaeraA[\"n\"], __chimaeraA[\"sel\"], __chimaeraA)")
        );
        assert!(src.contains("\nreturn sel + n;\n"));
        // A key named `args` wins over the whole object.
        assert_eq!(eval_params(Some(&json!({"args": 1}))), ["args"]);
        assert_eq!(eval_params(None), ["args"]);

        assert_eq!(
            eval_answer(r#"{"ok":true,"value":{"a":1}}"#).unwrap(),
            json!({"a": 1})
        );
        assert_eq!(
            eval_answer(r#"{"ok":true,"value":null}"#).unwrap(),
            json!(null)
        );
        assert_eq!(
            eval_answer(r#"{"ok":false,"error":"TypeError: x is undefined"}"#).unwrap_err(),
            "TypeError: x is undefined"
        );
        assert!(eval_answer("42").is_err());
    }

    #[test]
    fn screenshot_geometry() {
        assert_eq!(shot_width(1280.4, 1600), 1280.0);
        assert_eq!(shot_width(2400.0, 1600), 1600.0);
        let c = |x, y, width, height| Clip {
            x,
            y,
            width,
            height,
        };
        assert_eq!(
            clip_within(c(-10.0, 5.0, 50.0, 50.0), 100.0, 40.0),
            Some(c(0.0, 5.0, 40.0, 35.0))
        );
        assert_eq!(clip_within(c(200.0, 0.0, 10.0, 10.0), 100.0, 100.0), None);
    }
}
