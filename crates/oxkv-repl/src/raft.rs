//! The Raft-like driver.
//!
//! The protocol core is a deterministic state machine. A node handles
//! [`RaftMessage`] values. Time and peer messages are injected. The driver
//! reads no clock, draws no randomness, and performs no I/O.
//!
//! The election rule is deterministic. A candidate wins with a majority of
//! granted votes. A vote is granted only when the candidate log is at least as
//! up to date as the voter log. A leader commits at a majority. The leader
//! appends one no-op at its new term, so it can commit entries of earlier
//! terms.

use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

use oxkv_contracts::receipt::IdempotencyKey;
use oxkv_contracts::version::{CommitVersion, LeaseTerm};

use crate::driver::{AdvanceReport, AppendReceipt, ReplicationDriver, TerminalReceipt};
use crate::error::ReplError;
use crate::fence::{Fence, Leadership};
use crate::ids::{NodeId, Tick};
use crate::log::{EntryPayload, Log, LogIndex, LogRecord, OwnerFence};
use crate::read::ReadView;
use crate::state::{ApplyReport, KvState};

/// The role of one Raft node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// The node follows a leader.
    Follower,
    /// The node asks for votes.
    Candidate,
    /// The node serves writes.
    Leader,
}

impl fmt::Display for Role {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Follower => "Follower",
            Self::Candidate => "Candidate",
            Self::Leader => "Leader",
        })
    }
}

/// One message between Raft nodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RaftMessage {
    /// A candidate asks for a vote.
    RequestVote {
        /// The candidate term.
        term: LeaseTerm,
        /// The candidate identity.
        candidate: NodeId,
        /// The candidate last log index.
        last_index: LogIndex,
        /// The candidate last log term.
        last_term: LeaseTerm,
    },
    /// A voter answers a vote request.
    VoteResponse {
        /// The voter term.
        term: LeaseTerm,
        /// The voter identity.
        voter: NodeId,
        /// True when the voter granted the vote.
        granted: bool,
    },
    /// A leader replicates records.
    AppendEntries {
        /// The leader term.
        term: LeaseTerm,
        /// The leader identity.
        leader: NodeId,
        /// The index before the first record.
        prev_index: LogIndex,
        /// The term at `prev_index`.
        prev_term: LeaseTerm,
        /// The records after `prev_index`.
        entries: Vec<LogRecord>,
        /// The leader commit index.
        leader_commit: LogIndex,
    },
    /// A follower answers a replication request.
    AppendResponse {
        /// The follower term.
        term: LeaseTerm,
        /// The follower identity.
        follower: NodeId,
        /// True when the previous index matched.
        success: bool,
        /// The follower last log index.
        match_index: LogIndex,
    },
    /// A leader transfers authority.
    Transfer {
        /// The new term.
        term: LeaseTerm,
        /// The old leader.
        from: NodeId,
        /// The new leader.
        to: NodeId,
    },
}

/// One Raft protocol node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RaftNode {
    id: NodeId,
    role: Role,
    term: LeaseTerm,
    voted_for: Option<NodeId>,
    log: Log,
    commit_index: LogIndex,
    votes: BTreeSet<NodeId>,
    members: BTreeSet<NodeId>,
}

impl RaftNode {
    /// Creates a follower node.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::PlacementViolation`] when the node is not a member
    /// or the member set is empty.
    pub fn new(id: NodeId, members: impl IntoIterator<Item = NodeId>) -> Result<Self, ReplError> {
        let members = members.into_iter().collect::<BTreeSet<_>>();
        if members.is_empty() || !members.contains(&id) {
            return Err(ReplError::PlacementViolation {
                reason: "the node is not a member",
            });
        }
        Ok(Self {
            id,
            role: Role::Follower,
            term: LeaseTerm::new(0),
            voted_for: None,
            log: Log::new(),
            commit_index: LogIndex::ZERO,
            votes: BTreeSet::new(),
            members,
        })
    }

    /// Returns the node identity.
    #[must_use]
    pub const fn id(&self) -> NodeId {
        self.id
    }

