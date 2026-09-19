//! Recover transaction legs from the keystore's own render lines.
//!
//! The approver is handed `render_lines` and nothing else — no structured `to`
//! or `data`. Reading the legs back out of those lines is not a workaround: it
//! means the interpretation is derived from the exact text the human is looking
//! at, so the two cannot disagree.
//!
//! Fails closed. An unrecognised shape yields no leg, and the caller shows the
//! verbatim lines alone — which is what it did before this existed.

/// One transaction leg found in a render block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxLeg {
    /// The `[n]` the keystore printed, so a caller can line the two up.
    pub index: usize,
    pub chain_id: u64,
    /// Empty for a contract creation.
    pub to: String,
    pub data: String,
    pub creation: bool,
    /// The native amount sent, as a decimal integer, when the block printed one.
    pub value: Option<String>,
}

const LEG: &str = "] Transaction on chain ";
const CREATION: &str = "CONTRACT CREATION";

/// What a render block contained.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenderScan {
    /// Every `[n]` item, including the message and digest ones there is nothing
    /// to decode in. A caller needs this to know whether an interpretation
    /// covers the whole request or only part of it.
    pub items: usize,
    pub legs: Vec<TxLeg>,
    /// The `Account:` the whole block is signed by.
    pub account: Option<String>,
}

/// Extract every transaction leg. Message and digest legs are counted but not
/// returned: there is no calldata to decode in them.
pub fn parse_render_lines(lines: &[String]) -> RenderScan {
    let mut out: Vec<TxLeg> = Vec::new();
    let mut items = 0usize;
    let mut account = None;

    for line in lines {
        let t = line.trim();
        if let Some(v) = t.strip_prefix("Account:") {
            if items == 0 && account.is_none() {
                account = Some(v.trim().to_string());
            }
            continue;
        }

        if let Some(leg) = start_of_leg(t) {
            items += 1;
            out.push(leg);
            continue;
        }
        // Any other `[n]` header ends the current leg — a message or digest
        // item must not inherit the previous transaction's fields.
        if is_item_header(t) {
            items += 1;
            continue;
        }

        let Some(cur) = out.last_mut() else { continue };
        if t.contains(CREATION) {
            cur.creation = true;
            cur.to.clear();
        } else if let Some(v) = t.strip_prefix("To:") {
            if cur.to.is_empty() && !cur.creation {
                cur.to = v.trim().to_string();
            }
        } else if let Some(v) = t.strip_prefix("Value:") {
            if cur.value.is_none() {
                cur.value = decimal_of(v);
            }
        } else if let Some(v) = t.strip_prefix("Data:") {
            let v = v.trim();
            // `Data: (none)` is the keystore's empty-calldata spelling.
            if cur.data.is_empty() && v.starts_with("0x") {
                cur.data = v.to_string();
            }
        }
    }
    RenderScan { items, legs: out, account }
}

/// The keystore prints a number as `0x…`, `0x… (decimal)` or a decimal: the decimal, or none.
fn decimal_of(v: &str) -> Option<String> {
    let v = v.trim();
    if let Some((_, rest)) = v.split_once('(') {
        let d = rest.strip_suffix(')')?.trim();
        return Some(d.to_string()).filter(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()));
    }
    if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) {
        return Some(v.to_string());
    }
    let hex = v.strip_prefix("0x").or_else(|| v.strip_prefix("0X"))?;
    alloy::primitives::U256::from_str_radix(hex, 16).ok().map(|n| n.to_string())
}

/// `  [3] Transaction on chain 11155111` -> a leg with index 3 on that chain.
fn start_of_leg(t: &str) -> Option<TxLeg> {
    let rest = t.strip_prefix('[')?;
    let at = rest.find(LEG)?;
    let index = rest[..at].parse().ok()?;
    let chain_id = rest[at + LEG.len()..].trim().parse().ok()?;
    Some(TxLeg { index, chain_id, to: String::new(), data: String::new(), creation: false, value: None })
}

