//! replica-sync engine: scan two folders, plan the changes that make the
//! replica match the source, and apply the approved ones safely.

pub mod diff;
pub mod execute;
pub mod model;
pub mod moves;
pub mod pairs;
pub mod plan;
pub mod rules;
pub mod safety;
pub mod scan;
pub mod trash;
pub mod volume;

#[cfg(test)]
mod testutil;
