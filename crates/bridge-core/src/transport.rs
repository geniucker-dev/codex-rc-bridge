//! WebSocket transport with bounded writers, relay-local ACKs and structured shutdown.
use crate::{
    delivery::{Delivery, DeliveryError, Outcome},
    presence::Presence,
    wire::{MAX_FRAME, Side, WireError},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use std::sync::Arc;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    sync::{Mutex, Notify, mpsc, watch},
    time::{Duration, Instant, timeout},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{
        self, Message, client::IntoClientRequest, http::HeaderValue, protocol::WebSocketConfig,
    },
};
use tokio_util::sync::CancellationToken;

pub type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("WebSocket network operation failed")]
    Network,
    #[error("WebSocket handshake rejected on {side:?} (HTTP {status})")]
    Handshake { side: Side, status: u16 },
    #[error("host WebSocket registration no longer exists")]
    HostRegistrationMissing,
    #[error("HTTP proxy rejected the tunnel (HTTP {0})")]
    Proxy(u16),
    #[error("WebSocket closed with code {0}")]
    Closed(u16),
    #[error("WebSocket configuration or proof is invalid")]
    Invalid,
    #[error("target is offline or presence evidence expired")]
    Offline,
    #[error(transparent)]
    Delivery(#[from] DeliveryError),
}
impl TransportError {
    pub fn retryable(&self) -> bool {
        matches!(
            self,
            Self::Network
                | Self::Offline
                | Self::Closed(1000 | 1001 | 1005 | 1006 | 1011 | 1012 | 1013 | 1014)
                | Self::Proxy(408 | 429 | 500..=599)
                | Self::Handshake {
                    status: 408 | 429 | 500..=599,
                    ..
                }
                | Self::Handshake {
                    side: Side::In,
                    status: 401 | 403 | 404,
                }
                | Self::Delivery(
                    DeliveryError::ReplayGap | DeliveryError::Wire(WireError::OwnerMismatch)
                )
        )
    }
}
pub(crate) fn socket_error(error: tungstenite::Error, side: Side) -> TransportError {
    match error {
        tungstenite::Error::Http(response) => {
            let status = response.status().as_u16();
            let missing = if side == Side::In && status == 404 {
                response.body().as_ref()
                    .filter(|body| body.len() <= 8192)
                    .and_then(|body| serde_json::from_slice::<serde_json::Value>(body).ok())
                    .is_some_and(|body| {
                        body.get("detail").and_then(|v| v.as_str())
                            == Some("Remote app server not found")
                    })
            } else {
                false
            };
            if missing {
                TransportError::HostRegistrationMissing
            } else {
                TransportError::Handshake { side, status }
            }
        }
        tungstenite::Error::ConnectionClosed
        | tungstenite::Error::AlreadyClosed
        // Tungstenite reports EOF without a Close frame as a protocol error.
        // It is a lost transport, not evidence of a malformed peer message.
        | tungstenite::Error::Protocol(tungstenite::error::ProtocolError::ResetWithoutClosingHandshake)
        | tungstenite::Error::Io(_)
        | tungstenite::Error::Tls(_) => TransportError::Network,
        _ => TransportError::Invalid,
    }
}

