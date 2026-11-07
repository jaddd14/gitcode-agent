//! Provider-independent messages and streaming protocol adapters.

pub mod anthropic;
pub mod canonical;
pub mod openai_chat;
pub mod openai_responses;
pub mod registry;
pub mod retry;
pub mod sse;
pub mod transport;
