//! Corpus checking: runs the engine over many PDF files, each in a child process of this
//! tool so that a crash, hang or runaway allocation is recorded rather than fatal.

pub mod child;
pub mod conformance;
pub mod engine;
pub mod guard;
pub mod mutool;
pub mod report;
pub mod run;
pub mod walk;
pub mod worker;