pub fn socket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_FRAME))
        .max_frame_size(Some(MAX_FRAME))
        .write_buffer_size(0)
        .max_write_buffer_size(MAX_FRAME * 2)
}
pub(crate) async fn connect(
    side: Side,
    endpoint: &str,
    headers: Vec<(&'static str, String)>,
    proxy: Option<&str>,
) -> Result<Socket, TransportError> {
    let url = url::Url::parse(endpoint).map_err(|_| TransportError::Invalid)?;
    let mut request = endpoint
        .into_client_request()
        .map_err(|_| TransportError::Invalid)?;
    request
        .headers_mut()
        .insert("originator", HeaderValue::from_static("codex_rc_bridge"));
    request.headers_mut().insert(
        "user-agent",
        HeaderValue::from_str(&crate::upstream::user_agent())
            .map_err(|_| TransportError::Invalid)?,
    );
    for (name, value) in headers {
        let mut value = HeaderValue::from_str(&value).map_err(|_| TransportError::Invalid)?;
        value.set_sensitive(true);
        let name = tungstenite::http::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| TransportError::Invalid)?;
        request.headers_mut().insert(name, value);
    }
    timeout(Duration::from_secs(15), async {
        let stream = match proxy {
            None => connect_url(&url).await?,
            Some(proxy) => proxy_tunnel(proxy).await?,
        };
        stream
            .set_nodelay(true)
            .map_err(|_| TransportError::Network)?;
        let (socket, _) = tokio_tungstenite::client_async_tls_with_config(
            request,
            stream,
            Some(socket_config()),
            None,
        )
        .await
        .map_err(|e| socket_error(e, side))?;
        Ok(socket)
    })
    .await
    .map_err(|_| TransportError::Network)?
}
async fn connect_url(url: &url::Url) -> Result<TcpStream, TransportError> {
    let port = url.port_or_known_default().ok_or(TransportError::Invalid)?;
    // host_str() includes IPv6 brackets; DNS lookup of that string fails.
    let stream = match url.host().ok_or(TransportError::Invalid)? {
        url::Host::Ipv4(ip) => TcpStream::connect(std::net::SocketAddr::from((ip, port))).await,
        url::Host::Ipv6(ip) => TcpStream::connect(std::net::SocketAddr::from((ip, port))).await,
        url::Host::Domain(host) => TcpStream::connect((host, port)).await,
    };
    stream.map_err(|_| TransportError::Network)
}
async fn proxy_tunnel(proxy: &str) -> Result<TcpStream, TransportError> {
    let url = url::Url::parse(proxy).map_err(|_| TransportError::Invalid)?;
    if url.scheme() != "http" || url.query().is_some() || url.fragment().is_some() {
        return Err(TransportError::Invalid);
    }
    let mut stream = connect_url(&url).await?;
    let mut request = zeroize::Zeroizing::new(String::from(
        "CONNECT chatgpt.com:443 HTTP/1.1\r\nHost: chatgpt.com:443\r\n",
    ));
    if !url.username().is_empty() || url.password().is_some() {
        let user = percent_encoding::percent_decode_str(url.username())
            .decode_utf8()
            .map_err(|_| TransportError::Invalid)?;
        let pass = percent_encoding::percent_decode_str(url.password().unwrap_or(""))
            .decode_utf8()
            .map_err(|_| TransportError::Invalid)?;
        let credentials = zeroize::Zeroizing::new(format!("{user}:{pass}"));
        request.push_str("Proxy-Authorization: Basic ");
        request.push_str(&STANDARD.encode(credentials.as_bytes()));
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|_| TransportError::Network)?;
    let mut response = Vec::new();
    // Read exactly through the CONNECT headers, never consume TLS bytes ahead of the TLS client.
    while response.len() < 16384 {
        response.push(
            stream
                .read_u8()
                .await
                .map_err(|_| TransportError::Network)?,
        );
        if response.ends_with(b"\r\n\r\n") {
            let line = std::str::from_utf8(&response)
                .map_err(|_| TransportError::Invalid)?
                .lines()
                .next()
                .ok_or(TransportError::Invalid)?;
            let mut fields = line.split_whitespace();
            if !matches!(fields.next(), Some("HTTP/1.0" | "HTTP/1.1")) {
                return Err(TransportError::Invalid);
            }
            let status = fields
                .next()
                .and_then(|s| s.parse::<u16>().ok())
                .ok_or(TransportError::Invalid)?;
            if status != 200 {
                return Err(TransportError::Proxy(status));
            }
            return Ok(stream);
        }
    }
    Err(TransportError::Invalid)
}

