#!/usr/bin/env python3
"""Compile contract and token ABIs into the compact DB this module embeds.

Reads the upstream `repo/*.json` ABIs plus `abi_list.csv` (the only place contract
identity lives — upstream's own release build drops it) and writes
`rust-lib/assets/abi-db.json` + its PROVENANCE.md.

    ./tools/build-abi-db.py                         # fetch master tarball
    ./tools/build-abi-db.py --tarball x.tgz         # offline upstream input
    ./tools/build-abi-db.py --token-abis tokens.json # add token-list snapshot
"""

import argparse
import csv
import hashlib
import io
import json
import os
import sys
import tarfile
import urllib.request
from datetime import date

# NOT FROM UPSTREAM. `abi_list.csv` has four columns — name, chain, address, label —
# and decimals are not among them; an ABI does not carry them either, since they are a
# call to the live contract. Every row here was read off the deployed contract by hand
# and is keyed by (chain, address), so a wrong address matches nothing rather than
# mislabelling a different token. Omitting a token is safe: the decoder then shows raw
# units, which is what it did before this table existed.
DECIMALS = {
    # WETH, chain 1
    (1, "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"): 18,
}

# NOT FROM UPSTREAM EITHER. Contracts the wallet's own apps call that the upstream list does
# not carry: Uniswap's SwapRouter02 (the router every V3 swap goes through, at one address on
# Ethereum, Optimism and Arbitrum, its own on Base and Sepolia), the V2 router and USDC on
# Sepolia, and USDC on Ethereum. `name` is the ABI file — under tools/extra-abis/ for
# SwapRouter02, otherwise an upstream one reused for a further deployment of the same code.
# Addresses were read off the deployed contracts (SwapRouter02.WETH9(), QuoterV2.factory())
# on 2026-09-11; a wrong address matches nothing rather than mislabelling.
EXTRA_ROWS = [
    ("swaprouter02", 1, "0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45", "Uniswap V3: SwapRouter02"),
    ("swaprouter02", 10, "0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45", "Uniswap V3: SwapRouter02"),
    ("swaprouter02", 42161, "0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45", "Uniswap V3: SwapRouter02"),
    ("swaprouter02", 8453, "0x2626664c2603336e57b271c5c0b26f421741e481", "Uniswap V3: SwapRouter02"),
    ("swaprouter02", 11155111, "0x3bfa4769fb09eefc5a80d6e87c3b9c650f7ae48e", "Uniswap V3: SwapRouter02"),
    ("uniswap2", 11155111, "0xee567fe1712faf6149d80da1e6934e354124cfe3", "UniSwap Router02"),
    ("erc20", 1, "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48", "USDC"),
    ("erc20", 11155111, "0x1c7d4b196cb0c7b01d743fbc6116a902379c7238", "USDC"),
]
DECIMALS.update({
    (1, "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"): 6,
    (11155111, "0x1c7d4b196cb0c7b01d743fbc6116a902379c7238"): 6,
})
EXTRA_ABI_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "extra-abis")

UPSTREAM = "keycard-tech/eth-abi-repo"
TARBALL = f"https://github.com/{UPSTREAM}/archive/refs/heads/master.tar.gz"

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
ASSET = os.path.join(ROOT, "rust-lib", "assets", "abi-db.json")
PROVENANCE = os.path.join(ROOT, "rust-lib", "assets", "PROVENANCE.md")

KEPT_MUTABILITY = ("view", "pure", "payable", "nonpayable")
READ_ONLY = ("view", "pure")


def canonical_signature(func):
    """`name(type,type)` — what the 4-byte selector is keccak'd from."""

    def ty(item):
        t = item["type"]
        if t.startswith("tuple"):
            return f"({','.join(ty(c) for c in item.get('components', []))}){t[len('tuple'):]}"
        return t

    return f"{func['name']}({','.join(ty(i) for i in func.get('inputs', []))})"


def named_shape(func):
    """Parameter names recursively, so one contract never borrows another's labels."""
    def shape(item):
        return (
            item.get("name") or "",
            item["type"],
            tuple(shape(component) for component in item.get("components", [])),
        )

    return tuple(shape(item) for item in func.get("inputs", []))


