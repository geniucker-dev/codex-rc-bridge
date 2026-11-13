//! Outer transport envelope only. Business payloads, chunk strings, duplicate inner
//! fields, numeric spellings and future extension values remain raw JSON.
use serde::{
    Deserialize,
    de::{self, MapAccess, Visitor},
};
use serde_json::value::RawValue;
use std::collections::HashSet;

pub const MAX_FRAME: usize = 153600;
pub const MAX_SEGMENTS: u64 = 10486;
pub const MAX_MESSAGE_BYTES: u64 = 1 << 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    In,
    Out,
}
impl Side {
    pub fn index(self) -> usize {
        match self {
            Self::In => 0,
            Self::Out => 1,
        }
    }
    pub fn other(self) -> Self {
        match self {
            Self::In => Self::Out,
            Self::Out => Self::In,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    ClientMessage,
    ClientChunk,
    ServerMessage,
    ServerChunk,
    Ack,
    Ping,
    Pong,
    ClientClosed,
}
impl Kind {
    pub fn is_chunk(self) -> bool {
        matches!(self, Self::ClientChunk | Self::ServerChunk)
    }
}
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WireError {
    #[error("malformed transport envelope")]
    Malformed,
    #[error("unknown or unexpected transport control type")]
    Unknown,
    #[error("transport envelope belongs to a different controller or environment")]
    OwnerMismatch,
    #[error("translated transport frame exceeds the frame limit")]
    FrameLimit,
}

// Intentionally not Debug: envelopes contain conversation data.
#[derive(Clone)]
pub struct Envelope {
    fields: Vec<(String, Box<RawValue>)>,
    kind: Kind,
    client: String,
    stream: String,
    seq: u64,
}
struct RawFields(Vec<(String, Box<RawValue>)>);
impl<'de> Deserialize<'de> for RawFields {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct FieldsVisitor;
        impl<'de> Visitor<'de> for FieldsVisitor {
            type Value = RawFields;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an outer JSON object without duplicate keys")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut keys = HashSet::new();
                let mut fields = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    if !keys.insert(key.clone()) {
                        return Err(de::Error::custom("duplicate outer field"));
                    }
                    fields.push((key, map.next_value::<Box<RawValue>>()?));
                }
                Ok(RawFields(fields))
            }
        }
        deserializer.deserialize_map(FieldsVisitor)
    }
}
impl Envelope {
    pub fn parse(side: Side, raw: &str) -> Result<Self, WireError> {
        Self::parse_inner(side, raw, true)
    }
    pub fn publication(side: Side, raw: &str) -> Result<Self, WireError> {
        Self::parse_inner(side.other(), raw, false)
    }
    fn parse_inner(side: Side, raw: &str, require_environment: bool) -> Result<Self, WireError> {
        if raw.len() > MAX_FRAME {
            return Err(WireError::Malformed);
        }
        let RawFields(fields) = serde_json::from_str(raw).map_err(|_| WireError::Malformed)?;
        let mut envelope = Self {
            fields,
            kind: Kind::Ack,
            client: String::new(),
            stream: String::new(),
            seq: 0,
        };
        let kind: String = envelope.required("type")?;
        envelope.kind = match (side, kind.as_str()) {
            (_, "ack") => Kind::Ack,
            (Side::In, "client_message") => Kind::ClientMessage,
            (Side::In, "client_message_chunk") => Kind::ClientChunk,
            (Side::In, "ping") => Kind::Ping,
            (Side::In, "client_closed") => Kind::ClientClosed,
            (Side::Out, "server_message") => Kind::ServerMessage,
            (Side::Out, "server_message_chunk") => Kind::ServerChunk,
            (Side::Out, "pong") => Kind::Pong,
            _ => return Err(WireError::Unknown),
        };
        envelope.client = envelope.required("client_id")?;
        envelope.stream = envelope.required("stream_id")?;
        for value in [&envelope.client, &envelope.stream] {
            if value.is_empty() || value.chars().count() > 256 {
                return Err(WireError::Malformed);
            }
        }
        envelope.seq = envelope.required("seq_id")?;
        if (require_environment && side == Side::Out) || envelope.raw("env_id").is_some() {
            envelope.required::<String>("env_id")?;
        }
        if envelope.raw("cursor").is_some() {
            envelope.required::<Option<String>>("cursor")?;
        }
        if envelope.raw("skip_history").is_some() {
            envelope.required::<bool>("skip_history")?;
        }
        if envelope.kind == Kind::Ping && envelope.raw("state").is_some() {
            envelope.required::<String>("state")?;
        }
        if envelope.kind == Kind::Pong
            && !matches!(
                envelope.required::<String>("status")?.as_str(),
                "active" | "unknown"
            )
        {
            return Err(WireError::Malformed);
        }
        if envelope.raw("segment_id").is_some() {
            envelope.required::<u64>("segment_id")?;
        }
        if envelope.raw("segment_count").is_some()
            && envelope.required::<u64>("segment_count")? == 0
        {
            return Err(WireError::Malformed);
        }
        if envelope.kind.is_chunk() {
            let segment: u64 = envelope.required("segment_id")?;
            let count: u64 = envelope.required("segment_count")?;
            let size: u64 = envelope.required("message_size_bytes")?;
            let chunk: String = envelope.required("message_chunk_base64")?;
            if !(2..=MAX_SEGMENTS).contains(&count)
                || segment >= count
                || size == 0
                || size > MAX_MESSAGE_BYTES
                || chunk.is_empty()
            {
                return Err(WireError::Malformed);
            }
        }
        Ok(envelope)
    }
    pub fn kind(&self) -> Kind {
        self.kind
    }
    pub fn client(&self) -> &str {
        &self.client
    }
    pub fn stream(&self) -> &str {
        &self.stream
    }
    pub fn seq(&self) -> u64 {
        self.seq
    }
    pub fn raw(&self, field: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == field)
            .map(|(_, v)| v.get())
    }
    pub fn required<T: serde::de::DeserializeOwned>(&self, field: &str) -> Result<T, WireError> {
        serde_json::from_str(self.raw(field).ok_or(WireError::Malformed)?)
            .map_err(|_| WireError::Malformed)
    }
    pub fn check_owner(&self, client: &str, environment: &str) -> Result<(), WireError> {
        if self.client != client || self.required::<String>("env_id")? != environment {
            return Err(WireError::OwnerMismatch);
        }
        Ok(())
    }
    /// Publication ACKs are consumed by their local outbox; translating one is a programming error.
    pub fn translate(
        mut self,
        client: &str,
        stream: &str,
        environment: Option<&str>,
        seq: u64,
    ) -> Result<String, WireError> {
        if self.kind == Kind::Ack {
            return Err(WireError::Unknown);
        }
        self.set("client_id", client)?;
        self.set("stream_id", stream)?;
        self.set("seq_id", seq)?;
        self.fields
            .retain(|(k, _)| k != "cursor" && (environment.is_some() || k != "env_id"));
        if let Some(env) = environment {
            self.set("env_id", env)?;
        }
        let mut output = String::from("{");
        for (index, (name, value)) in self.fields.iter().enumerate() {
            if index != 0 {
                output.push(',');
            }
            output.push_str(&serde_json::to_string(name).map_err(|_| WireError::Malformed)?);
            output.push(':');
            output.push_str(value.get());
        }
        output.push('}');
        if output.len() > MAX_FRAME {
            return Err(WireError::FrameLimit);
        }
        Ok(output)
    }
    fn set(&mut self, field: &str, value: impl serde::Serialize) -> Result<(), WireError> {
        let value = serde_json::value::to_raw_value(&value).map_err(|_| WireError::Malformed)?;
        if let Some((_, old)) = self.fields.iter_mut().find(|(k, _)| k == field) {
            *old = value;
        } else {
            self.fields.push((field.to_owned(), value));
        }
        Ok(())
    }
}
