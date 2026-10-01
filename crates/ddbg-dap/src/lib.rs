//! Debug Adapter Protocol client.
//!
//! This crate is responsible only for protocol communication: framing,
//! serialization, request/response correlation and event dispatch.

pub mod codec;
pub mod protocol;

pub use anyhow::Result;
pub mod client;
pub mod transport;

pub use client::{DapClient, Incoming};
pub use transport::{AdapterCommand, AdapterProcess};
