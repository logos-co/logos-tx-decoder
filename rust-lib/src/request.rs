//! A whole signing request, read once.
//!
//! Both surfaces that show this library's reading — `evm_signer_ui` over the C ABI and
//! `evm_signer_cli` as a Rust crate — go through here. A request read twice in two
//! languages is two things to drift, and the one place they must agree is the text a
//! human approves.

use crate::db::AbiDb;
use crate::decode::{decode_call, DecodedCall};
use crate::intent::parse_render_lines;
use crate::render::{describe, value_line};

/// One transaction leg, decoded, and the lines a surface shows for it.
pub struct LegReading {
    /// The `[n]` the keystore printed.
    pub index: usize,
    pub chain_id: u64,
    pub to: String,
    /// What the decode found, for a caller adding a layer of its own over it.
    pub call: DecodedCall,
    pub lines: Vec<String>,
}

/// Every transaction leg of a keystore render block, with the block's item count: an
/// interpretation of item 2 of 3 must not read as a description of the whole request.
pub struct RequestReading {
    pub items: usize,
    pub legs: Vec<LegReading>,
}

/// Read a request as the lines to show beneath the keystore's own.
pub fn read_request(db: &AbiDb, render_lines: &[String]) -> RequestReading {
    let scan = parse_render_lines(render_lines);
    let legs = scan
        .legs
        .into_iter()
        .map(|leg| {
            let call = decode_call(db, leg.chain_id, &leg.to, &leg.data);
            let mut lines = describe(&call);
            // Under the header, where the rest of the leg's reading follows it.
            if let Some(line) = value_line(leg.value.as_deref()) {
                lines.insert(lines.len().min(1), line);
            }
            LegReading { index: leg.index, chain_id: leg.chain_id, to: leg.to, call, lines }
        })
        .collect();
    RequestReading { items: scan.items, legs }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Uniswap app's swap, as the keystore renders it.
    fn swap_lines() -> Vec<String> {
        let w = |h: &str| format!("{:0>64}", h);
        let inner = format!(
            "04e45aaf{}{}{}{}{}{}{}",
            w("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"),
            w("dac17f958d2ee523a2206206994597c13d831ec7"),
            w("64"),
            w("a1e277ea6b97effc5b61b3bf5de03f438981247e"),
            w("e8d4a51000"),
            w("a3d"),
            w("0")
        );
        let data = format!("0x5ae401dc{}{}{}{}{}{inner}{}", w("6aaef2e8"), w("40"), w("1"), w("20"), w("e4"), "0".repeat(56));
        vec![
            "Account: 0xa1E277eA6b97eFfc5b61B3BF5dE03F438981247E".into(),
            "1 item(s) to sign:".into(),
            "  [1] Transaction on chain 1".into(),
            "      To: 0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45".into(),
            "      Value: 0xe8d4a51000 (1000000000000)".into(),
            format!("      Data: {data}"),
        ]
    }

    #[test]
    fn a_request_reads_as_its_legs_and_the_value_each_carries() {
        let db = AbiDb::embedded().unwrap();
        let read = read_request(&db, &swap_lines());
        assert_eq!(read.items, 1);
        let leg = &read.legs[0];
        assert_eq!((leg.index, leg.chain_id), (1, 1));
        assert_eq!(leg.lines[1], "  Sends 0.000001 of the native coin with this call (value 1000000000000 wei).");
        let text = leg.lines.join("\n");
        assert!(text.contains("Sells exactly 0.000001 WETH (amountIn 1000000000000)"), "{text}");
        assert!(text.contains("Sends what it buys to 0xa1E277eA6b97eFfc5b61B3BF5dE03F438981247E."), "{text}");
        // The decode itself is handed over, for a caller's own layer over the same call.
        assert_eq!(leg.call.inner[0].function.as_ref().unwrap().name, "exactInputSingle");
    }

    #[test]
    fn a_block_with_nothing_to_decode_still_counts_its_items() {
        let db = AbiDb::embedded().unwrap();
        let read = read_request(&db, &["2 item(s) to sign:".into(), "  [1] Sign text message".into(), "  [2] Sign digest".into()]);
        assert_eq!((read.items, read.legs.len()), (2, 0));
    }
}