    /// Returns the node role.
    #[must_use]
    pub const fn role(&self) -> Role {
        self.role
    }

    /// Returns the node term.
    #[must_use]
    pub const fn term(&self) -> LeaseTerm {
        self.term
    }

    /// Returns the node log.
    #[must_use]
    pub const fn log(&self) -> &Log {
        &self.log
    }

    /// Returns the node commit index.
    #[must_use]
    pub const fn commit_index(&self) -> LogIndex {
        self.commit_index
    }

    /// Returns the member set.
    #[must_use]
    pub const fn members(&self) -> &BTreeSet<NodeId> {
        &self.members
    }

    /// Returns the vote count in the current term.
    #[must_use]
    pub fn vote_count(&self) -> usize {
        self.votes.len()
    }

    /// Starts a campaign for one term.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::InvariantViolation`] when the term is lower than
    /// the node term.
    pub fn campaign(&mut self, term: LeaseTerm) -> Result<(), ReplError> {
        if term <= self.term {
            return Err(ReplError::InvariantViolation {
                reason: "a campaign term must increase",
            });
        }
        self.term = term;
        self.role = Role::Candidate;
        self.voted_for = Some(self.id);
        self.votes.clear();
        self.votes.insert(self.id);
        Ok(())
    }

    /// Makes the node the leader for one term.
    pub fn become_leader(&mut self, term: LeaseTerm) {
        self.term = term;
        self.role = Role::Leader;
        self.voted_for = Some(self.id);
        self.votes.clear();
        self.votes.insert(self.id);
    }

    /// Handles one message and returns the messages to send.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::InvariantViolation`] when a replicated record is
    /// not contiguous.
    pub fn handle(&mut self, message: &RaftMessage) -> Result<Vec<RaftMessage>, ReplError> {
        match message {
            RaftMessage::RequestVote {
                term,
                candidate,
                last_index,
                last_term,
            } => Ok(self.handle_vote_request(*term, *candidate, *last_index, *last_term)),
            RaftMessage::VoteResponse {
                term,
                voter,
                granted,
            } => {
                if *term > self.term {
                    self.step_down(*term);
                }
                if *term == self.term && *granted && self.role == Role::Candidate {
                    self.votes.insert(*voter);
                }
                Ok(Vec::new())
            }
            RaftMessage::AppendEntries {
                term,
                leader,
                prev_index,
                prev_term,
                entries,
                leader_commit,
            } => Ok(self.handle_append_entries(
                *term,
                *leader,
                *prev_index,
                *prev_term,
                entries,
                *leader_commit,
            )?),
            RaftMessage::AppendResponse { term, .. } => {
                if *term > self.term {
                    self.step_down(*term);
                }
                Ok(Vec::new())
            }
            RaftMessage::Transfer { term, .. } => {
                if *term >= self.term {
                    self.step_down(*term);
                }
                Ok(Vec::new())
            }
        }
    }

    fn step_down(&mut self, term: LeaseTerm) {
        self.term = term;
        self.role = Role::Follower;
        self.voted_for = None;
        self.votes.clear();
    }

    fn handle_vote_request(
        &mut self,
        term: LeaseTerm,
        candidate: NodeId,
        last_index: LogIndex,
        last_term: LeaseTerm,
    ) -> Vec<RaftMessage> {
        if term < self.term {
            return vec![RaftMessage::VoteResponse {
                term: self.term,
                voter: self.id,
                granted: false,
            }];
        }
        if term > self.term {
            self.step_down(term);
        }
        let candidate_is_current = self.log.last_term() > last_term
            || (self.log.last_term() == last_term && self.log.last_index() <= last_index);
        let free_vote = self.voted_for.is_none() || self.voted_for == Some(candidate);
        let granted = candidate_is_current && free_vote;
        if granted {
            self.voted_for = Some(candidate);
        }
        vec![RaftMessage::VoteResponse {
            term: self.term,
            voter: self.id,
            granted,
        }]
    }

