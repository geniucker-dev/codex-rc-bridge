//! Transport identity mapping. The ledger claims source streams before any publication.
use crate::{
    device::digest_url,
    storage::SqliteStore,
    wire::{Envelope, Kind, Side, WireError},
};
use std::{collections::HashMap, sync::Arc};
use uuid::Uuid;

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct StreamKey {
    pub client: String,
    pub stream: String,
}
impl StreamKey {
    pub fn from_envelope(envelope: &Envelope) -> Self {
        Self {
            client: envelope.client().into(),
            stream: envelope.stream().into(),
        }
    }
}
struct Route {
    mapped: String,
    sent_seq: u64,
    translated: Option<(u64, u64)>,
}
#[derive(Debug, thiserror::Error)]
pub enum RoutingError {
    #[error(transparent)]
    Wire(#[from] WireError),
    #[error("stream history storage failed")]
    Storage,
    #[error("source stream was retired")]
    Retired,
    #[error("active stream capacity exhausted")]
    Capacity,
    #[error("stream sequence exhausted")]
    Sequence,
}
pub struct Router {
    store: Arc<SqliteStore>,
    tenant: Uuid,
    input: Uuid,
    pub client: String,
    pub environment: String,
    routes: HashMap<StreamKey, Route>,
    reverse: HashMap<String, StreamKey>,
    max_streams: usize,
}
impl Router {
    pub fn new(
        store: Arc<SqliteStore>,
        tenant: Uuid,
        input: Uuid,
        client: String,
        environment: String,
    ) -> Self {
        Self {
            store,
            tenant,
            input,
            client,
            environment,
            routes: HashMap::new(),
            reverse: HashMap::new(),
            max_streams: 256,
        }
    }
    pub fn keys(&self) -> Vec<StreamKey> {
        self.routes.keys().cloned().collect()
    }
    pub fn mapped(&self, key: &StreamKey) -> Option<&str> {
        self.routes.get(key).map(|r| r.mapped.as_str())
    }
    pub fn source(&self, mapped: &str) -> Option<&StreamKey> {
        self.reverse.get(mapped)
    }
    /// Reserve a source before either forwarding it or rejecting an unrecoverable first frame.
    pub(crate) async fn claim_source(&self, key: &StreamKey) -> Result<bool, RoutingError> {
        let hash = digest_url(
            &serde_json::to_vec(&[&key.client, &key.stream]).map_err(|_| RoutingError::Storage)?,
        );
        self.store
            .claim_stream(self.tenant, self.input, &hash)
            .await
            .map_err(|_| RoutingError::Storage)
    }
    pub fn forget(&mut self, key: &StreamKey) {
        if let Some(route) = self.routes.remove(key) {
            self.reverse.remove(&route.mapped);
        }
    }
    pub fn terminate(&mut self, key: &StreamKey) -> Vec<(Side, String)> {
        let mut notices = vec![(Side::In,serde_json::json!({"type":"pong","client_id":key.client,"stream_id":key.stream,"seq_id":0,"status":"unknown","skip_history":true}).to_string())];
        if let Some(route) = self.routes.get(key)
            && let Some(seq) = route.sent_seq.checked_add(1)
        {
            notices.push((Side::Out,serde_json::json!({"type":"client_closed","client_id":self.client,"env_id":self.environment,"stream_id":route.mapped,"seq_id":seq}).to_string()));
        }
        self.forget(key);
        notices
    }
    pub async fn incoming(&mut self, frame: Envelope) -> Result<Option<String>, RoutingError> {
        if frame.kind() == Kind::Ack {
            return Ok(None);
        }
        let key = StreamKey::from_envelope(&frame);
        if !self.routes.contains_key(&key) {
            if !matches!(
                frame.kind(),
                Kind::ClientMessage | Kind::ClientChunk | Kind::Ping
            ) {
                return Ok(None);
            }
            if !self.claim_source(&key).await? {
                return Err(RoutingError::Retired);
            }
            if self.routes.len() >= self.max_streams {
                return Err(RoutingError::Capacity);
            }
            let mapped = Uuid::new_v4().to_string();
            self.reverse.insert(mapped.clone(), key.clone());
            self.routes.insert(
                key.clone(),
                Route {
                    mapped,
                    sent_seq: 0,
                    translated: None,
                },
            );
        }
        let route = self.routes.get_mut(&key).ok_or(RoutingError::Retired)?;
        let seq = match route.translated {
            Some((source, target)) if source == frame.seq() => target,
            _ => {
                let seq = frame.seq().max(
                    route
                        .sent_seq
                        .checked_add(1)
                        .ok_or(RoutingError::Sequence)?,
                );
                route.translated = Some((frame.seq(), seq));
                route.sent_seq = seq;
                seq
            }
        };
        let closed = frame.kind() == Kind::ClientClosed;
        let raw = frame.translate(&self.client, &route.mapped, Some(&self.environment), seq)?;
        if closed {
            self.forget(&key);
        }
        Ok(Some(raw))
    }
    pub fn outgoing(&self, frame: Envelope) -> Result<Option<String>, RoutingError> {
        frame.check_owner(&self.client, &self.environment)?;
        if frame.kind() == Kind::Ack {
            return Ok(None);
        }
        let Some(key) = self.source(frame.stream()) else {
            return Ok(None);
        };
        let seq = frame.seq();
        Ok(Some(frame.translate(
            &key.client,
            &key.stream,
            None,
            seq,
        )?))
    }
    pub fn subscription(&mut self, key: &StreamKey) -> Result<String, RoutingError> {
        let route = self.routes.get_mut(key).ok_or(RoutingError::Retired)?;
        route.sent_seq = route
            .sent_seq
            .checked_add(1)
            .ok_or(RoutingError::Sequence)?;
        Ok(serde_json::json!({"type":"ping","client_id":self.client,"env_id":self.environment,"stream_id":route.mapped,
            "seq_id":route.sent_seq,"state":"foreground","skip_history":true}).to_string())
    }
}
