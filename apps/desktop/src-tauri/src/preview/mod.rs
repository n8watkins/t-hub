//! Provider-neutral Preview domain and runtime foundation.
//!
//! The desktop UI, control server, CLI, and MCP adapters must remain thin
//! callers of this module. This module never accepts shell commands, argument
//! vectors, environment variables, or URLs copied from conversation text.

pub mod discovery;
pub mod endpoint;
pub mod model;
pub mod profile;
pub mod runtime;
pub mod service;