    fn handle_append_entries(
        &mut self,
        term: LeaseTerm,
        _leader: NodeId,
        prev_index: LogIndex,
        prev_term: LeaseTerm,
        entries: &[LogRecord],
        leader_commit: LogIndex,
    ) -> Result<Vec<RaftMessage>, ReplError> {
        if term < self.term {
            return Ok(vec![RaftMessage::AppendResponse {
                term: self.term,
                follower: self.id,
                success: false,
                match_index: self.log.last_index(),
            }]);
        }
        self.term = term;
        self.role = Role::Follower;
        if self.log.term_at(prev_index) != Some(prev_term) {
            return Ok(vec![RaftMessage::AppendResponse {
                term: self.term,
                follower: self.id,
                success: false,
                match_index: self.log.last_index(),
            }]);
        }
        let mut offset = 0;
        while offset < entries.len() {
            let entry = &entries[offset];
            if self.log.term_at(entry.index()) == Some(entry.term()) {
                offset = offset.saturating_add(1);
                continue;
            }
            self.log.truncate_from(entry.index())?;
            for rest in &entries[offset..] {
                self.log.append_record(rest.clone())?;
            }
            break;
        }
        let matched = leader_commit.min(self.log.last_index());
        if matched > self.commit_index {
            self.commit_index = matched;
        }
        Ok(vec![RaftMessage::AppendResponse {
            term: self.term,
            follower: self.id,
            success: true,
            match_index: self.log.last_index(),
        }])
    }
}

/// A Raft-like replication group.
///
/// The driver owns one deterministic group. It creates one [`RaftNode`] per
/// member. The `acquire` call runs one election. The `advance` call replicates
/// records and moves the commit index to the majority match index.
#[derive(Debug, Clone)]
pub struct RaftDriver {
    members: BTreeSet<NodeId>,
    nodes: BTreeMap<NodeId, RaftNode>,
    live: BTreeSet<NodeId>,
    leader: Option<NodeId>,
    matches: BTreeMap<NodeId, LogIndex>,
    next: BTreeMap<NodeId, LogIndex>,
    state: KvState,
    crashed: bool,
}

impl RaftDriver {
    /// Creates a Raft driver for one member set.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::PlacementViolation`] when the member set is empty.
    pub fn new(members: impl IntoIterator<Item = NodeId>) -> Result<Self, ReplError> {
        let members = members.into_iter().collect::<BTreeSet<_>>();
        if members.is_empty() {
            return Err(ReplError::PlacementViolation {
                reason: "the member set is empty",
            });
        }
        let mut nodes = BTreeMap::new();
        for member in &members {
            nodes.insert(*member, RaftNode::new(*member, members.iter().copied())?);
        }
        Ok(Self {
            live: members.clone(),
            members,
            nodes,
            leader: None,
            matches: BTreeMap::new(),
            next: BTreeMap::new(),
            state: KvState::new(),
            crashed: false,
        })
    }

    /// Returns the quorum size.
    #[must_use]
    pub fn quorum(&self) -> usize {
        self.members.len() / 2 + 1
    }

    /// Returns one node.
    #[must_use]
    pub fn node(&self, id: NodeId) -> Option<&RaftNode> {
        self.nodes.get(&id)
    }

    /// Delivers one injected peer message to one node.
    ///
    /// A transport calls this method. The deterministic driver uses the same
    /// node state machine for its internal messages.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::PlacementViolation`] when the target is not a
    /// node. Returns [`ReplError::InvariantViolation`] when a record is not
    /// contiguous.
    pub fn deliver(
        &mut self,
        to: NodeId,
        message: &RaftMessage,
    ) -> Result<Vec<RaftMessage>, ReplError> {
        self.nodes
            .get_mut(&to)
            .ok_or(ReplError::PlacementViolation {
                reason: "the message target is not a node",
            })?
            .handle(message)
    }

    /// Kills the current leader.
    ///
    /// The durable log and term stay. The volatile role, the applied state,
    /// and the leader identity are lost.
    pub fn crash_leader(&mut self) {
        if let Some(leader) = self.leader.take() {
            self.live.remove(&leader);
            if let Some(node) = self.nodes.get_mut(&leader) {
                node.role = Role::Follower;
            }
        }
        self.matches.clear();
        self.next.clear();
        self.state.reset();
        self.crashed = true;
    }

