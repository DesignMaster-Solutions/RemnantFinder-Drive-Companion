//! Remnant Finder companion drive core library.

pub mod api;
pub mod auth;
pub mod cache;
pub mod config;
pub mod sync;
pub mod vfs;
pub mod webdav;

pub use config::AppConfig;
pub use sync::SyncStatusSnapshot;
