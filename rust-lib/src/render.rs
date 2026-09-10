//! A [`DecodedCall`] as lines for an approval surface.
//!
//! These are ADDITIONAL to the raw fields, never a replacement: the caller must
//! still show `to`, `value` and the full calldata. Every line says which tier it
//! came from, because an unverified reading on a signing screen that looks like
//! a verified one is worse than no reading at all.

use crate::decode::{Arg, Confidence, DecodedCall, Kind};
use crate::units;

/// Which argument of a call is an amount in the contract's own units, by signature.
///
/// A position table rather than a rule about `uint256`: on a token contract a `uint256`
/// is just as likely to be a deadline or a token id, and scaling one of those would
/// invent a number that means nothing. These three are the ERC-20 standard's, where the
/// unit is fixed by the standard itself.
const AMOUNT_ARGS: &[(&str, &[usize])] = &[
    ("transfer(address,uint256)", &[1]),
    ("approve(address,uint256)", &[1]),
    ("transferFrom(address,address,uint256)", &[2]),
];

pub fn describe(d: &DecodedCall) -> Vec<String> {
    let mut out = Vec::new();

    match d.kind {
        Kind::PlainTransfer => {
            out.push("Interpreted: plain value transfer — no calldata, no contract call.".into());
            if let Some(c) = &d.contract {
                out.push(format!("  Recipient is a known contract: {}", c.label));
            }
            if let Some(line) = named_address(d) {
                out.push(line);
            }
        }
        Kind::ContractCreation => {
            out.push("Interpreted: CONTRACT CREATION — no recipient.".into());
        }
        Kind::Malformed => {
            out.push("Interpreted: unavailable — the request is malformed.".into());
        }
        Kind::Call => {
            out.push(header(d));
            if let Some(line) = named_address(d) {
                out.push(line);
            }
            if let Some(f) = &d.function {
                out.push(format!("  Function: {}", f.signature));
                match &d.args {
                    Some(args) if !args.is_empty() => {
                        render_args(args, 2, &mut out);
                        if let Some(line) = amount_in_units(d, &f.signature, args) {
                            out.push(line);
                        }
                    }
                    Some(_) => out.push("  (no arguments)".into()),
                    None => {}
                }
            }
        }
    }

    for w in &d.warnings {
        out.push(format!("  ! {w}"));
    }
    out
}

fn header(d: &DecodedCall) -> String {
    let selector = d.selector.as_deref().unwrap_or("(none)");
    match (d.confidence, &d.contract) {
        (Some(Confidence::Verified), Some(c)) => format!(
            "Interpreted: {} — VERIFIED (this address is {} on chain {}, and it declares this function)",
            c.label, c.label, c.chain
        ),
        (Some(Confidence::SignatureOnly), Some(c)) => format!(
            "Interpreted: UNVERIFIED — address is {}, but the reading below is a selector guess",
            c.label
        ),
        (Some(Confidence::SignatureOnly), None) => {
            "Interpreted: UNVERIFIED — guessed from the 4-byte selector alone".into()
        }
        (Some(Confidence::Unknown), _) | (None, _) => {
            format!("Interpreted: unavailable — selector {selector} could not be resolved")
        }
        (Some(Confidence::Verified), None) => {
            format!("Interpreted: selector {selector}")
        }
    }
}

/// What this address is called, when a token list says so and the ABI database does not.
///
/// Its own line, and never folded into the header: the header states a CONFIDENCE about the
/// code, and this is a claim about the address's name. A list a user can add to is not the
/// same claim as the one compiled in, so the line says which answered.
fn named_address(d: &DecodedCall) -> Option<String> {
    let t = d.token.as_ref()?;
    if d.contract.is_some() && d.confidence == Some(Confidence::Verified) {
        // The header already named it, with more behind the name than a list has.
        return None;
    }
    Some(if t.source == "embedded" {
        format!(
            "  Address is {} ({}) according to the token list — a name, not a check of the code.",
            t.symbol, t.name
        )
    } else {
        format!(
            "  Address is {} ({}) according to a {} token list on this device — a name, not a \
             check of the code.",
            t.symbol, t.name, t.source
        )
    })
}

/// The raw amount restated in the token's own units, when — and only when — the decimals
/// came from an ADDRESS match and the signature says which argument is the amount.
///
/// An address match is either the ABI database's own `decimals` for a VERIFIED contract, or
/// a token list naming this exact (chain, address). Both are keyed by address, so a wrong
/// address matches nothing rather than scaling by another token's units. What is still
/// forbidden is taking decimals from a SELECTOR match: the common decimals are not
/// universal, and reading 6-decimal USDC as 18 is wrong by a factor of a trillion, and
/// wrong in the direction that looks harmless.
///
/// The argument POSITION still rests on the signature, which may be a guess — so the line
/// says whose units it is using, and the raw argument stays exactly where it was above it.
///
/// ADDITIVE: an extra line.
fn amount_in_units(d: &DecodedCall, signature: &str, args: &[Arg]) -> Option<String> {
    let verified = d.confidence == Some(Confidence::Verified);
    let (label, decimals) = match (&d.contract, &d.token) {
        (Some(c), _) if verified && c.decimals.is_some() => (c.label.clone(), c.decimals?),
        (_, Some(t)) => (t.symbol.clone(), t.decimals),
        _ => return None,
    };
    let idx = *AMOUNT_ARGS.iter().find(|(sig, _)| *sig == signature)?.1.first()?;
    let arg = args.get(idx)?;
    let scaled = units::scale(arg.value.as_deref()?, decimals)?;
    Some(format!("  In {label} units: {scaled} {label}"))
}

