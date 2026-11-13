//! The parity harness.
//!
//! One transcript runs through two executors. The [`SequentialExecutor`]
//! applies every operation in input order. The [`InterleavedExecutor`]
//! interleaves independent table work. Both executors must return the same
//! observation bytes for the same transcript.
//!
//! The harness compiles for wasm32. It uses no OS thread. The lane count
//! models scheduling capacity and does not change a result.

use oxkv_contracts::InternalDiagnostic;

use crate::core::{CoreError, Observation, Operation, Ports, SemanticCore};

/// One ordered input transcript.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Transcript {
    operations: Vec<Operation>,
}

impl Transcript {
    /// Creates a transcript.
    #[must_use]
    pub const fn new(operations: Vec<Operation>) -> Self {
        Self { operations }
    }

    /// Appends one operation.
    pub fn push(&mut self, operation: Operation) {
        self.operations.push(operation);
    }

    /// Returns the operations in input order.
    #[must_use]
    pub fn operations(&self) -> &[Operation] {
        &self.operations
    }

    /// Returns the number of operations.
    #[must_use]
    pub fn len(&self) -> usize {
        self.operations.len()
    }

    /// Returns true when the transcript has no operation.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }
}

/// The observable outputs of one run, indexed by input position.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Observations {
    slots: Vec<Option<Observation>>,
}

impl Observations {
    /// Returns one observation.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&Observation> {
        self.slots.get(index).and_then(Option::as_ref)
    }

    /// Returns every observation in input order.
    pub fn values(&self) -> impl Iterator<Item = &Observation> {
        self.slots.iter().filter_map(Option::as_ref)
    }

    /// Returns the number of slots.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Returns true when the run has no slot.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Encodes every slot in input order.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut encoder = crate::encode::Encoder::new();
        for (index, slot) in self.slots.iter().enumerate() {
            encoder.u64(u64::try_from(index).unwrap_or(u64::MAX));
            match slot {
                Some(observation) => {
                    encoder.u8(1);
                    encoder.bytes(&observation.encode());
                }
                None => encoder.u8(0),
            }
        }
        encoder.finish()
    }
}

/// One executor over the semantic core.
pub trait Executor {
    /// Returns the executor name.
    #[must_use]
    fn name(&self) -> &'static str;

    /// Runs one transcript and returns every observation.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError`] when the core cannot apply an operation.
    fn run(&self, transcript: &Transcript, ports: Ports<'_>) -> Result<Observations, CoreError>;
}

/// An executor that applies operations in input order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SequentialExecutor;

impl Executor for SequentialExecutor {
    fn name(&self) -> &'static str {
        "sequential"
    }

    fn run(&self, transcript: &Transcript, ports: Ports<'_>) -> Result<Observations, CoreError> {
        let mut core = SemanticCore::new(ports);
        let mut slots = Vec::with_capacity(transcript.len());
        for operation in transcript.operations() {
            slots.push(Some(core.apply(operation)?));
        }
        Ok(Observations { slots })
    }
}

/// An executor that interleaves independent table work.
///
/// Operations on one table keep their input order. Operations without a table
/// form one ordered group. The lane count changes the schedule and does not
/// change a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterleavedExecutor {
    lanes: usize,
}

impl InterleavedExecutor {
    /// Creates an interleaved executor with the given lane count.
    #[must_use]
    pub const fn new(lanes: usize) -> Self {
        Self {
            lanes: if lanes == 0 { 1 } else { lanes },
        }
    }

    /// Creates an interleaved executor from a scheduler adapter.
    #[must_use]
    pub fn from_scheduler(scheduler: &dyn crate::adapter::Scheduler) -> Self {
        Self::new(scheduler.lane_count())
    }

    /// Returns the lane count.
    #[must_use]
    pub const fn lanes(self) -> usize {
        self.lanes
    }
}

impl Default for InterleavedExecutor {
    fn default() -> Self {
        Self::new(1)
    }
}

