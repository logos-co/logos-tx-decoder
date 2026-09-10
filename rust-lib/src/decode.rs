//! Calldata -> a typed interpretation that never claims more than it knows.
//!
//! A 4-byte selector is not proof of anything: anyone can deploy a contract
//! whose function collides with `transfer`. Only [`Confidence::Verified`] ties
//! the interpretation to the address being called.

use alloy::dyn_abi::{DynSolValue, JsonAbiExt};
use alloy::json_abi::Param;
use serde::Serialize;

use crate::db::{checksum, parse_address, AbiDb};
use crate::tokens::TokenRegistry;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A recipient and no calldata: a plain value transfer.
    PlainTransfer,
    /// No recipient: this deploys code.
    ContractCreation,
    Call,
    /// Calldata too short to hold a selector.
    Malformed,
}

/// How much the interpretation is worth. Only meaningful for [`Kind::Call`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// `to` is a contract in the database AND it declares this selector.
    Verified,
    /// The selector resolves to a signature, but nothing ties it to `to`.
    SignatureOnly,
    /// Nothing usable. Only the raw bytes mean anything.
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContractRef {
    pub name: String,
    pub label: String,
    pub chain: u64,
    pub address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decimals: Option<u8>,
    pub imported: bool,
}

/// What a token list says this ADDRESS is called. Beside `contract`, never inside it: an
/// ABI entry says the code declares a function, this says the address has a name and a
/// unit. Present or not, it leaves `confidence` exactly where it was.
#[derive(Debug, Clone, Serialize)]
pub struct TokenRef {
    pub symbol: String,
    pub name: String,
    pub decimals: u8,
    /// Which list answered: `embedded` is the vendored snapshot, the rest are lists this
    /// device was told about. Rendered, because they are not the same claim.
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FunctionRef {
    pub name: String,
    pub signature: String,
    #[serde(rename = "stateMutability")]
    pub state_mutability: String,
    #[serde(rename = "readOnly")]
    pub read_only: bool,
}

/// One decoded argument. Scalars carry `value`; tuples carry `components`;
/// arrays carry `items`.
#[derive(Debug, Clone, Serialize)]
pub struct Arg {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub components: Option<Vec<Arg>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<Arg>>,
}

impl Arg {
    fn scalar(name: &str, ty: &str, value: String) -> Self {
        Arg { name: name.into(), ty: ty.into(), value: Some(value), components: None, items: None }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DecodedCall {
    pub kind: Kind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<Confidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contract: Option<ContractRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<TokenRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<FunctionRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<Arg>>,
    pub warnings: Vec<String>,
}

impl DecodedCall {
    fn bare(kind: Kind, warnings: Vec<String>) -> Self {
        DecodedCall {
            kind,
            confidence: None,
            selector: None,
            contract: None,
            token: None,
            function: None,
            args: None,
            warnings,
        }
    }
}

/// Decode one call. `to` may be empty for a contract creation; `data` may be
/// empty, `0x`, or `0x`-prefixed hex.
pub fn decode_call(db: &AbiDb, chain: u64, to: &str, data: &str) -> DecodedCall {
    decode_call_with(db, &TokenRegistry::default(), chain, to, data)
}

/// As [`decode_call`], with a registry that can NAME the called address and supply its
/// decimals. A hit never changes `confidence` — see [`crate::tokens`].
pub fn decode_call_with(
    db: &AbiDb,
    tokens: &TokenRegistry,
    chain: u64,
    to: &str,
    data: &str,
) -> DecodedCall {
    let bytes = match parse_hex(data) {
        Ok(b) => b,
        Err(e) => return DecodedCall::bare(Kind::Malformed, vec![e]),
    };

    let to = to.trim();
    if to.is_empty() || to == "0x" {
        return DecodedCall::bare(
            Kind::ContractCreation,
            vec!["No recipient: this deploys a new contract. Its code is the calldata.".into()],
        );
    }

    let addr = match parse_address(to) {
        Ok(a) => a,
        Err(e) => return DecodedCall::bare(Kind::Malformed, vec![e.to_string()]),
    };
    let token = tokens.get(chain, &addr).map(|t| TokenRef {
        symbol: t.symbol.clone(),
        name: t.name.clone(),
        decimals: t.decimals,
        source: t.source.as_str().to_string(),
    });
    let contract_idx = db.contract_at(chain, &addr);
    let contract = contract_idx.map(|i| {
        let c = db.contract(i);
        ContractRef {
            name: c.name.clone(),
            label: c.label.clone(),
            chain: c.chain,
            address: checksum(&addr),
            decimals: c.decimals,
            imported: c.imported,
        }
    });

    if bytes.is_empty() {
        let mut out = DecodedCall::bare(Kind::PlainTransfer, vec![]);
        out.contract = contract;
        out.token = token;
        return out;
    }
    if bytes.len() < 4 {
        return DecodedCall::bare(
            Kind::Malformed,
            vec![format!("calldata is {} byte(s): too short to hold a selector", bytes.len())],
        );
    }

    let selector: [u8; 4] = bytes[..4].try_into().expect("checked length");
    let hits = db.by_selector(selector);
    let mut warnings = Vec::new();

    // An entry the called contract actually declares beats a global match.
    let declared = contract_idx
        .and_then(|ci| hits.iter().copied().find(|&h| db.entry(h).contracts.contains(&ci)));

    let (chosen, mut confidence) = match declared {
        Some(h) => (Some(h), Confidence::Verified),
        None if !hits.is_empty() => (hits.last().copied(), Confidence::SignatureOnly),
        None => (None, Confidence::Unknown),
    };

    if declared.is_none() {
        match &contract {
            Some(c) if !hits.is_empty() => warnings.push(format!(
                "{} is a known contract, but its ABI does not declare this selector. \
                 The function named here comes from a DIFFERENT contract's ABI — proxies \
                 do this legitimately, and so does an attacker.",
                c.label
            )),
            Some(c) => warnings
                .push(format!("{} is a known contract, but this selector is not in the database.", c.label)),
            None if !hits.is_empty() => warnings.push(
                "This contract is not in the database. The function named here is a guess \
                 from the 4-byte selector alone and proves nothing about what the code does."
                    .into(),
            ),
            None => warnings.push("Neither this contract nor this selector is known.".into()),
        }
    }
    // Only a genuine collision is worth flagging. Several entries can share one
    // canonical signature — the same function with different parameter names —
    // and saying "2 signatures share this selector" about those is pure noise.
    let mut distinct: Vec<String> = hits.iter().map(|&h| db.entry(h).func.signature()).collect();
    distinct.sort();
    distinct.dedup();
    if distinct.len() > 1 {
        warnings.push(format!(
            "{} DIFFERENT signatures share this selector: {}",
            distinct.len(),
            distinct.join(", ")
        ));
    }

    let mut function = None;
    let mut args = None;
    if let Some(h) = chosen {
        let entry = db.entry(h);
        if entry.read_only() {
            warnings.push(
                "This is a read-only function. A transaction calling it changes nothing \
                 on-chain but still spends gas."
                    .into(),
            );
        }
        function = Some(FunctionRef {
            name: entry.func.name.clone(),
            signature: entry.func.signature(),
            state_mutability: format!("{:?}", entry.func.state_mutability).to_lowercase(),
            read_only: entry.read_only(),
        });
        match entry.func.abi_decode_input(&bytes[4..]) {
            Ok(values) => args = Some(build_args(&entry.func.inputs, &values)),
            Err(e) => {
                // The selector fits but the body does not: the match is almost
                // certainly wrong, so stop asserting it.
                confidence = Confidence::Unknown;
                warnings.push(format!(
                    "The argument bytes do NOT decode against this signature ({e}). \
                     Treat the name above as unproven."
                ));
            }
        }
    }

    DecodedCall {
        kind: Kind::Call,
        confidence: Some(confidence),
        selector: Some(format!("0x{}", hex::encode(selector))),
        contract,
        token,
        function,
        args,
        warnings,
    }
}

fn build_args(params: &[Param], values: &[DynSolValue]) -> Vec<Arg> {
    params
        .iter()
        .zip(values)
        .enumerate()
        .map(|(i, (p, v))| {
            // Solidity allows unnamed parameters; position is all we can show.
            let name = if p.name.is_empty() { format!("[{i}]") } else { p.name.clone() };
            build_arg(&name, &p.ty, &p.components, v)
        })
        .collect()
}

fn build_arg(name: &str, ty: &str, components: &[Param], v: &DynSolValue) -> Arg {
    match v {
        DynSolValue::Address(a) => Arg::scalar(name, ty, a.to_checksum(None)),
        DynSolValue::Bool(b) => Arg::scalar(name, ty, b.to_string()),
        DynSolValue::Int(i, _) => Arg::scalar(name, ty, i.to_string()),
        DynSolValue::Uint(u, _) => Arg::scalar(name, ty, u.to_string()),
        DynSolValue::String(s) => Arg::scalar(name, ty, s.clone()),
        DynSolValue::Bytes(b) => Arg::scalar(name, ty, format!("0x{}", hex::encode(b))),
        DynSolValue::FixedBytes(w, sz) => {
            Arg::scalar(name, ty, format!("0x{}", hex::encode(&w[..*sz])))
        }
        DynSolValue::Function(f) => Arg::scalar(name, ty, format!("0x{}", hex::encode(f.as_slice()))),
        DynSolValue::Tuple(vs) => Arg {
            name: name.into(),
            ty: ty.into(),
            value: None,
            components: Some(build_args(components, vs)),
            items: None,
        },
        DynSolValue::Array(vs) | DynSolValue::FixedArray(vs) => {
            let elem = element_type(ty);
            let items = vs
                .iter()
                .enumerate()
                .map(|(i, item)| build_arg(&format!("[{i}]"), elem, components, item))
                .collect();
            Arg { name: name.into(), ty: ty.into(), value: None, components: None, items: Some(items) }
        }
    }
}

/// `uint256[3]` -> `uint256`. Strips one trailing array suffix.
fn element_type(ty: &str) -> &str {
    match ty.rfind('[') {
        Some(i) if ty.ends_with(']') => &ty[..i],
        _ => ty,
    }
}

fn parse_hex(s: &str) -> Result<Vec<u8>, String> {
    let t = s.trim();
    let t = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")).unwrap_or(t);
    if t.is_empty() {
        return Ok(Vec::new());
    }
    hex::decode(t).map_err(|e| format!("calldata is not hex: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const WETH: &str = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2";
    const VITALIK: &str = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045";
    // transfer(0xd8dA…6045, 1000000000)
    const TRANSFER: &str = "0xa9059cbb000000000000000000000000d8da6bf26964af9d7eed9e03e53415d37aa96045000000000000000000000000000000000000000000000000000000003b9aca00";

    fn db() -> AbiDb {
        AbiDb::embedded().unwrap()
    }

    #[test]
    fn a_known_contract_and_selector_is_verified() {
        let d = decode_call(&db(), 1, WETH, TRANSFER);
        assert_eq!(d.kind, Kind::Call);
        assert_eq!(d.confidence, Some(Confidence::Verified));
        assert_eq!(d.contract.unwrap().label, "WETH");

        let f = d.function.unwrap();
        assert_eq!(f.signature, "transfer(address,uint256)");
        assert!(!f.read_only);

        // WETH9 names these `dst`/`wad` — proof the names come from the real ABI.
        let args = d.args.unwrap();
        assert_eq!(args[0].name, "dst");
        assert_eq!(args[0].value.as_deref(), Some(VITALIK));
        assert_eq!(args[1].name, "wad");
        assert_eq!(args[1].value.as_deref(), Some("1000000000"));
    }

    #[test]
    fn the_same_calldata_to_an_unknown_contract_is_only_a_signature() {
        let d = decode_call(&db(), 1, "0x000000000000000000000000000000000000dEaD", TRANSFER);
        assert_eq!(d.confidence, Some(Confidence::SignatureOnly));
        assert!(d.contract.is_none());
        assert_eq!(d.function.unwrap().signature, "transfer(address,uint256)");
        assert!(
            d.warnings.iter().any(|w| w.contains("proves nothing")),
            "expected an explicit disclaimer, got {:?}",
            d.warnings
        );
    }

    #[test]
    fn the_same_calldata_on_another_chain_is_not_verified() {
        // WETH's address is not this contract on chain 137.
        let d = decode_call(&db(), 137, WETH, TRANSFER);
        assert_eq!(d.confidence, Some(Confidence::SignatureOnly));
        assert!(d.contract.is_none());
    }

    #[test]
    fn an_unknown_selector_decodes_to_nothing() {
        let d = decode_call(&db(), 1, WETH, "0xdeadbeef");
        assert_eq!(d.confidence, Some(Confidence::Unknown));
        assert!(d.function.is_none());
        assert!(d.args.is_none());
        assert!(d.warnings.iter().any(|w| w.contains("not in the database")), "{:?}", d.warnings);
    }

    #[test]
    fn a_selector_whose_body_does_not_decode_is_downgraded() {
        // transfer's selector with one byte of argument data.
        let d = decode_call(&db(), 1, WETH, "0xa9059cbb00");
        assert_eq!(d.confidence, Some(Confidence::Unknown));
        assert!(d.function.is_some(), "the candidate name is still reported");
        assert!(d.args.is_none());
        assert!(d.warnings.iter().any(|w| w.contains("do NOT decode")));
    }

    #[test]
    fn no_calldata_is_a_plain_transfer() {
        for data in ["", "0x"] {
            let d = decode_call(&db(), 1, VITALIK, data);
            assert_eq!(d.kind, Kind::PlainTransfer, "for {data:?}");
            assert!(d.confidence.is_none());
        }
    }

    #[test]
    fn no_recipient_is_a_contract_creation() {
        let d = decode_call(&db(), 1, "", "0x6080604052");
        assert_eq!(d.kind, Kind::ContractCreation);
        assert!(d.warnings.iter().any(|w| w.contains("deploys")));
    }

    #[test]
    fn junk_is_malformed_not_a_panic() {
        assert_eq!(decode_call(&db(), 1, WETH, "0xzz").kind, Kind::Malformed);
        assert_eq!(decode_call(&db(), 1, "0xnot-an-address", "0x").kind, Kind::Malformed);
        assert_eq!(decode_call(&db(), 1, WETH, "0xa9059c").kind, Kind::Malformed);
    }

    #[test]
    fn a_read_only_function_is_called_out() {
        // balanceOf(address) on WETH.
        let data = format!("0x70a08231000000000000000000000000{}", &VITALIK[2..].to_lowercase());
        let d = decode_call(&db(), 1, WETH, &data);
        assert!(d.function.unwrap().read_only);
        assert!(d.warnings.iter().any(|w| w.contains("read-only")));
        // WETH9 leaves this parameter unnamed; it must still render positionally.
        assert_eq!(d.args.unwrap()[0].name, "[0]");
    }

    #[test]
    fn tuple_arguments_keep_their_component_names() {
        let db = db();
        // Aave v3 Pool's supply — flat, but confirms the verified path on a real DeFi entry.
        let pool = db
            .contract_at(1, &parse_address("0x97287a4f35e583d924f78ad88db8afce1379189a").unwrap())
            .expect("aave v3 pool is in the snapshot");
        assert_eq!(db.contract(pool).label, "Aave_v3: Pool");

        // A tuple-carrying signature must round-trip its inner names.
        let entry = (0..db.stats().1)
            .map(|i| db.entry(i))
            .find(|e| e.func.name == "swapDebt")
            .expect("swapDebt is in the snapshot");
        let names: Vec<_> =
            entry.func.inputs[0].components.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"debtAsset"), "got {names:?}");
    }

    #[test]
    fn an_imported_abi_makes_an_unknown_contract_verified() {
        let mut db = db();
        let addr = "0x000000000000000000000000000000000000bEEF";
        assert_eq!(decode_call(&db, 1, addr, TRANSFER).confidence, Some(Confidence::SignatureOnly));

        db.import(
            "my_token",
            "My Token",
            1,
            addr,
            r#"[{"type":"function","name":"transfer","inputs":[{"name":"to","type":"address"},{"name":"amount","type":"uint256"}],"outputs":[],"stateMutability":"nonpayable"}]"#,
        )
        .unwrap();

        let d = decode_call(&db, 1, addr, TRANSFER);
        assert_eq!(d.confidence, Some(Confidence::Verified));
        assert_eq!(d.contract.unwrap().label, "My Token");
    }

    /// Encode a real tuple-bearing signature with alloy, then decode it back: the
    /// only way to prove nested component names survive the whole path.
    #[test]
    fn a_tuple_call_round_trips_with_its_component_names() {
        use alloy::dyn_abi::{DynSolValue, JsonAbiExt};
        use alloy::primitives::U256;

        let db = db();
        let idx = (0..db.stats().1)
            .find(|&i| db.entry(i).func.name == "configureEModeCategory")
            .expect("aave's configureEModeCategory is in the snapshot");
        let func = &db.entry(idx).func;

        // configureEModeCategory(uint8 id, (uint16,uint16,uint16,string) category)
        let category = DynSolValue::Tuple(vec![
            DynSolValue::Uint(U256::from(9000u64), 16),
            DynSolValue::Uint(U256::from(9300u64), 16),
            DynSolValue::Uint(U256::from(10100u64), 16),
            DynSolValue::String("stablecoins".into()),
        ]);
        let encoded = func
            .abi_encode_input(&[DynSolValue::Uint(U256::from(1u64), 8), category])
            .expect("encodes");

        let calldata = format!("0x{}", hex::encode(&encoded));

        let d = decode_call(&db, 1, "0x000000000000000000000000000000000000dEaD", &calldata);
        let args = d.args.expect("decodes");
        assert_eq!(args[0].name, "id");
        assert_eq!(args[0].value.as_deref(), Some("1"));

        let cat = &args[1];
        assert_eq!(cat.name, "category");
        let kids = cat.components.as_ref().expect("tuple renders components");
        let names: Vec<_> = kids.iter().map(|k| k.name.as_str()).collect();
        assert_eq!(names, ["ltv", "liquidationThreshold", "liquidationBonus", "label"], "got {names:?}");
        assert_eq!(kids[3].value.as_deref(), Some("stablecoins"));
    }

    /// An imported name-variant of a known function must not read as a collision.
    #[test]
    fn a_name_variant_is_not_reported_as_a_selector_collision() {
        let mut db = db();
        let addr = "0x000000000000000000000000000000000000bEEF";
        db.import("my_token", "My Token", 1, addr,
            r#"[{"type":"function","name":"transfer","inputs":[{"name":"to","type":"address"},{"name":"amount","type":"uint256"}],"outputs":[],"stateMutability":"nonpayable"}]"#)
            .unwrap();

        for target in [addr, "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"] {
            let d = decode_call(&db, 1, target, TRANSFER);
            assert_eq!(d.confidence, Some(Confidence::Verified));
            assert!(!d.warnings.iter().any(|w| w.contains("share this selector")),
                "{target} warned about a non-collision: {:?}", d.warnings);
        }

        // Each contract keeps its own argument names.
        assert_eq!(decode_call(&db, 1, addr, TRANSFER).args.unwrap()[0].name, "to");
        assert_eq!(
            decode_call(&db, 1, "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2", TRANSFER)
                .args.unwrap()[0].name,
            "dst"
        );
    }

    #[test]
    fn element_type_strips_one_suffix() {
        assert_eq!(element_type("uint256[]"), "uint256");
        assert_eq!(element_type("uint256[3]"), "uint256");
        assert_eq!(element_type("uint256[][2]"), "uint256[]");
        assert_eq!(element_type("address"), "address");
    }
}
