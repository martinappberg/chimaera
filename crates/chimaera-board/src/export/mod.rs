//! Exports: the board leaving the workbench. Every exporter reuses the same
//! page emission the renderer rasterizes ([`crate::render`]) — one engine,
//! so the pane, the CLI and the export cannot disagree.
//!
//! PPTX is the exception that proves the rule: it does not go through the SVG
//! at all, because its whole point is *editability at the destination* — it
//! re-emits the same normalized objects as native DrawingML shapes with real
//! text, and declares a per-object fate ([`ObjectFate`]) so degradation is
//! stated before the file is opened, never discovered after.

mod chart_xml;
pub mod pdf;
pub mod pptx;
pub mod svg;

pub use pptx::{write_pptx, write_pptx_with, ChartFidelity, PptxOptions};

use serde::Serialize;

/// How faithfully one object landed at the export destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportTier {
    /// A first-class editable object at the destination.
    Native,
    /// Decomposed into a group of editable primitives — a chart's bars,
    /// axes and labels. The composite identity stays in the board file,
    /// and that is correct: the file is the source of truth.
    Grouped,
    /// Vector output that stands in for the authored geometry (an unmapped
    /// preset exported as its bounding box).
    Vector,
    /// Pixels or a placeholder box. Objects that could not be exported at
    /// all also land here, with a reason beginning `skipped:` — the lowest
    /// tier, so any consumer ranking by fidelity surfaces them.
    Raster,
}

/// The declared fate of one object: which tier it landed at, and why.
#[derive(Debug, Clone, Serialize)]
pub struct ObjectFate {
    pub id: String,
    pub tier: ExportTier,
    pub reason: String,
}

/// Per-object outcomes for a whole export, in page-then-z order — the
/// degradation contract as data, stated by the same code that emitted the
/// file so the two cannot drift.
#[derive(Debug, Clone, Serialize)]
pub struct ExportReport {
    pub objects: Vec<ObjectFate>,
}
