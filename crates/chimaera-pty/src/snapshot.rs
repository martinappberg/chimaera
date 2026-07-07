//! Snapshot rendering: serialize a headless `Term`'s full state into an ANSI
//! escape stream that reconstructs it inside a fresh xterm.js terminal of the
//! same size.
//!
//! Layout of the stream:
//!   1. `ESC c` full reset (RIS)
//!   2. `CSI ? 1049 h` if the session is on the alternate screen
//!   3. every buffer line, oldest scrollback line first, with minimal SGR
//!      transitions; hard line breaks are `CR LF`, soft-wrapped lines are
//!      emitted at full width so the receiving terminal re-wraps naturally
//!      (no `CR LF` after the last visible row)
//!   4. `SGR 0`, window title (OSC 2), re-enabled private modes
//!      (DECCKM / bracketed paste), cursor position (CUP), the pending SGR
//!      state at the cursor, and cursor visibility (DECTCEM)

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor};

/// Style flags that affect SGR output.
const STYLE_FLAGS: Flags = Flags::BOLD
    .union(Flags::DIM)
    .union(Flags::ITALIC)
    .union(Flags::UNDERLINE)
    .union(Flags::DOUBLE_UNDERLINE)
    .union(Flags::UNDERCURL)
    .union(Flags::DOTTED_UNDERLINE)
    .union(Flags::DASHED_UNDERLINE)
    .union(Flags::INVERSE)
    .union(Flags::HIDDEN)
    .union(Flags::STRIKEOUT);

/// The SGR-relevant part of a cell's attributes.
#[derive(Clone, PartialEq)]
struct SgrState {
    fg: Color,
    bg: Color,
    flags: Flags,
}

impl SgrState {
    fn default_state() -> Self {
        SgrState {
            fg: Color::Named(NamedColor::Foreground),
            bg: Color::Named(NamedColor::Background),
            flags: Flags::empty(),
        }
    }

    fn of_cell(cell: &Cell) -> Self {
        SgrState {
            fg: cell.fg,
            bg: cell.bg,
            flags: cell.flags & STYLE_FLAGS,
        }
    }

    fn is_default(&self) -> bool {
        *self == Self::default_state()
    }

    /// Emit `CSI 0 ; ... m` establishing this state from any previous one.
    fn emit(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"\x1b[0");
        if self.flags.contains(Flags::BOLD) {
            out.extend_from_slice(b";1");
        }
        if self.flags.contains(Flags::DIM) {
            out.extend_from_slice(b";2");
        }
        if self.flags.contains(Flags::ITALIC) {
            out.extend_from_slice(b";3");
        }
        if self.flags.intersects(
            Flags::UNDERLINE | Flags::UNDERCURL | Flags::DOTTED_UNDERLINE | Flags::DASHED_UNDERLINE,
        ) {
            out.extend_from_slice(b";4");
        }
        if self.flags.contains(Flags::DOUBLE_UNDERLINE) {
            out.extend_from_slice(b";21");
        }
        if self.flags.contains(Flags::INVERSE) {
            out.extend_from_slice(b";7");
        }
        if self.flags.contains(Flags::HIDDEN) {
            out.extend_from_slice(b";8");
        }
        if self.flags.contains(Flags::STRIKEOUT) {
            out.extend_from_slice(b";9");
        }
        emit_color(out, self.fg, true);
        emit_color(out, self.bg, false);
        out.push(b'm');
    }
}

