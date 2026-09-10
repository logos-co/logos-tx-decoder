//! logos-tx-decoder — offline EVM calldata decoding for the signing path.
//!
//! A library, not a module: it links into whatever is already showing a human
//! what they are about to sign. No network I/O on any path, and no Logos
//! runtime dependency — [`ffi`] exposes a small C ABI for C++ consumers.
//!
//! The interpretation is advisory and labelled as such unless the called
//! address is itself in the database — see [`Confidence`].
//!
//! A [`TokenRegistry`] can name an address and supply its decimals. It never moves
//! [`Confidence`]: naming a token says nothing about what its code does.

mod db;
mod decode;
mod ffi;
mod intent;
mod render;
mod tokens;
mod units;

pub use db::{checksum, parse_address, AbiDb, Contract, DbError, Entry, SCHEMA};
pub use decode::{
    decode_call, decode_call_with, Arg, Confidence, ContractRef, DecodedCall, FunctionRef, Kind,
    TokenRef,
};
pub use ffi::LogosTxDecoder;
pub use intent::{parse_render_lines, RenderScan, TxLeg};
pub use render::describe;
pub use tokens::{TokenInfo, TokenRegistry, TokenSource, TOKEN_SCHEMA};