type PingState = Arc<Mutex<Option<(Vec<u8>, Instant)>>>;
pub async fn relay<A, B>(
    input: WebSocketStream<A>,
    output: WebSocketStream<B>,
    delivery: Arc<Mutex<Delivery>>,
    presence: watch::Sender<Presence>,
    stop: CancellationToken,
) -> Result<(), TransportError>
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    let (mut in_writer, in_reader) = input.split();
    let (mut out_writer, out_reader) = output.split();
    let changed = [Arc::new(Notify::new()), Arc::new(Notify::new())];
    let (in_control, in_commands) = mpsc::channel(16);
    let (out_control, out_commands) = mpsc::channel(16);
    let in_ping = Arc::new(Mutex::new(None));
    let out_ping = Arc::new(Mutex::new(None));
    let result=async {
        delivery.lock().await.reconnect()?;
        let mut online=presence.subscribe();
        tokio::select! {
            biased;
            _=stop.cancelled()=>Ok(()),
            _=online.wait_for(|p|!p.online())=>Err(TransportError::Offline),
            result=receive(Side::In,in_reader,delivery.clone(),changed.clone(),presence.clone(),in_control,in_ping.clone())=>result,
            result=receive(Side::Out,out_reader,delivery.clone(),changed.clone(),presence.clone(),out_control,out_ping.clone())=>result,
            result=publish(Side::In,&mut in_writer,delivery.clone(),changed[0].clone(),in_commands,in_ping)=>result,
            result=publish(Side::Out,&mut out_writer,delivery.clone(),changed[1].clone(),out_commands,out_ping)=>result,
            result=maintenance(delivery.clone(),changed.clone())=>result,
        }
    }.await;
    if result.as_ref().err().is_none_or(|e| !e.retryable()) {
        // Retirement must happen even if the sockets have already failed. It prevents future replay.
        delivery.lock().await.retire_all()?;
        let _ = timeout(Duration::from_secs(2), async {
            loop {
                let next = delivery.lock().await.outboxes[1].next_frame();
                let Some((id, segment, raw)) = next else {
                    break;
                };
                out_writer
                    .send(Message::Text(raw.into()))
                    .await
                    .map_err(|e| socket_error(e, Side::Out))?;
                delivery.lock().await.outboxes[1].sent(&id, segment);
            }
            Ok::<_, TransportError>(())
        })
        .await;
    }
    let _ = timeout(Duration::from_secs(5), async {
        tokio::join!(in_writer.close(), out_writer.close())
    })
    .await;
    result
}
async fn receive<S>(
    side: Side,
    mut reader: SplitStream<WebSocketStream<S>>,
    delivery: Arc<Mutex<Delivery>>,
    changed: [Arc<Notify>; 2],
    presence: watch::Sender<Presence>,
    control: mpsc::Sender<Message>,
    ping: PingState,
) -> Result<(), TransportError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    while let Some(message) = reader.next().await {
        match message.map_err(|e| socket_error(e, side))? {
            Message::Text(raw) => {
                let result = delivery
                    .lock()
                    .await
                    .receive(side, &raw, Instant::now())
                    .await;
                match result {
                    Err(DeliveryError::Wire(WireError::Malformed)) => continue,
                    Err(error) => return Err(error.into()),
                    Ok(Outcome::Forwarded { activity: true }) => {
                        presence.send_modify(|p| p.activity(Instant::now()));
                    }
                    _ => {}
                }
                for notification in &changed {
                    notification.notify_one();
                }
            }
            Message::Ping(data) => control
                .send(Message::Pong(data))
                .await
                .map_err(|_| TransportError::Network)?,
            Message::Pong(data) => {
                let mut outstanding = ping.lock().await;
                if outstanding
                    .as_ref()
                    .is_some_and(|(payload, _)| payload.as_slice() == data.as_ref())
                {
                    *outstanding = None;
                }
            }
            Message::Close(frame) => {
                return Err(TransportError::Closed(
                    frame.map_or(1005, |f| f.code.into()),
                ));
            }
            Message::Binary(_) | Message::Frame(_) => {}
        }
    }
    Err(TransportError::Network)
}
async fn publish<S>(
    side: Side,
    writer: &mut SplitSink<WebSocketStream<S>, Message>,
    delivery: Arc<Mutex<Delivery>>,
    changed: Arc<Notify>,
    mut control: mpsc::Receiver<Message>,
    ping: PingState,
) -> Result<(), TransportError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut ticker = tokio::time::interval(Duration::from_secs(10));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let next = delivery.lock().await.outboxes[side.index()].next_frame();
        let (message, publication) = tokio::select! {
            biased;
            Some(command)=control.recv()=>(command,None),
            _=ticker.tick()=>{
                let mut outstanding=ping.lock().await;
                if let Some((_,sent))=&*outstanding {
                    if sent.elapsed()>=Duration::from_secs(60){return Err(TransportError::Network);}continue;
                }
                let nonce=uuid::Uuid::new_v4().as_bytes().to_vec();*outstanding=Some((nonce.clone(),Instant::now()));
                (Message::Ping(nonce.into()),None)
            }
            _=std::future::ready(()),if next.is_some()=>{let (id,segment,raw)=next.expect("guarded frame");(Message::Text(raw.into()),Some((id,segment)))},
            _=changed.notified()=>continue,
        };
        timeout(Duration::from_secs(15), writer.send(message))
            .await
            .map_err(|_| TransportError::Network)?
            .map_err(|e| socket_error(e, side))?;
        if let Some((id, segment)) = publication {
            delivery.lock().await.outboxes[side.index()].sent(&id, segment);
        }
    }
}
async fn maintenance(
    delivery: Arc<Mutex<Delivery>>,
    changed: [Arc<Notify>; 2],
) -> Result<(), TransportError> {
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    let mut next_ping = Instant::now() + Duration::from_secs(30);
    loop {
        ticker.tick().await;
        let now = Instant::now();
        let ping = now >= next_ping;
        if ping {
            next_ping = now + Duration::from_secs(30);
        }
        delivery.lock().await.maintain(now, ping)?;
        for notification in &changed {
            notification.notify_one();
        }
    }
}

