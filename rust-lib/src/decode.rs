//! Calldata -> a typed interpretation that never claims more than it knows.
//!
//! A 4-byte selector is not proof of anything: anyone can deploy a contract
//! whose function collides with `transfer`. [`Confidence::Verified`] ties the
//! interpretation to a source ABI for the address being called;
//! [`Confidence::Listed`] names the weaker token-list association explicitly.

use alloy::dyn_abi::{DynSolValue, JsonAbiExt};
use alloy::json_abi::Param;
use serde::Serialize;

use crate::db::{checksum, parse_address, AbiDb};
use crate::router::{self, RouterStep};

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
    /// A token list ties `to` to a token identity and the selector is part of
    /// the standard ERC-20 interface, but no source-verified ABI backed it.
    Listed,
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
    pub function: Option<FunctionRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<Arg>>,
    /// The calls a `multicall` carries, each decoded against the SAME contract: a router's
    /// multicall is one transaction whose meaning is entirely in its parts, and a human shown
    /// "multicall, 2 item(s)" has been shown nothing. Empty for anything else.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub inner: Vec<DecodedCall>,
    /// What a verified swap router call does, read from its arguments. See [`crate::router`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub router: Option<RouterStep>,
    pub warnings: Vec<String>,
}

impl DecodedCall {
    fn bare(kind: Kind, warnings: Vec<String>) -> Self {
        DecodedCall {
            kind,
            confidence: None,
            selector: None,
            contract: None,
            function: None,
            args: None,
            inner: Vec::new(),
            router: None,
            warnings,
        }
    }
}

/// Decode one call. `to` may be empty for a contract creation; `data` may be
/// empty, `0x`, or `0x`-prefixed hex.
pub fn decode_call(db: &AbiDb, chain: u64, to: &str, data: &str) -> DecodedCall {
    decode_call_at(db, chain, to, data, 0)
}

/// How deep a multicall inside a multicall is followed. Two levels is one more than any
/// router emits; past that the bytes are shown raw rather than chased.
const MAX_INNER_DEPTH: usize = 2;