fn render_args(args: &[Arg], depth: usize, out: &mut Vec<String>) {
    let pad = args.iter().map(|a| a.name.chars().count()).max().unwrap_or(0);
    let indent = "  ".repeat(depth);
    for a in args {
        let label = format!("{}{:<pad$}", indent, format!("{}:", a.name), pad = pad + 1);
        match (&a.value, &a.components, &a.items) {
            (Some(v), _, _) => out.push(format!("{label} {v}")),
            (_, Some(kids), _) => {
                out.push(format!("{label} ({})", a.ty));
                render_args(kids, depth + 1, out);
            }
            (_, _, Some(items)) => {
                out.push(format!("{label} {} item(s)", items.len()));
                render_args(items, depth + 1, out);
            }
            _ => out.push(format!("{label} ?")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::AbiDb;
    use crate::decode::{decode_call, decode_call_with};
    use crate::tokens::TokenRegistry;

    const WETH: &str = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2";
    const TRANSFER: &str = "0xa9059cbb000000000000000000000000d8da6bf26964af9d7eed9e03e53415d37aa96045000000000000000000000000000000000000000000000000000000003b9aca00";

    fn lines(chain: u64, to: &str, data: &str) -> Vec<String> {
        describe(&decode_call(&AbiDb::embedded().unwrap(), chain, to, data))
    }

    /// USDC on mainnet: six decimals, and NOT in the ABI database — the exact shape the
    /// token list exists to cover.
    const USDC: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";

    fn registry(source: &str) -> TokenRegistry {
        TokenRegistry::from_json(&format!(
            r#"{{"tokens":[{{"chainId":1,"address":"{USDC}","symbol":"USDC","name":"USD Coin","decimals":6,"source":"{source}"}}]}}"#
        ))
        .unwrap()
    }

    fn listed_lines(reg: &TokenRegistry, chain: u64, to: &str, data: &str) -> Vec<String> {
        describe(&decode_call_with(&AbiDb::embedded().unwrap(), reg, chain, to, data))
    }

    #[test]
    fn a_listed_address_is_named_and_its_amount_restated() {
        let l = listed_lines(&registry("embedded"), 1, USDC, TRANSFER);
        assert!(l.iter().any(|x| x.contains("Address is USDC")), "{l:#?}");
        // 1_000_000_000 raw at six decimals is 1000 USDC, not 1e-9 ETH-shaped.
        assert!(l.iter().any(|x| x.contains("In USDC units: 1000 USDC")), "{l:#?}");
    }

    #[test]
    fn naming_an_address_never_makes_the_reading_verified() {
        // The whole point of keeping the registry out of the ABI database. The function is
        // still a selector guess, and the first line must go on saying so.
        let l = listed_lines(&registry("embedded"), 1, USDC, TRANSFER);
        assert!(l[0].contains("UNVERIFIED"), "{l:#?}");
        assert!(l.iter().any(|x| x.contains("not a check of the code")), "{l:#?}");
    }

    #[test]
    fn a_list_this_device_was_told_about_says_so_on_the_line() {
        // A user can add a custom token, so a friendly symbol on a hostile address is
        // reachable. It is allowed to name the address; it is not allowed to do it quietly.
        for src in ["custom", "downloaded"] {
            let l = listed_lines(&registry(src), 1, USDC, TRANSFER);
            let named = l.iter().find(|x| x.contains("Address is USDC")).expect("named");
            assert!(named.contains(src), "{src} must be on the line: {named}");
        }
        let shipped = listed_lines(&registry("embedded"), 1, USDC, TRANSFER);
        let named = shipped.iter().find(|x| x.contains("Address is USDC")).unwrap();
        assert!(!named.contains("custom") && !named.contains("downloaded"), "{named}");
    }

    #[test]
    fn a_registry_that_does_not_hold_the_address_restates_nothing() {
        // The control that keeps the relaxation honest: having A list is not having THIS
        // address. Decimals may never come from a selector match.
        let l = listed_lines(&registry("embedded"), 1, "0x000000000000000000000000000000000000dEaD", TRANSFER);
        assert!(l[0].contains("UNVERIFIED"), "{l:#?}");
        assert!(!l.iter().any(|x| x.contains(" units:")), "{l:#?}");
        assert!(!l.iter().any(|x| x.contains("Address is")), "{l:#?}");
    }

    #[test]
    fn a_hit_on_another_chain_is_not_a_hit() {
        let l = listed_lines(&registry("embedded"), 137, USDC, TRANSFER);
        assert!(!l.iter().any(|x| x.contains(" units:")), "{l:#?}");
        assert!(!l.iter().any(|x| x.contains("Address is")), "{l:#?}");
    }

    #[test]
    fn a_verified_contract_is_not_renamed_by_a_list() {
        // WETH is in the ABI database with its own decimals. The header already names it
        // with more behind the name than a list has, so the list adds no second opinion.
        let reg = TokenRegistry::from_json(&format!(
            r#"{{"tokens":[{{"chainId":1,"address":"{WETH}","symbol":"NOTWETH","decimals":2}}]}}"#
        ))
        .unwrap();
        let l = listed_lines(&reg, 1, WETH, TRANSFER);
        assert!(l[0].contains("VERIFIED"), "{l:#?}");
        assert!(!l.iter().any(|x| x.contains("NOTWETH")), "a list cannot rename a verified contract: {l:#?}");
    }

    #[test]
    fn a_verified_call_says_so_on_the_first_line() {
        let l = lines(1, WETH, TRANSFER);
        assert!(l[0].contains("VERIFIED"), "{l:#?}");
        assert!(l[0].contains("WETH"));
        assert!(l.iter().any(|x| x.contains("transfer(address,uint256)")));
        assert!(l.iter().any(|x| x.contains("dst:") && x.contains("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")));
        assert!(l.iter().any(|x| x.contains("wad:") && x.contains("1000000000")));
    }

    #[test]
    fn an_unverified_call_is_marked_on_the_first_line_too() {
        let l = lines(1, "0x000000000000000000000000000000000000dEaD", TRANSFER);
        assert!(l[0].contains("UNVERIFIED"), "{l:#?}");
        // The reading is still offered, but never without the flag.
        assert!(l.iter().any(|x| x.contains("transfer(address,uint256)")));
        assert!(l.iter().any(|x| x.starts_with("  ! ")));
    }

    #[test]
    fn a_verified_token_amount_is_restated_in_that_tokens_units() {
        // The raw argument is what is signed and stays exactly where it was. This is an
        // extra line, and it is also the only way a human can check the requester's
        // claim in section 1 of the signer against the bytes in section 2.
        let l = lines(1, WETH, TRANSFER);
        assert!(l.iter().any(|x| x.contains("wad:") && x.contains("1000000000")), "{l:#?}");
        assert!(
            l.iter().any(|x| x.contains("In WETH units: 0.000000001 WETH")),
            "{l:#?}"
        );
    }

    #[test]
    fn an_unverified_match_is_never_restated_in_units() {
        // The whole risk. A selector match says the CALL looks like a transfer; it says
        // nothing about which token this address is, so its decimals are unknown. Reading
        // 6-decimal USDC as 18 understates the amount by a factor of a trillion.
        for to in ["0x000000000000000000000000000000000000dEaD", WETH] {
            let chain = if to == WETH { 137 } else { 1 };
            let l = lines(chain, to, TRANSFER);
            assert!(!l[0].contains("VERIFIED") || l[0].contains("UNVERIFIED"), "{l:#?}");
            assert!(!l.iter().any(|x| x.contains("In ") && x.contains(" units:")), "{l:#?}");
        }
    }

    #[test]
    fn an_imported_token_shows_raw_units() {
        // An import brings an ABI, and an ABI does not carry decimals — that is a call to
        // the live contract, which this library never makes.
        let mut db = AbiDb::embedded().unwrap();
        let addr = "0x000000000000000000000000000000000000bEEF";
        let abi = r#"[{"type":"function","name":"transfer","stateMutability":"nonpayable",
            "inputs":[{"name":"dst","type":"address"},{"name":"wad","type":"uint256"}],
            "outputs":[{"name":"","type":"bool"}]}]"#;
        db.import("erc20", "FAKE", 1, addr, abi).unwrap();
        let l = describe(&decode_call(&db, 1, addr, TRANSFER));
        assert!(l[0].contains("VERIFIED"), "the ABI match is still verified: {l:#?}");
        assert!(!l.iter().any(|x| x.contains(" units:")), "{l:#?}");
    }

    #[test]
    fn an_unknown_selector_offers_nothing_to_misread() {
        let l = lines(1, WETH, "0xdeadbeef");
        assert!(l[0].contains("unavailable"), "{l:#?}");
        assert!(!l.iter().any(|x| x.contains("Function:")));
    }

    #[test]
    fn a_plain_transfer_is_described_as_one() {
        let l = lines(1, "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045", "0x");
        assert!(l[0].contains("plain value transfer"), "{l:#?}");
    }

    #[test]
    fn every_tier_produces_a_nonempty_first_line() {
        for (to, data) in [
            (WETH, TRANSFER),
            ("0x000000000000000000000000000000000000dEaD", TRANSFER),
            (WETH, "0xdeadbeef"),
            (WETH, "0x"),
            ("", "0x6080"),
            (WETH, "0xzz"),
        ] {
            let l = lines(1, to, data);
            assert!(!l.is_empty() && l[0].starts_with("Interpreted:"), "{to} {data} -> {l:#?}");
        }
    }
}