/// Append `;<sgr color params>` for a foreground (`fg = true`) or background
/// color. Default fg/bg emit nothing (SGR 0 already restored them).
fn emit_color(out: &mut Vec<u8>, color: Color, fg: bool) {
    let (named_base, bright_base, extended) = if fg {
        (30u16, 90u16, b"38")
    } else {
        (40u16, 100u16, b"48")
    };
    match color {
        Color::Named(named) => {
            let code = match named {
                NamedColor::Black => Some(named_base),
                NamedColor::Red => Some(named_base + 1),
                NamedColor::Green => Some(named_base + 2),
                NamedColor::Yellow => Some(named_base + 3),
                NamedColor::Blue => Some(named_base + 4),
                NamedColor::Magenta => Some(named_base + 5),
                NamedColor::Cyan => Some(named_base + 6),
                NamedColor::White => Some(named_base + 7),
                NamedColor::BrightBlack => Some(bright_base),
                NamedColor::BrightRed => Some(bright_base + 1),
                NamedColor::BrightGreen => Some(bright_base + 2),
                NamedColor::BrightYellow => Some(bright_base + 3),
                NamedColor::BrightBlue => Some(bright_base + 4),
                NamedColor::BrightMagenta => Some(bright_base + 5),
                NamedColor::BrightCyan => Some(bright_base + 6),
                NamedColor::BrightWhite => Some(bright_base + 7),
                // Dim variants: the DIM flag carries the dimming; map to the
                // base color.
                NamedColor::DimBlack => Some(named_base),
                NamedColor::DimRed => Some(named_base + 1),
                NamedColor::DimGreen => Some(named_base + 2),
                NamedColor::DimYellow => Some(named_base + 3),
                NamedColor::DimBlue => Some(named_base + 4),
                NamedColor::DimMagenta => Some(named_base + 5),
                NamedColor::DimCyan => Some(named_base + 6),
                NamedColor::DimWhite => Some(named_base + 7),
                // Foreground/Background are the defaults restored by SGR 0;
                // the remaining specials (Cursor, DimForeground, ...) have no
                // portable SGR encoding.
                _ => None,
            };
            if let Some(code) = code {
                out.extend_from_slice(format!(";{code}").as_bytes());
            }
        }
        Color::Indexed(idx) => {
            out.push(b';');
            out.extend_from_slice(extended);
            out.extend_from_slice(format!(";5;{idx}").as_bytes());
        }
        Color::Spec(rgb) => {
            out.push(b';');
            out.extend_from_slice(extended);
            out.extend_from_slice(format!(";2;{};{};{}", rgb.r, rgb.g, rgb.b).as_bytes());
        }
    }
}

/// True if a trailing cell can be omitted without changing what a fresh
/// terminal would show: a plain space with default background and no
/// attributes that make a space visible.
fn is_trim_blank(cell: &Cell) -> bool {
    cell.c == ' '
        && cell.zerowidth().is_none()
        && cell.bg == Color::Named(NamedColor::Background)
        && !cell.flags.intersects(
            Flags::INVERSE
                | Flags::UNDERLINE
                | Flags::DOUBLE_UNDERLINE
                | Flags::UNDERCURL
                | Flags::DOTTED_UNDERLINE
                | Flags::DASHED_UNDERLINE
                | Flags::STRIKEOUT,
        )
}

