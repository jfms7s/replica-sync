//! Everything the desktop app needs that can be tested without a webview:
//! app errors as codes, settings, paths, the plan tree and the session.

pub mod error;
pub mod paths;
pub mod progress;
pub mod session;
pub mod settings;
#[cfg(test)]
mod testutil;
pub mod tree;
