//! Invalidation-only SSE: clients fetch the latest authorized snapshot, never replay old targets.
use super::*;
use axum::response::sse::{Event, Sse};
use futures_util::{Stream, stream};
use std::{convert::Infallible, time::Duration};
use tokio::{
    sync::{OwnedSemaphorePermit, watch},
    time::{Interval, MissedTickBehavior},
};
use tokio_util::sync::CancellationToken;

struct Subscription {
    auth: Arc<ManagementAuth>,
    token: SecretString,
    tenant: Uuid,
    changes: watch::Receiver<()>,
    stop: CancellationToken,
    heartbeat: Interval,
    first: bool,
    done: bool,
    _global: OwnedSemaphorePermit,
    _tenant: OwnedSemaphorePermit,
}
pub(super) async fn subscribe(
    State(state): State<ApiState>,
    Extension(identity): Extension<SessionIdentity>,
    headers: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let controller = state.controller(&identity)?;
    let global = state
        .streams
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::unavailable())?;
    let tenant = state
        .registry
        .streams(identity.tenant_id)?
        .try_acquire_owned()
        .map_err(|_| ApiError::unavailable())?;
    let mut heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(5),
        Duration::from_secs(5),
    );
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let subscription = Subscription {
        auth: state.auth,
        token: SecretString::new(cookie_token(&headers)?.to_owned()),
        tenant: identity.tenant_id,
        changes: controller.subscribe(),
        stop: controller.shutdown(),
        heartbeat,
        first: true,
        done: false,
        _global: global,
        _tenant: tenant,
    };
    Ok(Sse::new(stream::unfold(
        subscription,
        |mut sub| async move {
            if sub.done || sub.stop.is_cancelled() {
                return None;
            }
            let event = if sub.first {
                sub.first = false;
                "changed"
            } else {
                tokio::select! {
                    biased;
                    _ = sub.stop.cancelled() => return None,
                    result = sub.changes.changed() => {
                        if result.is_err() { return None; }
                        "changed"
                    },
                    _ = sub.heartbeat.tick() => "heartbeat",
                }
            };
            // Recheck before every event, including heartbeats. Logout, expiry and password
            // reset affect existing streams; the raw cookie never enters an event or URL.
            let valid = tokio::select! {
                biased;
                _ = sub.stop.cancelled() => return None,
                result = tokio::time::timeout(Duration::from_secs(5), sub.auth.session(sub.token.expose(), unix_time())) => result,
            };
            let event = match valid {
                Ok(Ok(identity)) if identity.tenant_id == sub.tenant => event,
                Ok(Err(ManagementError::Unauthorized)) => {
                    sub.done = true;
                    "session_expired"
                }
                _ => {
                    sub.done = true;
                    "unavailable"
                }
            };
            Some((Ok(Event::default().event(event).data("{}")), sub))
        },
    )))
}