#[cfg(test)]
mod error_tests {
    use super::*;
    #[tokio::test]
    async fn proxy_connect_supports_ipv6_ipv4_and_domain_hosts() {
        for host in ["[::1]", "127.0.0.1", "localhost"] {
            let bind = if host == "[::1]" {
                "[::1]:0"
            } else {
                "127.0.0.1:0"
            };
            let listener = tokio::net::TcpListener::bind(bind).await.unwrap();
            let proxy = format!("http://{host}:{}", listener.local_addr().unwrap().port());
            // Exercise the same validation used by saved deployment settings.
            crate::upstream::Upstream::new(Some(&proxy)).unwrap();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    request.push(stream.read_u8().await.unwrap());
                }
                assert_eq!(
                    request,
                    b"CONNECT chatgpt.com:443 HTTP/1.1\r\nHost: chatgpt.com:443\r\n\r\n"
                );
                stream
                    .write_all(b"HTTP/1.1 200 Connection established\r\n\r\ntunnel-data")
                    .await
                    .unwrap();
            });
            let result = timeout(Duration::from_secs(3), proxy_tunnel(&proxy)).await;
            if !matches!(&result, Ok(Ok(_))) {
                server.abort();
            }
            let mut tunnel = result.unwrap().unwrap();
            let mut data = [0; 11];
            tunnel.read_exact(&mut data).await.unwrap();
            assert_eq!(&data, b"tunnel-data");
            server.await.unwrap();
        }
    }
    #[test]
    fn host_handshake_recovery_is_specific_to_status_body_and_side() {
        let missing = br#"{"detail":"Remote app server not found"}"#.to_vec();
        for side in [Side::In, Side::Out] {
            for status in [401, 403, 404, 500] {
                for body in [
                    Some(missing.clone()),
                    None,
                    Some(b"not json".to_vec()),
                    Some(vec![b' '; 8193]),
                ] {
                    let recognized =
                        side == Side::In && status == 404 && body.as_ref() == Some(&missing);
                    let response = tungstenite::http::Response::builder()
                        .status(status)
                        .body(body)
                        .unwrap();
                    let error = socket_error(tungstenite::Error::Http(Box::new(response)), side);
                    assert_eq!(
                        matches!(error, TransportError::HostRegistrationMissing),
                        recognized
                    );
                    assert_eq!(
                        error.retryable(),
                        !recognized && (side == Side::In || status == 500)
                    );
                }
            }
        }
    }
    #[test]
    fn unexpected_eof_retries_but_malformed_frames_remain_terminal() {
        use tungstenite::{Error, error::ProtocolError};
        for side in [Side::In, Side::Out] {
            assert!(matches!(
                socket_error(
                    Error::Protocol(ProtocolError::ResetWithoutClosingHandshake),
                    side
                ),
                TransportError::Network
            ));
            let malformed = socket_error(Error::Protocol(ProtocolError::InvalidOpcode(3)), side);
            assert!(matches!(malformed, TransportError::Invalid));
            assert!(!malformed.retryable());
        }
    }
}
