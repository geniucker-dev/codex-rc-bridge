//! Stable, secret-free operation failures shared by HTTP and desktop transports.
use crate::{
    auth::AuthError, chains::ChainError, configuration::ConfigurationError, device::DeviceError,
    enrollment::EnrollmentError, upstream::ControlError,
};

/// Contains only a public status and a fixed code, never the underlying error text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OperationError {
    pub status: u16,
    pub code: &'static str,
}

fn auth_code(error: &AuthError) -> &'static str {
    match error {
        AuthError::SignInRequired => "account_sign_in_required",
        AuthError::StepUp => "device_authorization_required",
        AuthError::Network => "upstream_auth_network",
        AuthError::Rejected | AuthError::OAuthRequest => "upstream_auth_rejected",
        AuthError::Identity => "upstream_auth_identity",
        AuthError::Transaction => "upstream_auth_transaction",
        AuthError::Callback => "upstream_auth_callback",
        AuthError::Declined => "upstream_auth_declined",
        AuthError::Response => "upstream_auth_response",
        AuthError::Storage => "internal_error",
    }
}
fn control_code(error: &ControlError) -> &'static str {
    match error {
        ControlError::Auth(error) => auth_code(error),
        ControlError::Registration | ControlError::Device(crate::device::DeviceError::StepUp) => {
            "device_authorization_required"
        }
        _ => "operation_failed_check_authorization_or_state",
    }
}
fn enrollment_code(error: &EnrollmentError) -> &'static str {
    match error {
        EnrollmentError::Auth(error) => auth_code(error),
        EnrollmentError::Control(error) => control_code(error),
        _ => "operation_failed_check_authorization_or_state",
    }
}
fn delete_code(error: &crate::lifecycle::DeleteError) -> &'static str {
    use crate::lifecycle::DeleteError;
    match error {
        DeleteError::Auth(error) => auth_code(error),
        DeleteError::Control(error) => control_code(error),
        DeleteError::Enrollment(error) => enrollment_code(error),
        _ => "operation_failed_check_authorization_or_state",
    }
}
fn runtime_code(error: &crate::runtime::RuntimeError) -> &'static str {
    match error.root() {
        crate::runtime::RuntimeError::Control(error) => control_code(error),
        _ => "operation_failed_check_authorization_or_state",
    }
}
impl From<ChainError> for OperationError {
    fn from(error: ChainError) -> Self {
        let code = match &error {
            ChainError::Closed | ChainError::Worker => "temporarily_unavailable",
            ChainError::Runtime(error) => runtime_code(error),
            ChainError::Deletion(error) => delete_code(error),
            ChainError::Configuration(error) => match error {
                ConfigurationError::Auth(error) => auth_code(error),
                ConfigurationError::Control(error) => control_code(error),
                ConfigurationError::Enrollment(error) => enrollment_code(error),
                ConfigurationError::Runtime(error) => runtime_code(error),
                ConfigurationError::Lifecycle(error) => delete_code(error),
                ConfigurationError::PairInput(error) => error.code(),
                ConfigurationError::PairOutput(error) => {
                    use crate::output_pairing::PairOutputError;
                    match error {
                        PairOutputError::CleanupPending => "out_pairing_cleanup_pending",
                        PairOutputError::NoEnvironment => "out_pairing_no_environment",
                        PairOutputError::MultipleEnvironments => {
                            "out_pairing_multiple_environments"
                        }
                        PairOutputError::Control(error) => control_code(error),
                        _ => "operation_failed_check_authorization_or_state",
                    }
                }
                _ => "operation_failed_check_authorization_or_state",
            },
            _ => "operation_failed_check_authorization_or_state",
        };
        Self {
            status: match code {
                "temporarily_unavailable" => 503,
                "internal_error" => 500,
                _ => 409,
            },
            code,
        }
    }
}

