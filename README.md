# logos-tx-decoder

Turns EVM calldata into something a human can read — or says plainly that it
cannot. A **library**, not a module: it links into whatever is already showing a
person what they are about to sign, so the answer depends on no other process.

`evm_signer_ui` links it and decodes locally, which is what a Keycard Shell does:
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
| `listed` | a token list names `to`, and the selector is standard ERC-20; the deployed code was not verified |
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

1,616 contracts and 4,156 functions, compiled from
[keycard-tech/eth-abi-repo](https://github.com/keycard-tech/eth-abi-repo), which
fetches ABIs from Etherscan against a hand-curated `abi_list.csv`, plus every EVM
entry in Uniswap Labs Default v22.19.0. The Uniswap snapshot contains 1,523 EVM
tokens: 685 had source-verified ABIs in Sourcify, while the remaining 838 carry
only the standard ERC-20 interface at the visibly weaker `listed` tier.

The current database also includes SwapRouter02 on Ethereum, Optimism, Arbitrum,
Base and Sepolia, plus Uniswap V2 Router02 on Sepolia. A Sepolia call to
`0x3bFA…e48E` with selector `0x5ae401dc` therefore resolves as a verified
`multicall(uint256,bytes[])` and its inner swaps are decoded too.

Refresh the Uniswap snapshot and then build the database:

```bash
./tools/fetch-token-list-abis.py https://tokens.uniswap.org/ \
  --output /tmp/uniswap-token-abis.json \
  --cache-dir /tmp/uniswap-token-abi-cache
./tools/build-abi-db.py \
  --rev 0c7df41dbfad039e4a96d1201a33fd43ac669488 \
  --token-abis /tmp/uniswap-token-abis.json
```

See `rust-lib/assets/PROVENANCE.md` for the pinned revision and hashes. Coverage
is still an allowlist, not a universal lookup: contracts absent from both sources
decode at `signature_only` at best. `AbiDb::import` closes that gap for Rust
consumers without any network access. `ETHERSCAN_API_KEY`, when present while the
fetch utility runs, enables Etherscan V2 as a fallback after Sourcify; no key or
network access is ever used by the decoder itself.

## Build and test

```bash
cargo test --manifest-path rust-lib/Cargo.toml
nix build path:.        # lib/liblogos_tx_decoder.a + include/tx_decoder.h
```

There is no Logos runtime dependency, so the tests need no host, no daemon and
no nix.
