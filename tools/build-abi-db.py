#!/usr/bin/env python3
"""Compile keycard-tech/eth-abi-repo into the compact DB this module embeds.

Reads the upstream `repo/*.json` ABIs plus `abi_list.csv` (the only place contract
identity lives — upstream's own release build drops it) and writes
`rust-lib/assets/abi-db.json` + its PROVENANCE.md.

    ./tools/build-abi-db.py                  # fetch master tarball
    ./tools/build-abi-db.py --tarball x.tgz  # offline, from a saved tarball
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
        "stateMutability": func["stateMutability"],
    }


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


def build(csv_text, abis, upstream_rev):
    contracts, functions, order = [], {}, []
    unlisted = sorted(abis)

    def ingest(abi, contract_idx):
        for func in abi:
            if func.get("type") != "function" or func.get("stateMutability") not in KEPT_MUTABILITY:
                continue
            key = canonical_signature(func)
            entry = functions.get(key)
            if entry is None:
                entry = {"a": abi_entry(func), "c": []}
                functions[key] = entry
                order.append(key)
            if contract_idx is not None and contract_idx not in entry["c"]:
                entry["c"].append(contract_idx)

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

    # No CSV row means no identity, but the signatures still decode. Keep them
    # unattributed so they land in the advisory tier rather than vanishing.
    for name in unlisted:
        print(f"  · repo/{name}.json has no abi_list.csv row — kept unattributed", file=sys.stderr)
        ingest(abis[name], None)

    return {
        "schema": 1,
        "source": UPSTREAM,
        "upstream_rev": upstream_rev,
        "generated": date.today().isoformat(),
        "contracts": contracts,
        "functions": [functions[k] for k in order],
    }


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

`decimals`, where present, is the ONE field not from upstream: `abi_list.csv` has no
such column and an ABI does not carry decimals — reading them is a call to the live
contract, which this library never makes. Those rows are hand-checked and keyed by
`(chain, address)`, so a wrong address matches nothing rather than mislabelling another
token. A contract without the field renders raw units.

Entries are stored as ABI JSON rather than signature strings so nested tuple component
names survive; `alloy`'s human-readable parser cannot round-trip those. Selectors are
NOT stored — they are derived from the ABI at load time by the same keccak the decoder
uses, so a wrong selector here is not a failure mode.

Upstream is MIT-licensed, (c) 2025 Status Research & Development GmbH. Refresh with
`./tools/build-abi-db.py` and update every row above — the hashes are the point.
""")


def main():
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--tarball", help="read this tarball instead of fetching master")
    ap.add_argument("--rev", default="master", help="recorded upstream rev label")
    args = ap.parse_args()

    if args.tarball:
        blob = open(args.tarball, "rb").read()
    else:
        print(f"fetching {TARBALL}")
        blob = urllib.request.urlopen(TARBALL, timeout=120).read()

    csv_text, abis = load_tree(blob)
    print(f"read abi_list.csv + {len(abis)} ABI files")

    db = build(csv_text, abis, args.rev)
    os.makedirs(os.path.dirname(ASSET), exist_ok=True)
    with open(ASSET, "w") as f:
        json.dump(db, f, separators=(",", ":"))

    write_provenance(db, hashlib.sha256(blob).hexdigest())
    ro = sum(1 for f in db["functions"] if f["a"]["stateMutability"] in READ_ONLY)
    print(f"wrote {ASSET} — {len(db['contracts'])} contracts, {len(db['functions'])} functions "
          f"({len(db['functions']) - ro} state-changing), {os.path.getsize(ASSET)} bytes")


if __name__ == "__main__":
    main()
