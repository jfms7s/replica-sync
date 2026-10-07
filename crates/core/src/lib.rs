//! replica-sync engine: scan two folders, plan the changes that make the
//! replica match the source, and apply the approved ones safely.

pub mod model;
pub mod rules;
pub mod scan;

#[cfg(test)]
mod testutil;
