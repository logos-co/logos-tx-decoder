//! What a swap router call does, read from its own arguments: which token leaves, which
//! arrives, how much of each, and where it goes.
//!
//! Only for a VERIFIED router and a signature in [`SWAPS`], where the positions are fixed by
//! the router's own ABI. Each token is named by this database, with how sure that is, and an
//! amount is restated in a token's units only for a verified token whose decimals are known.

use serde::Serialize;

use crate::db::{checksum, parse_address, AbiDb};
use crate::decode::{Arg, Confidence};

/// A token an argument names, as far as the database can say.
#[derive(Debug, Clone, Serialize)]
pub struct TokenRef {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decimals: Option<u8>,
    /// `verified`: a source-verified contract in this database. `listed`: a token list names
    /// the address, and nothing here checked its code.
    pub confidence: Confidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Bound {
    Exact,
    AtLeast,
    AtMost,
}

/// One side of a swap: a token and an amount the call carries for it.
#[derive(Debug, Clone, Serialize)]
pub struct Side {
    pub address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<TokenRef>,
    /// The raw integer, exactly as the argument holds it.
    pub amount: String,
    pub bound: Bound,
    /// The argument the amount came from, so a reader can find it above.
    pub arg: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RouterStep {
    Swap {
        sell: Side,
        buy: Side,
        /// Pool fees in hundredths of a basis point, hop by hop. Empty for a V2 path.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        fees: Vec<u32>,
        recipient: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        price_limit: Option<String>,
    },
    /// `unwrapWETH9`: the router's wrapped native coin, back to the native coin.
    Unwrap { amount_minimum: String, recipient: Option<String> },
    /// `refundETH`: whatever native coin the router still holds, back to the sender.
    Refund,
    /// `sweepToken`: the router's balance of a token, out to a recipient.
    Sweep { token: Side, recipient: Option<String> },
}

#[derive(Clone, Copy)]
enum Layout {
    /// One tuple: (tokenIn, tokenOut, fee, recipient, amount, bound amount, sqrtPriceLimitX96).
    Single { exact_in: bool },
    /// One tuple: (path, recipient, amount, bound amount); the path is token (fee token)*.
    Path { exact_in: bool },
    /// (amount, bound amount, path[], to): Uniswap V2 through the same router.
    V2 { exact_in: bool },
    Unwrap,
    Refund,
    Sweep,
}

/// SwapRouter02's calls, by canonical signature. The Uniswap app sends through it.
const SWAPS: &[(&str, Layout)] = &[
    ("exactInputSingle((address,address,uint24,address,uint256,uint256,uint160))", Layout::Single { exact_in: true }),
    ("exactOutputSingle((address,address,uint24,address,uint256,uint256,uint160))", Layout::Single { exact_in: false }),
    ("exactInput((bytes,address,uint256,uint256))", Layout::Path { exact_in: true }),
    ("exactOutput((bytes,address,uint256,uint256))", Layout::Path { exact_in: false }),
    ("swapExactTokensForTokens(uint256,uint256,address[],address)", Layout::V2 { exact_in: true }),
    ("swapTokensForExactTokens(uint256,uint256,address[],address)", Layout::V2 { exact_in: false }),
    ("unwrapWETH9(uint256)", Layout::Unwrap),
    ("unwrapWETH9(uint256,address)", Layout::Unwrap),
    ("refundETH()", Layout::Refund),
    ("sweepToken(address,uint256)", Layout::Sweep),
    ("sweepToken(address,uint256,address)", Layout::Sweep),
];

/// Read a verified router call. `None` for anything the table does not describe, or whose
/// arguments do not have the shape it says.
pub fn read(db: &AbiDb, chain: u64, signature: &str, args: &[Arg]) -> Option<RouterStep> {
    let layout = SWAPS.iter().find(|(s, _)| *s == signature)?.1;
    let value = |a: &Arg| a.value.clone();
    let side = |address: String, amount: &Arg, bound: Bound| {
        Some(Side { token: token(db, chain, &address), address, amount: value(amount)?, bound, arg: amount.name.clone() })
    };
    let split = |exact_in: bool, sell_token: String, buy_token: String, a: &Arg, b: &Arg| {
        Some(if exact_in {
            (side(sell_token, a, Bound::Exact)?, side(buy_token, b, Bound::AtLeast)?)
        } else {
            (side(sell_token, b, Bound::AtMost)?, side(buy_token, a, Bound::Exact)?)
        })
    };
    match layout {
        Layout::Single { exact_in } => {
            let f = args.first()?.components.as_ref()?;
            let [token_in, token_out, fee, recipient, a, b, limit] = f.as_slice() else { return None };
            let (sell, buy) = split(exact_in, value(token_in)?, value(token_out)?, a, b)?;
            Some(RouterStep::Swap {
                sell,
                buy,
                fees: vec![value(fee)?.parse().ok()?],
                recipient: value(recipient)?,
                price_limit: value(limit),
            })
        }
        Layout::Path { exact_in } => {
            let f = args.first()?.components.as_ref()?;
            let [path, recipient, a, b] = f.as_slice() else { return None };
            let (mut tokens, mut fees) = hops(&value(path)?)?;
            // An exact-output path is written from the token that arrives.
            if !exact_in {
                tokens.reverse();
                fees.reverse();
            }
            let (sell, buy) = split(exact_in, tokens.first()?.clone(), tokens.last()?.clone(), a, b)?;
            Some(RouterStep::Swap { sell, buy, fees, recipient: value(recipient)?, price_limit: None })
        }
        Layout::V2 { exact_in } => {
            let [a, b, path, to] = args else { return None };
            let path: Vec<String> = path.items.as_ref()?.iter().filter_map(value).collect();
            let (sell, buy) = split(exact_in, path.first()?.clone(), path.last()?.clone(), a, b)?;
            Some(RouterStep::Swap { sell, buy, fees: Vec::new(), recipient: value(to)?, price_limit: None })
        }
        Layout::Unwrap => Some(RouterStep::Unwrap {
            amount_minimum: value(args.first()?)?,
            recipient: args.get(1).and_then(value),
        }),
        Layout::Refund => Some(RouterStep::Refund),
        Layout::Sweep => {
            let address = value(args.first()?)?;
            Some(RouterStep::Sweep {
                token: side(address, args.get(1)?, Bound::AtLeast)?,
                recipient: args.get(2).and_then(value),
            })
        }
    }
}

/// A V3 path, `token (fee token)*`, as its tokens and fees in the order written.
fn hops(path: &str) -> Option<(Vec<String>, Vec<u32>)> {
    let bytes = hex::decode(path.strip_prefix("0x")?).ok()?;
    if bytes.len() < 20 || (bytes.len() - 20) % 23 != 0 {
        return None;
    }
    let address = |at: usize| -> Option<String> { Some(checksum(&bytes[at..at + 20].try_into().ok()?)) };
    let mut tokens = vec![address(0)?];
    let mut fees = Vec::new();
    let mut at = 20;
    while at < bytes.len() {
        fees.push(u32::from_be_bytes([0, bytes[at], bytes[at + 1], bytes[at + 2]]));
        tokens.push(address(at + 3)?);
        at += 23;
    }
    Some((tokens, fees))
}

fn token(db: &AbiDb, chain: u64, address: &str) -> Option<TokenRef> {
    let idx = db.contract_at(chain, &parse_address(address).ok()?)?;
    let c = db.contract(idx);
    let confidence = if db.is_verified(idx) { Confidence::Verified } else { Confidence::Listed };
    Some(TokenRef { label: c.label.clone(), decimals: c.decimals, confidence })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::decode_call;

    const ROUTER: &str = "0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45";
    const WETH: &str = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2";
    const USDT: &str = "0xdAC17F958D2ee523a2206206994597C13D831ec7";
    const USDC: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";
    const ALICE: &str = "0xa1E277eA6b97eFfc5b61B3BF5dE03F438981247E";

    fn db() -> AbiDb {
        AbiDb::embedded().unwrap()
    }

    fn word(hex_body: &str) -> String {
        format!("{:0>64}", hex_body.trim_start_matches("0x").to_ascii_lowercase())
    }

    fn step(data: &str) -> RouterStep {
        let d = decode_call(&db(), 1, ROUTER, data);
        assert_eq!(d.confidence, Some(Confidence::Verified), "{:?}", d.warnings);
        d.router.expect("a router step")
    }

    /// exactInputSingle((WETH, USDT, 100, Alice, 10^12, 2621, 0)): the swap in the report.
    fn exact_input_single() -> String {
        format!("0x04e45aaf{}{}{}{}{}{}{}", word(WETH), word(USDT), word("64"), word(ALICE),
                word("e8d4a51000"), word("a3d"), word("0"))
    }

    #[test]
    fn an_exact_input_swap_names_both_tokens_and_bounds_what_arrives() {
        let RouterStep::Swap { sell, buy, fees, recipient, price_limit } = step(&exact_input_single()) else {
            panic!("not a swap")
        };
        assert_eq!((sell.token.as_ref().unwrap().label.as_str(), sell.amount.as_str(), sell.bound), ("WETH", "1000000000000", Bound::Exact));
        assert_eq!(sell.token.unwrap().confidence, Confidence::Verified);
        let usdt = buy.token.clone().unwrap();
        assert_eq!((usdt.label.as_str(), usdt.decimals, usdt.confidence), ("USDT", Some(6), Confidence::Verified));
        assert_eq!((buy.amount.as_str(), buy.bound, buy.arg.as_str()), ("2621", Bound::AtLeast, "amountOutMinimum"));
        assert_eq!((fees, recipient.as_str(), price_limit.as_deref()), (vec![100], ALICE, Some("0")));
    }

    #[test]
    fn an_exact_output_swap_bounds_what_leaves() {
        // exactOutputSingle((WETH, USDC, 500, Alice, amountOut 5 USDC, amountInMaximum 0.01 WETH, 0))
        let data = format!("0x5023b4df{}{}{}{}{}{}{}", word(WETH), word(USDC), word("1f4"), word(ALICE),
                           word("4c4b40"), word("2386f26fc10000"), word("0"));
        let RouterStep::Swap { sell, buy, .. } = step(&data) else { panic!("not a swap") };
        assert_eq!((sell.amount.as_str(), sell.bound, sell.arg.as_str()), ("10000000000000000", Bound::AtMost, "amountInMaximum"));
        assert_eq!((buy.amount.as_str(), buy.bound, buy.token.unwrap().label.as_str()), ("5000000", Bound::Exact, "USDC"));
    }

    #[test]
    fn a_multi_hop_path_is_read_from_its_bytes_in_the_right_direction() {
        let path = format!("{}{}{}{}{}", &WETH[2..], "0001f4", &USDC[2..], "000064", &USDT[2..]).to_ascii_lowercase();
        let tuple = |selector: &str| format!("0x{selector}{}{}{}{}{}{}{}", word("20"), word("80"), word(ALICE),
                                             word("e8d4a51000"), word("a3d"), word(&format!("{:x}", path.len() / 2)),
                                             format!("{path:0<192}"));
        let RouterStep::Swap { sell, buy, fees, .. } = step(&tuple("b858183f")) else { panic!("exactInput") };
        assert_eq!((sell.address.as_str(), buy.address.as_str(), fees), (WETH, USDT, vec![500, 100]));
        // exactOutput writes the path from the token that ARRIVES: the same bytes sell USDT.
        let RouterStep::Swap { sell, buy, fees, .. } = step(&tuple("09b81346")) else { panic!("exactOutput") };
        assert_eq!((sell.address.as_str(), sell.bound, buy.address.as_str(), fees), (USDT, Bound::AtMost, WETH, vec![100, 500]));
    }

    #[test]
    fn a_token_only_a_list_names_says_so() {
        // SOFID: a token list gives its address and 6 decimals; no verified ABI is its.
        let sofid = "0x0cb6d03b0ac88a463f67b7ad99f9f3ec4678092e";
        let data = format!("0x04e45aaf{}{}{}{}{}{}{}", word(WETH), word(sofid), word("bb8"), word(ALICE),
                           word("1"), word("2"), word("0"));
        let RouterStep::Swap { buy, .. } = step(&data) else { panic!("not a swap") };
        let t = buy.token.unwrap();
        assert_eq!((t.label.as_str(), t.decimals, t.confidence), ("SOFID", Some(6), Confidence::Listed));
    }

    #[test]
    fn a_token_the_database_does_not_know_is_left_unnamed() {
        let stranger = "0x1111111111111111111111111111111111111111";
        let data = format!("0x04e45aaf{}{}{}{}{}{}{}", word(WETH), word(stranger), word("bb8"), word(ALICE),
                           word("1"), word("2"), word("0"));
        let RouterStep::Swap { buy, .. } = step(&data) else { panic!("not a swap") };
        assert!(buy.token.is_none());
    }

    #[test]
    fn the_same_call_to_an_unknown_address_is_not_read() {
        let d = decode_call(&db(), 1, "0x1111111111111111111111111111111111111111", &exact_input_single());
        assert_eq!(d.confidence, Some(Confidence::SignatureOnly));
        assert!(d.router.is_none(), "positions mean nothing on a contract this database does not know");
    }

    #[test]
    fn a_path_whose_length_is_not_hops_is_refused() {
        assert!(hops("0x00").is_none());
        assert!(hops(&format!("0x{}00", &WETH[2..])).is_none());
    }
}
