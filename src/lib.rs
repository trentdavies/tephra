//! tephra's internal modules, exposed as a library so integration tests can
//! exercise low-level building blocks (like `gitx`) directly against real
//! git repositories, in addition to driving the compiled `tephra` binary
//! end-to-end for CLI-level behavior.

pub mod agent;
pub mod bridge;
pub mod config;
pub mod gitx;
pub mod notify;