    /// Isolates the current leader without a crash.
    ///
    /// The leader keeps its role and term. The rest of the group elects a new
    /// leader at a higher term. A write from the old token then fails with
    /// [`ReplError::StaleEpoch`].
    pub fn partition_leader(&mut self) {
        if let Some(leader) = self.leader.take() {
            self.live.remove(&leader);
        }
    }

    /// Rejoins every member to the live set.
    pub fn heal(&mut self) {
        self.live.clone_from(&self.members);
    }

    /// Restarts the crashed authority.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::InvariantViolation`] when the committed log has a
    /// gap.
    pub fn restart(&mut self) -> Result<ApplyReport, ReplError> {
        self.crashed = false;
        self.heal();
        self.recover()
    }

    fn check(&mut self, token: &Leadership) -> Result<(), ReplError> {
        if self.crashed {
            return Err(ReplError::InvariantViolation {
                reason: "the authority is not running",
            });
        }
        let Some(leader) = self.leader else {
            return Err(ReplError::StaleEpoch {
                presented: token.term_value(),
                current: self.max_term().get(),
            });
        };
        let node = self
            .nodes
            .get(&leader)
            .ok_or(ReplError::InvariantViolation {
                reason: "the leader is not a node",
            })?;
        if token.holder() != leader || token.term_value() != node.term.get() {
            return Err(ReplError::StaleEpoch {
                presented: token.term_value(),
                current: node.term.get(),
            });
        }
        if self.live.len() < self.quorum() {
            return Err(ReplError::LeaseExpired {
                term: token.term_value(),
            });
        }
        Ok(())
    }

    fn max_term(&self) -> LeaseTerm {
        self.nodes
            .values()
            .map(RaftNode::term)
            .max()
            .unwrap_or_else(|| LeaseTerm::new(0))
    }

    fn elect(&mut self) -> Result<Leadership, ReplError> {
        if self.live.len() < self.quorum() {
            return Err(ReplError::TierUnavailable { tier: "quorum" });
        }
        let order = self.live.iter().copied().collect::<Vec<_>>();
        for candidate in order {
            let term = LeaseTerm::new(self.max_term().get().checked_add(1).ok_or(
                ReplError::InvariantViolation {
                    reason: "the election term overflowed",
                },
            )?);
            let candidate_node =
                self.nodes
                    .get_mut(&candidate)
                    .ok_or(ReplError::InvariantViolation {
                        reason: "the candidate is not a node",
                    })?;
            candidate_node.campaign(term)?;
            let last_index = candidate_node.log().last_index();
            let last_term = candidate_node.log().last_term();
            let mut votes = 1_usize;
            for voter in self.live.clone() {
                if voter == candidate {
                    continue;
                }
                let voter_node =
                    self.nodes
                        .get_mut(&voter)
                        .ok_or(ReplError::InvariantViolation {
                            reason: "the voter is not a node",
                        })?;
                let responses = voter_node.handle(&RaftMessage::RequestVote {
                    term,
                    candidate,
                    last_index,
                    last_term,
                })?;
                for response in responses {
                    if let RaftMessage::VoteResponse {
                        term: response_term,
                        granted: true,
                        ..
                    } = response
                        && response_term == term
                    {
                        votes = votes.saturating_add(1);
                    }
                }
            }
            if votes >= self.quorum() {
                self.become_leader(candidate, term)?;
                return Ok(Leadership::new(candidate, Fence::new(term)));
            }
        }
        Err(ReplError::TierUnavailable { tier: "quorum" })
    }

    fn become_leader(&mut self, leader: NodeId, term: LeaseTerm) -> Result<(), ReplError> {
        let node = self
            .nodes
            .get_mut(&leader)
            .ok_or(ReplError::InvariantViolation {
                reason: "the leader is not a node",
            })?;
        node.become_leader(term);
        node.log
            .append(term, OwnerFence::new(term), EntryPayload::Noop)?;
        let last = node.log.last_index();
        self.leader = Some(leader);
        self.matches.clear();
        self.next.clear();
        for member in &self.members {
            let match_index = if *member == leader {
                last
            } else {
                LogIndex::ZERO
            };
            self.matches.insert(*member, match_index);
            self.next.insert(*member, last);
        }
        self.replicate(leader)?;
        self.replicate(leader)?;
        Ok(())
    }

