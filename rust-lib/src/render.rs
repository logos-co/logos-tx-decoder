//! A [`DecodedCall`] as lines for an approval surface.
//!
//! These are ADDITIONAL to the raw fields, never a replacement: the caller must
//! still show `to`, `value` and the full calldata. Every line says which tier it
//! came from, because an unverified reading on a signing screen that looks like
//! a verified one is worse than no reading at all.

use crate::decode::{Arg, Confidence, DecodedCall, Kind};

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
                    Some(args) if !args.is_empty() => render_args(args, 2, &mut out),
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
