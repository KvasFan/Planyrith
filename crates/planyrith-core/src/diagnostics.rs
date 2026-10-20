//! Implements docs/04-document-model.md §13 (validate/repair), the statistics
//! of docs/08 §2 (`info`), and the warning codes of docs/08 §9 / docs/01 §7
//! shared by command outputs.
//!
//! Owner key: `later` (validation needs `mesh::validate` and `document`).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::document::{ContainerRef, Document, EntityCounts};
use crate::ids::Pid;

/// Stable warning codes (docs/08 §9).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub enum WarningCode {
    /// Faces merged automatically.
    W2001,
    /// Vertices merged by tolerance.
    W2002,
    /// Detached dimension anchors.
    W2003,
    /// Unsupported import entity skipped.
    W2004,
    /// Texture downscaled.
    W2005,
}

impl WarningCode {
    /// The code as printed (`"W2001"`).
    pub const fn as_str(self) -> &'static str {
        match self {
            WarningCode::W2001 => "W2001",
            WarningCode::W2002 => "W2002",
            WarningCode::W2003 => "W2003",
            WarningCode::W2004 => "W2004",
            WarningCode::W2005 => "W2005",
        }
    }
}

/// A non-fatal note attached to a command output or report (docs/01 §7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Warning {
    /// Stable code.
    pub code: WarningCode,
    /// Human-readable message.
    pub message: String,
}

impl Warning {
    /// Builds a warning.
    pub fn new(code: WarningCode, message: impl Into<String>) -> Self {
        Warning {
            code,
            message: message.into(),
        }
    }
}

/// Severity of a validation problem.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Cosmetic or recoverable.
    Warning,
    /// The document violates an invariant; `repair` may fix it.
    Error,
}

/// Kinds of problems found by [`validate`] (docs/04 §13).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ProblemKind {
    /// A mesh invariant of docs/03 §3.1 is violated.
    MeshInvariant,
    /// Two vertices closer than `tolerances.length`.
    NearVertices,
    /// Face with area below `length²`.
    DegenerateFace,
    /// Dimension/text anchor pointing at a missing entity (`W2003`).
    DanglingAnchor,
    /// Image in the store with no material or image entity using it.
    UnreferencedImage,
    /// A definition contains itself directly or indirectly.
    CyclicNesting,
    /// Two tags share a name.
    DuplicateTagName,
    /// Scene refers to a deleted tag/style.
    SceneDanglingReference,
    /// Group/instance/image transform is not invertible.
    NonInvertibleTransform,
    /// `next_pid` is not above every pid in use.
    PidCounter,
    /// Pid bound twice or missing for an entity in the order list.
    PidBookkeeping,
    /// Definition `usage_count` differs from the actual count.
    UsageCount,
}

/// One problem (docs/04 §13).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Problem {
    /// Kind.
    pub kind: ProblemKind,
    /// Severity.
    pub severity: Severity,
    /// Container involved, if any.
    pub container: Option<ContainerRef>,
    /// Entity involved, if any.
    pub entity: Option<Pid>,
    /// Detail text.
    pub message: String,
}

/// Output of [`validate`] (docs/04 §13; exit code 3 when `has_errors`, docs/08 §8).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct Report {
    /// Problems in deterministic order (container, then pid, then kind).
    pub problems: Vec<Problem>,
}

impl Report {
    /// `true` when no problems were found.
    pub fn is_clean(&self) -> bool {
        self.problems.is_empty()
    }

    /// `true` when at least one problem has `Severity::Error`.
    pub fn has_errors(&self) -> bool {
        self.problems.iter().any(|p| p.severity == Severity::Error)
    }
}

/// Which fixes [`repair`] may apply (docs/08 §2 `--fix`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub struct RepairFixes {
    /// Merge vertices within tolerance.
    pub merge_vertices: bool,
    /// Remove faces with area below `length²`.
    pub degenerate_faces: bool,
    /// Delete dangling anchors, scene references, unreferenced images.
    pub dangling_refs: bool,
}

impl RepairFixes {
    /// Every fix enabled (`--fix all`).
    pub const ALL: RepairFixes = RepairFixes {
        merge_vertices: true,
        degenerate_faces: true,
        dangling_refs: true,
    };
}

/// Output of [`repair`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct RepairReport {
    /// Problems that were fixed.
    pub fixed: Vec<Problem>,
    /// What is still wrong.
    pub remaining: Report,
    /// Warnings produced while fixing (`W2001`, `W2002`).
    pub warnings: Vec<Warning>,
}

/// Document statistics for `planyrith info` (docs/08 §2).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct Statistics {
    /// Counts in the model container.
    pub model: EntityCounts,
    /// Counts summed over all definitions.
    pub definitions_total: EntityCounts,
    /// Number of definitions.
    pub definitions: usize,
    /// Number of materials.
    pub materials: usize,
    /// Number of tags.
    pub tags: usize,
    /// Number of scenes.
    pub scenes: usize,
    /// Number of styles.
    pub styles: usize,
    /// Number of images.
    pub images: usize,
}

/// Runs every check of docs/04 §13 without mutating.
pub fn validate(doc: &Document) -> Report {
    todo!("docs/04 §13 validate")
}

/// Applies the enabled fixes and re-validates (docs/04 §13 `repair`).
pub fn repair(doc: &mut Document, fixes: &RepairFixes) -> RepairReport {
    todo!("docs/04 §13 repair")
}

/// Counts for `planyrith info`.
pub fn statistics(doc: &Document) -> Statistics {
    todo!("docs/08 §2 info statistics")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_flags() {
        let mut r = Report::default();
        assert!(r.is_clean() && !r.has_errors());
        r.problems.push(Problem {
            kind: ProblemKind::DanglingAnchor,
            severity: Severity::Warning,
            container: None,
            entity: Some(Pid(3)),
            message: "detached".into(),
        });
        assert!(!r.is_clean() && !r.has_errors());
        assert_eq!(WarningCode::W2003.as_str(), "W2003");
    }
}
