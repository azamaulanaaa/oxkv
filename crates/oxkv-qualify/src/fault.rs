//! Fault injection at named points.
//!
//! A fault is a scenario input. The scenario records the injected fault and
//! the named outcome. The suite asserts that every fault kind runs.

/// One step of a commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CommitStep {
    /// The write is validated.
    Validate,
    /// The staged row is applied.
    Apply,
    /// The receipt is written.
    Receipt,
    /// The audit record is appended.
    Audit,
    /// The search index is updated.
    Index,
    /// The generation is published.
    Publish,
}

impl CommitStep {
    /// Every commit step in order.
    pub const ALL: [Self; 6] = [
        Self::Validate,
        Self::Apply,
        Self::Receipt,
        Self::Audit,
        Self::Index,
        Self::Publish,
    ];

    /// Returns the canonical step name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Validate => "validate",
            Self::Apply => "apply",
            Self::Receipt => "receipt",
            Self::Audit => "audit",
            Self::Index => "index",
            Self::Publish => "publish",
        }
    }
}

impl core::fmt::Display for CommitStep {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The kind of one fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FaultKind {
    /// The link cuts after a frame.
    LinkCut,
    /// A cursor expires.
    CursorExpiry,
    /// A conditional-write header is lost.
    ConditionalWriteLoss,
    /// A snapshot page is corrupt.
    CorruptPage,
    /// A wire frame is truncated.
    TruncatedFrame,
    /// A crash happens at one commit step.
    CrashAtCommit,
    /// A frame is delivered more than one time.
    DuplicateDelivery,
}

impl FaultKind {
    /// Every fault kind.
    pub const ALL: [Self; 7] = [
        Self::LinkCut,
        Self::CursorExpiry,
        Self::ConditionalWriteLoss,
        Self::CorruptPage,
        Self::TruncatedFrame,
        Self::CrashAtCommit,
        Self::DuplicateDelivery,
    ];

    /// Returns the canonical kind name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::LinkCut => "link_cut",
            Self::CursorExpiry => "cursor_expiry",
            Self::ConditionalWriteLoss => "conditional_write_loss",
            Self::CorruptPage => "corrupt_page",
            Self::TruncatedFrame => "truncated_frame",
            Self::CrashAtCommit => "crash_at_commit",
            Self::DuplicateDelivery => "duplicate_delivery",
        }
    }
}

impl core::fmt::Display for FaultKind {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// One injected fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// The link cuts after the given zero-based frame.
    LinkCut {
        /// The frame index.
        after_frame: u64,
    },
    /// The cursor expires when the replica has applied the given count.
    CursorExpiry {
        /// The applied frame count.
        after_frames: u64,
    },
    /// The conditional-write header is lost for the given write.
    ConditionalWriteLoss {
        /// The zero-based write index.
        write_index: u64,
    },
    /// One page in a snapshot is corrupt.
    CorruptPage {
        /// The zero-based page index.
        page_sequence: u64,
    },
    /// One wire frame is truncated.
    TruncatedFrame {
        /// The zero-based frame index.
        frame_index: u64,
    },
    /// A crash happens at one commit step.
    CrashAtCommit {
        /// The step that crashes.
        step: CommitStep,
    },
    /// One frame is delivered more than one time.
    DuplicateDelivery {
        /// The zero-based frame index.
        frame_index: u64,
        /// The number of additional copies.
        copies: u8,
    },
}

impl Fault {
    /// Returns the fault kind.
    #[must_use]
    pub const fn kind(self) -> FaultKind {
        match self {
            Self::LinkCut { .. } => FaultKind::LinkCut,
            Self::CursorExpiry { .. } => FaultKind::CursorExpiry,
            Self::ConditionalWriteLoss { .. } => FaultKind::ConditionalWriteLoss,
            Self::CorruptPage { .. } => FaultKind::CorruptPage,
            Self::TruncatedFrame { .. } => FaultKind::TruncatedFrame,
            Self::CrashAtCommit { .. } => FaultKind::CrashAtCommit,
            Self::DuplicateDelivery { .. } => FaultKind::DuplicateDelivery,
        }
    }

