//! Relay-local bounded publication and receive histories; never persist business payloads.
use crate::{
    routing::{Router, RoutingError, StreamKey},
    wire::{Envelope, Kind, Side, WireError},
};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use tokio::time::{Duration, Instant};

#[derive(Debug, thiserror::Error)]
pub enum DeliveryError {
    #[error(transparent)]
    Wire(#[from] WireError),
    #[error(transparent)]
    Routing(#[from] RoutingError),
    #[error("publication queue capacity exhausted")]
    Capacity,
    #[error("publication changed within the same sequence")]
    Changed,
    #[error("reconnect to repair a receive gap")]
    ReplayGap,
}
#[derive(Clone, Copy)]
pub struct Limits {
    pub bytes: usize,
    pub frames: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            bytes: 64 * 1024 * 1024,
            frames: 8192,
        }
    }
}
#[derive(Clone, PartialEq, Eq)]
pub struct PublicationId {
    pub stream: StreamKey,
    pub seq: u64,
}
struct Publication {
    id: PublicationId,
    count: u64,
    chunked: bool,
    best_effort: bool,
    parts: BTreeMap<u64, String>,
    acked: HashSet<u64>,
    sent: HashSet<u64>,
}
pub struct Outbox {
    groups: VecDeque<Publication>,
    high: HashMap<StreamKey, u64>,
    bytes: usize,
    frames: usize,
    limits: Limits,
}
impl Outbox {
    pub fn new(limits: Limits) -> Self {
        Self {
            groups: VecDeque::new(),
            high: HashMap::new(),
            bytes: 0,
            frames: 0,
            limits,
        }
    }
    pub fn len(&self) -> usize {
        self.frames
    }
    pub fn is_empty(&self) -> bool {
        self.frames == 0
    }
    pub fn put(&mut self, side: Side, raw: String, best_effort: bool) -> Result<(), DeliveryError> {
        let frame = Envelope::publication(side, &raw)?;
        if frame.kind() == Kind::Ack {
            return Err(DeliveryError::Changed);
        }
        let id = PublicationId {
            stream: StreamKey::from_envelope(&frame),
            seq: frame.seq(),
        };
        let chunked = frame.kind().is_chunk();
        let (segment, count) = if chunked {
            (
                frame.required("segment_id")?,
                frame.required("segment_count")?,
            )
        } else {
            (0, 1)
        };
        let existing = self.groups.iter().position(|g| g.id == id);
        if let Some(index) = existing {
            let group = &self.groups[index];
            if (group.count, group.chunked) != (count, chunked) {
                return Err(DeliveryError::Changed);
            }
            if let Some(previous) = group.parts.get(&segment) {
                return if previous == &raw {
                    Ok(())
                } else {
                    Err(DeliveryError::Changed)
                };
            }
        }
        if self.bytes + raw.len() > self.limits.bytes + if best_effort { 1024 * 1024 } else { 0 }
            || self.frames >= self.limits.frames + if best_effort { 512 } else { 0 }
        {
            return Err(DeliveryError::Capacity);
        }
        let index = existing.unwrap_or_else(|| {
            self.groups.push_back(Publication {
                id,
                count,
                chunked,
                best_effort,
                parts: BTreeMap::new(),
                acked: HashSet::new(),
                sent: HashSet::new(),
            });
            self.groups.len() - 1
        });
        self.bytes += raw.len();
        self.frames += 1;
        self.groups[index].parts.insert(segment, raw);
        Ok(())
    }
    fn remove(&mut self, index: usize) {
        if let Some(group) = self.groups.remove(index) {
            self.bytes -= group.parts.values().map(String::len).sum::<usize>();
            self.frames -= group.parts.len();
        }
    }
    pub fn forget(&mut self, stream: &StreamKey) {
        for index in (0..self.groups.len()).rev() {
            if &self.groups[index].id.stream == stream {
                self.remove(index);
            }
        }
        self.high.remove(stream);
    }
    pub fn acknowledge(&mut self, frame: &Envelope) -> Result<(), DeliveryError> {
        let stream = StreamKey::from_envelope(frame);
        let Some(index) = self
            .groups
            .iter()
            .position(|g| g.id.stream == stream && g.id.seq == frame.seq())
        else {
            return Ok(());
        };
        let group = &mut self.groups[index];
        if group.chunked {
            let segment = match frame.raw("segment_id") {
                Some(_) => frame.required::<u64>("segment_id")?,
                None => 0,
            };
            if segment >= group.count
                || (frame.raw("segment_count").is_some()
                    && frame.required::<u64>("segment_count")? != group.count)
            {
                return Err(WireError::Malformed.into());
            }
            group.acked.insert(segment);
        }
        let high = *self
            .high
            .entry(stream.clone())
            .and_modify(|h| *h = (*h).max(frame.seq()))
            .or_insert(frame.seq());
        let mut index = 0;
        while index < self.groups.len() {
            let group = &self.groups[index];
            if group.id.stream != stream {
                index += 1;
                continue;
            }
            if group.id.seq > high || (group.chunked && group.acked.len() as u64 != group.count) {
                break;
            }
            self.remove(index);
        }
        if !self.groups.iter().any(|g| g.id.stream == stream) {
            self.high.remove(&stream);
        }
        Ok(())
    }
    pub fn reconnect(&mut self) {
        for group in &mut self.groups {
            group.sent.clear();
        }
    }
    pub fn next_frame(&self) -> Option<(PublicationId, u64, String)> {
        for group in &self.groups {
            for (segment, raw) in &group.parts {
                if !group.sent.contains(segment) {
                    return Some((group.id.clone(), *segment, raw.clone()));
                }
            }
        }
        None
    }
    pub fn sent(&mut self, id: &PublicationId, segment: u64) {
        if let Some(index) = self.groups.iter().position(|g| &g.id == id) {
            let group = &mut self.groups[index];
            group.sent.insert(segment);
            if group.best_effort && group.sent.len() as u64 == group.count {
                self.remove(index);
            }
        }
    }
}
#[derive(Clone, Default)]
struct Receiver {
    seq: Option<u64>,
    pending: Option<u64>,
    next_segment: u64,
    count: u64,
    cursor: Option<String>,
    established: bool,
}
enum Classification {
    New,
    Duplicate,
    Gap,
}
impl Receiver {
    fn classify(&self, frame: &Envelope) -> Result<Classification, WireError> {
        let seq = frame.seq();
        if self.seq.is_some_and(|s| seq <= s) {
            return Ok(Classification::Duplicate);
        }
        let chunked = frame.kind().is_chunk();
        let segment = if chunked {
            frame.required("segment_id")?
        } else {
            0
        };
        if let Some(pending) = self.pending {
            if seq < pending {
                return Ok(Classification::Duplicate);
            }
            if seq > pending || !chunked || frame.required::<u64>("segment_count")? != self.count {
                return Ok(Classification::Gap);
            }
            return Ok(if segment < self.next_segment {
                Classification::Duplicate
            } else if segment > self.next_segment {
                Classification::Gap
            } else {
                Classification::New
            });
        }
        if self
            .seq
            .is_some_and(|s| s.checked_add(1).is_none_or(|next| seq > next))
            || (chunked && segment != 0)
        {
            return Ok(Classification::Gap);
        }
        Ok(Classification::New)
    }
    fn accept(&mut self, frame: &Envelope) -> Result<(), WireError> {
        if frame.kind().is_chunk() {
            self.pending = Some(frame.seq());
            self.count = frame.required("segment_count")?;
            self.next_segment = frame.required::<u64>("segment_id")? + 1;
            if self.next_segment != self.count {
                return Ok(());
            }
        }
        self.seq = Some(frame.seq());
        self.pending = None;
        self.next_segment = 0;
        if let Some(cursor) = cursor(frame)? {
            self.cursor = Some(cursor);
        }
        Ok(())
    }
}
fn cursor(frame: &Envelope) -> Result<Option<String>, WireError> {
    if frame.raw("cursor").is_some() {
        frame.required("cursor")
    } else {
        Ok(None)
    }
}
struct Gap {
    stream: StreamKey,
    cursor: String,
    seq: u64,
    deadline: Instant,
}
#[derive(Default)]
struct History {
    streams: HashMap<StreamKey, Receiver>,
    cursor: Option<String>,
    latest: Option<String>,
    gap: Option<Gap>,
}
impl History {
    fn checkpoint(&mut self, frame: &Envelope) -> Result<(), WireError> {
        if let Some(cursor) = cursor(frame)? {
            self.latest = Some(cursor);
        }
        if self.gap.is_none() && !self.streams.values().any(|r| r.pending.is_some()) {
            self.cursor = self.latest.clone();
        }
        Ok(())
    }
    fn forget(&mut self, key: &StreamKey) {
        self.streams.remove(key);
        if self.gap.as_ref().is_some_and(|g| &g.stream == key) {
            self.gap = None;
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Forwarded { activity: bool },
    Ack,
    Duplicate,
    Unmapped,
    Retired,
}
pub struct Delivery {
    pub router: Router,
    pub outboxes: [Outbox; 2],
    history: [History; 2],
    restore: HashSet<StreamKey>,
    pong_at: HashMap<StreamKey, Instant>,
    source_at: HashMap<StreamKey, Instant>,
}
impl Delivery {
    pub fn new(router: Router, limits: Limits) -> Self {
        Self {
            router,
            outboxes: [Outbox::new(limits), Outbox::new(limits)],
            history: Default::default(),
            restore: HashSet::new(),
            pong_at: HashMap::new(),
            source_at: HashMap::new(),
        }
    }
    pub fn subscribe_cursor(&self, side: Side) -> Option<&str> {
        let h = &self.history[side.index()];
        h.gap
            .as_ref()
            .map(|g| g.cursor.as_str())
            .or(h.cursor.as_deref())
    }
    pub fn reconnect(&mut self) -> Result<(), DeliveryError> {
        for outbox in &mut self.outboxes {
            outbox.reconnect();
        }
        self.restore.extend(self.router.keys());
        self.restore_subscriptions()
    }
    fn enqueue(&mut self, side: Side, raw: String, best_effort: bool) -> Result<(), DeliveryError> {
        self.outboxes[side.index()].put(side, raw, best_effort)
    }
    fn forget(&mut self, key: &StreamKey, mapped: Option<String>) {
        self.outboxes[0].forget(key);
        self.history[0].forget(key);
        if let Some(mapped) = mapped {
            let out = StreamKey {
                client: self.router.client.clone(),
                stream: mapped,
            };
            self.outboxes[1].forget(&out);
            self.history[1].forget(&out);
        }
        self.restore.remove(key);
        self.pong_at.remove(key);
        self.source_at.remove(key);
    }
    pub fn retire(
        &mut self,
        key: &StreamKey,
        unknown: Option<String>,
    ) -> Result<(), DeliveryError> {
        let mapped = self.router.mapped(key).map(str::to_owned);
        let notices = self.router.terminate(key);
        self.forget(key, mapped);
        for (side, raw) in notices {
            self.enqueue(
                side,
                if side == Side::In {
                    unknown.clone().unwrap_or(raw)
                } else {
                    raw
                },
                true,
            )?;
        }
        Ok(())
    }
    pub fn retire_all(&mut self) -> Result<(), DeliveryError> {
        for key in self.router.keys() {
            self.retire(&key, None)?;
        }
        Ok(())
    }
    fn restore_subscriptions(&mut self) -> Result<(), DeliveryError> {
        for key in self.restore.iter().cloned().collect::<Vec<_>>() {
            if self.router.mapped(&key).is_none() {
                self.restore.remove(&key);
                continue;
            }
            if self.history[0]
                .streams
                .get(&key)
                .is_some_and(|r| r.pending.is_some())
            {
                continue;
            }
            match self.router.subscription(&key) {
                Ok(raw) => match self.enqueue(Side::Out, raw, false) {
                    Ok(()) => {}
                    Err(DeliveryError::Capacity) => self.retire(&key, None)?,
                    Err(e) => return Err(e),
                },
                Err(RoutingError::Sequence | RoutingError::Retired) => self.retire(&key, None)?,
                Err(e) => return Err(e.into()),
            }
            self.restore.remove(&key);
        }
        Ok(())
    }
    pub fn maintain(&mut self, now: Instant, ping: bool) -> Result<(), DeliveryError> {
        for side in [Side::In, Side::Out] {
            if let Some(gap) = &self.history[side.index()].gap
                && now >= gap.deadline
            {
                let stream = gap.stream.clone();
                let key = if side == Side::In {
                    Some(stream.clone())
                } else {
                    self.router.source(&stream.stream).cloned()
                };
                if let Some(key) = key {
                    self.retire(&key, None)?;
                } else {
                    self.history[side.index()].forget(&stream);
                }
            }
        }
        if ping {
            for key in self.router.keys() {
                let pong = *self.pong_at.entry(key.clone()).or_insert(now);
                let source = *self.source_at.entry(key.clone()).or_insert(now);
                if now.duration_since(pong) >= Duration::from_secs(600)
                    || now.duration_since(source) >= Duration::from_secs(600)
                {
                    self.retire(&key, None)?;
                } else {
                    self.restore.insert(key);
                }
            }
            self.restore_subscriptions()?;
        }
        Ok(())
    }
    pub async fn receive(
        &mut self,
        side: Side,
        raw: &str,
        now: Instant,
    ) -> Result<Outcome, DeliveryError> {
        let frame = Envelope::parse(side, raw)?;
        if side == Side::Out {
            frame.check_owner(&self.router.client, &self.router.environment)?;
        }
        if frame.kind() == Kind::Ack {
            self.outboxes[side.index()].acknowledge(&frame)?;
            return Ok(Outcome::Ack);
        }
        let stream = StreamKey::from_envelope(&frame);
        let key = if side == Side::In {
            Some(stream.clone())
        } else {
            self.router.source(&stream.stream).cloned()
        };
        let index = side.index();
        let Some(key) = key else {
            self.history[index].checkpoint(&frame)?;
            return Ok(Outcome::Unmapped);
        };
        let mut receiver = self.history[index]
            .streams
            .get(&stream)
            .cloned()
            .unwrap_or_default();
        let classification = receiver.classify(&frame)?;
        let unknown = side == Side::Out
            && frame.kind() == Kind::Pong
            && frame.required::<String>("status")? == "unknown";
        if matches!(classification, Classification::Duplicate)
            && (!unknown || !receiver.established)
        {
            self.history[index].checkpoint(&frame)?;
            return Ok(Outcome::Duplicate);
        }
        if matches!(classification, Classification::Gap) && !unknown {
            if let Some(cursor) = receiver.cursor.clone()
                && self.history[index].gap.is_none()
            {
                self.history[index].gap = Some(Gap {
                    stream,
                    cursor,
                    seq: frame.seq(),
                    deadline: now + Duration::from_secs(30),
                });
                return Err(DeliveryError::ReplayGap);
            }
            if side == Side::In && self.router.mapped(&key).is_none() {
                // This source has never reached incoming(), so no durable claim exists yet.
                // Record rejection before emitting unknown; later frames must not revive it.
                self.router.claim_source(&key).await?;
            }
            self.retire(&key, None)?;
            self.history[index].checkpoint(&frame)?;
            return Ok(Outcome::Retired);
        }
        let mapped = self.router.mapped(&key).map(str::to_owned);
        let translated = if side == Side::In {
            self.router.incoming(frame.clone()).await
        } else {
            self.router.outgoing(frame.clone())
        };
        let translated = match translated {
            Ok(Some(raw)) => raw,
            Ok(None) => {
                self.history[index].streams.remove(&stream);
                self.history[index].checkpoint(&frame)?;
                return Ok(Outcome::Unmapped);
            }
            Err(
                RoutingError::Retired
                | RoutingError::Capacity
                | RoutingError::Sequence
                | RoutingError::Wire(WireError::FrameLimit),
            ) => {
                self.retire(&key, None)?;
                self.history[index].checkpoint(&frame)?;
                return Ok(Outcome::Retired);
            }
            Err(error) => return Err(error.into()),
        };
        if unknown && receiver.established {
            self.retire(&key, Some(translated))?;
            self.history[index].checkpoint(&frame)?;
            return Ok(Outcome::Retired);
        }
        self.pong_at.entry(key.clone()).or_insert(now);
        if side == Side::Out && frame.kind() == Kind::Pong {
            self.pong_at.insert(key.clone(), now);
        }
        if side == Side::In {
            self.source_at.insert(key.clone(), now);
        }
        let closed = frame.kind() == Kind::ClientClosed;
        if closed {
            self.forget(&key, mapped);
        }
        match self.enqueue(side.other(), translated, closed) {
            Ok(()) => {}
            Err(DeliveryError::Capacity) => {
                self.retire(&key, None)?;
                self.history[index].checkpoint(&frame)?;
                return Ok(Outcome::Retired);
            }
            Err(error) => return Err(error),
        }
        receiver.accept(&frame)?;
        let alive = side == Side::Out
            && (matches!(frame.kind(), Kind::ServerMessage | Kind::ServerChunk)
                || (frame.kind() == Kind::Pong && frame.required::<String>("status")? == "active"));
        if alive {
            receiver.established = true;
        }
        let activity = alive && receiver.pending.is_none();
        if self.history[index]
            .gap
            .as_ref()
            .is_some_and(|g| g.stream == stream && receiver.seq.is_some_and(|s| s >= g.seq))
        {
            self.history[index].gap = None;
        }
        if !closed {
            self.history[index].streams.insert(stream, receiver);
        }
        self.history[index].checkpoint(&frame)?;
        self.restore_subscriptions()?;
        Ok(Outcome::Forwarded { activity })
    }
}
