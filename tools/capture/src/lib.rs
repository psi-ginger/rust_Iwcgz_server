//! Record/replay TCP proxy for the golden packet corpus (task T-007).
//!
//! Sits between a real client and the legacy cluster and writes both directions of
//! a session to disk byte for byte. The recordings are the acceptance corpus that
//! later proves the Rust services behave identically, so **the only guarantee
//! that matters is that the files contain exactly the bytes that crossed the
//! sockets** — in order, unmodified, with nothing dropped or reordered.
//!
//! It deliberately does not parse, decrypt or frame anything. The front link is
//! plaintext and the game link is ciphered, and both are recorded as opaque
//! bytes; understanding the stream is `gz-net`'s job, and a proxy that understood
//! it would risk altering it.

pub mod json;
pub mod meta;
pub mod proxy;
pub mod recorder;
pub mod verify;

pub use meta::{DirectionMeta, Meta};
pub use proxy::{Done, run, serve_connection};
pub use recorder::{Dir, Direction, Session};