impl Executor for InterleavedExecutor {
    fn name(&self) -> &'static str {
        "interleaved"
    }

    fn run(&self, transcript: &Transcript, ports: Ports<'_>) -> Result<Observations, CoreError> {
        let groups = group_operations(transcript);
        let mut core = SemanticCore::new(ports);
        let mut slots: Vec<Option<Observation>> = (0..transcript.len()).map(|_| None).collect();
        let mut positions = vec![0_usize; groups.len()];
        let mut remaining = transcript.len();
        let mut round = 0_usize;
        while remaining > 0 {
            let mut advanced = 0_usize;
            let mut offset = 0_usize;
            while offset < groups.len() && advanced < self.lanes {
                let group_index = (round + offset) % groups.len();
                if let Some(index) = groups
                    .get(group_index)
                    .and_then(|group| group.get(positions[group_index]))
                    .copied()
                {
                    let operation = transcript
                        .operations()
                        .get(index)
                        .ok_or(CoreError::Incomplete { slot: index })?;
                    let observation = core.apply(operation)?;
                    if let Some(slot) = slots.get_mut(index) {
                        *slot = Some(observation);
                    }
                    positions[group_index] = positions[group_index].saturating_add(1);
                    remaining = remaining.saturating_sub(1);
                    advanced = advanced.saturating_add(1);
                }
                offset = offset.saturating_add(1);
            }
            if advanced == 0 {
                let slot = slots.iter().position(Option::is_none).unwrap_or(remaining);
                return Err(CoreError::Incomplete { slot });
            }
            round = round.saturating_add(1);
        }
        Ok(Observations { slots })
    }
}

fn group_operations(transcript: &Transcript) -> Vec<Vec<usize>> {
    let mut groups: Vec<(Option<String>, Vec<usize>)> = Vec::new();
    for (index, operation) in transcript.operations().iter().enumerate() {
        let key = operation.table().map(str::to_owned);
        if let Some((_, indices)) = groups
            .iter_mut()
            .find(|(existing, _)| existing.as_deref() == key.as_deref())
        {
            indices.push(index);
        } else {
            groups.push((key, vec![index]));
        }
    }
    groups.into_iter().map(|(_, indices)| indices).collect()
}

/// An error from a parity comparison.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParityError {
    /// The two runs have different lengths.
    #[error("The runs have different lengths: {left} and {right}.")]
    Length {
        /// The left length.
        left: usize,
        /// The right length.
        right: usize,
    },
    /// One slot differs.
    #[error("The observation at slot {slot} differs.")]
    Observation {
        /// The slot index.
        slot: usize,
        /// The left observation.
        left: Option<Box<Observation>>,
        /// The right observation.
        right: Option<Box<Observation>>,
    },
    /// The encoded observations differ.
    #[error("The encoded observations differ.")]
    Bytes,
}

impl ParityError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Length { .. } | Self::Observation { .. } | Self::Bytes => {
                "internal.invariant_violation"
            }
        }
    }

    /// Returns the operator diagnostic for an internal invariant violation.
    ///
    /// The diagnostic is for operators and logs. It is not a wire value. The
    /// public code stays `internal.invariant_violation`.
    #[must_use]
    pub const fn diagnostic(&self) -> InternalDiagnostic {
        match self {
            Self::Length { .. } | Self::Observation { .. } | Self::Bytes => {
                InternalDiagnostic::PortableCodec
            }
        }
    }
}

/// Compares two runs by observation and by byte.
///
/// # Errors
///
/// Returns [`ParityError`] when a length, a slot, or the encoded bytes differ.
pub fn compare(left: &Observations, right: &Observations) -> Result<(), ParityError> {
    if left.len() != right.len() {
        return Err(ParityError::Length {
            left: left.len(),
            right: right.len(),
        });
    }
    for (slot, (left_slot, right_slot)) in left.slots.iter().zip(&right.slots).enumerate() {
        if left_slot != right_slot {
            return Err(ParityError::Observation {
                slot,
                left: left_slot.clone().map(Box::new),
                right: right_slot.clone().map(Box::new),
            });
        }
    }
    if left.to_bytes() != right.to_bytes() {
        return Err(ParityError::Bytes);
    }
    Ok(())
}

/// Runs one transcript through one executor.
///
/// # Errors
///
/// Returns [`CoreError`] when the core cannot apply an operation.
pub fn run_transcript(
    executor: &dyn Executor,
    transcript: &Transcript,
    ports: Ports<'_>,
) -> Result<Observations, CoreError> {
    executor.run(transcript, ports)
}