def strip_input(item):
    """Keep name/type/components; drop indexed, internalType and friends."""
    out = {"name": item.get("name") or "", "type": item["type"]}
    if "components" in item:
        out["components"] = [strip_input(c) for c in item["components"]]
    return out


def abi_entry(func):
    """The minimal alloy-deserializable Function. Outputs are irrelevant to calldata."""
    return {
        "type": "function",
        "name": func["name"],
        "inputs": [strip_input(i) for i in func.get("inputs", [])],
        "outputs": [],
        "stateMutability": mutability(func),
    }


def mutability(func):
    """Normalize pre-Solidity-0.6 ABIs that only carry constant/payable."""
    if func.get("stateMutability") in KEPT_MUTABILITY:
        return func["stateMutability"]
    if func.get("constant") is True:
        return "view"
    return "payable" if func.get("payable") is True else "nonpayable"


def load_tree(tarball_bytes):
    """-> (abi_list.csv text, {basename: parsed ABI})."""
    abis, csv_text = {}, None
    with tarfile.open(fileobj=io.BytesIO(tarball_bytes), mode="r:gz") as tf:
        for member in tf.getmembers():
            if not member.isfile():
                continue
            parts = member.name.split("/")
            if parts[-1] == "abi_list.csv":
                csv_text = tf.extractfile(member).read().decode("utf-8")
            elif len(parts) >= 3 and parts[-2] == "repo" and parts[-1].endswith(".json"):
                abis[parts[-1][: -len(".json")]] = json.loads(tf.extractfile(member).read())
    if csv_text is None:
        raise SystemExit("abi_list.csv not found in tarball")
    return csv_text, abis


def load_token_snapshot(path):
    with open(path) as f:
        snapshot = json.load(f)
    if snapshot.get("schema") != 1:
        raise SystemExit(f"{path}: unsupported token ABI snapshot schema")
    if not isinstance(snapshot.get("tokens"), list) or not isinstance(snapshot.get("abis"), list):
        raise SystemExit(f"{path}: expected `tokens` and `abis` arrays")
    return snapshot


