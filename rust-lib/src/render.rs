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
        }
        Kind::ContractCreation => {
            out.push("Interpreted: CONTRACT CREATION — no recipient.".into());
        }
        Kind::Malformed => {
            out.push("Interpreted: unavailable — the request is malformed.".into());
        }
        Kind::Call => {
            out.push(header(d));
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
            // What the multicall actually does, one part at a time, each described exactly
            // as it would be on its own and indented under the call that carries it.
            for (i, call) in d.inner.iter().enumerate() {
                out.push(format!("  Inner call {} of {}:", i + 1, d.inner.len()));
                for line in describe(call) {
                    out.push(format!("    {line}"));
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
        (Some(Confidence::Listed), Some(c)) => format!(
            "Interpreted: {} — LISTED (a token list names this address on chain {}; code not checked)",
            c.label, c.chain
        ),
        (Some(Confidence::Listed), None) => {
            "Interpreted: LISTED token interface — address identity unavailable".into()
        }
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

/// The raw amount restated in the token's own units, when — and only when — all three
/// hold: the address is VERIFIED (a selector match says nothing about which token this
/// is), that contract's decimals are known rather than assumed, and the signature says
/// which argument is the amount. Anything short of that shows raw units, because the
/// common decimals are not universal — reading 6-decimal USDC as 18 is wrong by a
/// factor of a trillion, and wrong in the direction that looks harmless.
///
/// ADDITIVE: an extra line. The raw argument stays exactly where it was.
fn amount_in_units(d: &DecodedCall, signature: &str, args: &[Arg]) -> Option<String> {
    if d.confidence != Some(Confidence::Verified) {
        return None;
    }
    let c = d.contract.as_ref()?;
    let decimals = c.decimals?;
    let idx = *AMOUNT_ARGS.iter().find(|(sig, _)| *sig == signature)?.1.first()?;
    let arg = args.get(idx)?;
    let scaled = units::scale(arg.value.as_deref()?, decimals)?;
    Some(format!("  In {} units: {} {}", c.label, scaled, c.label))
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
    #[test]
    fn a_multicall_describes_its_parts_under_it() {
        let db = crate::db::AbiDb::embedded().unwrap();
        let d = crate::decode::decode_call(&db, 1, "0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45", "0x5ae401dc000000000000000000000000000000000000000000000000000000006aa1f940000000000000000000000000000000000000000000000000000000000000004000000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000040000000000000000000000000000000000000000000000000000000000000016000000000000000000000000000000000000000000000000000000000000000e404e45aaf000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb4800000000000000000000000000000000000000000000000000000000000001f400000000000000000000000070997970c51812dc3a010c7d01b50e0d17dc79c800000000000000000000000000000000000000000000000014d1120d7b16000000000000000000000000000000000000000000000000000000000000df6862f0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000004449404b7c00000000000000000000000000000000000000000000000000000000df6862f000000000000000000000000070997970c51812dc3a010c7d01b50e0d17dc79c800000000000000000000000000000000000000000000000000000000");
        let lines = describe(&d);
        let text = lines.join("\n");
        assert!(text.contains("Function: multicall(uint256,bytes[])"), "{text}");
        assert!(text.contains("  Inner call 1 of 2:"), "{text}");
        assert!(text.contains("    Function: exactInputSingle("), "{text}");
        assert!(text.contains("  Inner call 2 of 2:"), "{text}");
        assert!(text.contains("    Function: unwrapWETH9(uint256,address)"), "{text}");
        // The inner lines sit under the call, indented past its own argument lines.
        let inner = lines.iter().position(|l| l.starts_with("  Inner call 1")).unwrap();
        assert!(lines[inner + 1].starts_with("    "), "{:?}", lines[inner + 1]);
    }

    use super::*;
    use crate::db::AbiDb;
    use crate::decode::decode_call;

    const WETH: &str = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2";
    const TRANSFER: &str = "0xa9059cbb000000000000000000000000d8da6bf26964af9d7eed9e03e53415d37aa96045000000000000000000000000000000000000000000000000000000003b9aca00";

    fn lines(chain: u64, to: &str, data: &str) -> Vec<String> {
        describe(&decode_call(&AbiDb::embedded().unwrap(), chain, to, data))
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
    fn a_list_only_token_is_labelled_without_claiming_verification() {
        let l = lines(
            1,
            "0xeec6574eabba52bac3f0277f2cd5ac7e67197886",
            TRANSFER,
        );
        assert!(l[0].contains("KII — LISTED"), "{l:#?}");
        assert!(l[0].contains("code not checked"), "{l:#?}");
        assert!(!l.iter().any(|line| line.contains(" units:")), "{l:#?}");
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
            ("0xeec6574eabba52bac3f0277f2cd5ac7e67197886", TRANSFER),
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
