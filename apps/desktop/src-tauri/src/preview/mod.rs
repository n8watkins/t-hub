//! Provider-neutral Preview domain and runtime foundation.
//!
//! The desktop UI, control server, CLI, and MCP adapters must remain thin
//! callers of this module. This module never accepts shell commands, argument
//! vectors, environment variables, or URLs copied from conversation text.

pub mod discovery;
pub mod endpoint;
#[allow(dead_code)]
pub(crate) mod managed_runner;
pub mod model;
#[cfg(target_os = "linux")]
pub(crate) mod proc_listener;
pub mod profile;
pub mod runtime;
pub mod service;
pub(crate) mod supervisor;
