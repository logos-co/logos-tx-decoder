//! logos-tx-decoder — offline EVM calldata decoding for the signing path.
//!
//! A library, not a module: it links into whatever is already showing a human
//! what they are about to sign. No network I/O on any path, and no Logos
//! runtime dependency — [`ffi`] exposes a small C ABI for C++ consumers.
//!
//! The interpretation is advisory and labelled as such unless the called
//! address is itself in the database — see [`Confidence`].

mod db;
mod decode;
mod ffi;
mod intent;
mod render;
mod request;
mod router;
mod units;

pub use db::{checksum, parse_address, AbiDb, Contract, DbError, Entry, SCHEMA};
pub use decode::{decode_call, Arg, Confidence, ContractRef, DecodedCall, FunctionRef, Kind};
pub use ffi::LogosTxDecoder;
pub use intent::{parse_render_lines, RenderScan, TxLeg};
pub use render::{describe, describe_with, value_line, Context};
pub use request::{read_request, LegReading, RequestReading};
pub use router::{Bound, RouterStep, Side, TokenRef};
