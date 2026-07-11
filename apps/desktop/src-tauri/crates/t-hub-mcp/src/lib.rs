//! Library surface of the T-Hub MCP crate.
//!
//! The crate ships as a stdio binary (`src/main.rs`); this thin library target
//! exists so OTHER crates can inspect the static tool catalog without shelling out
//! to the binary. In particular, the app crate (`t-hub`) uses it as a dev-dependency
//! to assert TIER PARITY - that every MCP tool's declared [`tools::Tier`] matches the
//! server-side `control::required_tier` enforcement (item-3 ledger #16), so the
//! annotation-vs-enforcement drift that motivated the socket-gate work cannot recur.
//!
//! Only the dependency-light, self-contained [`tools`] module is re-exported here;
//! the transport modules (`server`, `control_client`, `protocol`) stay bin-private.

pub mod tools;