fn is_item_header(t: &str) -> bool {
    let Some(rest) = t.strip_prefix('[') else { return false };
    match rest.find("] ") {
        Some(i) => rest[..i].chars().all(|c| c.is_ascii_digit()) && i > 0,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Byte-for-byte the shape `keystore_module::approval::render()` emits.
    fn keystore_render() -> Vec<String> {
        [
            "Account: 0xd8da6bf26964af9d7eed9e03e53415d37aa96045",
            "Commitment: 3f2a...",
            "2 item(s) to sign:",
            "  [1] Transaction on chain 1",
            "      To: 0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            "      Value: 0",
            "      Nonce: 0x5 (5)",
            "      Gas limit: 21000",
            "      Selector: 0xa9059cbb",
            "      Data: 0xa9059cbb0000000000000000000000001111111111111111111111111111111111111111",
            "  [2] Sign text message",
            "      Text: hello",
            "      Bytes: 0x68656c6c6f",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    #[test]
    fn one_transaction_leg_is_recovered_and_the_message_leg_is_not() {
        let legs = parse_render_lines(&keystore_render()).legs;
        assert_eq!(legs.len(), 1);
        assert_eq!(legs[0].index, 1);
        assert_eq!(legs[0].chain_id, 1);
        assert_eq!(legs[0].to, "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        assert!(legs[0].data.starts_with("0xa9059cbb"));
        assert!(!legs[0].creation);
    }

    #[test]
    fn the_account_and_each_legs_value_are_read_as_decimals() {
        let scan = parse_render_lines(&keystore_render());
        assert_eq!(scan.account.as_deref(), Some("0xd8da6bf26964af9d7eed9e03e53415d37aa96045"));
        assert_eq!(scan.legs[0].value.as_deref(), Some("0"));
        for (printed, want) in [("0xe8d4a51000 (1000000000000)", Some("1000000000000")),
                                ("0xe8d4a51000", Some("1000000000000")),
                                ("1000", Some("1000")),
                                ("(none)", None), ("0xzz", None)] {
            assert_eq!(decimal_of(printed).as_deref(), want, "{printed}");
        }
    }

    #[test]
    fn a_message_leg_cannot_inherit_the_previous_transactions_fields() {
        // The `Bytes:` line of leg 2 must not become leg 1's data, and leg 2
        // must not appear at all.
        let legs = parse_render_lines(&keystore_render()).legs;
        assert_eq!(legs.len(), 1);
        assert!(!legs[0].data.contains("68656c6c6f"));
    }

    #[test]
    fn contract_creation_is_recognised_and_has_no_recipient() {
        let lines: Vec<String> = [
            "  [1] Transaction on chain 1",
            "      ** CONTRACT CREATION — no recipient **",
            "      Value: 0",
            "      Data: 0x6080604052",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        let legs = parse_render_lines(&lines).legs;
        assert_eq!(legs.len(), 1);
        assert!(legs[0].creation);
        assert!(legs[0].to.is_empty());
        assert_eq!(legs[0].data, "0x6080604052");
    }

    #[test]
    fn empty_calldata_is_left_empty() {
        let lines: Vec<String> = [
            "  [1] Transaction on chain 137",
            "      To: 0x1111111111111111111111111111111111111111",
            "      Data: (none)",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        let legs = parse_render_lines(&lines).legs;
        assert_eq!(legs[0].chain_id, 137);
        assert!(legs[0].data.is_empty(), "`(none)` must not become calldata");
    }

    #[test]
    fn several_transaction_legs_keep_their_own_fields() {
        let lines: Vec<String> = [
            "  [1] Transaction on chain 1",
            "      To: 0x1111111111111111111111111111111111111111",
            "      Data: 0xaaaaaaaa",
            "  [2] Transaction on chain 10",
            "      To: 0x2222222222222222222222222222222222222222",
            "      Data: 0xbbbbbbbb",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        let legs = parse_render_lines(&lines).legs;
        assert_eq!(legs.len(), 2);
        assert_eq!((legs[0].index, legs[0].chain_id, legs[0].data.as_str()), (1, 1, "0xaaaaaaaa"));
        assert_eq!((legs[1].index, legs[1].chain_id, legs[1].data.as_str()), (2, 10, "0xbbbbbbbb"));
    }

    #[test]
    fn items_counts_every_leg_including_the_undecodable_ones() {
        let scan = parse_render_lines(&keystore_render());
        assert_eq!(scan.items, 2, "one transaction and one message");
        assert_eq!(scan.legs.len(), 1, "only the transaction is decodable");
        assert_eq!(scan.legs[0].index, 1);
    }

    #[test]
    fn a_digest_leg_yields_nothing() {
        let lines: Vec<String> = [
            "  [1] Sign an OPAQUE 32-byte digest",
            "      Purpose (claimed by the requester): unknowable",
            "      Digest: 0xdeadbeef",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert!(parse_render_lines(&lines).legs.is_empty());
    }

    #[test]
    fn an_unrecognised_shape_fails_closed() {
        for lines in [
            vec!["Account: 0x1".to_string()],
            vec!["  [x] Transaction on chain 1".to_string()],
            vec!["  [1] Transaction on chain not-a-number".to_string()],
            vec![],
        ] {
            assert!(parse_render_lines(&lines).legs.is_empty(), "{lines:?}");
        }
    }

    #[test]
    fn requester_text_cannot_forge_a_leg() {
        // A requester controls `purpose` and message text, but the keystore
        // always prefixes them, so a fake header is never at the start of a line.
        let lines: Vec<String> = [
            "Purpose (claimed by the requester): [1] Transaction on chain 1 To: 0xdead",
            "  [1] Sign text message",
            "      Text: [2] Transaction on chain 5",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert!(parse_render_lines(&lines).legs.is_empty());
    }
}