def build(csv_text, abis, upstream_rev, token_snapshot=None):
    contracts, functions, order = [], {}, []
    unlisted = sorted(abis)

    def ingest(abi, contract_idx, evidence="verified"):
        for func in abi:
            if func.get("type") != "function" or mutability(func) not in KEPT_MUTABILITY:
                continue
            # Names and mutability are not part of a selector, but they ARE part
            # of what the signer says. Keep variants as separate candidates so
            # a token never inherits WETH's `dst`/`wad`, for example.
            key = (canonical_signature(func), named_shape(func), mutability(func))
            entry = functions.get(key)
            if entry is None:
                entry = {"a": abi_entry(func), "c": [], "l": []}
                functions[key] = entry
                order.append(key)
            if contract_idx is not None:
                field = "c" if evidence == "verified" else "l"
                if contract_idx not in entry[field]:
                    entry[field].append(contract_idx)

    for row in csv.reader(io.StringIO(csv_text)):
        if len(row) < 3 or not row[0].strip():
            continue
        name = row[0].strip()
        abi = abis.get(name)
        if abi is None:
            print(f"  ! {name}: in abi_list.csv but no repo/{name}.json", file=sys.stderr)
            continue
        unlisted.remove(name)
        chain, address = int(row[1]), row[2].strip().lower()
        entry = {
            "name": name,
            "label": (row[3].strip() if len(row) > 3 else "") or name,
            "chain": chain,
            "address": address,
        }
        if (chain, address) in DECIMALS:
            entry["decimals"] = DECIMALS[(chain, address)]
        contracts.append(entry)
        ingest(abi, len(contracts) - 1)

    # This repo's own rows, after upstream's: a further deployment of upstream code reuses
    # its ABI by name, and SwapRouter02's ABI ships beside this script.
    for name, chain, address, label in EXTRA_ROWS:
        abi = abis.get(name)
        if abi is None:
            path = os.path.join(EXTRA_ABI_DIR, f"{name}.json")
            with open(path) as f:
                abi = json.load(f)
            abis[name] = abi
        entry = {"name": name, "label": label, "chain": chain, "address": address.lower()}
        if (chain, address.lower()) in DECIMALS:
            entry["decimals"] = DECIMALS[(chain, address.lower())]
        contracts.append(entry)
        ingest(abi, len(contracts) - 1)

    if token_snapshot:
        # A token list establishes a name/address/decimals claim and that the entry is
        # intended to be ERC-20. It does NOT establish deployed bytecode. Its standard
        # ABI is therefore tracked separately (`l`) from source-verified ABI evidence
        # (`c`); the runtime renders those matches as LISTED rather than VERIFIED.
        with open(os.path.join(EXTRA_ABI_DIR, "erc20.json")) as f:
            erc20 = json.load(f)
        by_identity = {
            (contract["chain"], contract["address"].lower()): i
            for i, contract in enumerate(contracts)
        }
        token_abis = token_snapshot["abis"]
        for token in token_snapshot["tokens"]:
            chain, address = int(token["chainId"]), token["address"].lower()
            identity = (chain, address)
            idx = by_identity.get(identity)
            if idx is None:
                symbol = token.get("symbol") or token.get("name") or address
                contracts.append({
                    "name": f"token:{symbol}",
                    "label": symbol,
                    "chain": chain,
                    "address": address,
                    "decimals": int(token["decimals"]),
                })
                idx = len(contracts) - 1
                by_identity[identity] = idx
            else:
                known = contracts[idx].get("decimals")
                decimals = int(token["decimals"])
                if known is not None and known != decimals:
                    raise SystemExit(
                        f"token-list decimals conflict for chain {chain} {address}: "
                        f"database has {known}, list has {decimals}")
                contracts[idx]["decimals"] = decimals

            ingest(erc20, idx, "listed")
            abi_idx = token.get("abi")
            if abi_idx is not None:
                try:
                    verified_abi = token_abis[int(abi_idx)]
                except (IndexError, TypeError, ValueError) as error:
                    raise SystemExit(
                        f"invalid ABI index for chain {chain} {address}: {abi_idx}") from error
                ingest(verified_abi, idx, "verified")

    # No CSV row means no identity, but the signatures still decode. Keep them
    # unattributed so they land in the advisory tier rather than vanishing.
    for name in unlisted:
        print(f"  · repo/{name}.json has no abi_list.csv row — kept unattributed", file=sys.stderr)
        ingest(abis[name], None)

    # `l` is absent for the original database entries rather than repeated as an
    # empty array thousands of times. Rust deserializes absence as empty.
    packed_functions = []
    for key in order:
        entry = functions[key]
        if not entry["l"]:
            entry.pop("l")
        packed_functions.append(entry)

    db = {
        "schema": 1,
        "source": UPSTREAM,
        "upstream_rev": upstream_rev,
        "generated": date.today().isoformat(),
        "contracts": contracts,
        "functions": packed_functions,
    }
    if token_snapshot:
        db["token_list"] = {
            **token_snapshot["tokenList"],
            "abiProviders": token_snapshot.get("providers", []),
            "abiStats": token_snapshot.get("stats", {}),
        }
    return db


