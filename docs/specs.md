# logos-tx-decoder — what it asserts, and what it refuses to

## Scope

Decodes EVM calldata against an embedded ABI database. Nothing else: it holds no
keys, signs nothing, reaches no network, and depends on no Logos module. It is
linked into a signing surface rather than called across IPC, so what a human
reads depends on that process alone.

## The confidence contract

Every `Kind::Call` result carries a `confidence`. The distinction exists because
a 4-byte selector is a 32-bit hash of a name nobody verified.

| tier | asserted | NOT asserted |
|---|---|---|
| `verified` | `(chain, to)` is in the database, and that contract's ABI declares this selector; the arguments decoded cleanly | that the deployed bytecode still matches the ABI Etherscan served |
| `signature_only` | some contract somewhere declares a function with this selector, and the argument bytes fit it | anything at all about `to` |
| `unknown` | nothing, beyond the raw bytes | — |

Three rules keep the tiers honest:

1. **A verified match must come from the called contract's own ABI.** A global
   selector hit while `to` is known does not upgrade — it produces
   `signature_only` plus a warning naming the mismatch, because a proxy and an
   impersonator are indistinguishable from here.
2. **A failed argument decode downgrades to `unknown`.** The selector fitting
   while the body does not is evidence the match is wrong. The candidate name is
   still reported, explicitly marked unproven.
3. **Chain is part of the key.** The same address on another chain is a
   different contract, and the database says so.

## Reading the legs out of the render lines

`describe_render_lines` takes the keystore's `render_lines` and recovers the
transaction legs by parsing them. That is deliberate, not a workaround.

The keystore hands an approver `{ok, handle, bundle_id, requester, render_lines}`
and nothing else — no structured `to`, `data` or `chain_id`. Parsing the lines
means **the interpretation is derived from the exact text on screen**, so the two
cannot describe different bytes. It also needs no change to the keystore, which
keeps authoring the authoritative render as the only party that parsed the intent.

The parser fails closed: an unrecognised shape yields no leg, and the caller
shows the verbatim lines alone — which is what it did before this existed. It is
pinned to the keystore's format by tests, and if that format changes the tests
fail and the behaviour degrades to "no interpretation", never to a wrong one.

Message and digest legs are counted but not decoded; there is no calldata in
them. `items` reports the total so a caller can tell a reading of the whole
request from a reading of item 2 of 3.

## What this library cannot tell you

* **That the code does what the ABI says.** An ABI is a calling convention, not
  behaviour. `verified` means the call is well-formed for a contract we can name.
* **Token amounts in human units.** `wad: 1000000000` is the exact integer.
  Applying decimals needs a token list, and a decoder that silently divides by
  the wrong power of ten is worse than one that does not divide.
* **Anything about contracts outside the snapshot.** 87 contracts is a curated
  allowlist. Absence is not suspicion, and presence is not endorsement.

## Obligations on a consumer

A surface that shows this output must:

1. **Keep showing the raw fields.** The decoded lines are additional. The
   recipient, value and full calldata stay on screen whether or not anything
   decoded.
2. **Keep the interpretation visually distinct and captioned.** A reading that
   looks like the signer's own words is worse than no reading.
3. **Treat a decode failure as nothing at all.** Every entry point returns JSON
   with an `ok` field and never unwinds; an empty `legs` is the normal case. A
   decoder problem must never surface as an approval problem.
4. **Never let decoded text reach a commitment.** The keystore hashes the parsed
   intent. If decoded text entered that hash, refreshing the ABI database would
   invalidate in-flight approvals.

`logos-evm-signer-ui` satisfies all four; its `.rep` states them as the contract
for `interpretationLines`, and its doc-test asserts the VERIFIED line appears
beside the keystore's verbatim ones.