/// Registration repair needs guidance specific to the original OUT identity.
/// Other operations retain the general classification above.
pub fn restore_record(error: ChainError) -> OperationError {
    let code = match error {
        ChainError::Configuration(ConfigurationError::Enrollment(EnrollmentError::Control(
            ControlError::Device(
                DeviceError::Binding | DeviceError::Challenge | DeviceError::Signature,
            )
            | ControlError::Invalid,
        ))) => "out_record_identity_mismatch",
        other => return other.into(),
    };
    OperationError { status: 409, code }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{device::DeviceError, storage::StorageError};

    #[test]
    fn authorization_codes_preserve_the_public_transport_contract() {
        for (error, expected) in [
            (AuthError::Network, "upstream_auth_network"),
            (AuthError::Rejected, "upstream_auth_rejected"),
            (AuthError::OAuthRequest, "upstream_auth_rejected"),
            (AuthError::Identity, "upstream_auth_identity"),
            (AuthError::Transaction, "upstream_auth_transaction"),
            (AuthError::Callback, "upstream_auth_callback"),
            (AuthError::Declined, "upstream_auth_declined"),
            (AuthError::Response, "upstream_auth_response"),
            (AuthError::StepUp, "device_authorization_required"),
        ] {
            let mapped =
                OperationError::from(ChainError::Configuration(ConfigurationError::Auth(error)));
            assert_eq!(
                mapped,
                OperationError {
                    status: 409,
                    code: expected
                }
            );
        }
        assert_eq!(
            OperationError::from(ChainError::Configuration(ConfigurationError::Auth(
                AuthError::Storage,
            ))),
            OperationError {
                status: 500,
                code: "internal_error"
            },
        );
        for error in [ChainError::Closed, ChainError::Worker] {
            assert_eq!(
                OperationError::from(error),
                OperationError {
                    status: 503,
                    code: "temporarily_unavailable"
                },
            );
        }
    }

    #[test]
    fn repair_reports_identity_registration_and_account_recovery_separately() {
        for control in [
            ControlError::Device(DeviceError::Binding),
            ControlError::Device(DeviceError::Challenge),
            ControlError::Device(DeviceError::Signature),
            ControlError::Invalid,
        ] {
            assert_eq!(
                restore_record(ChainError::Configuration(ConfigurationError::Enrollment(
                    EnrollmentError::Control(control),
                ))),
                OperationError {
                    status: 409,
                    code: "out_record_identity_mismatch"
                },
            );
        }
        assert_eq!(
            restore_record(ChainError::Configuration(ConfigurationError::Enrollment(
                EnrollmentError::Control(ControlError::Registration),
            ))),
            OperationError {
                status: 409,
                code: "device_authorization_required"
            },
        );
        for nested in [false, true] {
            {
                let auth = AuthError::SignInRequired;
                let error = if nested {
                    EnrollmentError::Control(ControlError::Auth(auth))
                } else {
                    EnrollmentError::Auth(auth)
                };
                assert_eq!(
                    restore_record(ChainError::Configuration(ConfigurationError::Enrollment(
                        error
                    ))),
                    OperationError {
                        status: 409,
                        code: "account_sign_in_required"
                    },
                );
            }
        }
    }

    #[test]
    fn repair_overrides_stay_scoped_and_other_failures_keep_the_general_code() {
        let registration_error = || {
            ChainError::Configuration(ConfigurationError::Enrollment(EnrollmentError::Control(
                ControlError::Registration,
            )))
        };
        assert_eq!(
            OperationError::from(registration_error()).code,
            "device_authorization_required",
        );
        for make in [
            || ChainError::Closed,
            || ChainError::Unavailable,
            || ChainError::Configuration(ConfigurationError::State),
            || ChainError::Configuration(ConfigurationError::Auth(AuthError::Network)),
            || ChainError::Configuration(ConfigurationError::Auth(AuthError::Storage)),
            || {
                ChainError::Configuration(ConfigurationError::Enrollment(EnrollmentError::Auth(
                    AuthError::Network,
                )))
            },
        ] {
            assert_eq!(restore_record(make()), OperationError::from(make()));
        }
    }

    #[test]
    fn underlying_database_text_never_enters_the_public_error() {
        let error = ChainError::Storage(StorageError::Database(sqlx::Error::Protocol(
            "synthetic-sensitive-value".into(),
        )));
        let mapped = OperationError::from(error);
        assert_eq!(
            mapped,
            OperationError {
                status: 409,
                code: "operation_failed_check_authorization_or_state"
            },
        );
        assert!(!format!("{mapped:?}").contains("synthetic-sensitive-value"));
    }
}
