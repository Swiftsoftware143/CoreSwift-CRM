//! The helpdesk: what a ticket is once it is meant to replace a real support desk.
//!
//! Phase 1 is deliberately narrow. It makes the queue trustworthy for email — one ticket per
//! conversation, a number a customer can quote, notes a customer can never see, and machine mail kept
//! out of the way — before any of the larger surface (knowledge base, AI answers, SLAs) is built on top.

pub mod canned;
pub mod ingest;
pub mod junk;
pub mod kb;
pub mod merge;
pub mod reports;
pub mod sla;

pub use ingest::{ingest_inbound, IngestOutcome};
pub use sla::SlaPass;