    fn replicate(&mut self, leader: NodeId) -> Result<LogIndex, ReplError> {
        let followers = self.live.iter().copied().collect::<Vec<_>>();
        for follower in followers {
            if follower != leader {
                self.replicate_follower(leader, follower)?;
            }
        }
        self.advance_commit(leader)
    }

    fn replicate_follower(&mut self, leader: NodeId, follower: NodeId) -> Result<(), ReplError> {
        let leader_term = self
            .nodes
            .get(&leader)
            .ok_or(ReplError::InvariantViolation {
                reason: "the leader is not a node",
            })?
            .term;
        let next = self
            .next
            .get(&follower)
            .copied()
            .filter(|index| *index > LogIndex::ZERO)
            .unwrap_or_else(|| LogIndex::new(1));
        let prev = LogIndex::new(next.get().saturating_sub(1));
        let (prev_term, entries, leader_commit, leader_last) = {
            let node = self
                .nodes
                .get(&leader)
                .ok_or(ReplError::InvariantViolation {
                    reason: "the leader is not a node",
                })?;
            (
                node.log.term_at(prev).unwrap_or_else(|| LeaseTerm::new(0)),
                node.log.entries_after(prev),
                node.commit_index,
                node.log.last_index(),
            )
        };
        let responses = self
            .nodes
            .get_mut(&follower)
            .ok_or(ReplError::InvariantViolation {
                reason: "the follower is not a node",
            })?
            .handle(&RaftMessage::AppendEntries {
                term: leader_term,
                leader,
                prev_index: prev,
                prev_term,
                entries,
                leader_commit,
            })?;
        for response in responses {
            if let RaftMessage::AppendResponse {
                term,
                success,
                match_index,
                ..
            } = response
            {
                if term > leader_term {
                    self.leader = None;
                    return Err(ReplError::LeaseExpired {
                        term: leader_term.get(),
                    });
                }
                if success {
                    self.matches.insert(follower, match_index);
                    self.next.insert(follower, match_index.next()?);
                } else {
                    let proposed = match_index.next()?;
                    let max_next = leader_last.next()?;
                    self.next.insert(follower, proposed.min(max_next));
                }
            }
        }
        Ok(())
    }

    fn advance_commit(&mut self, leader: NodeId) -> Result<LogIndex, ReplError> {
        let leader_last = self
            .nodes
            .get(&leader)
            .ok_or(ReplError::InvariantViolation {
                reason: "the leader is not a node",
            })?
            .log
            .last_index();
        self.matches.insert(leader, leader_last);
        let quorum = self.quorum();
        let mut matches = self
            .live
            .iter()
            .map(|member| self.matches.get(member).copied().unwrap_or(LogIndex::ZERO))
            .collect::<Vec<_>>();
        matches.sort_unstable_by(|left, right| right.cmp(left));
        let candidate =
            *matches
                .get(quorum.saturating_sub(1))
                .ok_or(ReplError::InvariantViolation {
                    reason: "the majority match index is absent",
                })?;
        let (commit, candidate_term, leader_term) = {
            let node = self
                .nodes
                .get(&leader)
                .ok_or(ReplError::InvariantViolation {
                    reason: "the leader is not a node",
                })?;
            (
                node.commit_index,
                node.log
                    .term_at(candidate)
                    .unwrap_or_else(|| LeaseTerm::new(0)),
                node.term,
            )
        };
        let commit = if candidate > commit && candidate_term == leader_term {
            candidate
        } else {
            commit
        };
        if let Some(node) = self.nodes.get_mut(&leader) {
            node.commit_index = commit;
        }
        Ok(commit)
    }

