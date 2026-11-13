//! Safe connection observations, with attribution supplied at the failing operation.
use super::RuntimeError;
use crate::{auth::AuthError, transport::TransportError, upstream::ControlError, wire::Side};
use tokio::sync::watch;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultSide {
    Controller,
    Device,
}
impl From<Side> for FaultSide {
    fn from(side: Side) -> Self {
        match side {
            Side::In => Self::Controller,
            Side::Out => Self::Device,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionStatus {
    CheckingTarget,
    TargetOffline,
    Connecting,
    Connected,
    Reconnecting,
    Stopped,
    SignInRequired,
    DeviceAuthorizationRequired,
    ControllerRegistrationRequired,
    AccessDenied,
    InvalidState,
    ProtocolError,
    Failed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Observation {
    pub connection: ConnectionStatus,
    pub side: Option<FaultSide>,
}
impl From<ConnectionStatus> for Observation {
    fn from(connection: ConnectionStatus) -> Self {
        Self {
            connection,
            side: None,
        }
    }
}
#[derive(Clone)]
pub struct ConnectionReporter {
    state: watch::Sender<Observation>,
    changes: watch::Sender<()>,
}
impl ConnectionReporter {
    pub(crate) fn refresh(&self) {
        self.changes.send_replace(());
    }
    pub fn new(changes: watch::Sender<()>) -> Self {
        Self {
            state: watch::channel(ConnectionStatus::CheckingTarget.into()).0,
            changes,
        }
    }
    pub fn subscribe(&self) -> watch::Receiver<Observation> {
        self.state.subscribe()
    }
    pub fn report(&self, next: impl Into<Observation>) {
        let next = next.into();
        if self.state.send_if_modified(|state| {
            if *state == next {
                return false;
            }
            *state = next;
            true
        }) {
            self.changes.send_replace(());
        }
    }
    pub fn finish(&self, result: &Result<(), RuntimeError>) {
        self.report(match result {
            Ok(()) => ConnectionStatus::Stopped.into(),
            Err(error) => failure(error),
        });
    }
}
impl Default for ConnectionReporter {
    fn default() -> Self {
        Self::new(watch::channel(()).0)
    }
}
pub fn failure(error: &RuntimeError) -> Observation {
    if let RuntimeError::At { side, source } = error {
        let mut observed = failure(source);
        observed.side = Some(*side);
        return observed;
    }
    let (connection, side) = match error {
        RuntimeError::State => (ConnectionStatus::InvalidState, None),
        RuntimeError::Control(ControlError::Auth(AuthError::SignInRequired)) => {
            (ConnectionStatus::SignInRequired, None)
        }
        RuntimeError::Control(
            ControlError::Auth(AuthError::StepUp)
            | ControlError::Registration
            | ControlError::Device(crate::device::DeviceError::StepUp),
        ) => (
            ConnectionStatus::DeviceAuthorizationRequired,
            Some(FaultSide::Device),
        ),
        RuntimeError::Control(ControlError::HostRegistrationMissing) => (
            ConnectionStatus::ControllerRegistrationRequired,
            Some(FaultSide::Controller),
        ),
        RuntimeError::Control(ControlError::Rejected(401 | 403)) => {
            (ConnectionStatus::AccessDenied, None)
        }
        RuntimeError::Transport(TransportError::Handshake {
            side,
            status: 401 | 403,
        }) => (ConnectionStatus::AccessDenied, Some((*side).into())),
        RuntimeError::Control(ControlError::Invalid | ControlError::Device(_))
        | RuntimeError::Transport(TransportError::Invalid | TransportError::Delivery(_)) => {
            (ConnectionStatus::ProtocolError, None)
        }
        _ => (ConnectionStatus::Failed, None),
    };
    Observation { connection, side }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn classification_preserves_attribution_and_distinguishes_account_and_device_auth() {
        for (error, side, expected) in [
            (
                ControlError::Auth(AuthError::SignInRequired),
                FaultSide::Controller,
                ConnectionStatus::SignInRequired,
            ),
            (
                ControlError::Auth(AuthError::SignInRequired),
                FaultSide::Device,
                ConnectionStatus::SignInRequired,
            ),
            (
                ControlError::Auth(AuthError::StepUp),
                FaultSide::Device,
                ConnectionStatus::DeviceAuthorizationRequired,
            ),
            (
                ControlError::Registration,
                FaultSide::Device,
                ConnectionStatus::DeviceAuthorizationRequired,
            ),
            (
                ControlError::HostRegistrationMissing,
                FaultSide::Controller,
                ConnectionStatus::ControllerRegistrationRequired,
            ),
            (
                ControlError::Rejected(403),
                FaultSide::Controller,
                ConnectionStatus::AccessDenied,
            ),
            (
                ControlError::Rejected(401),
                FaultSide::Device,
                ConnectionStatus::AccessDenied,
            ),
            (
                ControlError::Invalid,
                FaultSide::Device,
                ConnectionStatus::ProtocolError,
            ),
        ] {
            assert_eq!(
                failure(&RuntimeError::from(error).at(side)),
                Observation {
                    connection: expected,
                    side: Some(side)
                }
            );
        }
        for side in [Side::In, Side::Out] {
            assert_eq!(
                failure(&RuntimeError::Transport(TransportError::Handshake {
                    side,
                    status: 403
                }))
                .side,
                Some(side.into())
            );
        }
        assert_eq!(
            failure(&RuntimeError::State),
            ConnectionStatus::InvalidState.into()
        );
        assert_eq!(
            failure(&RuntimeError::Worker),
            ConnectionStatus::Failed.into()
        );
    }
}