def write_provenance(db, tarball_sha):
    ro = sum(1 for f in db["functions"] if f["a"]["stateMutability"] in READ_ONLY)
    total = len(db["functions"])
    rows = [
        ("Source", f"https://github.com/{UPSTREAM}"),
        ("Upstream rev", db["upstream_rev"]),
        ("Fetched", db["generated"]),
        ("Tarball sha256", tarball_sha),
        ("Contracts", str(len(db["contracts"]))),
        ("Functions", f"{total} ({total - ro} state-changing, {ro} read-only)"),
        ("Asset bytes", str(os.path.getsize(ASSET))),
        ("Asset sha256", hashlib.sha256(open(ASSET, "rb").read()).hexdigest()),
    ]
    token_list = db.get("token_list")
    if token_list:
        sources = token_list.get("abiStats", {}).get("abiSources", {})
        source_text = ", ".join(f"{key}: {value}" for key, value in sorted(sources.items()))
        rows.extend([
            ("Token list", token_list.get("source", "")),
            ("Token-list sha256", token_list.get("sha256", "")),
            ("Token-list version", token_list.get("version", "")),
            ("Token-list EVM entries", str(token_list.get("evmTokens", 0))),
            ("Token ABI sources", source_text),
        ])
    table = "\n".join(f"| {k} | {v} |" for k, v in rows)
    with open(PROVENANCE, "w") as f:
        f.write(f"""# rust-lib/assets/abi-db.json

Lives inside `rust-lib/` because the module builder stages only that directory into the
nix sandbox; `include_str!` from anywhere else compiles locally and fails in nix.

Generated by `tools/build-abi-db.py` from [{UPSTREAM}](https://github.com/{UPSTREAM}),
which fetches its ABIs from Etherscan's `getabi` endpoint against a hand-curated
`abi_list.csv`. Upstream also ships a merged `abi.json` on its release page, but that
build drops the contract name and address, so this module compiles from the `repo/` tree
instead and keeps contract identity — the difference between "this IS Aave v3 Pool" and
"some contract, and the calldata happens to fit a signature Aave also has".

| | |
|---|---|
{table}

For the original keycard rows, `decimals`, where present, is the ONE field not from
upstream: `abi_list.csv` has no such column and an ABI does not carry decimals — reading
them is a call to the live contract, which this library never makes. Those few rows are
hand-checked and keyed by `(chain, address)`. Token-list rows take decimals from the list
and keep that weaker provenance in their LISTED tier. A contract without the field
renders raw units.

The token-list rows come from the Uniswap-format list recorded above. The list supplies
identity and decimals, not deployed bytecode. Every EVM row is therefore associated with
the standard ERC-20 interface at the distinct **LISTED** confidence tier. Where
`fetch-token-list-abis.py` also found a source-verified ABI through Sourcify or Etherscan,
that contract's own selectors qualify for **VERIFIED**. Missing explorer coverage never
silently promotes the standard interface, and non-EVM list entries are excluded.

Entries are stored as ABI JSON rather than signature strings so nested tuple component
names survive; `alloy`'s human-readable parser cannot round-trip those. Selectors are
NOT stored — they are derived from the ABI at load time by the same keccak the decoder
uses, so a wrong selector here is not a failure mode.

Upstream is MIT-licensed, (c) 2025 Status Research & Development GmbH. Refresh the token
snapshot with `./tools/fetch-token-list-abis.py`, pass it to
`./tools/build-abi-db.py --token-abis …`, and update every row above — the hashes are the
point.
""")


def main():
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--tarball", help="read this tarball instead of fetching master")
    ap.add_argument("--rev", default="master", help="recorded upstream rev label")
    ap.add_argument(
        "--token-abis",
        help="snapshot written by tools/fetch-token-list-abis.py")
    args = ap.parse_args()

    if args.tarball:
        blob = open(args.tarball, "rb").read()
    else:
        tarball_url = (
            TARBALL if args.rev == "master"
            else f"https://github.com/{UPSTREAM}/archive/{args.rev}.tar.gz"
        )
        print(f"fetching {tarball_url}")
        blob = urllib.request.urlopen(tarball_url, timeout=120).read()

    csv_text, abis = load_tree(blob)
    print(f"read abi_list.csv + {len(abis)} ABI files")

    token_snapshot = load_token_snapshot(args.token_abis) if args.token_abis else None
    db = build(csv_text, abis, args.rev, token_snapshot)
    os.makedirs(os.path.dirname(ASSET), exist_ok=True)
    with open(ASSET, "w") as f:
        json.dump(db, f, separators=(",", ":"))

    write_provenance(db, hashlib.sha256(blob).hexdigest())
    ro = sum(1 for f in db["functions"] if f["a"]["stateMutability"] in READ_ONLY)
    print(f"wrote {ASSET} — {len(db['contracts'])} contracts, {len(db['functions'])} functions "
          f"({len(db['functions']) - ro} state-changing), {os.path.getsize(ASSET)} bytes")


if __name__ == "__main__":
    main()
