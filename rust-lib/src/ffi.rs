//! C ABI, for consumers that are not Rust — evm_signer_ui's C++ backend.
//!
//! Everything crosses as JSON text: the surface stays small, no struct layout
//! is shared, and the caller already speaks JSON. Every returned string is
//! owned by the caller and must go back through
//! [`logos_tx_decoder_string_free`].
//!
//! No entry point may unwind: a panic across the ABI is undefined behaviour, so
//! each one is wrapped and reports the failure as JSON instead.

use std::ffi::{c_char, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};

use serde_json::json;

use crate::db::AbiDb;
use crate::decode::decode_call;
use crate::intent::parse_render_lines;
use crate::render::{describe, describe_with, value_line, Context};

/// Opaque to C. Holds the parsed ABI database, which is worth building once.
pub struct LogosTxDecoder {
    db: AbiDb,
}

fn out(v: serde_json::Value) -> *mut c_char {
    // A NUL can only appear here if serde produced one, which it cannot.
    CString::new(v.to_string()).unwrap_or_default().into_raw()
}

fn fail(e: impl std::fmt::Display) -> *mut c_char {
    out(json!({ "ok": false, "error": e.to_string() }))
}

/// `catch_unwind` + a null check on the handle, for the body of every call.
fn guarded(
    d: *mut LogosTxDecoder,
    f: impl FnOnce(&mut LogosTxDecoder) -> *mut c_char,
) -> *mut c_char {
    if d.is_null() {
        return fail("decoder handle is null");
    }
    match catch_unwind(AssertUnwindSafe(|| f(unsafe { &mut *d }))) {
        Ok(p) => p,
        Err(_) => fail("decoder panicked"),
    }
}

fn borrow<'a>(s: *const c_char) -> Result<&'a str, String> {
    if s.is_null() {
        return Err("null string argument".into());
    }
    unsafe { CStr::from_ptr(s) }.to_str().map_err(|e| format!("argument is not utf-8: {e}"))
}

/// Build a decoder over the embedded ABI database. NULL on failure.
#[no_mangle]
pub extern "C" fn logos_tx_decoder_new() -> *mut LogosTxDecoder {
    match catch_unwind(|| AbiDb::embedded()) {
        Ok(Ok(db)) => Box::into_raw(Box::new(LogosTxDecoder { db })),
        _ => std::ptr::null_mut(),
    }
}

/// Free a decoder. Safe on NULL.
#[no_mangle]
pub extern "C" fn logos_tx_decoder_free(d: *mut LogosTxDecoder) {
    if !d.is_null() {
        drop(unsafe { Box::from_raw(d) });
    }
}

/// Free a string returned by any function here. Safe on NULL.
#[no_mangle]
pub extern "C" fn logos_tx_decoder_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(unsafe { CString::from_raw(s) });
    }
}

/// THE call for an approval surface. `render_lines_json` is the keystore's
/// `render_lines` as a JSON array of strings; the reply is
/// `{ ok, legs: [ { index, chainId, kind, confidence, lines } ] }`.
///
/// `legs` is empty when nothing decodable was found — which is not an error.
/// The caller keeps showing the verbatim lines either way.
#[no_mangle]
pub extern "C" fn logos_tx_decoder_describe_render_lines(
    d: *mut LogosTxDecoder,
    render_lines_json: *const c_char,
) -> *mut c_char {
    guarded(d, |d| {
        let text = match borrow(render_lines_json) {
            Ok(t) => t,
            Err(e) => return fail(e),
        };
        let lines: Vec<String> = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(e) => return fail(format!("render_lines is not a json array of strings: {e}")),
        };

        let scan = parse_render_lines(&lines);
        let ctx = Context { account: scan.account.clone() };
        let legs: Vec<_> = scan
            .legs
            .into_iter()
            .map(|leg| {
                let decoded = decode_call(&d.db, leg.chain_id, &leg.to, &leg.data);
                let mut described = describe_with(&decoded, &ctx);
                if let Some(line) = value_line(leg.value.as_deref()) {
                    described.insert(described.len().min(1), line);
                }
                json!({
                    "index": leg.index,
                    "chainId": leg.chain_id,
                    // What was decoded, not only how it reads. A caller that wants to add
                    // its own layer over this — naming the address from a token list, say —
                    // otherwise has to re-parse the render lines in its own language, and a
                    // second parser of the keystore's text is a second thing to drift.
                    // Nothing here is a new claim: it is what this decode already used.
                    "to": leg.to,
                    "kind": decoded.kind,
                    "confidence": decoded.confidence,
                    "function": decoded.function,
                    "args": decoded.args,
                    "router": decoded.router,
                    "lines": described,
                })
            })
            .collect();

        // `items` lets a caller decide whether to label each interpretation
        // with its item number: an interpretation of item 2 of 3 must not read
        // as a description of the whole request.
        out(json!({ "ok": true, "items": scan.items, "legs": legs }))
    })
}

