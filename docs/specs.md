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
| `verified` | `(chain, to)` is in the database, and a source-verified ABI for that contract declares this selector; the arguments decoded cleanly | that the deployed bytecode still matches the ABI snapshot |
| `listed` | a Uniswap-format token list names `(chain, to)`, and the selector is in the standard ERC-20 interface; the arguments decoded cleanly | that the deployed bytecode implements the interface, or that the list's name and decimals are correct |
| `signature_only` | some contract somewhere declares a function with this selector, and the argument bytes fit it | anything at all about `to` |
| `unknown` | nothing, beyond the raw bytes | — |

Four rules keep the tiers honest:

1. **A verified match must come from the called contract's own ABI.** A global
   selector hit while `to` is known does not upgrade — it produces
   `signature_only` plus a warning naming the mismatch, because a proxy and an
   impersonator are indistinguishable from here.
2. **A failed argument decode downgrades to `unknown`.** The selector fitting
   while the body does not is evidence the match is wrong. The candidate name is
   still reported, explicitly marked unproven.
3. **Chain is part of the key.** The same address on another chain is a
   different contract, and the database says so.
4. **Only a verified match may restate an amount in token units.** A `listed`
   match deliberately does not: token-list decimals are useful metadata, not a
   source-code or bytecode check. Decimals are a
   property of the address, never of the selector, and this library never calls a
   contract to read them — a known token's decimals are compiled in beside its ABI,
   and a token without them shows raw units. The restatement is an extra line; the
   raw argument is never replaced.

## What a swap router call does

A verified call to SwapRouter02, for a signature in `router.rs`'s table, also gets a
plain reading of its own arguments, under "What it does":

* **Which token leaves and which arrives**, with the bound on each amount:
  `exactly`, `at least` or `at most`. The single-pool swaps name their tokens in the
  tuple. The multi-hop ones name them in the path bytes, and an exact-output path is
  written from the token that arrives. Uniswap V2 swaps through the same router name
  them in `path[]`.
* **Each token as this database knows it.** A verified contract's amount is restated
  in its units, by rule 4. A token only a list names keeps its raw base units, and the
  line says why. An address the database does not know is shown as an address.
* **The pool fee**, hop by hop, as a percentage, and **the price limit** when there is one.
* **Where the proceeds go**, against the request's `Account:`. A recipient that is not
  the signing account is flagged with `!`. The router's stand-ins, `MSG_SENDER` and
  `ADDRESS_THIS`, are named.
* **The steps around a swap:** `unwrapWETH9`, `refundETH` and `sweepToken`.

`describe_render_lines` also restates a leg's `Value:` in the native coin, beneath its
header. It adds the multicall's deadline as a UTC date too. Every figure names the
argument it came from, and the raw arguments stay above it. A call to any other contract,
or at a lower tier, gets none of this, because an argument's position means nothing
without a verified ABI behind it.

## One request, one reading

`read_request` turns a keystore render block into its legs and the lines to show under
each, and `describe_render_lines` is that same function behind the C ABI. `evm_signer_ui`
takes the second, `evm_signer_cli` the first. A request read twice in two languages is two
things to drift, and the text a human approves is the one place they must agree. A test
asserts the two give the same lines for the same request.

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
* **That token-list metadata is true.** A `listed` row names the source and says
  that the code was not checked. It is never rendered as `verified`, and its
  decimals never restate a signed amount as though the identity were proven.
* **Anything about contracts outside the snapshot.** The source-verified and
  token-list sets are finite allowlists. Absence is not suspicion, and presence
  is not endorsement.

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
