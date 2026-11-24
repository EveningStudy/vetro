use std::time::{SystemTime, UNIX_EPOCH};

use vetro_agent_api::AgentError;
use vetro_proto::rpc::ErrorCode;

pub mod files;
mod history;
pub mod home;
mod links;
pub mod registry;
pub mod server;
pub mod service;
mod sessions;
pub mod storage;
pub mod workspace;

pub use service::Core;

fn internal(error: impl std::fmt::Display) -> AgentError {
    AgentError::new(ErrorCode::Internal, error.to_string())
}

fn bad_request(message: impl Into<String>) -> AgentError {
    AgentError::new(ErrorCode::BadRequest, message)
}

fn not_found(message: impl Into<String>) -> AgentError {
    AgentError::new(ErrorCode::NotFound, message)
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn page_limit(limit: Option<u32>, default: u32, max: u32) -> usize {
    limit.unwrap_or(default).clamp(1, max) as usize
}