/// Decode one call directly. Mostly for tests and callers that already hold
/// structured fields.
#[no_mangle]
pub extern "C" fn logos_tx_decoder_decode_call(
    d: *mut LogosTxDecoder,
    chain_id: u64,
    to: *const c_char,
    data: *const c_char,
) -> *mut c_char {
    guarded(d, |d| {
        let (to, data) = match (borrow(to), borrow(data)) {
            (Ok(t), Ok(v)) => (t, v),
            (Err(e), _) | (_, Err(e)) => return fail(e),
        };
        let decoded = decode_call(&d.db, chain_id, to, data);
        match serde_json::to_value(&decoded) {
            Ok(serde_json::Value::Object(mut m)) => {
                m.insert("ok".into(), json!(true));
                m.insert("lines".into(), json!(describe(&decoded)));
                out(serde_json::Value::Object(m))
            }
            Ok(_) => fail("decode produced a non-object"),
            Err(e) => fail(e),
        }
    })
}

/// `{ ok, schema, source, upstreamRev, generated, contracts, functions }`.
#[no_mangle]
pub extern "C" fn logos_tx_decoder_db_status(d: *mut LogosTxDecoder) -> *mut c_char {
    guarded(d, |d| {
        let (contracts, functions, imported) = d.db.stats();
        out(json!({
            "ok": true,
            "schema": crate::db::SCHEMA,
            "source": d.db.source,
            "upstreamRev": d.db.upstream_rev,
            "generated": d.db.generated,
            "contracts": contracts,
            "functions": functions,
            "importedFunctions": imported,
        }))
    })
}

#[cfg(test)]
mod tests {
    /// The Uniswap app's swap as the keystore renders it: 0.000001 ETH for at least 0.002621 USDT.
    fn swap_render_lines() -> String {
        let w = |h: &str| format!("{:0>64}", h);
        let inner = format!("04e45aaf{}{}{}{}{}{}{}", w("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"),
                            w("dac17f958d2ee523a2206206994597c13d831ec7"), w("64"),
                            w("a1e277ea6b97effc5b61b3bf5de03f438981247e"), w("e8d4a51000"), w("a3d"), w("0"));
        let data = format!("0x5ae401dc{}{}{}{}{}{inner}{}", w("6aaef2e8"), w("40"), w("1"), w("20"), w("e4"), "0".repeat(56));
        serde_json::to_string(&[
            "Account: 0xa1E277eA6b97eFfc5b61B3BF5dE03F438981247E".to_string(),
            "1 item(s) to sign:".into(),
            "  [1] Transaction on chain 1".into(),
            "      To: 0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45".into(),
            "      Value: 0xe8d4a51000 (1000000000000)".into(),
            "      Nonce: 0x2b (43)".into(),
            format!("      Data: {data}"),
        ])
        .unwrap()
    }

    use super::*;

    fn call<F: FnOnce(*mut LogosTxDecoder) -> *mut c_char>(f: F) -> serde_json::Value {
        let d = logos_tx_decoder_new();
        assert!(!d.is_null(), "decoder must build");
        let p = f(d);
        let v: serde_json::Value =
            serde_json::from_str(unsafe { CStr::from_ptr(p) }.to_str().unwrap()).unwrap();
        logos_tx_decoder_string_free(p);
        logos_tx_decoder_free(d);
        v
    }

    fn cstr(s: &str) -> CString {
        CString::new(s).unwrap()
    }

    #[test]
    fn the_database_builds_over_the_abi() {
        let v = call(|d| logos_tx_decoder_db_status(d));
        assert_eq!(v["ok"], true);
        assert!(v["contracts"].as_u64().unwrap() >= 80);
    }

    #[test]
    fn render_lines_become_per_leg_interpretations() {
        let lines = json!([
            "Account: 0xd8da6bf26964af9d7eed9e03e53415d37aa96045",
            "1 item(s) to sign:",
            "  [1] Transaction on chain 1",
            "      To: 0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            "      Value: 0",
            "      Selector: 0xa9059cbb",
            "      Data: 0xa9059cbb000000000000000000000000d8da6bf26964af9d7eed9e03e53415d37aa96045000000000000000000000000000000000000000000000000000000003b9aca00",
        ])
        .to_string();

        let arg = cstr(&lines);
        let v = call(|d| logos_tx_decoder_describe_render_lines(d, arg.as_ptr()));

        assert_eq!(v["ok"], true);
        let legs = v["legs"].as_array().unwrap();
        assert_eq!(legs.len(), 1);
        assert_eq!(legs[0]["index"], 1);
        assert_eq!(legs[0]["confidence"], "verified");
        let text = legs[0]["lines"].to_string();
        assert!(text.contains("WETH") && text.contains("transfer(address,uint256)"), "{text}");
    }

