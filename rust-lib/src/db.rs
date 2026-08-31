//! The ABI database: the vendored snapshot, plus whatever a caller imported.
//!
//! Selectors are derived here from the ABI, never read from the asset, so the
//! selector the lookup keys on is the one `alloy` would compute for the bytes.

use std::collections::HashMap;

use alloy::json_abi::Function;
use alloy::primitives::Address;
use serde::Deserialize;

const EMBEDDED: &str = include_str!("../assets/abi-db.json");

/// A contract this database can name, keyed by (chain, address).
#[derive(Debug, Clone, Deserialize)]
pub struct Contract {
    pub name: String,
    pub label: String,
    pub chain: u64,
    pub address: String,
    /// False for the vendored snapshot, true once a caller imported it.
    #[serde(default)]
    pub imported: bool,
}

#[derive(Deserialize)]
struct WireFn {
    a: Function,
    #[serde(default)]
    c: Vec<usize>,
}

#[derive(Deserialize)]
struct WireDb {
    schema: u32,
    source: String,
    upstream_rev: String,
    generated: String,
    contracts: Vec<Contract>,
    functions: Vec<WireFn>,
}

/// One known function, and which contracts in this database declare it.
pub struct Entry {
    pub func: Function,
    pub contracts: Vec<usize>,
}

