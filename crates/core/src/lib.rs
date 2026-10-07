//! replica-sync engine: scan two folders, plan the changes that make the
//! replica match the source, and apply the approved ones safely.

pub mod diff;
pub mod model;
pub mod moves;
pub mod plan;
pub mod rules;
pub mod safety;
pub mod scan;
pub mod trash;

#[cfg(test)]
mod testutil;