    fn authority(&self) -> Option<(&Log, LogIndex)> {
        let id = self.leader.or_else(|| {
            self.nodes
                .values()
                .max_by_key(|node| node.commit_index)
                .map(RaftNode::id)
        })?;
        let node = self.nodes.get(&id)?;
        Some((&node.log, node.commit_index))
    }
}

impl ReplicationDriver for RaftDriver {
    fn acquire(&mut self, _now: Tick) -> Result<Leadership, ReplError> {
        if self.crashed {
            return Err(ReplError::InvariantViolation {
                reason: "the authority is not running",
            });
        }
        if let Some(leader) = self.leader
            && self.live.contains(&leader)
            && let Some(node) = self.nodes.get(&leader)
        {
            return Ok(Leadership::new(leader, Fence::new(node.term)));
        }
        self.elect()
    }

    fn renew(&mut self, token: &Leadership, _now: Tick) -> Result<(), ReplError> {
        self.check(token)?;
        let leader = self.leader.ok_or(ReplError::StaleEpoch {
            presented: token.term_value(),
            current: self.max_term().get(),
        })?;
        self.replicate(leader)?;
        Ok(())
    }

    fn release(&mut self, token: &Leadership) -> Result<(), ReplError> {
        self.check(token)?;
        if let Some(leader) = self.leader.take()
            && let Some(node) = self.nodes.get_mut(&leader)
        {
            node.role = Role::Follower;
        }
        Ok(())
    }

    fn append(
        &mut self,
        token: &Leadership,
        payload: EntryPayload,
        _now: Tick,
    ) -> Result<AppendReceipt, ReplError> {
        self.check(token)?;
        let leader = self.leader.ok_or(ReplError::StaleEpoch {
            presented: token.term_value(),
            current: self.max_term().get(),
        })?;
        let commit = self
            .nodes
            .get(&leader)
            .map_or(LogIndex::ZERO, RaftNode::commit_index);
        if let Some(idempotency) = payload.idempotency()
            && let Some(record) = self
                .nodes
                .get(&leader)
                .and_then(|node| node.log.find_idempotency(idempotency))
        {
            return Ok(AppendReceipt::new(
                record.index(),
                record.term(),
                record.index() <= commit,
            ));
        }
        let record = self
            .nodes
            .get_mut(&leader)
            .ok_or(ReplError::InvariantViolation {
                reason: "the leader is not a node",
            })?
            .log
            .append(token.term(), OwnerFence::new(token.term()), payload)?;
        Ok(AppendReceipt::new(record.index(), record.term(), false))
    }

    fn advance(&mut self, token: &Leadership, _now: Tick) -> Result<AdvanceReport, ReplError> {
        self.check(token)?;
        let leader = self.leader.ok_or(ReplError::StaleEpoch {
            presented: token.term_value(),
            current: self.max_term().get(),
        })?;
        self.replicate(leader)?;
        let commit = self.replicate(leader)?;
        Ok(AdvanceReport::new(commit, token.term()))
    }

    fn commit_index(&self) -> LogIndex {
        self.leader
            .and_then(|leader| self.nodes.get(&leader))
            .map_or_else(
                || {
                    self.nodes
                        .values()
                        .map(RaftNode::commit_index)
                        .max()
                        .unwrap_or(LogIndex::ZERO)
                },
                RaftNode::commit_index,
            )
    }

    fn applied_index(&self) -> LogIndex {
        self.state.applied_index()
    }

    fn apply(&mut self, up_to: LogIndex) -> Result<ApplyReport, ReplError> {
        let commit = self.commit_index();
        if up_to > commit {
            return Err(ReplError::InvariantViolation {
                reason: "the apply index is above the commit index",
            });
        }
        let id = self.leader.or_else(|| {
            self.nodes
                .values()
                .max_by_key(|node| node.commit_index)
                .map(RaftNode::id)
        });
        let id = id.ok_or(ReplError::InvariantViolation {
            reason: "the authority log is absent",
        })?;
        let log = &self
            .nodes
            .get(&id)
            .ok_or(ReplError::InvariantViolation {
                reason: "the authority node is absent",
            })?
            .log;
        self.state.apply(log, up_to)
    }