impl Entry {
    pub fn read_only(&self) -> bool {
        matches!(self.func.state_mutability,
            alloy::json_abi::StateMutability::View | alloy::json_abi::StateMutability::Pure)
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum DbError {
    #[error("abi database is schema {found}, this build understands {expected}")]
    Schema { found: u32, expected: u32 },
    #[error("abi database is malformed: {0}")]
    Malformed(String),
    #[error("{0}")]
    BadAddress(String),
    #[error("abi json is not a function array or {{\"abi\": [...]}}: {0}")]
    BadAbi(String),
}

pub const SCHEMA: u32 = 1;

pub struct AbiDb {
    pub source: String,
    pub upstream_rev: String,
    pub generated: String,
    contracts: Vec<Contract>,
    entries: Vec<Entry>,
    by_selector: HashMap<[u8; 4], Vec<usize>>,
    by_address: HashMap<(u64, [u8; 20]), usize>,
    imported_functions: usize,
}

impl AbiDb {
    /// Parse the vendored snapshot. Fails only if this build and the asset disagree.
    pub fn embedded() -> Result<Self, DbError> {
        Self::from_json(EMBEDDED)
    }

    fn from_json(s: &str) -> Result<Self, DbError> {
        let wire: WireDb = serde_json::from_str(s).map_err(|e| DbError::Malformed(e.to_string()))?;
        if wire.schema != SCHEMA {
            return Err(DbError::Schema { found: wire.schema, expected: SCHEMA });
        }

        let mut db = AbiDb {
            source: wire.source,
            upstream_rev: wire.upstream_rev,
            generated: wire.generated,
            contracts: wire.contracts,
            entries: Vec::with_capacity(wire.functions.len()),
            by_selector: HashMap::new(),
            by_address: HashMap::new(),
            imported_functions: 0,
        };

        for (i, c) in db.contracts.iter().enumerate() {
            let addr = parse_address(&c.address)?;
            db.by_address.entry((c.chain, addr)).or_insert(i);
        }
        for f in wire.functions {
            db.push_entry(Entry { func: f.a, contracts: f.c });
        }
        Ok(db)
    }

    fn push_entry(&mut self, entry: Entry) {
        let selector: [u8; 4] = entry.func.selector().into();
        let idx = self.entries.len();
        self.entries.push(entry);
        self.by_selector.entry(selector).or_default().push(idx);
    }

    /// Every function whose selector matches, most recently added last.
    pub fn by_selector(&self, selector: [u8; 4]) -> &[usize] {
        self.by_selector.get(&selector).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn entry(&self, idx: usize) -> &Entry {
        &self.entries[idx]
    }

    pub fn contract(&self, idx: usize) -> &Contract {
        &self.contracts[idx]
    }

    pub fn contract_at(&self, chain: u64, address: &[u8; 20]) -> Option<usize> {
        self.by_address.get(&(chain, *address)).copied()
    }

    pub fn stats(&self) -> (usize, usize, usize) {
        (self.contracts.len(), self.entries.len(), self.imported_functions)
    }

    /// Ingest caller-supplied ABI JSON — an Etherscan `getabi` array, or an
    /// object with an `abi` key. Never fetches; the caller brings the bytes.
    /// Returns how many functions were added.
    pub fn import(
        &mut self,
        name: &str,
        label: &str,
        chain: u64,
        address: &str,
        abi_json: &str,
    ) -> Result<usize, DbError> {
        let addr = parse_address(address)?;
        let raw: serde_json::Value =
            serde_json::from_str(abi_json).map_err(|e| DbError::BadAbi(e.to_string()))?;
        let items = match &raw {
            serde_json::Value::Array(v) => v.clone(),
            serde_json::Value::Object(m) => match m.get("abi") {
                Some(serde_json::Value::Array(v)) => v.clone(),
                _ => return Err(DbError::BadAbi("object has no `abi` array".into())),
            },
            _ => return Err(DbError::BadAbi("expected an array or object".into())),
        };

        let idx = self.contracts.len();
        self.contracts.push(Contract {
            name: name.to_string(),
            label: if label.is_empty() { name.to_string() } else { label.to_string() },
            chain,
            address: format!("0x{}", hex::encode(addr)),
            imported: true,
        });
        // An import is a deliberate act by the caller, so it wins over the snapshot.
        self.by_address.insert((chain, addr), idx);

        let mut added = 0;
        for item in items {
            if item.get("type").and_then(|t| t.as_str()) != Some("function") {
                continue;
            }
            let Ok(func) = serde_json::from_value::<Function>(item) else { continue };
            // Reuse an existing entry only if it names the arguments identically.
            // A same-selector entry with different names belongs to a different
            // contract, and showing its names here would misattribute them.
            let twin = self
                .entries
                .iter_mut()
                .find(|e| e.func.selector() == func.selector() && same_named_shape(&e.func, &func));
            match twin {
                Some(existing) => {
                    if !existing.contracts.contains(&idx) {
                        existing.contracts.push(idx);
                    }
                }
                None => {
                    self.push_entry(Entry { func, contracts: vec![idx] });
                    self.imported_functions += 1;
                    added += 1;
                }
            }
        }
        Ok(added)
    }
}

/// Same function name AND the same parameter names, recursively. Types are
/// already equal whenever selectors match, so only the names are in question.
fn same_named_shape(a: &Function, b: &Function) -> bool {
    fn params_match(a: &[alloy::json_abi::Param], b: &[alloy::json_abi::Param]) -> bool {
        a.len() == b.len()
            && a.iter().zip(b).all(|(x, y)| {
                x.name == y.name && params_match(&x.components, &y.components)
            })
    }
    a.name == b.name && params_match(&a.inputs, &b.inputs)
}

/// Accept `0x…`-prefixed or bare 40-hex, any case. No checksum requirement:
/// callers pass addresses that came off the wire.
pub fn parse_address(s: &str) -> Result<[u8; 20], DbError> {
    let t = s.trim();
    let t = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")).unwrap_or(t);
    let bytes = hex::decode(t).map_err(|_| DbError::BadAddress(format!("not hex: {s}")))?;
    bytes
        .try_into()
        .map_err(|_| DbError::BadAddress(format!("address must be 20 bytes, got {s}")))
}

/// EIP-55 for display. Never used for comparison.
pub fn checksum(addr: &[u8; 20]) -> String {
    Address::from(*addr).to_checksum(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> AbiDb {
        AbiDb::embedded().expect("embedded db parses")
    }

    #[test]
    fn the_embedded_database_loads() {
        let (contracts, functions, imported) = db().stats();
        assert!(contracts >= 80, "got {contracts} contracts");
        assert!(functions >= 1500, "got {functions} functions");
        assert_eq!(imported, 0);
    }

    #[test]
    fn weth_is_a_known_contract_and_declares_transfer() {
        let db = db();
        let weth = parse_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2").unwrap();
        let idx = db.contract_at(1, &weth).expect("WETH is in the snapshot");
        assert_eq!(db.contract(idx).label, "WETH");

        let sel = hex::decode("a9059cbb").unwrap();
        let hits = db.by_selector(sel.try_into().unwrap());
        assert!(hits.iter().any(|&h| db.entry(h).contracts.contains(&idx)));
    }

    #[test]
    fn weth_is_not_known_on_another_chain() {
        let weth = parse_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2").unwrap();
        assert!(db().contract_at(137, &weth).is_none());
    }

    #[test]
    fn read_only_functions_are_flagged() {
        let db = db();
        // balanceOf(address) — view on every ERC-20.
        let hits = db.by_selector([0x70, 0xa0, 0x82, 0x31]);
        assert!(!hits.is_empty());
        assert!(hits.iter().all(|&h| db.entry(h).read_only()));
    }

    #[test]
    fn addresses_parse_with_or_without_prefix_and_case() {
        let a = parse_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2").unwrap();
        let b = parse_address("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2").unwrap();
        assert_eq!(a, b);
        assert_eq!(checksum(&a), "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        assert!(parse_address("0x1234").is_err());
        assert!(parse_address("not-hex-at-all-not-hex-at-all-not-hex-at!").is_err());
    }

    #[test]
    fn an_import_names_a_contract_the_snapshot_does_not_know() {
        let mut db = db();
        let before = db.stats();
        let n = db
            .import(
                "my_thing",
                "My Thing",
                1,
                "0x000000000000000000000000000000000000dEaD",
                r#"[{"type":"function","name":"frobnicate","inputs":[{"name":"how","type":"uint256"}],"outputs":[],"stateMutability":"nonpayable"}]"#,
            )
            .unwrap();
        assert_eq!(n, 1);

        let addr = parse_address("0x000000000000000000000000000000000000dEaD").unwrap();
        let idx = db.contract_at(1, &addr).unwrap();
        assert_eq!(db.contract(idx).label, "My Thing");
        assert!(db.contract(idx).imported);
        assert_eq!(db.stats().1, before.1 + 1);
    }

    #[test]
    fn importing_an_identically_named_twin_adds_membership_only() {
        let mut db = db();
        let before = db.stats().1;
        // Byte-identical to WETH9's own transfer, names included.
        let added = db
            .import("weth_clone", "WETH Clone", 1, "0x000000000000000000000000000000000000cAfE",
                r#"[{"type":"function","name":"transfer","inputs":[{"name":"dst","type":"address"},{"name":"wad","type":"uint256"}],"outputs":[],"stateMutability":"nonpayable"}]"#)
            .unwrap();
        assert_eq!(added, 0, "an identically-named twin must not duplicate");
        assert_eq!(db.stats().1, before);
    }

    #[test]
    fn importing_a_known_selector_with_different_names_keeps_them_apart() {
        let mut db = db();
        let before = db.stats().1;
        let added = db
            .import(
                "my_token",
                "My Token",
                1,
                "0x000000000000000000000000000000000000bEEF",
                r#"[{"type":"function","name":"transfer","inputs":[{"name":"to","type":"address"},{"name":"amount","type":"uint256"}],"outputs":[],"stateMutability":"nonpayable"}]"#,
            )
            .unwrap();
        // WETH names these dst/wad, so this differently-named twin gets its own
        // entry rather than borrowing WETH's names.
        assert_eq!(added, 1);
        assert_eq!(db.stats().1, before + 1);

        let addr = parse_address("0x000000000000000000000000000000000000bEEF").unwrap();
        let idx = db.contract_at(1, &addr).unwrap();
        let hits = db.by_selector([0xa9, 0x05, 0x9c, 0xbb]);
        assert!(hits.iter().any(|&h| db.entry(h).contracts.contains(&idx)));
    }

    #[test]
    fn import_rejects_junk() {
        let mut db = db();
        assert!(db.import("x", "", 1, "0xnope", "[]").is_err());
        assert!(db.import("x", "", 1, "0x000000000000000000000000000000000000dEaD", "{").is_err());
        assert!(db
            .import("x", "", 1, "0x000000000000000000000000000000000000dEaD", r#"{"no":"abi"}"#)
            .is_err());
    }
}
