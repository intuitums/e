//! Wire dialects: one module per upstream API shape. Each exposes
//! `run(&Request, &Sender<Event>)` with the contract documented on
//! `providers::stream` — exactly one terminal event per call.

pub mod anthropic;
pub mod completions;
pub mod google;
pub mod responses;