/// Render the terminal's content as plain text: the last `last_n` logical
/// lines (soft-wrapped rows joined), scrollback included, trailing blank
/// lines dropped. What a human reading the screen sees — for agents
/// inspecting a terminal whose shell emits no journal marks.
pub(crate) fn screen_text<T>(term: &Term<T>, last_n: usize) -> String {
    let grid = term.grid();
    let alt_screen = term.mode().contains(TermMode::ALT_SCREEN);
    let cols = grid.columns();
    let screen_lines = grid.screen_lines() as i32;
    let history = if alt_screen {
        0
    } else {
        grid.history_size() as i32
    };
    // Wrapped rows join into one logical line, so overshoot the scan window
    // rather than counting exactly; the tail cut below makes it precise.
    let margin = (last_n as i32).saturating_mul(2).saturating_add(screen_lines);
    let first_line = (-history).max(screen_lines - 1 - margin);

    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for line in first_line..screen_lines {
        let row = &grid[Line(line)];
        let wrapped = row[Column(cols - 1)].flags.contains(Flags::WRAPLINE);
        let mut len = row.len();
        if !wrapped {
            while len > 0 && is_trim_blank(&row[Column(len - 1)]) {
                len -= 1;
            }
        }
        for col in 0..len {
            let cell = &row[Column(col)];
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            if cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
                current.push(' ');
            } else {
                current.push(cell.c);
            }
            if let Some(zerowidth) = cell.zerowidth() {
                current.extend(zerowidth.iter());
            }
        }
        if !wrapped {
            lines.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let skip = lines.len().saturating_sub(last_n);
    lines[skip..].join("\n")
}

/// Render the full state of `term` as an escape stream (see module docs).
///
/// `title` is passed separately because `Term` does not expose its window
/// title; the session tracks it via the event listener.
pub(crate) fn render_snapshot<T>(term: &Term<T>, title: Option<&str>) -> Vec<u8> {
    let grid = term.grid();
    let mode = term.mode();
    let alt_screen = mode.contains(TermMode::ALT_SCREEN);
    let cols = grid.columns();
    let screen_lines = grid.screen_lines() as i32;

    let mut out: Vec<u8> = Vec::with_capacity(4096);

    // Full reset, so the stream is self-contained on any fresh terminal.
    out.extend_from_slice(b"\x1bc");

    // While the alternate screen is active the primary scrollback is not
    // reachable by the client anyway; switch to alt and render only it.
    if alt_screen {
        out.extend_from_slice(b"\x1b[?1049h");
    }

    // Oldest scrollback line first, through the last visible row. On the
    // alternate screen there is no history.
    let first_line = if alt_screen {
        0
    } else {
        -(grid.history_size() as i32)
    };
    let last_line = screen_lines - 1;

    let mut sgr = SgrState::default_state();
    let mut char_buf = [0u8; 4];
    for line in first_line..=last_line {
        let row = &grid[Line(line)];
        let wrapped = row[Column(cols - 1)].flags.contains(Flags::WRAPLINE);

        // Soft-wrapped rows are emitted at full width so the receiving
        // terminal re-wraps into the following row by itself; other rows are
        // trimmed of invisible trailing blanks.
        let mut len = row.len();
        if !wrapped {
            while len > 0 && is_trim_blank(&row[Column(len - 1)]) {
                len -= 1;
            }
        }

        for col in 0..len {
            let cell = &row[Column(col)];
            // The spacer that follows a wide char holds no content.
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let cell_sgr = SgrState::of_cell(cell);
            if cell_sgr != sgr {
                cell_sgr.emit(&mut out);
                sgr = cell_sgr;
            }
            // A leading spacer (wide char pushed to the next row) renders as
            // a blank cell.
            let c = if cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
                ' '
            } else {
                cell.c
            };
            out.extend_from_slice(c.encode_utf8(&mut char_buf).as_bytes());
            if let Some(zerowidth) = cell.zerowidth() {
                for zw in zerowidth {
                    out.extend_from_slice(zw.encode_utf8(&mut char_buf).as_bytes());
                }
            }
        }

        // Hard break between rows; never after the last visible row, and not
        // after soft-wrapped rows (the terminal wraps those itself).
        if line != last_line && !wrapped {
            out.extend_from_slice(b"\r\n");
        }
    }

    // Back to a clean SGR state before the trailer.
    if !sgr.is_default() {
        out.extend_from_slice(b"\x1b[0m");
    }

    // Window title.
    if let Some(title) = title {
        out.extend_from_slice(b"\x1b]2;");
        out.extend_from_slice(title.as_bytes());
        out.push(0x07);
    }

    // Private modes that RIS cleared but the application expects.
    if mode.contains(TermMode::APP_CURSOR) {
        out.extend_from_slice(b"\x1b[?1h");
    }
    if mode.contains(TermMode::BRACKETED_PASTE) {
        out.extend_from_slice(b"\x1b[?2004h");
    }

    // Cursor position (1-based CUP) ...
    let cursor = grid.cursor.point;
    out.extend_from_slice(
        format!("\x1b[{};{}H", cursor.line.0 + 1, cursor.column.0 + 1).as_bytes(),
    );

    // ... the SGR state the application currently has active at the cursor ...
    let pending = SgrState::of_cell(&grid.cursor.template);
    if !pending.is_default() {
        pending.emit(&mut out);
    }

    // ... and cursor visibility (RIS left it visible).
    if !mode.contains(TermMode::SHOW_CURSOR) {
        out.extend_from_slice(b"\x1b[?25l");
    }

    out
}