fn decode_call_at(db: &AbiDb, chain: u64, to: &str, data: &str, depth: usize) -> DecodedCall {
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
    let listed = contract_idx.and_then(|ci| {
        hits.iter()
            .copied()
            .find(|&h| db.entry(h).listed_contracts.contains(&ci))
    });

    let (chosen, mut confidence) = match declared {
        Some(h) => (Some(h), Confidence::Verified),
        None if listed.is_some() => (listed, Confidence::Listed),
        None if !hits.is_empty() => (hits.last().copied(), Confidence::SignatureOnly),
        None => (None, Confidence::Unknown),
    };

    if confidence == Confidence::Listed {
        warnings.push(
            "The address, token name and standard ERC-20 shape come from a token list, not \
             from verified deployed source. Treat them as list claims, not a check of the code."
                .into(),
        );
    } else if declared.is_none() {
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
    let mut inner = Vec::new();
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
            Ok(values) => {
                if entry.func.name == "multicall" && depth < MAX_INNER_DEPTH {
                    inner = inner_calls(db, chain, to, &entry.func.inputs, &values, depth + 1);
                }
                args = Some(build_args(&entry.func.inputs, &values));
            }
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

    let router = match (&function, &args) {
        (Some(f), Some(a)) if confidence == Confidence::Verified => router::read(db, chain, &f.signature, a),
        _ => None,
    };
    DecodedCall {
        kind: Kind::Call,
        confidence: Some(confidence),
        selector: Some(format!("0x{}", hex::encode(selector))),
        contract,
        function,
        args,
        inner,
        router,
        warnings,
    }
}

/// The `bytes[]` a multicall carries, each decoded as a call to the same contract. Only a
/// `bytes[]` parameter is followed: a deadline or a block hash beside it is left as it is.
fn inner_calls(
    db: &AbiDb,
    chain: u64,
    to: &str,
    params: &[Param],
    values: &[DynSolValue],
    depth: usize,
) -> Vec<DecodedCall> {
    let mut out = Vec::new();
    for (p, v) in params.iter().zip(values) {
        if p.ty != "bytes[]" {
            continue;
        }
        if let DynSolValue::Array(items) = v {
            for item in items {
                if let DynSolValue::Bytes(b) = item {
                    out.push(decode_call_at(db, chain, to, &format!("0x{}", hex::encode(b)), depth));
                }
            }
        }
    }
    out
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
    fn a_token_list_only_contract_is_listed_not_verified() {
        // KII is in Uniswap Labs Default but had no source-verified ABI in the
        // snapshot. Its standard ERC-20 shape is useful, but must not be called
        // verified merely because a list names the address.
        let d = decode_call(
            &db(),
            1,
            "0xeec6574eabba52bac3f0277f2cd5ac7e67197886",
            TRANSFER,
        );
        assert_eq!(d.confidence, Some(Confidence::Listed));
        assert_eq!(d.contract.as_ref().unwrap().label, "KII");
        assert_eq!(d.function.as_ref().unwrap().signature, "transfer(address,uint256)");
        assert_eq!(d.args.as_ref().unwrap()[0].name, "to");
        assert_eq!(d.args.as_ref().unwrap()[1].name, "amount");
        assert!(d.warnings.iter().any(|warning| warning.contains("token list")));
    }

    #[test]
    fn a_source_verified_sepolia_token_uses_its_own_abi() {
        let d = decode_call(
            &db(),
            11155111,
            "0x1f9840a85d5aF5bf1D1762F925BDADdC4201F984",
            TRANSFER,
        );
        assert_eq!(d.confidence, Some(Confidence::Verified));
        assert_eq!(d.contract.as_ref().unwrap().label, "UNI");
        // The source ABI calls these `dst` and `rawAmount`; the generic ERC-20
        // fallback calls them `to` and `amount`. This proves the former won.
        assert_eq!(d.args.as_ref().unwrap()[0].name, "dst");
        assert_eq!(d.args.as_ref().unwrap()[1].name, "rawAmount");
    }

    // SwapRouter02.multicall(deadline, [exactInputSingle(WETH to USDC at 0.05 percent, 1.5 ETH,
    // min 3748.16 USDC, to Alice), unwrapWETH9(3748.16 USDC, Alice)]) — the shape every V3 swap
    // the wallet makes takes, built with `cast calldata`.
    const SWAP_MULTICALL: &str = "0x5ae401dc000000000000000000000000000000000000000000000000000000006aa1f940000000000000000000000000000000000000000000000000000000000000004000000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000040000000000000000000000000000000000000000000000000000000000000016000000000000000000000000000000000000000000000000000000000000000e404e45aaf000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb4800000000000000000000000000000000000000000000000000000000000001f400000000000000000000000070997970c51812dc3a010c7d01b50e0d17dc79c800000000000000000000000000000000000000000000000014d1120d7b16000000000000000000000000000000000000000000000000000000000000df6862f0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000004449404b7c00000000000000000000000000000000000000000000000000000000df6862f000000000000000000000000070997970c51812dc3a010c7d01b50e0d17dc79c800000000000000000000000000000000000000000000000000000000";
    const SWAP_ROUTER_02: &str = "0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45";

    #[test]
    fn a_router_multicall_is_decoded_into_its_parts() {
        let db = AbiDb::embedded().unwrap();
        let d = decode_call(&db, 1, SWAP_ROUTER_02, SWAP_MULTICALL);
        assert_eq!(d.confidence, Some(Confidence::Verified), "{:?}", d.warnings);
        assert_eq!(d.contract.as_ref().unwrap().label, "Uniswap V3: SwapRouter02");
        assert_eq!(d.function.as_ref().unwrap().signature, "multicall(uint256,bytes[])");
        assert_eq!(d.inner.len(), 2, "both calls the multicall carries");
        let swap = &d.inner[0];
        assert_eq!(swap.confidence, Some(Confidence::Verified));
        assert_eq!(swap.function.as_ref().unwrap().name, "exactInputSingle");
        let params = &swap.args.as_ref().unwrap()[0];
        let fields = params.components.as_ref().unwrap();
        assert_eq!(fields[0].value.as_deref(), Some("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"));
        assert_eq!(fields[2].value.as_deref(), Some("500"));
        assert_eq!(fields[4].value.as_deref(), Some("1500000000000000000"));
        let unwrap = &d.inner[1];
        assert_eq!(unwrap.function.as_ref().unwrap().signature, "unwrapWETH9(uint256,address)");
        assert_eq!(unwrap.args.as_ref().unwrap()[1].value.as_deref(), Some("0x70997970C51812dc3A010C7d01b50e0d17dc79C8"));
        // The same bytes at an address the database does not know: the parts are still
        // named from their selectors, and say so.
        let elsewhere = decode_call(&db, 1, "0x1111111111111111111111111111111111111111", SWAP_MULTICALL);
        assert_eq!(elsewhere.confidence, Some(Confidence::SignatureOnly));
        assert_eq!(elsewhere.inner.len(), 2);
        assert_eq!(elsewhere.inner[0].confidence, Some(Confidence::SignatureOnly));
    }

    #[test]
    fn the_sepolia_router_multicall_is_verified_too() {
        let db = AbiDb::embedded().unwrap();
        let d = decode_call(
            &db,
            11155111,
            "0x3bFA4769FB09eefC5a80d6E87c3B9C650f7Ae48E",
            SWAP_MULTICALL,
        );
        assert_eq!(d.confidence, Some(Confidence::Verified), "{:?}", d.warnings);
        assert_eq!(d.function.as_ref().unwrap().signature, "multicall(uint256,bytes[])");
        assert_eq!(d.inner.len(), 2);
        assert!(d.inner.iter().all(|call| call.confidence == Some(Confidence::Verified)));
    }

    #[test]
    fn the_wallets_other_deployments_are_known() {
        let db = AbiDb::embedded().unwrap();
        for (chain, addr) in [(11155111u64, "0x3bFA4769FB09eefC5a80d6E87c3B9C650f7Ae48E"), (8453, "0x2626664c2603336E57B271c5C0b26F421741e481"), (10, SWAP_ROUTER_02)] {
            let idx = db.contract_at(chain, &parse_address(addr).unwrap()).expect("router known");
            assert_eq!(db.contract(idx).label, "Uniswap V3: SwapRouter02");
        }
        let v2 = db.contract_at(11155111, &parse_address("0xeE567Fe1712Faf6149d80dA1E6934E354124CfE3").unwrap()).unwrap();
        assert_eq!(db.contract(v2).label, "UniSwap Router02");
        let usdc = db.contract_at(1, &parse_address("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48").unwrap()).unwrap();
        assert_eq!((db.contract(usdc).label.as_str(), db.contract(usdc).decimals), ("USDC", Some(6)));
        // An approval of USDC for the router decodes verified, in USDC units.
        let approve = "0x095ea7b300000000000000000000000068b3465833fb72a70ecdf485e0e4c7bd8665fc45000000000000000000000000000000000000000000000000000000003b9aca00";
        let d = decode_call(&db, 1, "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", approve);
        assert_eq!(d.confidence, Some(Confidence::Verified));
        assert_eq!(d.function.as_ref().unwrap().signature, "approve(address,uint256)");
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
