//! The token registry: what an ADDRESS is called, and in what units it counts.
//!
//! Deliberately separate from [`crate::db`]. An ABI entry and a token-list row are
//! different assertions: the first says "this address declares this function", the second
//! says "this address is called USDC and counts in 6 decimals". Merging them would let a
//! naming claim be read as a verification of the code, which is the one thing the
//! confidence tiers exist to keep apart.
//!
//! So a registry hit never moves [`crate::Confidence`]. All it can do is name an address
//! and supply decimals — both keyed by (chain, address), so a wrong address matches
//! nothing rather than mislabelling a different token.

use std::collections::HashMap;

use serde::Deserialize;

use crate::db::parse_address;

const EMBEDDED: &str = include_str!("../assets/token-list.json");

/// The registry's schema, bumped when the asset's shape changes.
pub const TOKEN_SCHEMA: u32 = 1;

/// Where a row came from. The vendored snapshot is `embedded`; a caller that hands in its
/// own list may say which of its buckets answered, and that label is rendered next to the
/// name. A list a user can add to is not the same claim as one shipped with the binary.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenSource {
    Embedded,
    Downloaded,
    Custom,
    Unknown,
}

impl TokenSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Embedded => "embedded",
            Self::Downloaded => "downloaded",
            Self::Custom => "custom",
            Self::Unknown => "unknown",
        }
    }

    /// Whether a name from this source may be shown without saying where it came from.
    /// Only the vendored snapshot may: everything else is a list this device was told
    /// about, and a signing screen has to say so.
    pub fn is_shipped(&self) -> bool {
        matches!(self, Self::Embedded)
    }
}

/// One row: what the address is called, and how its amounts scale.
#[derive(Debug, Clone)]
pub struct TokenInfo {
    pub symbol: String,
    pub name: String,
    pub decimals: u8,
    pub source: TokenSource,
}

#[derive(Deserialize)]
struct WireToken {
    #[serde(rename = "chainId")]
    chain_id: u64,
    address: String,
    symbol: String,
    #[serde(default)]
    name: Option<String>,
    decimals: u8,
    /// Present when a caller forwards a token_list_module reply verbatim.
    #[serde(default)]
    source: Option<TokenSource>,
}

#[derive(Deserialize)]
struct WireList {
    #[serde(default)]
    tokens: Vec<WireToken>,
}

/// Addresses this decoder can name, keyed by (chain, address).
#[derive(Debug, Clone, Default)]
pub struct TokenRegistry {
    by_addr: HashMap<(u64, [u8; 20]), TokenInfo>,
    chains: usize,
}

impl TokenRegistry {
    /// The vendored snapshot. Empty rather than fatal if the asset will not parse: a
    /// decoder that names nothing still decodes, and refusing to start would take the
    /// signing screen down with it.
    pub fn embedded() -> Self {
        Self::from_json(EMBEDDED).unwrap_or_default()
    }

    /// Parse a token list. Accepts a bare document (`{"tokens": [...]}`) and a
    /// `token_list_module` reply (`{"ok": true, "tokens": [...]}`) alike, so a caller can
    /// forward what it was given without reshaping it.
    ///
    /// A row whose address will not parse is skipped rather than failing the load: one bad
    /// row in a list of thousands should cost that row, not the whole registry.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        let wire: WireList = serde_json::from_str(json)?;
        let mut by_addr = HashMap::with_capacity(wire.tokens.len());
        let mut chains = std::collections::HashSet::new();
        for t in wire.tokens {
            let Ok(addr) = parse_address(&t.address) else { continue };
            chains.insert(t.chain_id);
            // First wins: a list that names one address twice has not told us which.
            by_addr.entry((t.chain_id, addr)).or_insert(TokenInfo {
                name: t.name.unwrap_or_else(|| t.symbol.clone()),
                symbol: t.symbol,
                decimals: t.decimals,
                source: t.source.unwrap_or(TokenSource::Embedded),
            });
        }
        Ok(Self { by_addr, chains: chains.len() })
    }

    pub fn get(&self, chain: u64, addr: &[u8; 20]) -> Option<&TokenInfo> {
        self.by_addr.get(&(chain, *addr))
    }

    pub fn len(&self) -> usize {
        self.by_addr.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_addr.is_empty()
    }

    pub fn chains(&self) -> usize {
        self.chains
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const USDC: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";

    fn doc(extra: &str) -> String {
        format!(
            r#"{{"tokens":[{{"chainId":1,"address":"{USDC}","symbol":"USDC","name":"USD Coin","decimals":6{extra}}}]}}"#
        )
    }

    #[test]
    fn the_vendored_snapshot_loads_and_names_real_addresses() {
        let r = TokenRegistry::embedded();
        assert!(r.len() > 1000, "vendored registry is {} rows", r.len());
        let weth = parse_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2").unwrap();
        let t = r.get(1, &weth).expect("mainnet WETH is in the Uniswap list");
        assert_eq!((t.symbol.as_str(), t.decimals), ("WETH", 18));
    }

    #[test]
    fn a_hit_is_keyed_by_chain_as_well_as_address() {
        // The same address is a different contract on a different chain, so a match on
        // one chain must not answer for another.
        let r = TokenRegistry::embedded();
        let weth = parse_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2").unwrap();
        assert!(r.get(1, &weth).is_some());
        assert!(r.get(11_155_111, &weth).is_none(), "mainnet WETH is not sepolia's");
    }

    #[test]
    fn a_reply_shaped_list_parses_and_keeps_its_bucket() {
        let r = TokenRegistry::from_json(&doc(r#","source":"custom""#)).unwrap();
        let t = r.get(1, &parse_address(USDC).unwrap()).unwrap();
        assert_eq!(t.source, TokenSource::Custom);
        assert!(!t.source.is_shipped(), "a user-added row is not a shipped one");
    }

    #[test]
    fn an_unlabelled_row_is_treated_as_the_shipped_snapshot() {
        let r = TokenRegistry::from_json(&doc("")).unwrap();
        assert_eq!(r.get(1, &parse_address(USDC).unwrap()).unwrap().source, TokenSource::Embedded);
    }

    #[test]
    fn one_unparseable_address_costs_that_row_and_no_other() {
        // Solana rows live in the same upstream document; they are not EVM addresses.
        let json = format!(
            r#"{{"tokens":[{{"chainId":1,"address":"5mbK36SZ7J19An8jFochhQS4of8g6BwUjbeCSxBSoWdp","symbol":"MICHI","decimals":9}},
                            {{"chainId":1,"address":"{USDC}","symbol":"USDC","decimals":6}}]}}"#
        );
        let r = TokenRegistry::from_json(&json).unwrap();
        assert_eq!(r.len(), 1);
        assert!(r.get(1, &parse_address(USDC).unwrap()).is_some());
    }

    #[test]
    fn a_list_that_names_one_address_twice_keeps_the_first() {
        let json = format!(
            r#"{{"tokens":[{{"chainId":1,"address":"{USDC}","symbol":"USDC","decimals":6}},
                            {{"chainId":1,"address":"{USDC}","symbol":"EVIL","decimals":18}}]}}"#
        );
        let r = TokenRegistry::from_json(&json).unwrap();
        assert_eq!(r.get(1, &parse_address(USDC).unwrap()).unwrap().symbol, "USDC");
    }

    #[test]
    fn a_broken_asset_is_an_empty_registry_rather_than_a_dead_decoder() {
        assert!(TokenRegistry::from_json("{ not json").is_err());
        assert!(TokenRegistry::from_json(r#"{"tokens":[]}"#).unwrap().is_empty());
    }
}
