# logos-tx-decoder

Turns EVM calldata into something a human can read — or says plainly that it
cannot. A **library**, not a module: it links into whatever is already showing a
person what they are about to sign, so the answer depends on no other process.

`signer_ui` links it and decodes locally, which is what a Keycard Shell does:
the thing displaying the transaction is the thing that interprets it.

Offline by construction. The ABI database is embedded at build time and **no
function performs network I/O**, so a decode cannot stall an approval, leak the
address being signed for, or depend on a server being honest.

## The one thing to understand

A 4-byte selector proves nothing. Anyone can deploy a contract whose function is
named `transfer` and does the opposite, and 4 bytes collide by accident too. So
every answer carries a tier, and the renderer puts it on the first line:

| `confidence` | meaning |
|---|---|
| `verified` | `to` is a contract **in the database** and it declares this selector |
| `signature_only` | the selector resolves, but nothing ties it to `to` — a guess |
| `unknown` | nothing matched, or the argument bytes failed to decode |

`verified` is the only tier that says anything about the address being called.
This is the part Keycard Shell's on-device database cannot do: its compiled
format keeps selectors and drops the contract address, so every hit there is
effectively `signature_only`.

Decoded output is **additional** to the raw fields, never a replacement.

## Consuming it

Link `liblogos_tx_decoder.a` and include `tx_decoder.h`. Under the Logos module
builder that is two lines — an `externalLibInputs` entry in `flake.nix` and
`EXTERNAL_LIBS logos_tx_decoder` in `logos_module()`; see `logos-evm-signer-ui`.

Ship the **static** archive, never a shared library: the builder copies every
`.so`/`.dylib` an external library ships into the plugin's `lib/`, and ui-host
then tries to load each one as a Qt plugin.

```c
LogosTxDecoder *d = logos_tx_decoder_new();
char *json = logos_tx_decoder_describe_render_lines(d, render_lines_json);
// { ok, items, legs: [ { index, chainId, kind, confidence, lines } ] }
logos_tx_decoder_string_free(json);
logos_tx_decoder_free(d);
```

`describe_render_lines` is the call an approval surface wants. It takes the
keystore's `render_lines` — the exact text on screen — recovers the transaction
legs from it, and decodes each. An interpretation derived from the displayed
text cannot describe different bytes than the human is reading, and it needs no
change to the keystore, which hands an approver those lines and nothing else.

`items` counts every `[n]` item including the undecodable ones, so a caller can
tell whether an interpretation covers the whole request or only part of it.

Everything crosses as JSON, every returned string is freed with
`logos_tx_decoder_string_free`, no function unwinds, and every one tolerates a
NULL handle or NULL string.

## Example

```
Interpreted: WETH — VERIFIED (this address is WETH on chain 1, and it declares this function)
  Function: transfer(address,uint256)
    dst: 0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045
    wad: 1000000000
  In WETH units: 0.000000001 WETH
```

The units line is additive — the raw argument is what is signed and stays. It appears
only on a `verified` match whose decimals are known, because decimals belong to the
address, not to the selector: reading 6-decimal USDC as 18 understates an amount by a
factor of a trillion.

The same calldata sent to an address the database does not know:

```
Interpreted: UNVERIFIED — guessed from the 4-byte selector alone
  Function: transfer(address,uint256)
    dst: 0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045
    wad: 1000000000
  ! This contract is not in the database. The function named here is a guess from
    the 4-byte selector alone and proves nothing about what the code does.
```

Argument names come from the real ABI — WETH9 calls them `dst`/`wad`, not
`to`/`amount`, and nested tuple components keep their names too.

## The database

87 contracts, 1841 functions, compiled from
[keycard-tech/eth-abi-repo](https://github.com/keycard-tech/eth-abi-repo), which
fetches ABIs from Etherscan against a hand-curated `abi_list.csv`. Refresh:

```bash
./tools/build-abi-db.py
```

See `rust-lib/assets/PROVENANCE.md` for the pinned revision and hashes. Coverage
is a curated allowlist, not a universal lookup: contracts outside those 87 decode
at `signature_only` at best. `AbiDb::import` closes that gap for Rust consumers
without any network access.

## Build and test

```bash
cargo test --manifest-path rust-lib/Cargo.toml
nix build path:.        # lib/liblogos_tx_decoder.a + include/tx_decoder.h
```

There is no Logos runtime dependency, so the tests need no host, no daemon and
no nix.
