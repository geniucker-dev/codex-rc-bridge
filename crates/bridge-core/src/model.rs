use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EndpointKind {
    In,
    Out,
}
impl EndpointKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::In => "in",
            Self::Out => "out",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountIdentity {
    pub workspace_id: String,
    pub user_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    pub tenant_id: Uuid,
    pub device_id: Uuid,
    pub account: AccountIdentity,
    pub kind: EndpointKind,
    pub identity_name: String,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub tenant_id: Uuid,
    pub in_id: Uuid,
    pub out_id: Uuid,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Target {
    pub environment_id: String,
    pub app_server_version: String,
}
