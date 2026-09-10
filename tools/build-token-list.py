#!/usr/bin/env python3
"""Compile a token list into the compact registry this module embeds.

Reads a Uniswap-style token list document and writes `rust-lib/assets/token-list.json`
plus its PROVENANCE row set. The projection keeps only what naming and units need —
chain, address, symbol, name, decimals — which is a third of the source document.

    ./tools/build-token-list.py --from <path-or-url>

The default source is the same snapshot `token_list_module` embeds, so a decode and the
wallet's own token screen agree about what an address is called.
"""

import argparse
import hashlib
import json
import sys
import urllib.request
from datetime import date
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ASSET = ROOT / "rust-lib" / "assets" / "token-list.json"

DEFAULT_SOURCE = (
    "https://raw.githubusercontent.com/logos-co/logos-evm-token-list-module/main/"
    "rust-lib/assets/uniswap-default.json"
)


def load(src: str) -> dict:
    if src.startswith("http://") or src.startswith("https://"):
        with urllib.request.urlopen(src) as r:
            raw = r.read()
    else:
        raw = Path(src).read_bytes()
    return json.loads(raw), hashlib.sha256(raw).hexdigest(), len(raw)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--from", dest="src", default=DEFAULT_SOURCE)
    args = ap.parse_args()

    doc, src_sha, src_bytes = load(args.src)
    rows = doc.get("tokens", doc if isinstance(doc, list) else [])

    seen, out = set(), []
    for t in rows:
        try:
            chain, addr = int(t["chainId"]), str(t["address"])
            dec, sym = int(t["decimals"]), str(t["symbol"])
        except (KeyError, TypeError, ValueError):
            continue
        # A row without usable units is worse than no row: it would name an address and
        # then leave the amount unscaled, which reads as "we checked" when we did not.
        if not (0 <= dec <= 36) or not addr.startswith("0x") or len(addr) != 42:
            continue
        key = (chain, addr.lower())
        if key in seen:            # first wins; a list that contradicts itself names nothing
            continue
        seen.add(key)
        out.append({"chainId": chain, "address": addr, "symbol": sym,
                    "name": str(t.get("name", sym)), "decimals": dec})

    out.sort(key=lambda r: (r["chainId"], r["address"].lower()))
    asset = {
        "schema": 1,
        "source": args.src,
        "list_name": doc.get("name", "unknown"),
        "list_version": doc.get("version"),
        "generated": date.today().isoformat(),
        "tokens": out,
    }
    body = json.dumps(asset, separators=(",", ":"), sort_keys=False) + "\n"
    ASSET.write_text(body)

    chains = sorted({r["chainId"] for r in out})
    print(f"source            {args.src}")
    print(f"source bytes      {src_bytes}")
    print(f"source sha256     {src_sha}")
    print(f"list              {asset['list_name']} {asset['list_version']}")
    print(f"tokens            {len(out)} (from {len(rows)} rows, {len(rows) - len(out)} dropped)")
    print(f"chains            {len(chains)}")
    print(f"asset bytes       {len(body.encode())}")
    print(f"asset sha256      {hashlib.sha256(body.encode()).hexdigest()}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