    fn recover(&mut self) -> Result<ApplyReport, ReplError> {
        self.state.reset();
        let commit = self.commit_index();
        let id = self.leader.or_else(|| {
            self.nodes
                .values()
                .max_by_key(|node| node.commit_index)
                .map(RaftNode::id)
        });
        let id = id.ok_or(ReplError::InvariantViolation {
            reason: "the authority log is absent",
        })?;
        let log = &self
            .nodes
            .get(&id)
            .ok_or(ReplError::InvariantViolation {
                reason: "the authority node is absent",
            })?
            .log;
        self.state.apply(log, commit)
    }

    fn read(&self, token: &Leadership, at: LogIndex, _now: Tick) -> Result<ReadView, ReplError> {
        if self.crashed {
            return Err(ReplError::InvariantViolation {
                reason: "the authority is not running",
            });
        }
        let leader = self.leader.ok_or(ReplError::StaleEpoch {
            presented: token.term_value(),
            current: self.max_term().get(),
        })?;
        let term = self
            .nodes
            .get(&leader)
            .map_or_else(|| LeaseTerm::new(0), RaftNode::term);
        if token.holder() != leader || token.term_value() != term.get() {
            return Err(ReplError::StaleEpoch {
                presented: token.term_value(),
                current: term.get(),
            });
        }
        self.state.read_at(at)
    }

    fn read_follower(&self, at: LogIndex) -> Result<ReadView, ReplError> {
        self.state.read_at(at)
    }

    fn members(&self) -> &BTreeSet<NodeId> {
        &self.members
    }

    fn leader(&self) -> Option<NodeId> {
        self.leader
    }

    fn term(&self) -> LeaseTerm {
        self.leader
            .and_then(|leader| self.nodes.get(&leader))
            .map_or_else(|| self.max_term(), RaftNode::term)
    }

    fn last_index(&self) -> LogIndex {
        self.leader
            .and_then(|leader| self.nodes.get(&leader))
            .map_or_else(
                || {
                    self.nodes
                        .values()
                        .map(|node| node.log.last_index())
                        .max()
                        .unwrap_or(LogIndex::ZERO)
                },
                |node| node.log.last_index(),
            )
    }

    fn transfer(
        &mut self,
        token: &Leadership,
        to: NodeId,
        _now: Tick,
    ) -> Result<Leadership, ReplError> {
        self.check(token)?;
        if !self.members.contains(&to) {
            return Err(ReplError::PlacementViolation {
                reason: "the transfer target is not a member",
            });
        }
        if !self.live.contains(&to) {
            return Err(ReplError::TierUnavailable { tier: "target" });
        }
        let new_term = LeaseTerm::new(self.term().get().checked_add(1).ok_or(
            ReplError::InvariantViolation {
                reason: "the transfer term overflowed",
            },
        )?);
        if let Some(old) = self.leader.take()
            && let Some(node) = self.nodes.get_mut(&old)
        {
            node.role = Role::Follower;
        }
        let node = self
            .nodes
            .get_mut(&to)
            .ok_or(ReplError::InvariantViolation {
                reason: "the transfer target is not a node",
            })?;
        node.become_leader(new_term);
        node.log
            .append(new_term, OwnerFence::new(new_term), EntryPayload::Noop)?;
        let last = node.log.last_index();
        self.leader = Some(to);
        self.matches.clear();
        self.next.clear();
        for member in &self.members {
            self.matches.insert(*member, LogIndex::ZERO);
            self.next.insert(*member, last);
        }
        self.matches.insert(to, last);
        self.replicate(to)?;
        self.replicate(to)?;
        Ok(Leadership::new(to, Fence::new(new_term)))
    }

    fn receipt(&self, idempotency: &IdempotencyKey) -> Option<TerminalReceipt> {
        let (log, commit) = self.authority()?;
        if let Some(record) = log.find_idempotency(idempotency)
            && record.index() <= commit
        {
            let version = log.commit_version_at(record.index());
            return Some(TerminalReceipt::committed(
                record.term(),
                record.index(),
                CommitVersion::new(version),
            ));
        }
        self.state.receipt_for(idempotency)
    }
}