    #[test]
    fn lines_with_nothing_decodable_are_not_an_error() {
        let arg = cstr(&json!(["Account: 0x1", "  [1] Sign text message"]).to_string());
        let v = call(|d| logos_tx_decoder_describe_render_lines(d, arg.as_ptr()));
        assert_eq!(v["ok"], true);
        assert!(v["legs"].as_array().unwrap().is_empty());
    }

    #[test]
    fn a_direct_decode_carries_both_the_struct_and_the_lines() {
        let to = cstr("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        let data = cstr("0xa9059cbb000000000000000000000000d8da6bf26964af9d7eed9e03e53415d37aa96045000000000000000000000000000000000000000000000000000000003b9aca00");
        let v = call(|d| logos_tx_decoder_decode_call(d, 1, to.as_ptr(), data.as_ptr()));
        assert_eq!(v["confidence"], "verified");
        assert_eq!(v["function"]["signature"], "transfer(address,uint256)");
        assert!(v["lines"].as_array().unwrap()[0].as_str().unwrap().contains("VERIFIED"));
    }

    #[test]
    fn a_router_swap_reads_as_what_it_does_for_the_account_signing() {
        let swap = std::ffi::CString::new(swap_render_lines()).unwrap();
        let v = call(|d| logos_tx_decoder_describe_render_lines(d, swap.as_ptr()));
        let lines: Vec<String> = serde_json::from_value(v["legs"][0]["lines"].clone()).unwrap();
        let text = lines.join("\n");
        for want in [
            "  Sends 0.000001 of the native coin with this call (value 1000000000000 wei).",
            "  Deadline: 2026-09-19 20:39:04 UTC (deadline 1789850344); the call reverts after it.",
            "        Sells exactly 0.000001 WETH (amountIn 1000000000000); WETH is a verified contract.",
            "        Buys at least 0.002621 USDT (amountOutMinimum 2621); USDT is a verified contract.",
            "        Pool fee: 0.01% (fee 100).",
            "        Sends what it buys to the account signing this.",
            "        No price limit (sqrtPriceLimitX96 0).",
        ] {
            assert!(lines.iter().any(|l| l == want), "missing {want:?} in\n{text}");
        }
        assert_eq!(lines[1], "  Sends 0.000001 of the native coin with this call (value 1000000000000 wei).", "under the header");
        assert_eq!(v["legs"][0]["router"], serde_json::Value::Null, "the multicall itself is not a step; its part is");
    }

    #[test]
    fn null_and_junk_arguments_are_refused_not_fatal() {
        let v = call(|d| logos_tx_decoder_describe_render_lines(d, std::ptr::null()));
        assert_eq!(v["ok"], false);

        let bad = cstr("not json");
        let v = call(|d| logos_tx_decoder_describe_render_lines(d, bad.as_ptr()));
        assert_eq!(v["ok"], false);

        // A null handle must not dereference.
        let p = logos_tx_decoder_describe_render_lines(std::ptr::null_mut(), bad.as_ptr());
        assert!(!p.is_null());
        logos_tx_decoder_string_free(p);
    }

    #[test]
    fn a_leg_carries_what_the_decode_used() {
        // A consumer adding its own layer needs the address and the decoded arguments. Both
        // are what this decode already used, so offering them asserts nothing new — and
        // saves a second parser of the keystore's text in another language.
        let lines = cstr(
            r#"["1 item(s) to sign:","  [1] Transaction on chain 1","      To: 0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2","      Data: 0xa9059cbb000000000000000000000000d8da6bf26964af9d7eed9e03e53415d37aa96045000000000000000000000000000000000000000000000000000000003b9aca00"]"#,
        );
        let v = call(|d| logos_tx_decoder_describe_render_lines(d, lines.as_ptr()));
        let leg = &v["legs"][0];
        assert_eq!(leg["chainId"], 1);
        assert_eq!(
            leg["to"].as_str().unwrap().to_lowercase(),
            "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"
        );
        assert_eq!(leg["function"]["signature"], "transfer(address,uint256)");
        assert_eq!(leg["args"][1]["value"], "1000000000");
        assert!(!leg["lines"].as_array().unwrap().is_empty(), "the rendered lines still stand");
    }

    #[test]
    fn freeing_null_is_safe() {
        logos_tx_decoder_free(std::ptr::null_mut());
        logos_tx_decoder_string_free(std::ptr::null_mut());
    }
}
