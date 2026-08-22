//! cliphistory core library: daemon engine, storage, discovery, module manager,
//! IPC and CLI plumbing. The `cliphistory` binary is a thin shell around this.

pub mod cli;
pub mod config;
pub mod constants;
pub mod discovery;
pub mod engine;
pub mod ipc;
pub mod plugins;
pub mod storage;