    /// Returns the canonical fault text.
    #[must_use]
    pub fn describe(self) -> String {
        match self {
            Self::LinkCut { after_frame } => format!("link_cut:after_frame={after_frame}"),
            Self::CursorExpiry { after_frames } => {
                format!("cursor_expiry:after_frames={after_frames}")
            }
            Self::ConditionalWriteLoss { write_index } => {
                format!("conditional_write_loss:write_index={write_index}")
            }
            Self::CorruptPage { page_sequence } => {
                format!("corrupt_page:page_sequence={page_sequence}")
            }
            Self::TruncatedFrame { frame_index } => {
                format!("truncated_frame:frame_index={frame_index}")
            }
            Self::CrashAtCommit { step } => format!("crash_at_commit:step={step}"),
            Self::DuplicateDelivery {
                frame_index,
                copies,
            } => format!("duplicate_delivery:frame_index={frame_index}:copies={copies}"),
        }
    }
}

/// A set of injected faults.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FaultPlan {
    faults: Vec<Fault>,
}

impl FaultPlan {
    /// Creates an empty plan.
    #[must_use]
    pub fn new() -> Self {
        Self { faults: Vec::new() }
    }

    /// Creates a plan with one fault.
    #[must_use]
    pub fn with(fault: Fault) -> Self {
        Self {
            faults: vec![fault],
        }
    }

    /// Appends one fault.
    pub fn push(&mut self, fault: Fault) {
        self.faults.push(fault);
    }

    /// Returns every fault.
    #[must_use]
    pub fn faults(&self) -> &[Fault] {
        &self.faults
    }

    /// Returns true when the plan has the fault kind.
    #[must_use]
    pub fn has(&self, kind: FaultKind) -> bool {
        self.faults.iter().any(|fault| fault.kind() == kind)
    }

    /// Returns true when the plan crashes at the step.
    #[must_use]
    pub fn crashes_at(&self, step: CommitStep) -> bool {
        self.faults.iter().any(
            |fault| matches!(fault, Fault::CrashAtCommit { step: crashed } if *crashed == step),
        )
    }

    /// Returns the link-cut frame.
    #[must_use]
    pub fn link_cut_after(&self) -> Option<u64> {
        self.faults.iter().find_map(|fault| match fault {
            Fault::LinkCut { after_frame } => Some(*after_frame),
            _ => None,
        })
    }

    /// Returns the frame that is truncated.
    #[must_use]
    pub fn truncated_frame(&self) -> Option<u64> {
        self.faults.iter().find_map(|fault| match fault {
            Fault::TruncatedFrame { frame_index } => Some(*frame_index),
            _ => None,
        })
    }

    /// Returns the count of extra copies for one frame.
    #[must_use]
    pub fn duplicate_copies(&self, frame_index: u64) -> u8 {
        self.faults
            .iter()
            .filter_map(|fault| match fault {
                Fault::DuplicateDelivery {
                    frame_index: index,
                    copies,
                } if *index == frame_index => Some(*copies),
                _ => None,
            })
            .max()
            .unwrap_or(0)
    }

    /// Returns the corrupt page index.
    #[must_use]
    pub fn corrupt_page(&self) -> Option<u64> {
        self.faults.iter().find_map(|fault| match fault {
            Fault::CorruptPage { page_sequence } => Some(*page_sequence),
            _ => None,
        })
    }

    /// Returns true when the cursor expires after the frame count.
    #[must_use]
    pub fn cursor_expires_after(&self, frames: u64) -> bool {
        self.faults.iter().any(|fault| {
            matches!(fault, Fault::CursorExpiry { after_frames } if *after_frames <= frames)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{CommitStep, Fault, FaultKind, FaultPlan};

    #[test]
    fn every_fault_kind_has_a_distinct_name() {
        let mut names: Vec<&str> = FaultKind::ALL.iter().map(|kind| kind.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), FaultKind::ALL.len());
    }

    #[test]
    fn a_plan_reports_its_faults() {
        let mut plan = FaultPlan::new();
        plan.push(Fault::LinkCut { after_frame: 2 });
        plan.push(Fault::DuplicateDelivery {
            frame_index: 2,
            copies: 1,
        });
        plan.push(Fault::CrashAtCommit {
            step: CommitStep::Audit,
        });
        assert!(plan.has(FaultKind::LinkCut));
        assert_eq!(plan.link_cut_after(), Some(2));
        assert_eq!(plan.duplicate_copies(2), 1);
        assert!(plan.crashes_at(CommitStep::Audit));
        assert!(!plan.crashes_at(CommitStep::Apply));
    }

    #[test]
    fn a_cursor_expires_at_or_after_its_point() {
        let plan = FaultPlan::with(Fault::CursorExpiry { after_frames: 3 });
        assert!(!plan.cursor_expires_after(2));
        assert!(plan.cursor_expires_after(3));
        assert!(plan.cursor_expires_after(4));
    }
}
